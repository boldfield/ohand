//! The single secure host HTTP effect.
//!
//! One POST per call with: https only and certificate validation against fixed roots, an
//! explicit destination/credential grant list, no redirects followed (a 3xx is returned as a
//! bare status), a bounded response body, a real-time budget plus an optional dispatch-clock
//! deadline, and cancellation. The whole exchange (name lookup, connect, handshake, request,
//! response) runs on the calling thread in waits of at most one poll step, so cancellation and
//! expiry end it promptly and, when `send` returns, no thread, socket or copy of the secret
//! from this call is left behind. The secret is resolved at dispatch after the destination
//! check, written only onto an established, verified TLS stream, and masked out of everything
//! returned.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicUsize;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ohand_core::providers::contracts::{CancelToken, Clock};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use url::{Host, Url};
use zeroize::Zeroizing;

use super::credential::{CredentialResolver, SecretValue};
use super::dns::NameService;
use super::error::HostTransportError;
use super::exchange::{self, clock_expired, Budget, Session, SocketCounter};
use super::policy::{parse_https_url, DestinationPolicy};
use super::redact::mask_secret;

const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-connection",
    "upgrade",
    "te",
    "trailer",
    "expect",
];

/// Names that carry credentials; they may only be written by the attachment rule.
const CREDENTIAL_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "api-key",
];

/// Where and how the resolved secret is written: `header: <scheme> <secret>` or, without a
/// scheme, `header: <secret>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRule {
    pub header: String,
    pub scheme: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CredentialUse {
    pub reference: String,
    pub rule: AttachmentRule,
}

/// The credential reference names private configuration and is left out.
impl std::fmt::Debug for CredentialUse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialUse")
            .field("rule", &self.rule)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct SecureRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub credential: Option<CredentialUse>,
}

impl std::fmt::Debug for SecureRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecureRequest")
            .field("header_count", &self.headers.len())
            .field("body_len", &self.body.len())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecureResponse {
    pub status: u16,
    /// Lowercase names with verbatim values. Any occurrence of the dispatched secret in a name
    /// or a value is masked before the response is returned.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The body reached the size bound: it holds exactly `max_response_bytes + 1` bytes and
    /// the remainder was not read.
    pub truncated: bool,
}

impl std::fmt::Debug for SecureResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecureResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

/// Dispatch-clock deadline, for callers whose budget is expressed on an injected clock.
#[derive(Clone, Copy)]
pub struct ClockDeadline<'a> {
    pub clock: &'a dyn Clock,
    pub deadline_ms: u64,
}

pub struct HostTransportBuilder {
    policy: DestinationPolicy,
    resolver: Arc<dyn CredentialResolver>,
    extra_roots: Vec<Vec<u8>>,
    name_service: NameService,
}

impl HostTransportBuilder {
    /// Replaces the system hosts file and nameservers so tests can stall or answer name
    /// lookups deterministically.
    #[cfg(test)]
    pub(crate) fn name_service(mut self, name_service: NameService) -> HostTransportBuilder {
        self.name_service = name_service;
        self
    }

    /// Trusts one more root certificate (DER), in addition to the bundled public roots.
    pub fn trust_root_der(mut self, certificate_der: Vec<u8>) -> HostTransportBuilder {
        self.extra_roots.push(certificate_der);
        self
    }

    pub fn build(self) -> Result<HostTransport, HostTransportError> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for der in self.extra_roots {
            roots
                .add(CertificateDer::from(der))
                .map_err(|_| HostTransportError::Setup)?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| HostTransportError::Setup)?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(HostTransport {
            inner: Arc::new(Inner {
                policy: self.policy,
                resolver: self.resolver,
                tls: Arc::new(tls),
                name_service: self.name_service,
                open_sockets: Arc::new(AtomicUsize::new(0)),
            }),
        })
    }
}

struct Inner {
    policy: DestinationPolicy,
    resolver: Arc<dyn CredentialResolver>,
    tls: Arc<ClientConfig>,
    name_service: NameService,
    open_sockets: SocketCounter,
}

#[derive(Clone)]
pub struct HostTransport {
    inner: Arc<Inner>,
}

impl HostTransport {
    pub fn builder(
        policy: DestinationPolicy,
        resolver: Arc<dyn CredentialResolver>,
    ) -> HostTransportBuilder {
        HostTransportBuilder {
            policy,
            resolver,
            extra_roots: Vec::new(),
            name_service: NameService::default(),
        }
    }

    /// Sockets (DNS and TCP) currently open on behalf of any dispatch.
    #[cfg(test)]
    pub(crate) fn open_sockets(&self) -> usize {
        self.inner.open_sockets.load(Ordering::SeqCst)
    }

    pub fn send(
        &self,
        request: &SecureRequest,
        cancel: &CancelToken,
        clock_deadline: Option<ClockDeadline<'_>>,
    ) -> Result<SecureResponse, HostTransportError> {
        if cancel.is_cancelled() {
            return Err(HostTransportError::Cancelled);
        }
        let url = parse_https_url(&request.url)?;
        validate_headers(request)?;
        self.inner.policy.check(
            &url,
            request
                .credential
                .as_ref()
                .map(|credential| credential.reference.as_str()),
        )?;
        if request.timeout.is_zero() || clock_expired(clock_deadline) {
            return Err(HostTransportError::Timeout);
        }
        let budget = Budget::new(cancel, request.timeout, clock_deadline);

        // Resolved only now, after the destination is approved, and used for this call alone;
        // it is dropped when this call returns.
        let credential = match &request.credential {
            Some(credential) => {
                let secret = self
                    .inner
                    .resolver
                    .resolve(&credential.reference)
                    .map_err(|_| HostTransportError::CredentialUnavailable)?;
                Some((&credential.rule, secret))
            }
            None => None,
        };
        budget.check()?;

        let (addresses, server_name) = self.destination(&url, &budget)?;
        let tcp = exchange::connect(&addresses, &budget, &self.inner.open_sockets)?;
        let mut session = Session::open(tcp, Arc::clone(&self.inner.tls), server_name, &budget)?;
        let head = request_head(
            &url,
            request,
            credential.as_ref().map(|(rule, secret)| (*rule, secret)),
        );
        session.write_all(&head)?;
        drop(head);
        session.write_all(&request.body)?;
        let read_limit = request.max_response_bytes.saturating_add(1);
        let raw = exchange::read_response(&mut session, read_limit)?;
        drop(session);

        // A redirect is reported, never followed, and neither its target nor its body is
        // returned.
        if (300..400).contains(&raw.status) {
            return Ok(SecureResponse {
                status: raw.status,
                headers: Vec::new(),
                body: Vec::new(),
                truncated: false,
            });
        }
        let truncated = raw.body.len() > request.max_response_bytes;
        let mut body = raw.body;
        let mut headers = raw.headers;
        if let Some((_, secret)) = &credential {
            let secret = secret.as_bytes();
            mask_secret(&mut body, secret, truncated);
            // Header names were lowercased while parsing, so a secret echoed in the name
            // position (in any letter case) is matched against its lowercase form; values are
            // kept verbatim and matched as-is.
            let lowercase_secret = Zeroizing::new(secret.to_ascii_lowercase());
            for (name, value) in &mut headers {
                let mut masked_name = std::mem::take(name).into_bytes();
                mask_secret(&mut masked_name, &lowercase_secret, false);
                *name = String::from_utf8_lossy(&masked_name).into_owned();
                let mut masked_value = std::mem::take(value).into_bytes();
                mask_secret(&mut masked_value, secret, false);
                *value = String::from_utf8_lossy(&masked_value).into_owned();
            }
        }
        Ok(SecureResponse {
            status: raw.status,
            headers,
            body,
            truncated,
        })
    }

    /// The addresses to connect to and the name the certificate must carry.
    fn destination(
        &self,
        url: &Url,
        budget: &Budget<'_>,
    ) -> Result<(Vec<SocketAddr>, ServerName<'static>), HostTransportError> {
        let port = url
            .port_or_known_default()
            .ok_or(HostTransportError::InvalidRequest)?;
        let (ips, server_name) = match url.host() {
            Some(Host::Domain(name)) => {
                let server_name = ServerName::try_from(name.to_string())
                    .map_err(|_| HostTransportError::InvalidRequest)?;
                let ips =
                    self.inner
                        .name_service
                        .resolve(name, budget, &self.inner.open_sockets)?;
                (ips, server_name)
            }
            Some(Host::Ipv4(address)) => (
                vec![IpAddr::V4(address)],
                ServerName::IpAddress(IpAddr::V4(address).into()),
            ),
            Some(Host::Ipv6(address)) => (
                vec![IpAddr::V6(address)],
                ServerName::IpAddress(IpAddr::V6(address).into()),
            ),
            None => return Err(HostTransportError::InvalidRequest),
        };
        let addresses = ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        Ok((addresses, server_name))
    }
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn is_clean_value(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte == b'\t' || (0x20..0x7f).contains(&byte))
}

fn validate_headers(request: &SecureRequest) -> Result<(), HostTransportError> {
    for (name, value) in &request.headers {
        let lowered = name.to_ascii_lowercase();
        if !is_token(name)
            || !is_clean_value(value)
            || RESERVED_HEADERS.contains(&lowered.as_str())
            || CREDENTIAL_HEADERS.contains(&lowered.as_str())
        {
            return Err(HostTransportError::InvalidRequest);
        }
    }
    if let Some(credential) = &request.credential {
        let lowered = credential.rule.header.to_ascii_lowercase();
        let scheme_ok = credential.rule.scheme.as_deref().is_none_or(is_token);
        let duplicate = request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&lowered));
        if !is_token(&credential.rule.header)
            || RESERVED_HEADERS.contains(&lowered.as_str())
            || !scheme_ok
            || duplicate
        {
            return Err(HostTransportError::InvalidRequest);
        }
    }
    Ok(())
}

/// The request line and headers. The credential, when present, is written as
/// `header: <scheme> <secret>` or `header: <secret>`; the buffer is wiped when dropped.
fn request_head(
    url: &Url,
    request: &SecureRequest,
    credential: Option<(&AttachmentRule, &SecretValue)>,
) -> Zeroizing<Vec<u8>> {
    let mut target = url.path().to_string();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let mut authority = url.host_str().unwrap_or_default().to_string();
    if let Some(port) = url.port() {
        authority.push_str(&format!(":{port}"));
    }
    let mut head = Zeroizing::new(Vec::with_capacity(512));
    head.extend_from_slice(format!("POST {target} HTTP/1.1\r\nhost: {authority}\r\n").as_bytes());
    head.extend_from_slice(format!("content-length: {}\r\n", request.body.len()).as_bytes());
    head.extend_from_slice(b"connection: close\r\n");
    for (name, value) in &request.headers {
        head.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    if let Some((rule, secret)) = credential {
        head.extend_from_slice(rule.header.as_bytes());
        head.extend_from_slice(b": ");
        if let Some(scheme) = &rule.scheme {
            head.extend_from_slice(scheme.as_bytes());
            head.push(b' ');
        }
        head.extend_from_slice(secret.as_bytes());
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(b"\r\n");
    head
}
