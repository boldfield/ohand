//! The single secure host HTTP effect.
//!
//! One POST per call with: https only and certificate validation against fixed roots, an
//! explicit destination/credential grant list, no redirects followed (a 3xx is returned as a
//! bare status), a bounded response body, a real-time budget plus an optional dispatch-clock
//! deadline, and cancellation. Cancellation and expiry shut down the live connection, so the
//! exchange ends and the resolved secret is dropped promptly rather than at the relative budget. The secret is resolved at dispatch after the
//! destination check, attached by the transport, and masked out of everything returned.

use std::error::Error as StdError;
use std::io::Read;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ohand_core::providers::contracts::{CancelToken, Clock};
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use zeroize::Zeroizing;

use super::abort::{AbortHandle, AbortableTls};
use super::credential::{CredentialResolver, SecretValue};
use super::error::HostTransportError;
use super::policy::{parse_https_url, DestinationPolicy};
use super::redact::mask_secret;

/// How often a waiting call re-checks cancellation and deadlines.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// The worker's own HTTP timeout outlives the caller's budget so the caller's deadline decides.
const WORKER_GRACE: Duration = Duration::from_millis(250);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialUse {
    pub reference: String,
    pub rule: AttachmentRule,
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
}

impl HostTransportBuilder {
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
            }),
        })
    }
}

struct Inner {
    policy: DestinationPolicy,
    resolver: Arc<dyn CredentialResolver>,
    tls: Arc<ClientConfig>,
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
        }
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

        // Resolved only now, after the destination is approved, and used for this call alone.
        let attachment = match &request.credential {
            Some(credential) => {
                let secret = self
                    .inner
                    .resolver
                    .resolve(&credential.reference)
                    .map_err(|_| HostTransportError::CredentialUnavailable)?;
                let header_value = header_value_for(&credential.rule, &secret);
                Some(ResolvedAttachment {
                    header: credential.rule.header.clone(),
                    header_value,
                    secret,
                })
            }
            None => None,
        };
        if cancel.is_cancelled() {
            return Err(HostTransportError::Cancelled);
        }

        let abort = Arc::new(AbortHandle::default());
        let job = Job {
            url: url.to_string(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            timeout: request.timeout + WORKER_GRACE,
            max_response_bytes: request.max_response_bytes,
            attachment,
            tls: Arc::clone(&self.inner.tls),
            abort: Arc::clone(&abort),
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            // The caller may have left (cancelled or timed out); the result is then dropped.
            let _ = sender.send(perform(job));
        });

        let started = Instant::now();
        let outcome = loop {
            if cancel.is_cancelled() {
                break Err(HostTransportError::Cancelled);
            }
            if started.elapsed() >= request.timeout || clock_expired(clock_deadline) {
                break Err(HostTransportError::Timeout);
            }
            match receiver.recv_timeout(POLL_INTERVAL) {
                Ok(outcome) => {
                    if cancel.is_cancelled() {
                        break Err(HostTransportError::Cancelled);
                    }
                    break outcome;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break Err(HostTransportError::Unreachable),
            }
        };
        // Whatever ended the wait, the exchange ends with it: the worker's socket is shut down
        // so it stops reading and writing and drops the resolved secret.
        abort.abort();
        outcome
    }
}

fn clock_expired(clock_deadline: Option<ClockDeadline<'_>>) -> bool {
    clock_deadline.is_some_and(|deadline| deadline.clock.now_ms() >= deadline.deadline_ms)
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

fn header_value_for(rule: &AttachmentRule, secret: &SecretValue) -> Zeroizing<String> {
    // SecretValue only holds printable ASCII, so this conversion cannot fail.
    let secret_text = String::from_utf8_lossy(secret.as_bytes());
    Zeroizing::new(match &rule.scheme {
        Some(scheme) => format!("{scheme} {secret_text}"),
        None => secret_text.into_owned(),
    })
}

struct ResolvedAttachment {
    header: String,
    header_value: Zeroizing<String>,
    secret: SecretValue,
}

struct Job {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: Duration,
    max_response_bytes: usize,
    attachment: Option<ResolvedAttachment>,
    tls: Arc<ClientConfig>,
    abort: Arc<AbortHandle>,
}

fn perform(job: Job) -> Result<SecureResponse, HostTransportError> {
    let agent = ureq::AgentBuilder::new()
        .tls_connector(Arc::new(AbortableTls {
            config: job.tls,
            abort: job.abort,
        }))
        .redirects(0)
        .https_only(true)
        .max_idle_connections(0)
        .timeout(job.timeout)
        .build();
    let mut call = agent.post(&job.url);
    for (name, value) in &job.headers {
        call = call.set(name, value);
    }
    if let Some(attachment) = &job.attachment {
        call = call.set(&attachment.header, &attachment.header_value);
    }
    let response = match call.send_bytes(&job.body) {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(transport)) => return Err(classify(&transport)),
    };

    let status = response.status();
    // A redirect is reported, never followed, and neither its target nor its body is returned.
    if (300..400).contains(&status) {
        return Ok(SecureResponse {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            truncated: false,
        });
    }

    let mut headers = Vec::new();
    for name in response.headers_names() {
        for value in response.all(&name) {
            headers.push((name.clone(), value.to_string()));
        }
    }
    let read_limit = (job.max_response_bytes as u64).saturating_add(1);
    let mut body = Vec::new();
    response
        .into_reader()
        .take(read_limit)
        .read_to_end(&mut body)
        .map_err(|error| classify_io(&error))?;
    let truncated = body.len() as u64 > job.max_response_bytes as u64;

    if let Some(attachment) = &job.attachment {
        let secret = attachment.secret.as_bytes();
        mask_secret(&mut body, secret, truncated);
        for (_, value) in &mut headers {
            let mut masked = std::mem::take(value).into_bytes();
            mask_secret(&mut masked, secret, false);
            *value = String::from_utf8_lossy(&masked).into_owned();
        }
    }
    Ok(SecureResponse {
        status,
        headers,
        body,
        truncated,
    })
}

fn classify(transport: &ureq::Transport) -> HostTransportError {
    let mut current: Option<&(dyn StdError + 'static)> = Some(transport);
    while let Some(error) = current {
        if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
            let classified = classify_io(io_error);
            if classified != HostTransportError::Unreachable {
                return classified;
            }
        }
        if let Some(tls_error) = error.downcast_ref::<rustls::Error>() {
            let classified = classify_tls(tls_error);
            if classified != HostTransportError::Unreachable {
                return classified;
            }
        }
        current = error.source();
    }
    match transport.kind() {
        ureq::ErrorKind::InvalidUrl
        | ureq::ErrorKind::UnknownScheme
        | ureq::ErrorKind::InsecureRequestHttpsOnly
        | ureq::ErrorKind::BadHeader => HostTransportError::InvalidRequest,
        _ => HostTransportError::Unreachable,
    }
}

fn classify_io(error: &std::io::Error) -> HostTransportError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        return HostTransportError::Timeout;
    }
    if let Some(tls_error) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        return classify_tls(tls_error);
    }
    HostTransportError::Unreachable
}

fn classify_tls(error: &rustls::Error) -> HostTransportError {
    match error {
        rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented => {
            HostTransportError::TlsVerificationFailed
        }
        _ => HostTransportError::Unreachable,
    }
}
