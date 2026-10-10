//! One HTTP/1.1 exchange over verified TLS, driven entirely on the calling thread.
//!
//! Nothing here blocks for longer than one poll step: the TCP connect is non-blocking and
//! polled, and every socket read or write has a short timeout after which the dispatch's
//! [`Budget`] is checked again. Cancellation or either deadline therefore ends every phase
//! (connect, handshake, request, response) within one step. When the caller gets its result,
//! the sockets are closed and no thread or connection is left running.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ohand_core::providers::contracts::CancelToken;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection};
use socket2::{Domain, Protocol, Socket, Type};

use super::dns::is_wait_over;
use super::error::HostTransportError;
use super::host::ClockDeadline;

/// The longest any single wait lasts before cancellation and deadlines are checked again.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How often a pending TCP connect is re-examined.
const CONNECT_POLL: Duration = Duration::from_millis(2);
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
const MAX_CHUNK_LINE: usize = 4096;
const HEAD_END: &[u8] = b"\r\n\r\n";
const LINE_END: &[u8] = b"\r\n";

/// The time a dispatch may still take and whether it was cancelled.
pub(crate) struct Budget<'a> {
    cancel: &'a CancelToken,
    until: Instant,
    clock_deadline: Option<ClockDeadline<'a>>,
}

impl<'a> Budget<'a> {
    pub(crate) fn new(
        cancel: &'a CancelToken,
        timeout: Duration,
        clock_deadline: Option<ClockDeadline<'a>>,
    ) -> Budget<'a> {
        Budget {
            cancel,
            until: Instant::now() + timeout,
            clock_deadline,
        }
    }

    pub(crate) fn check(&self) -> Result<(), HostTransportError> {
        if self.cancel.is_cancelled() {
            return Err(HostTransportError::Cancelled);
        }
        if Instant::now() >= self.until || clock_expired(self.clock_deadline) {
            return Err(HostTransportError::Timeout);
        }
        Ok(())
    }

    /// How long the next wait may last.
    pub(crate) fn step(&self) -> Duration {
        POLL_INTERVAL
            .min(self.until.saturating_duration_since(Instant::now()))
            .max(Duration::from_millis(1))
    }
}

pub(crate) fn clock_expired(clock_deadline: Option<ClockDeadline<'_>>) -> bool {
    clock_deadline.is_some_and(|deadline| deadline.clock.now_ms() >= deadline.deadline_ms)
}

/// Counts the sockets a transport currently has open, so tests can prove that none outlive
/// the dispatch that opened them.
pub(crate) type SocketCounter = Arc<AtomicUsize>;

struct Counted(SocketCounter);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A socket that is counted for as long as it is open.
pub(crate) struct Tracked<S> {
    socket: S,
    count: Counted,
}

impl<S> Tracked<S> {
    pub(crate) fn new(socket: S, counter: &SocketCounter) -> Tracked<S> {
        counter.fetch_add(1, Ordering::SeqCst);
        Tracked {
            socket,
            count: Counted(Arc::clone(counter)),
        }
    }
}

impl<S> Deref for Tracked<S> {
    type Target = S;

    fn deref(&self) -> &S {
        &self.socket
    }
}

impl<S> DerefMut for Tracked<S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self.socket
    }
}

/// Connects to the first address that accepts, within the budget.
pub(crate) fn connect(
    addresses: &[SocketAddr],
    budget: &Budget<'_>,
    sockets: &SocketCounter,
) -> Result<Tracked<TcpStream>, HostTransportError> {
    for address in addresses {
        if let Some(stream) = connect_one(*address, budget, sockets)? {
            return Ok(stream);
        }
    }
    Err(HostTransportError::Unreachable)
}

fn is_in_progress(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EINPROGRESS)
}

fn connect_one(
    address: SocketAddr,
    budget: &Budget<'_>,
    sockets: &SocketCounter,
) -> Result<Option<Tracked<TcpStream>>, HostTransportError> {
    budget.check()?;
    let Ok(socket) = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    ) else {
        return Ok(None);
    };
    let Tracked { socket, count } = Tracked::new(socket, sockets);
    if socket.set_nonblocking(true).is_err() {
        return Ok(None);
    }
    match socket.connect(&address.into()) {
        Ok(()) => {}
        Err(error) if is_in_progress(&error) => loop {
            budget.check()?;
            match socket.take_error() {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => return Ok(None),
            }
            if socket.peer_addr().is_ok() {
                break;
            }
            thread::sleep(CONNECT_POLL.min(budget.step()));
        },
        Err(_) => return Ok(None),
    }
    let stream: TcpStream = socket.into();
    let configured = stream.set_nonblocking(false).is_ok()
        && stream.set_nodelay(true).is_ok()
        && stream.set_read_timeout(Some(POLL_INTERVAL)).is_ok()
        && stream.set_write_timeout(Some(POLL_INTERVAL)).is_ok();
    Ok(configured.then_some(Tracked {
        socket: stream,
        count,
    }))
}

fn classify_tls(error: &rustls::Error) -> HostTransportError {
    match error {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
            HostTransportError::TlsVerificationFailed
        }
        _ => HostTransportError::Unreachable,
    }
}

/// A verified TLS connection plus the plaintext received on it and not yet consumed.
pub(crate) struct Session<'a> {
    tcp: Tracked<TcpStream>,
    tls: ClientConnection,
    budget: &'a Budget<'a>,
    received: Vec<u8>,
    tcp_closed: bool,
    /// The server ended the TLS stream with close_notify, so nothing was cut off in transit.
    close_notified: bool,
}

impl<'a> Session<'a> {
    /// Completes the handshake, including certificate verification, before returning.
    pub(crate) fn open(
        tcp: Tracked<TcpStream>,
        config: Arc<ClientConfig>,
        server_name: ServerName<'static>,
        budget: &'a Budget<'a>,
    ) -> Result<Session<'a>, HostTransportError> {
        let tls =
            ClientConnection::new(config, server_name).map_err(|_| HostTransportError::Setup)?;
        let mut session = Session {
            tcp,
            tls,
            budget,
            received: Vec::new(),
            tcp_closed: false,
            close_notified: false,
        };
        while session.tls.is_handshaking() {
            if session.tcp_closed {
                return Err(HostTransportError::Unreachable);
            }
            session.exchange_records()?;
        }
        Ok(session)
    }

    /// Moves TLS records one step: sends pending records if there are any, otherwise waits up
    /// to one poll step for incoming records and processes them.
    fn exchange_records(&mut self) -> Result<(), HostTransportError> {
        self.budget.check()?;
        if self.tls.wants_write() {
            return match self.tls.write_tls(&mut *self.tcp) {
                Ok(_) => Ok(()),
                Err(error) if is_wait_over(&error) => Ok(()),
                Err(_) => Err(HostTransportError::Unreachable),
            };
        }
        match self.tls.read_tls(&mut *self.tcp) {
            Ok(0) => self.tcp_closed = true,
            Ok(_) => {}
            Err(error) if is_wait_over(&error) => return Ok(()),
            Err(_) => return Err(HostTransportError::Unreachable),
        }
        self.tls
            .process_new_packets()
            .map(|_| ())
            .map_err(|error| classify_tls(&error))
    }

    pub(crate) fn write_all(&mut self, data: &[u8]) -> Result<(), HostTransportError> {
        let mut written = 0;
        while written < data.len() {
            self.budget.check()?;
            written += self
                .tls
                .writer()
                .write(&data[written..])
                .map_err(|_| HostTransportError::Unreachable)?;
            if self.tls.wants_write() {
                self.exchange_records()?;
            }
        }
        while self.tls.wants_write() {
            self.exchange_records()?;
        }
        Ok(())
    }

    /// Receives more plaintext; `false` once the server has closed the connection.
    fn fill(&mut self) -> Result<bool, HostTransportError> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            self.budget.check()?;
            match self.tls.reader().read(&mut chunk) {
                Ok(0) => {
                    self.close_notified = true;
                    return Ok(false);
                }
                Ok(count) => {
                    self.received.extend_from_slice(&chunk[..count]);
                    return Ok(true);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                // Closed without close_notify; framing decides whether that lost data.
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(_) => return Err(HostTransportError::Unreachable),
            }
            if self.tcp_closed {
                return Ok(false);
            }
            self.exchange_records()?;
        }
    }

    /// Bytes up to (not including) `delimiter`, consuming both.
    fn take_through(
        &mut self,
        delimiter: &[u8],
        max_len: usize,
    ) -> Result<Vec<u8>, HostTransportError> {
        loop {
            if let Some(position) = find(&self.received, delimiter) {
                let mut taken: Vec<u8> =
                    self.received.drain(..position + delimiter.len()).collect();
                taken.truncate(position);
                return Ok(taken);
            }
            if self.received.len() > max_len || !self.fill()? {
                return Err(HostTransportError::Unreachable);
            }
        }
    }

    /// Appends up to `count` body bytes; `false` if the connection ended first.
    fn take_exact(&mut self, count: usize, body: &mut Vec<u8>) -> Result<bool, HostTransportError> {
        let target = body.len() + count;
        while body.len() < target {
            if self.received.is_empty() && !self.fill()? {
                return Ok(false);
            }
            let take = self.received.len().min(target - body.len());
            body.extend(self.received.drain(..take));
        }
        Ok(true)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(crate) struct RawResponse {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    /// At most `read_limit` bytes of the decoded body.
    pub(crate) body: Vec<u8>,
}

enum Framing {
    Empty,
    Length(usize),
    Chunked,
    UntilClose,
}

/// Reads the final response head and at most `read_limit` body bytes. A redirect's body is
/// never read.
pub(crate) fn read_response(
    session: &mut Session<'_>,
    read_limit: usize,
) -> Result<RawResponse, HostTransportError> {
    let (status, headers) = loop {
        let head = session.take_through(HEAD_END, MAX_HEAD_BYTES)?;
        let (status, headers) = parse_head(&head)?;
        match status {
            101 => return Err(HostTransportError::Unreachable),
            100..=199 => continue,
            _ => break (status, headers),
        }
    };
    let mut body = Vec::new();
    match framing(status, &headers)? {
        Framing::Empty => {}
        Framing::Length(length) => {
            if !session.take_exact(length.min(read_limit), &mut body)? {
                return Err(HostTransportError::Unreachable);
            }
        }
        Framing::UntilClose => loop {
            let wanted = (read_limit - body.len()).min(session.received.len());
            body.extend(session.received.drain(..wanted));
            if body.len() >= read_limit {
                break;
            }
            if !session.fill()? {
                // Without close_notify the end of the body cannot be told from a truncation.
                if !session.close_notified {
                    return Err(HostTransportError::Unreachable);
                }
                break;
            }
        },
        Framing::Chunked => loop {
            let line = session.take_through(LINE_END, MAX_CHUNK_LINE)?;
            let size_text = line.split(|byte| *byte == b';').next().unwrap_or_default();
            let size = std::str::from_utf8(size_text)
                .ok()
                .and_then(|text| usize::from_str_radix(text.trim(), 16).ok())
                .ok_or(HostTransportError::Unreachable)?;
            if size == 0 {
                break;
            }
            let wanted = size.min(read_limit - body.len());
            if !session.take_exact(wanted, &mut body)? {
                return Err(HostTransportError::Unreachable);
            }
            if body.len() >= read_limit {
                break;
            }
            if !session.take_through(LINE_END, LINE_END.len())?.is_empty() {
                return Err(HostTransportError::Unreachable);
            }
        },
    }
    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

type Headers = Vec<(String, String)>;

fn parse_head(head: &[u8]) -> Result<(u16, Headers), HostTransportError> {
    let mut complete = head.to_vec();
    complete.extend_from_slice(HEAD_END);
    let mut slots = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut response = httparse::Response::new(&mut slots);
    match response.parse(&complete) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(HostTransportError::Unreachable),
    }
    let status = response.code.ok_or(HostTransportError::Unreachable)?;
    let headers = response
        .headers
        .iter()
        .map(|header| {
            (
                header.name.to_ascii_lowercase(),
                String::from_utf8_lossy(header.value).into_owned(),
            )
        })
        .collect();
    Ok((status, headers))
}

fn framing(status: u16, headers: &Headers) -> Result<Framing, HostTransportError> {
    if (300..400).contains(&status) || status == 204 || status == 304 {
        return Ok(Framing::Empty);
    }
    let mut transfer_codings = headers
        .iter()
        .filter(|(name, _)| name == "transfer-encoding")
        .flat_map(|(_, value)| value.split(','))
        .map(|coding| coding.trim().to_ascii_lowercase())
        .peekable();
    if transfer_codings.peek().is_some() {
        return Ok(match transfer_codings.last().as_deref() {
            Some("chunked") => Framing::Chunked,
            _ => Framing::UntilClose,
        });
    }
    let mut length = None;
    for (_, value) in headers.iter().filter(|(name, _)| name == "content-length") {
        let parsed: usize = value
            .trim()
            .parse()
            .map_err(|_| HostTransportError::Unreachable)?;
        if length.is_some_and(|earlier| earlier != parsed) {
            return Err(HostTransportError::Unreachable);
        }
        length = Some(parsed);
    }
    Ok(length.map_or(Framing::UntilClose, Framing::Length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn framing_follows_transfer_coding_then_length() {
        assert!(matches!(
            framing(200, &headers(&[("transfer-encoding", "gzip, chunked")])),
            Ok(Framing::Chunked)
        ));
        assert!(matches!(
            framing(200, &headers(&[("transfer-encoding", "gzip")])),
            Ok(Framing::UntilClose)
        ));
        assert!(matches!(
            framing(200, &headers(&[("content-length", "12")])),
            Ok(Framing::Length(12))
        ));
        assert!(matches!(
            framing(
                200,
                &headers(&[("content-length", "1"), ("content-length", "2")])
            ),
            Err(HostTransportError::Unreachable)
        ));
        assert!(matches!(
            framing(200, &headers(&[("content-length", "-1")])),
            Err(HostTransportError::Unreachable)
        ));
        assert!(matches!(
            framing(302, &headers(&[("content-length", "5")])),
            Ok(Framing::Empty)
        ));
        assert!(matches!(framing(200, &[].into()), Ok(Framing::UntilClose)));
    }

    #[test]
    fn heads_are_parsed_strictly() {
        let (status, parsed) = parse_head(b"HTTP/1.1 429 Slow Down\r\nRetry-After: 3").unwrap();
        assert_eq!(status, 429);
        assert_eq!(parsed, headers(&[("retry-after", "3")]));
        assert!(parse_head(b"NOT HTTP\r\n").is_err());
    }
}
