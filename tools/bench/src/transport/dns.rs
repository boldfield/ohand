//! Name resolution that the dispatch can abandon at any moment.
//!
//! System lookup (`getaddrinfo`) blocks a thread with no way to cancel it, so a stalled lookup
//! would outlive its dispatch. Instead this is a small stub resolver that runs on the calling
//! thread: `localhost` names map to loopback (RFC 6761), then the hosts file is consulted, then
//! A and AAAA queries go over UDP to the nameservers listed in the resolver configuration. Each
//! receive waits at most one poll step, so cancellation and both deadlines end the lookup within
//! a step and drop its socket. Truncated (TC) answers are refused rather than retried over TCP;
//! API endpoints answer well inside one datagram. Scoped resolver configuration (for example
//! split-horizon VPN DNS outside `resolv.conf`) is not consulted.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::error::HostTransportError;
use super::exchange::{Budget, SocketCounter, Tracked};

/// How long one nameserver gets to answer before the next is tried.
const NAMESERVER_WAIT: Duration = Duration::from_secs(2);
const DNS_PORT: u16 = 53;
const MAX_DATAGRAM: usize = 1232;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;
const FLAG_RESPONSE: u16 = 0x8000;
const FLAG_TRUNCATED: u16 = 0x0200;
const FLAG_RECURSION_DESIRED: u16 = 0x0100;
const RCODE_NAME_ERROR: u16 = 3;

/// Where names are looked up.
#[derive(Debug, Clone)]
pub(crate) struct NameService {
    hosts_path: PathBuf,
    nameservers: Nameservers,
}

#[derive(Debug, Clone)]
enum Nameservers {
    /// Read at each dispatch from a `resolv.conf`-format file.
    Configured(PathBuf),
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed(Vec<SocketAddr>),
}

impl Default for NameService {
    fn default() -> NameService {
        NameService {
            hosts_path: PathBuf::from("/etc/hosts"),
            nameservers: Nameservers::Configured(PathBuf::from("/etc/resolv.conf")),
        }
    }
}

impl NameService {
    /// A hosts file and an explicit nameserver list, for tests.
    #[cfg(test)]
    pub(crate) fn fixed(hosts_path: PathBuf, nameservers: Vec<SocketAddr>) -> NameService {
        NameService {
            hosts_path,
            nameservers: Nameservers::Fixed(nameservers),
        }
    }

    pub(crate) fn resolve(
        &self,
        name: &str,
        budget: &Budget<'_>,
        sockets: &SocketCounter,
    ) -> Result<Vec<IpAddr>, HostTransportError> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if name == "localhost" || name.ends_with(".localhost") {
            return Ok(vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ]);
        }
        let from_hosts = fs::read_to_string(&self.hosts_path)
            .map(|text| hosts_entries(&text, &name))
            .unwrap_or_default();
        if !from_hosts.is_empty() {
            return Ok(from_hosts);
        }
        let question = Question::new(&name)?;
        let nameservers = match &self.nameservers {
            Nameservers::Configured(path) => fs::read_to_string(path)
                .map(|text| configured_nameservers(&text))
                .unwrap_or_default(),
            Nameservers::Fixed(list) => list.clone(),
        };
        for nameserver in nameservers {
            match ask(nameserver, &question, budget, sockets)? {
                Answer::Addresses(addresses) if !addresses.is_empty() => return Ok(addresses),
                // The name has no address; another nameserver will not say otherwise.
                Answer::Addresses(_) | Answer::NoSuchName => {
                    return Err(HostTransportError::Unreachable)
                }
                Answer::Failed => {}
            }
        }
        Err(HostTransportError::Unreachable)
    }
}

fn hosts_entries(text: &str, name: &str) -> Vec<IpAddr> {
    let mut addresses = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let mut fields = line.split_whitespace();
        let Some(Ok(address)) = fields.next().map(str::parse::<IpAddr>) else {
            continue;
        };
        if fields.any(|alias| alias.eq_ignore_ascii_case(name)) && !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    addresses
}

fn configured_nameservers(text: &str) -> Vec<SocketAddr> {
    text.lines()
        .filter_map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                ["nameserver", address, ..] => Some(address),
                _ => None,
            },
        )
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .map(|address| SocketAddr::new(address, DNS_PORT))
        .collect()
}

/// The encoded question section for a name.
struct Question {
    encoded_name: Vec<u8>,
}

impl Question {
    fn new(name: &str) -> Result<Question, HostTransportError> {
        if name.is_empty() || name.len() > 253 {
            return Err(HostTransportError::InvalidRequest);
        }
        let mut encoded_name = Vec::with_capacity(name.len() + 2);
        for label in name.split('.') {
            let valid = (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
            if !valid {
                return Err(HostTransportError::InvalidRequest);
            }
            encoded_name.push(label.len() as u8);
            encoded_name.extend_from_slice(label.as_bytes());
        }
        encoded_name.push(0);
        Ok(Question { encoded_name })
    }

    fn section(&self, record_type: u16) -> Vec<u8> {
        let mut section = self.encoded_name.clone();
        section.extend_from_slice(&record_type.to_be_bytes());
        section.extend_from_slice(&CLASS_IN.to_be_bytes());
        section
    }

    fn query(&self, id: u16, record_type: u16) -> Vec<u8> {
        let mut message = Vec::with_capacity(12 + self.encoded_name.len() + 4);
        message.extend_from_slice(&id.to_be_bytes());
        message.extend_from_slice(&FLAG_RECURSION_DESIRED.to_be_bytes());
        message.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
        message.extend_from_slice(&self.section(record_type));
        message
    }
}

enum Answer {
    Addresses(Vec<IpAddr>),
    NoSuchName,
    Failed,
}

/// One nameserver, both record types, over one connected UDP socket.
fn ask(
    nameserver: SocketAddr,
    question: &Question,
    budget: &Budget<'_>,
    sockets: &SocketCounter,
) -> Result<Answer, HostTransportError> {
    budget.check()?;
    let local: SocketAddr = if nameserver.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let Ok(socket) = UdpSocket::bind(local) else {
        return Ok(Answer::Failed);
    };
    let socket = Tracked::new(socket, sockets);
    let id_bytes = *Uuid::new_v4().as_bytes();
    let pending_ids = [
        u16::from_be_bytes([id_bytes[0], id_bytes[1]]),
        u16::from_be_bytes([id_bytes[2], id_bytes[3]]),
    ];
    let types = [TYPE_A, TYPE_AAAA];
    if socket.connect(nameserver).is_err() {
        return Ok(Answer::Failed);
    }
    for (id, record_type) in pending_ids.iter().zip(types) {
        if socket.send(&question.query(*id, record_type)).is_err() {
            return Ok(Answer::Failed);
        }
    }

    let given_up_at = Instant::now() + NAMESERVER_WAIT;
    let mut answered = [false, false];
    let mut addresses = Vec::new();
    let mut buffer = [0u8; MAX_DATAGRAM];
    while answered.contains(&false) {
        budget.check()?;
        let now = Instant::now();
        if now >= given_up_at {
            return Ok(Answer::Failed);
        }
        let wait = budget.step().min(given_up_at - now);
        if socket.set_read_timeout(Some(wait)).is_err() {
            return Ok(Answer::Failed);
        }
        let length = match socket.recv(&mut buffer) {
            Ok(length) => length,
            Err(error) if is_wait_over(&error) => continue,
            Err(_) => return Ok(Answer::Failed),
        };
        let reply = &buffer[..length];
        for (slot, (id, record_type)) in pending_ids.iter().zip(types).enumerate() {
            if answered[slot] {
                continue;
            }
            match parse_reply(reply, *id, &question.section(record_type)) {
                Reply::NotOurs => {}
                Reply::Addresses(found) => {
                    answered[slot] = true;
                    addresses.extend(found);
                }
                Reply::NoSuchName => return Ok(Answer::NoSuchName),
                Reply::Failed => return Ok(Answer::Failed),
            }
        }
    }
    Ok(Answer::Addresses(addresses))
}

pub(crate) fn is_wait_over(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

enum Reply {
    NotOurs,
    Addresses(Vec<IpAddr>),
    NoSuchName,
    Failed,
}

fn read_u16(message: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *message.get(at)?,
        *message.get(at + 1)?,
    ]))
}

/// Position just past an encoded (possibly compressed) name.
fn skip_name(message: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *message.get(at)?;
        match length {
            0 => return Some(at + 1),
            _ if length & 0xC0 == 0xC0 => return Some(at + 2),
            _ if length & 0xC0 != 0 => return None,
            _ => at += 1 + length as usize,
        }
    }
}

fn parse_reply(message: &[u8], id: u16, question_section: &[u8]) -> Reply {
    let Some(reply_id) = read_u16(message, 0) else {
        return Reply::NotOurs;
    };
    let question_end = 12 + question_section.len();
    if reply_id != id || message.get(12..question_end) != Some(question_section) {
        return Reply::NotOurs;
    }
    let (Some(flags), Some(question_count), Some(answer_count)) = (
        read_u16(message, 2),
        read_u16(message, 4),
        read_u16(message, 6),
    ) else {
        return Reply::Failed;
    };
    if flags & FLAG_RESPONSE == 0 || flags & FLAG_TRUNCATED != 0 || question_count != 1 {
        return Reply::Failed;
    }
    match flags & 0x000F {
        0 => {}
        RCODE_NAME_ERROR => return Reply::NoSuchName,
        _ => return Reply::Failed,
    }
    parse_answers(message, question_end, answer_count).map_or(Reply::Failed, Reply::Addresses)
}

/// Accepts every A/AAAA record in the answer section whatever its owner name, so a CNAME
/// chain's final addresses are used. A wrong address is caught by certificate verification
/// before any request or credential is written.
fn parse_answers(message: &[u8], mut at: usize, answer_count: u16) -> Option<Vec<IpAddr>> {
    let mut addresses = Vec::new();
    for _ in 0..answer_count {
        at = skip_name(message, at)?;
        let record_type = read_u16(message, at)?;
        let class = read_u16(message, at + 2)?;
        let data_length = read_u16(message, at + 8)? as usize;
        let data = message.get(at + 10..at + 10 + data_length)?;
        at += 10 + data_length;
        if class != CLASS_IN {
            continue;
        }
        match (record_type, data.len()) {
            (TYPE_A, 4) => addresses.push(IpAddr::V4(Ipv4Addr::new(
                data[0], data[1], data[2], data[3],
            ))),
            (TYPE_AAAA, 16) => {
                let octets: [u8; 16] = data.try_into().ok()?;
                addresses.push(IpAddr::V6(Ipv6Addr::from(octets)));
            }
            _ => {}
        }
    }
    Some(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_entries_match_any_alias_and_skip_comments() {
        let text = "# comment 10.0.0.9 bench.test\n\
                    10.0.0.1 other.test bench.test # trailing\n\
                    not-an-address bench.test\n\
                    fd00::1\tBENCH.test\n";
        assert_eq!(
            hosts_entries(text, "bench.test"),
            vec![
                "10.0.0.1".parse::<IpAddr>().unwrap(),
                "fd00::1".parse().unwrap()
            ]
        );
        assert!(hosts_entries(text, "missing.test").is_empty());
    }

    #[test]
    fn nameservers_are_read_from_resolver_configuration() {
        let text = "search example.test\nnameserver 192.0.2.53\nnameserver ::1\nnameserver bad\n";
        assert_eq!(
            configured_nameservers(text),
            vec![
                "192.0.2.53:53".parse::<SocketAddr>().unwrap(),
                "[::1]:53".parse().unwrap()
            ]
        );
    }

    #[test]
    fn names_that_cannot_be_encoded_are_invalid() {
        for name in ["", "a..b", "spaced name.test", &"x".repeat(64)] {
            assert!(Question::new(name).is_err(), "{name}");
        }
        assert!(Question::new("api.bench.test").is_ok());
    }

    fn reply_for(query: &[u8], flags: u16, answers: &[(u16, &[u8])]) -> Vec<u8> {
        let mut reply = query.to_vec();
        reply[2..4].copy_from_slice(&flags.to_be_bytes());
        reply[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
        for (record_type, data) in answers {
            reply.extend_from_slice(&[0xC0, 12]);
            reply.extend_from_slice(&record_type.to_be_bytes());
            reply.extend_from_slice(&CLASS_IN.to_be_bytes());
            reply.extend_from_slice(&[0, 0, 0, 60]);
            reply.extend_from_slice(&(data.len() as u16).to_be_bytes());
            reply.extend_from_slice(data);
        }
        reply
    }

    #[test]
    fn replies_are_matched_to_their_query_and_parsed_strictly() {
        let question = Question::new("api.bench.test").unwrap();
        let query = question.query(0x1234, TYPE_A);
        let section = question.section(TYPE_A);

        let good = reply_for(
            &query,
            0x8180,
            &[(5, b"\x03cdn\x00"), (TYPE_A, &[192, 0, 2, 7])],
        );
        assert!(matches!(
            parse_reply(&good, 0x1234, &section),
            Reply::Addresses(found) if found == vec!["192.0.2.7".parse::<IpAddr>().unwrap()]
        ));
        assert!(matches!(
            parse_reply(&good, 0x4321, &section),
            Reply::NotOurs
        ));
        assert!(matches!(
            parse_reply(&good, 0x1234, &question.section(TYPE_AAAA)),
            Reply::NotOurs
        ));
        let truncated = reply_for(&query, 0x8380, &[]);
        assert!(matches!(
            parse_reply(&truncated, 0x1234, &section),
            Reply::Failed
        ));
        let missing = reply_for(&query, 0x8183, &[]);
        assert!(matches!(
            parse_reply(&missing, 0x1234, &section),
            Reply::NoSuchName
        ));
        let mut cut = good.clone();
        cut.truncate(good.len() - 2);
        assert!(matches!(parse_reply(&cut, 0x1234, &section), Reply::Failed));
    }
}
