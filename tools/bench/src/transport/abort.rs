//! Per-dispatch teardown and late credential release.
//!
//! One [`AbortHandle`] ties the waiting caller to the worker's connection. Cancellation and
//! expiry shut down every registered socket, interrupt a pending DNS wait, and drop the
//! credential. The credential is held on the caller's side and only released to the worker once
//! a TCP connection exists that the caller can tear down and the TLS handshake has completed;
//! until then the worker holds a random placeholder, never secret bytes.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ureq::{Error, ReadWrite, Resolver, TlsConnector};
use zeroize::Zeroizing;

use super::credential::SecretValue;

/// How often a blocked name lookup re-checks whether the dispatch has ended.
const LOOKUP_POLL: Duration = Duration::from_millis(5);

pub(crate) type Lookup = dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync;

pub(crate) fn system_lookup(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    std::net::ToSocketAddrs::to_socket_addrs(netloc).map(|addresses| addresses.collect())
}

/// A resolved secret and the full header value built from it.
pub(crate) struct HeldSecret {
    pub(crate) header_value: Zeroizing<String>,
    pub(crate) secret: SecretValue,
}

#[derive(Default)]
struct AbortState {
    aborted: bool,
    sockets: Vec<TcpStream>,
    unreleased: Option<Arc<HeldSecret>>,
    released: Option<Arc<HeldSecret>>,
}

/// Shared between the waiting caller and the worker's connector for exactly one dispatch.
#[derive(Default)]
pub(crate) struct AbortHandle {
    state: Mutex<AbortState>,
}

impl AbortHandle {
    fn locked(&self) -> std::sync::MutexGuard<'_, AbortState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Parks the credential until the connection is ready to carry it.
    pub(crate) fn hold(&self, held: HeldSecret) {
        let mut state = self.locked();
        if !state.aborted {
            state.unreleased = Some(Arc::new(held));
        }
    }

    /// Ends the dispatch: shuts down every connection it opened, and any it opens later, and
    /// drops the credential from the caller's side.
    pub(crate) fn abort(&self) {
        let mut state = self.locked();
        state.aborted = true;
        state.unreleased = None;
        state.released = None;
        for socket in state.sockets.drain(..) {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    pub(crate) fn is_aborted(&self) -> bool {
        self.locked().aborted
    }

    fn register(&self, socket: &TcpStream) {
        let mut state = self.locked();
        if state.aborted {
            let _ = socket.shutdown(Shutdown::Both);
        } else if let Ok(clone) = socket.try_clone() {
            state.sockets.push(clone);
        }
    }

    /// Hands the credential to the connection; `None` once the dispatch has ended.
    fn release(&self) -> Option<Arc<HeldSecret>> {
        let mut state = self.locked();
        if state.aborted {
            return None;
        }
        let held = state.unreleased.take()?;
        state.released = Some(Arc::clone(&held));
        Some(held)
    }

    /// The credential, if it was released to a connection and the dispatch is still live.
    pub(crate) fn released(&self) -> Option<Arc<HeldSecret>> {
        self.locked().released.clone()
    }

    #[cfg(test)]
    pub(crate) fn holds_credential(&self) -> bool {
        let state = self.locked();
        state.unreleased.is_some() || state.released.is_some()
    }
}

/// Name lookup that gives up as soon as the dispatch ends. The lookup itself runs on a
/// disposable thread that never sees a credential.
pub(crate) struct AbortableResolver {
    pub(crate) lookup: Arc<Lookup>,
    pub(crate) abort: Arc<AbortHandle>,
}

impl Resolver for AbortableResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let ended = || io::Error::new(io::ErrorKind::ConnectionAborted, "dispatch ended");
        if self.abort.is_aborted() {
            return Err(ended());
        }
        let (sender, receiver) = mpsc::channel();
        let lookup = Arc::clone(&self.lookup);
        let netloc = netloc.to_string();
        thread::spawn(move || {
            let _ = sender.send(lookup(&netloc));
        });
        loop {
            match receiver.recv_timeout(LOOKUP_POLL) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) if self.abort.is_aborted() => return Err(ended()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("name lookup failed"))
                }
            }
        }
    }
}

/// Registers each TCP connection with the dispatch's [`AbortHandle`] before any TLS bytes are
/// exchanged, hands it to the verifying rustls connector, and, when the request carries a
/// credential, wraps the finished TLS stream so the credential is written only into the
/// request head on that stream.
pub(crate) struct AbortableTls {
    pub(crate) config: Arc<rustls::ClientConfig>,
    pub(crate) abort: Arc<AbortHandle>,
    pub(crate) placeholder: Option<String>,
}

impl TlsConnector for AbortableTls {
    fn connect(&self, dns_name: &str, io: Box<dyn ReadWrite>) -> Result<Box<dyn ReadWrite>, Error> {
        if let Some(socket) = io.socket() {
            self.abort.register(socket);
        }
        let stream = TlsConnector::connect(&self.config, dns_name, io)?;
        Ok(match &self.placeholder {
            Some(placeholder) => Box::new(LateCredentialStream {
                inner: stream,
                placeholder: placeholder.clone().into_bytes(),
                abort: Arc::clone(&self.abort),
                pending: Vec::new(),
                head_sent: false,
            }),
            None => stream,
        })
    }
}

const HEAD_END: &[u8] = b"\r\n\r\n";

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Holds back the request head until it is complete, swaps the placeholder for the credential's
/// header value, and then passes everything through unchanged.
struct LateCredentialStream {
    inner: Box<dyn ReadWrite>,
    placeholder: Vec<u8>,
    abort: Arc<AbortHandle>,
    pending: Vec<u8>,
    head_sent: bool,
}

impl fmt::Debug for LateCredentialStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LateCredentialStream")
    }
}

impl Read for LateCredentialStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Write for LateCredentialStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.head_sent {
            return self.inner.write(buffer);
        }
        self.pending.extend_from_slice(buffer);
        let Some(head_end) = find(&self.pending, HEAD_END).map(|at| at + HEAD_END.len()) else {
            return Ok(buffer.len());
        };
        let held = self
            .abort
            .release()
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "dispatch ended"))?;
        let placeholder_at =
            find(&self.pending[..head_end], &self.placeholder).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "credential slot missing")
            })?;
        let value = held.header_value.as_bytes();
        let mut outgoing = Zeroizing::new(Vec::with_capacity(self.pending.len() + value.len()));
        outgoing.extend_from_slice(&self.pending[..placeholder_at]);
        outgoing.extend_from_slice(value);
        outgoing.extend_from_slice(&self.pending[placeholder_at + self.placeholder.len()..]);
        self.pending.clear();
        self.head_sent = true;
        self.inner.write_all(&outgoing)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.head_sent {
            self.inner.flush()
        } else {
            Ok(())
        }
    }
}

impl ReadWrite for LateCredentialStream {
    fn socket(&self) -> Option<&TcpStream> {
        self.inner.socket()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Wire(Arc<Mutex<Vec<u8>>>);

    impl fmt::Debug for Wire {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("Wire")
        }
    }

    impl Read for Wire {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for Wire {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl ReadWrite for Wire {
        fn socket(&self) -> Option<&TcpStream> {
            None
        }
    }

    const SLOT: &str = "bench-slot-0123456789abcdef";

    fn held(value: &str) -> HeldSecret {
        HeldSecret {
            header_value: Zeroizing::new(format!("Bearer {value}")),
            secret: SecretValue::from_bytes(value.as_bytes().to_vec()).unwrap(),
        }
    }

    fn late_stream(abort: &Arc<AbortHandle>) -> (LateCredentialStream, Wire) {
        let wire = Wire::default();
        let stream = LateCredentialStream {
            inner: Box::new(wire.clone()),
            placeholder: SLOT.as_bytes().to_vec(),
            abort: Arc::clone(abort),
            pending: Vec::new(),
            head_sent: false,
        };
        (stream, wire)
    }

    fn sent(wire: &Wire) -> String {
        String::from_utf8(wire.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn the_credential_replaces_the_placeholder_only_once_the_head_is_complete() {
        let abort = Arc::new(AbortHandle::default());
        abort.hold(held("CANARY-late"));
        let (mut stream, wire) = late_stream(&abort);

        let head = format!("POST / HTTP/1.1\r\nx-api-key: {SLOT}\r\nhost: h\r\n\r\n");
        let (first, second) = head.as_bytes().split_at(30);
        stream.write_all(first).unwrap();
        assert_eq!(sent(&wire), "");
        assert!(abort.released().is_none());

        stream.write_all(second).unwrap();
        stream.write_all(b"body").unwrap();
        assert_eq!(
            sent(&wire),
            "POST / HTTP/1.1\r\nx-api-key: Bearer CANARY-late\r\nhost: h\r\n\r\nbody"
        );
        assert!(abort.released().is_some());
    }

    #[test]
    fn an_aborted_dispatch_never_releases_the_credential_to_the_connection() {
        let abort = Arc::new(AbortHandle::default());
        abort.hold(held("CANARY-late"));
        let (mut stream, wire) = late_stream(&abort);
        abort.abort();

        let head = format!("POST / HTTP/1.1\r\nx-api-key: {SLOT}\r\n\r\n");
        let error = stream.write_all(head.as_bytes()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(sent(&wire), "");
        assert!(!abort.holds_credential());
        assert!(abort.released().is_none());
    }

    #[test]
    fn aborting_drops_a_credential_that_was_never_released() {
        let abort = AbortHandle::default();
        abort.hold(held("CANARY-late"));
        assert!(abort.holds_credential());
        abort.abort();
        assert!(!abort.holds_credential());
        abort.hold(held("CANARY-later"));
        assert!(!abort.holds_credential());
    }
}
