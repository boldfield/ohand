use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ohand_core::providers::anthropic::{
    AnthropicTransport, CredentialAttachment, HttpMethod, HttpRequest,
};
use ohand_core::providers::contracts::{CancelToken, ManualClock, SystemClock, TransportError};
use ohand_core::providers::openai::HttpTransport;

use super::dns::NameService;
use super::fixture::{Fixture, RecordedRequest, Reply, TestAuthority};
use super::redact::mask_secret;
use super::*;

const CANARY: &str = "CANARY-synthetic-9f3a7c0d41";
const REFERENCE: &str = "synthetic-credential-ref";

#[derive(Default)]
struct RecordingResolver {
    secrets: Mutex<HashMap<String, String>>,
    asked: Mutex<Vec<String>>,
}

impl RecordingResolver {
    fn with_secret(reference: &str, secret: &str) -> Arc<RecordingResolver> {
        let resolver = Arc::new(RecordingResolver::default());
        resolver.set(reference, secret);
        resolver
    }

    fn set(&self, reference: &str, secret: &str) {
        self.secrets
            .lock()
            .unwrap()
            .insert(reference.to_string(), secret.to_string());
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl CredentialResolver for RecordingResolver {
    fn resolve(&self, reference: &str) -> Result<SecretValue, CredentialError> {
        self.asked.lock().unwrap().push(reference.to_string());
        let secrets = self.secrets.lock().unwrap();
        let value = secrets.get(reference).ok_or(CredentialError::NotFound)?;
        SecretValue::from_bytes(value.clone().into_bytes())
    }
}

fn echo_ok(_: &RecordedRequest) -> Reply {
    Reply::ok(b"{\"ok\":true}")
}

fn start_fixture(
    authority: &TestAuthority,
    handler: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
) -> Fixture {
    Fixture::start(authority.issue("localhost"), handler)
}

fn transport_for(
    authority: &TestAuthority,
    policy: DestinationPolicy,
    resolver: Arc<RecordingResolver>,
) -> HostTransport {
    HostTransport::builder(policy, resolver)
        .trust_root_der(authority.root_der())
        .build()
        .unwrap()
}

fn credential_use() -> CredentialUse {
    CredentialUse {
        reference: REFERENCE.to_string(),
        rule: AttachmentRule {
            header: "x-api-key".to_string(),
            scheme: None,
        },
    }
}

fn request_to(url: String, credential: Option<CredentialUse>) -> SecureRequest {
    SecureRequest {
        url,
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body: b"{\"synthetic\":true}".to_vec(),
        timeout: Duration::from_secs(10),
        max_response_bytes: 64 * 1024,
        credential,
    }
}

fn send(
    transport: &HostTransport,
    request: &SecureRequest,
) -> Result<SecureResponse, HostTransportError> {
    transport.send(request, &CancelToken::new(), None)
}

fn approved(fixture: &Fixture) -> DestinationPolicy {
    DestinationPolicy::new()
        .approve_credential(&fixture.url("/"), REFERENCE)
        .unwrap()
}

fn assert_clean(error: HostTransportError) {
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(CANARY));
    assert!(!rendered.contains("localhost"));
}

// --- TLS verification -------------------------------------------------------------------

#[test]
fn trusted_server_round_trips_over_tls() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let response = send(
        &transport,
        &request_to(fixture.url("/v1/messages"), Some(credential_use())),
    )
    .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"{\"ok\":true}");
    let recorded = &fixture.requests()[0];
    assert!(recorded.request_line.starts_with("POST /v1/messages "));
    assert_eq!(recorded.body, b"{\"synthetic\":true}");
}

#[test]
fn certificate_from_an_untrusted_authority_is_refused_before_any_request() {
    let server_authority = TestAuthority::new();
    let other_authority = TestAuthority::new();
    let fixture = start_fixture(&server_authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&other_authority, approved(&fixture), resolver);

    let error = send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap_err();

    assert_eq!(error, HostTransportError::TlsVerificationFailed);
    assert_eq!(TransportError::from(error), TransportError::Unavailable);
    assert!(fixture.requests().is_empty());
    assert_clean(error);
}

#[test]
fn certificate_for_another_hostname_is_refused() {
    let authority = TestAuthority::new();
    let fixture = Fixture::start(authority.issue("other.invalid"), echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let error = send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap_err();

    assert_eq!(error, HostTransportError::TlsVerificationFailed);
    assert!(fixture.requests().is_empty());
}

#[test]
fn cleartext_and_userinfo_destinations_are_invalid() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    let cleartext = fixture.url("/").replacen("https://", "http://", 1);
    let with_userinfo = fixture.url("/").replacen("https://", "https://user:pw@", 1);
    for url in [cleartext, with_userinfo] {
        assert_eq!(
            send(&transport, &request_to(url, Some(credential_use()))).unwrap_err(),
            HostTransportError::InvalidRequest
        );
    }
    assert!(DestinationPolicy::new()
        .approve_origin("http://localhost:80/")
        .is_err());
    assert_eq!(fixture.connections(), 0);
    assert!(resolver.asked().is_empty());
}

// --- Destination and credential approval ----------------------------------------------------

#[test]
fn unapproved_destinations_are_rejected_without_connecting_or_resolving() {
    let authority = TestAuthority::new();
    let approved_fixture = start_fixture(&authority, echo_ok);
    let other_fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(
        &authority,
        approved(&approved_fixture),
        Arc::clone(&resolver),
    );

    let other_port = other_fixture.url("/");
    let lookalike_host = approved_fixture
        .url("/")
        .replace("localhost", "localhost.example.invalid");
    for url in [other_port, lookalike_host] {
        let error = send(&transport, &request_to(url, Some(credential_use()))).unwrap_err();
        assert_eq!(error, HostTransportError::DestinationNotApproved);
        assert_eq!(TransportError::from(error), TransportError::Rejected);
    }
    assert_eq!(other_fixture.connections(), 0);
    assert_eq!(approved_fixture.connections(), 0);
    assert!(resolver.asked().is_empty());
}

#[test]
fn credential_is_never_sent_to_an_origin_it_was_not_granted_for() {
    let authority = TestAuthority::new();
    let first = start_fixture(&authority, echo_ok);
    let second = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    resolver.set("second-credential-ref", "OTHER-synthetic-secret");
    let policy = DestinationPolicy::new()
        .approve_credential(&first.url("/"), REFERENCE)
        .unwrap()
        .approve_credential(&second.url("/"), "second-credential-ref")
        .unwrap();
    let transport = transport_for(&authority, policy, Arc::clone(&resolver));

    let error = send(
        &transport,
        &request_to(second.url("/"), Some(credential_use())),
    )
    .unwrap_err();

    assert_eq!(error, HostTransportError::CredentialNotApproved);
    assert_eq!(second.connections(), 0);
    assert!(resolver.asked().is_empty());
    // The same origin still serves requests that carry no credential.
    assert!(send(&transport, &request_to(second.url("/"), None)).is_ok());
    assert!(second.requests()[0].header("x-api-key").is_none());
}

#[test]
fn missing_credential_fails_unauthorized_and_never_falls_back_to_another_reference() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret("some-other-profile-ref", CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    let error = send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap_err();

    assert_eq!(error, HostTransportError::CredentialUnavailable);
    assert_eq!(TransportError::from(error), TransportError::Unauthorized);
    assert_eq!(resolver.asked(), vec![REFERENCE.to_string()]);
    assert_eq!(fixture.connections(), 0);
}

// --- Per-dispatch credential resolution and attachment ----------------------------------------

#[test]
fn secret_is_resolved_for_every_dispatch_and_attached_by_the_rule() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap();
    resolver.set(REFERENCE, "ROTATED-synthetic-secret");
    let mut bearer = credential_use();
    bearer.rule = AttachmentRule {
        header: "authorization".to_string(),
        scheme: Some("Bearer".to_string()),
    };
    send(&transport, &request_to(fixture.url("/"), Some(bearer))).unwrap();

    assert_eq!(resolver.asked().len(), 2);
    let recorded = fixture.requests();
    assert_eq!(recorded[0].header("x-api-key"), Some(CANARY));
    assert_eq!(
        recorded[1].header("authorization"),
        Some("Bearer ROTATED-synthetic-secret")
    );
}

#[test]
fn callers_cannot_supply_credential_or_reserved_headers_or_inject_lines() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    let rejected_headers = [
        ("Authorization", "Bearer smuggled"),
        ("X-Api-Key", "smuggled"),
        ("Cookie", "smuggled=1"),
        ("Host", "elsewhere.invalid"),
        ("x-note", "line one\r\nx-injected: yes"),
        ("bad name", "value"),
    ];
    for (name, value) in rejected_headers {
        let mut request = request_to(fixture.url("/"), None);
        request.headers.push((name.to_string(), value.to_string()));
        assert_eq!(
            send(&transport, &request).unwrap_err(),
            HostTransportError::InvalidRequest,
            "{name}"
        );
    }

    let mut duplicate = request_to(fixture.url("/"), Some(credential_use()));
    duplicate
        .headers
        .push(("X-API-Key".to_string(), "dup".to_string()));
    assert_eq!(
        send(&transport, &duplicate).unwrap_err(),
        HostTransportError::InvalidRequest
    );
    assert_eq!(fixture.connections(), 0);
}

#[test]
fn secrets_that_could_inject_header_lines_are_unusable() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, "line1\r\nx-injected: yes");
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let error = send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap_err();

    assert_eq!(error, HostTransportError::CredentialUnavailable);
    assert_eq!(fixture.connections(), 0);
}

// --- Secrets never leave through errors, debug output or response data -------------------------

#[test]
fn echoed_secrets_are_masked_in_error_bodies_and_headers_without_changing_length() {
    let authority = TestAuthority::new();
    let body = format!("{{\"error\":\"invalid key {CANARY} for request\"}}");
    let echoed_body = body.clone();
    let fixture = start_fixture(&authority, move |_| Reply::Full {
        status: 401,
        headers: vec![("x-echo".to_string(), format!("seen {CANARY}"))],
        body: echoed_body.clone().into_bytes(),
    });
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let response = send(
        &transport,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap();

    assert_eq!(response.status, 401);
    let text = String::from_utf8(response.body.clone()).unwrap();
    assert!(!text.contains(CANARY));
    assert_eq!(response.body.len(), body.len());
    assert!(text.contains(&"*".repeat(CANARY.len())));
    let echo = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-echo"))
        .unwrap();
    assert!(!echo.1.contains(CANARY));
    let debugged = format!("{response:?}");
    assert!(!debugged.contains(CANARY));
}

#[test]
fn failures_and_debug_output_never_contain_secret_values_or_endpoints() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::Hang);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    let mut slow = request_to(fixture.url("/"), Some(credential_use()));
    slow.timeout = Duration::from_millis(200);
    let timeout = send(&transport, &slow).unwrap_err();
    assert_eq!(timeout, HostTransportError::Timeout);
    assert_clean(timeout);

    let untrusted = HostTransport::builder(approved(&fixture), resolver)
        .build()
        .unwrap();
    let tls = send(
        &untrusted,
        &request_to(fixture.url("/"), Some(credential_use())),
    )
    .unwrap_err();
    assert_clean(tls);

    let secret = SecretValue::from_bytes(CANARY.as_bytes().to_vec()).unwrap();
    assert!(!format!("{secret:?}").contains(CANARY));
    let request = request_to(fixture.url("/"), Some(credential_use()));
    let rendered = format!("{request:?}");
    assert!(!rendered.contains(CANARY));
    assert!(!rendered.contains("localhost"));
}

#[test]
fn masking_covers_json_escaped_secrets_and_a_prefix_cut_at_the_size_bound() {
    let secret = b"ab\"cd-secret";
    let mut escaped = b"{\"m\":\"ab\\\"cd-secret and ab\"cd-secret\"}".to_vec();
    let length = escaped.len();
    mask_secret(&mut escaped, secret, false);
    let text = String::from_utf8(escaped).unwrap();
    assert!(!text.contains("cd-secret"));
    assert_eq!(text.len(), length);

    let mut cut = b"error: ab\"cd-sec".to_vec();
    mask_secret(&mut cut, secret, true);
    assert_eq!(cut, b"error: *********".to_vec());
}

#[test]
fn environment_resolver_maps_references_explicitly() {
    let variable = "OHAND_BENCH_TEST_ONLY_SYNTHETIC_SECRET_01";
    let value = Arc::new(Mutex::new(Some(std::ffi::OsString::from(CANARY))));
    let lookup_value = Arc::clone(&value);
    let resolver = EnvCredentialResolver::with_lookup(move |name| {
        (name == variable)
            .then(|| lookup_value.lock().unwrap().clone())
            .flatten()
    })
    .map_reference(REFERENCE, variable)
    .map_reference("unset-ref", "OHAND_BENCH_TEST_ONLY_UNSET_VARIABLE_01");

    assert!(resolver.resolve(REFERENCE).is_ok());
    assert_eq!(
        resolver.resolve("unmapped-ref").unwrap_err(),
        CredentialError::NotFound
    );
    assert_eq!(
        resolver.resolve("unset-ref").unwrap_err(),
        CredentialError::NotFound
    );
    *value.lock().unwrap() = Some(std::ffi::OsString::from("has space"));
    assert_eq!(
        resolver.resolve(REFERENCE).unwrap_err(),
        CredentialError::Malformed
    );
    *value.lock().unwrap() = None;
    assert_eq!(
        resolver.resolve(REFERENCE).unwrap_err(),
        CredentialError::NotFound
    );
}

// --- Redirects ----------------------------------------------------------------------------

#[test]
fn redirects_are_reported_not_followed_and_the_credential_goes_nowhere_else() {
    let authority = TestAuthority::new();
    let target = start_fixture(&authority, echo_ok);
    let target_url = target.url("/landing");
    let source = start_fixture(&authority, move |_| Reply::Full {
        status: 302,
        headers: vec![("location".to_string(), target_url.clone())],
        body: b"redirect body".to_vec(),
    });
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let policy = approved(&source)
        .approve_credential(&target.url("/"), REFERENCE)
        .unwrap();
    let transport = transport_for(&authority, policy, resolver);

    let response = send(
        &transport,
        &request_to(source.url("/"), Some(credential_use())),
    )
    .unwrap();

    assert_eq!(response.status, 302);
    assert!(response.body.is_empty());
    assert!(response.headers.is_empty());
    assert_eq!(source.requests().len(), 1);
    assert_eq!(target.connections(), 0);
}

// --- Body bounds ----------------------------------------------------------------------------

#[test]
fn oversized_bodies_stop_one_byte_past_the_bound() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::ok(&[b'a'; 10_000]));
    let resolver = RecordingResolver::default();
    let transport = transport_for(&authority, approved(&fixture), Arc::new(resolver));

    let mut request = request_to(fixture.url("/"), None);
    request.max_response_bytes = 1_000;
    let over = send(&transport, &request).unwrap();
    assert_eq!(over.body.len(), 1_001);
    assert!(over.truncated);

    request.max_response_bytes = 10_000;
    let exact = send(&transport, &request).unwrap();
    assert_eq!(exact.body.len(), 10_000);
    assert!(!exact.truncated);
}

#[test]
fn an_endless_body_is_cut_at_the_bound_quickly() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::Endless);
    let resolver = RecordingResolver::default();
    let transport = transport_for(&authority, approved(&fixture), Arc::new(resolver));

    let mut request = request_to(fixture.url("/"), None);
    request.max_response_bytes = 8_192;
    let started = Instant::now();
    let response = send(&transport, &request).unwrap();

    assert!(response.truncated);
    assert_eq!(response.body.len(), 8_193);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn chunked_bodies_are_decoded_and_bounded() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| {
        Reply::Raw(
            b"HTTP/1.1 100 Continue\r\n\r\n\
              HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n\
              5;ext=1\r\nhello\r\n7\r\n, world\r\n0\r\n\r\n"
                .to_vec(),
        )
    });
    let transport = transport_for(
        &authority,
        approved(&fixture),
        Arc::new(RecordingResolver::default()),
    );

    let mut request = request_to(fixture.url("/"), None);
    let whole = send(&transport, &request).unwrap();
    assert_eq!(whole.status, 200);
    assert_eq!(whole.body, b"hello, world");
    assert!(!whole.truncated);

    request.max_response_bytes = 6;
    let cut = send(&transport, &request).unwrap();
    assert_eq!(cut.body, b"hello, ");
    assert!(cut.truncated);
}

#[test]
fn close_delimited_bodies_end_at_close_and_short_length_framed_bodies_fail() {
    let authority = TestAuthority::new();
    let delimited = start_fixture(&authority, |_| {
        Reply::Raw(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\nuntil close".to_vec())
    });
    let short = start_fixture(&authority, |_| {
        Reply::Raw(b"HTTP/1.1 200 OK\r\ncontent-length: 50\r\n\r\nonly part".to_vec())
    });
    let policy = approved(&delimited)
        .approve_origin(&short.url("/"))
        .unwrap();
    let transport = transport_for(&authority, policy, Arc::new(RecordingResolver::default()));

    let response = send(&transport, &request_to(delimited.url("/"), None)).unwrap();
    assert_eq!(response.body, b"until close");
    assert_eq!(
        send(&transport, &request_to(short.url("/"), None)).unwrap_err(),
        HostTransportError::Unreachable
    );
    assert_no_open_sockets(&transport);
}

// --- Deadline and cancellation ----------------------------------------------------------------

/// The request reached the server, and the client then closed the connection well inside the
/// 60 s relative budget the caller gave: the exchange itself was aborted, not just abandoned.
fn assert_exchange_torn_down(fixture: &Fixture) {
    let waiting_since = Instant::now();
    while fixture.closed_connections() == 0 && waiting_since.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fixture.requests().len(), 1);
    assert_eq!(fixture.closed_connections(), 1);
}

#[test]
fn a_silent_server_hits_the_relative_deadline() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::Hang);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let mut request = request_to(fixture.url("/"), Some(credential_use()));
    request.timeout = Duration::from_millis(300);
    let started = Instant::now();
    let error = send(&transport, &request).unwrap_err();

    assert_eq!(error, HostTransportError::Timeout);
    assert_eq!(TransportError::from(error), TransportError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn cancellation_returns_promptly_while_the_request_is_in_flight() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::HoldUntilClosed);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        canceller.cancel();
    });
    let mut request = request_to(fixture.url("/"), Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport.send(&request, &cancel, None).unwrap_err();

    assert_eq!(error, HostTransportError::Cancelled);
    assert_eq!(TransportError::from(error), TransportError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_no_open_sockets(&transport);
    assert_exchange_torn_down(&fixture);
}

#[test]
fn cancelled_or_exhausted_calls_never_connect_or_resolve() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));

    let cancel = CancelToken::new();
    cancel.cancel();
    let request = request_to(fixture.url("/"), Some(credential_use()));
    assert_eq!(
        transport.send(&request, &cancel, None).unwrap_err(),
        HostTransportError::Cancelled
    );

    let mut no_budget = request.clone();
    no_budget.timeout = Duration::ZERO;
    assert_eq!(
        send(&transport, &no_budget).unwrap_err(),
        HostTransportError::Timeout
    );

    let clock = ManualClock::new();
    clock.advance_ms(500);
    let expired = ClockDeadline {
        clock: &clock,
        deadline_ms: 500,
    };
    assert_eq!(
        transport
            .send(&request, &CancelToken::new(), Some(expired))
            .unwrap_err(),
        HostTransportError::Timeout
    );
    assert_eq!(fixture.connections(), 0);
    assert!(resolver.asked().is_empty());
}

#[test]
fn the_dispatch_clock_deadline_ends_a_call_even_with_a_long_relative_budget() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::HoldUntilClosed);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let clock = Arc::new(ManualClock::new());
    let advancing = Arc::clone(&clock);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        advancing.advance_ms(10_000);
    });
    let mut request = request_to(fixture.url("/"), Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport
        .send(
            &request,
            &CancelToken::new(),
            Some(ClockDeadline {
                clock: clock.as_ref(),
                deadline_ms: 5_000,
            }),
        )
        .unwrap_err();

    assert_eq!(error, HostTransportError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_no_open_sockets(&transport);
    assert_exchange_torn_down(&fixture);
}

// --- Phases before the request is on the wire ----------------------------------------------

/// Everything a dispatch opened is closed by the time `send` returns. The exchange runs on the
/// calling thread, so there is no worker or lookup helper that could still hold a socket.
fn assert_no_open_sockets(transport: &HostTransport) {
    assert_eq!(
        transport.open_sockets(),
        0,
        "a socket outlived the dispatch that opened it"
    );
}

/// A loopback UDP nameserver that counts queries and either never answers or answers A
/// queries for one name with 127.0.0.1, other record types for that name with no records, and
/// every other name with NXDOMAIN.
struct FakeNameserver {
    address: std::net::SocketAddr,
    queries: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeNameserver {
    fn silent() -> FakeNameserver {
        FakeNameserver::start(None)
    }

    fn answering(name: &'static str) -> FakeNameserver {
        FakeNameserver::start(Some(name))
    }

    fn start(answered_name: Option<&'static str>) -> FakeNameserver {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(10)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        let queries = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (counted, stopping) = (Arc::clone(&queries), Arc::clone(&stop));
        thread::spawn(move || {
            let mut buffer = [0u8; 512];
            while !stopping.load(Ordering::SeqCst) {
                let Ok((length, peer)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                counted.fetch_add(1, Ordering::SeqCst);
                if let Some(name) = answered_name {
                    let reply = FakeNameserver::reply(&buffer[..length], name);
                    let _ = socket.send_to(&reply, peer);
                }
            }
        });
        FakeNameserver {
            address,
            queries,
            stop,
        }
    }

    fn reply(query: &[u8], answered_name: &str) -> Vec<u8> {
        let mut at = 12;
        let mut labels = Vec::new();
        while query[at] != 0 {
            let length = query[at] as usize;
            labels.push(String::from_utf8_lossy(&query[at + 1..at + 1 + length]).into_owned());
            at += 1 + length;
        }
        let record_type = u16::from_be_bytes([query[at + 1], query[at + 2]]);
        let mut reply = query[..at + 5].to_vec();
        if labels.join(".") != answered_name {
            reply[2..4].copy_from_slice(&0x8183u16.to_be_bytes());
            return reply;
        }
        reply[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        if record_type == 1 {
            reply[6..8].copy_from_slice(&1u16.to_be_bytes());
            reply.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
        }
        reply
    }

    fn queries(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }

    fn wait_for_query(&self) {
        let waiting_since = Instant::now();
        while self.queries() == 0 {
            assert!(waiting_since.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn name_service(&self) -> NameService {
        NameService::fixed(
            std::path::PathBuf::from("/nonexistent/bench-hosts"),
            vec![self.address],
        )
    }
}

impl Drop for FakeNameserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn transport_with_names(
    authority: &TestAuthority,
    policy: DestinationPolicy,
    nameserver: &FakeNameserver,
) -> HostTransport {
    HostTransport::builder(policy, RecordingResolver::with_secret(REFERENCE, CANARY))
        .trust_root_der(authority.root_der())
        .name_service(nameserver.name_service())
        .build()
        .unwrap()
}

#[test]
fn names_resolve_through_the_configured_nameserver() {
    let authority = TestAuthority::new();
    let fixture = Fixture::start(authority.issue("api.bench.test"), echo_ok);
    let nameserver = FakeNameserver::answering("api.bench.test");
    let url = format!("https://api.bench.test:{}/v1", fixture.port());
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_with_names(&authority, policy, &nameserver);

    let response = send(&transport, &request_to(url, Some(credential_use()))).unwrap();

    assert_eq!(response.status, 200);
    assert!(nameserver.queries() >= 1);
    assert_eq!(fixture.requests()[0].header("x-api-key"), Some(CANARY));
    assert_eq!(
        fixture.requests()[0].header("host"),
        Some(format!("api.bench.test:{}", fixture.port()).as_str())
    );
    assert_no_open_sockets(&transport);
}

#[test]
fn a_name_without_addresses_is_unreachable_without_connecting() {
    let authority = TestAuthority::new();
    let nameserver = FakeNameserver::answering("api.bench.test");
    let url = "https://missing.bench.test/v1".to_string();
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_with_names(&authority, policy, &nameserver);

    let error = send(&transport, &request_to(url, Some(credential_use()))).unwrap_err();

    assert_eq!(error, HostTransportError::Unreachable);
    assert!(nameserver.queries() >= 1);
    assert_no_open_sockets(&transport);
}

#[test]
fn cancellation_during_name_resolution_ends_the_lookup_and_closes_its_socket() {
    let authority = TestAuthority::new();
    let nameserver = Arc::new(FakeNameserver::silent());
    let url = "https://api.bench.test/v1".to_string();
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_with_names(&authority, policy, &nameserver);

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    let watched = Arc::clone(&nameserver);
    thread::spawn(move || {
        watched.wait_for_query();
        canceller.cancel();
    });
    let mut request = request_to(url, Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport.send(&request, &cancel, None).unwrap_err();

    assert_eq!(error, HostTransportError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_no_open_sockets(&transport);
}

#[test]
fn the_dispatch_clock_deadline_during_name_resolution_ends_the_lookup() {
    let authority = TestAuthority::new();
    let nameserver = Arc::new(FakeNameserver::silent());
    let url = "https://api.bench.test/v1".to_string();
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_with_names(&authority, policy, &nameserver);

    let clock = Arc::new(ManualClock::new());
    let advancing = Arc::clone(&clock);
    let watched = Arc::clone(&nameserver);
    thread::spawn(move || {
        watched.wait_for_query();
        advancing.advance_ms(10_000);
    });
    let mut request = request_to(url, Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport
        .send(
            &request,
            &CancelToken::new(),
            Some(ClockDeadline {
                clock: clock.as_ref(),
                deadline_ms: 5_000,
            }),
        )
        .unwrap_err();

    assert_eq!(error, HostTransportError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_no_open_sockets(&transport);
}

/// A loopback listener whose accept queue is full, so a further TCP connect to it stalls in
/// the SYN phase instead of completing or failing.
struct StalledListener {
    _listener: socket2::Socket,
    _queued: Vec<socket2::Socket>,
    port: u16,
}

impl StalledListener {
    fn start() -> StalledListener {
        use socket2::{Domain, Socket, Type};
        let listener = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
        let any_port: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        listener.bind(&any_port.into()).unwrap();
        listener.listen(0).unwrap();
        let address = listener.local_addr().unwrap().as_socket().unwrap();
        let mut queued = Vec::new();
        for _ in 0..64 {
            let probe = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
            match probe.connect_timeout(&address.into(), Duration::from_millis(200)) {
                Ok(()) => queued.push(probe),
                Err(_) => {
                    return StalledListener {
                        _listener: listener,
                        _queued: queued,
                        port: address.port(),
                    }
                }
            }
        }
        panic!("could not fill the listener's accept queue to stall a connect");
    }
}

#[test]
fn cancellation_while_the_tcp_connect_is_stalled_ends_it_promptly() {
    let authority = TestAuthority::new();
    let stalled = StalledListener::start();
    let url = format!("https://127.0.0.1:{}/v1", stalled.port);
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_for(
        &authority,
        policy,
        RecordingResolver::with_secret(REFERENCE, CANARY),
    );

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        canceller.cancel();
    });
    let mut request = request_to(url, Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport.send(&request, &cancel, None).unwrap_err();

    assert_eq!(error, HostTransportError::Cancelled);
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_no_open_sockets(&transport);
}

#[test]
fn the_relative_deadline_ends_a_stalled_tcp_connect() {
    let authority = TestAuthority::new();
    let stalled = StalledListener::start();
    let url = format!("https://127.0.0.1:{}/v1", stalled.port);
    let policy = DestinationPolicy::new().approve_origin(&url).unwrap();
    let transport = transport_for(&authority, policy, Arc::new(RecordingResolver::default()));

    let mut request = request_to(url, None);
    request.timeout = Duration::from_millis(200);
    let started = Instant::now();
    let error = send(&transport, &request).unwrap_err();

    assert_eq!(error, HostTransportError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_no_open_sockets(&transport);
}

/// A TCP server that accepts, reads whatever arrives and never answers, and records the bytes
/// received and whether the client closed.
struct SilentTcpServer {
    port: u16,
    received: Arc<Mutex<Vec<u8>>>,
    closed: Arc<AtomicUsize>,
}

impl SilentTcpServer {
    fn start() -> SilentTcpServer {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicUsize::new(0));
        let (seen, done) = (Arc::clone(&received), Arc::clone(&closed));
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => seen.lock().unwrap().extend_from_slice(&buffer[..count]),
                    }
                }
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
        SilentTcpServer {
            port,
            received,
            closed,
        }
    }
}

#[test]
fn cancellation_during_the_tls_handshake_tears_down_the_connection_without_the_credential() {
    let authority = TestAuthority::new();
    let server = SilentTcpServer::start();
    let url = format!("https://localhost:{}/", server.port);
    let policy = DestinationPolicy::new()
        .approve_credential(&url, REFERENCE)
        .unwrap();
    let transport = transport_for(
        &authority,
        policy,
        RecordingResolver::with_secret(REFERENCE, CANARY),
    );

    let cancel = CancelToken::new();
    let canceller = cancel.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });
    let mut request = request_to(url, Some(credential_use()));
    request.timeout = Duration::from_secs(60);
    let started = Instant::now();
    let error = transport.send(&request, &cancel, None).unwrap_err();

    assert_eq!(error, HostTransportError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_no_open_sockets(&transport);
    let waiting_since = Instant::now();
    while server.closed.load(Ordering::SeqCst) == 0
        && waiting_since.elapsed() < Duration::from_secs(3)
    {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.closed.load(Ordering::SeqCst), 1);
    let received = server.received.lock().unwrap().clone();
    assert!(!received.is_empty(), "the handshake had begun");
    assert!(!received
        .windows(CANARY.len())
        .any(|window| window == CANARY.as_bytes()));
}

// --- Bindings to the provider transport traits ---------------------------------------------------

#[test]
fn anthropic_binding_attaches_the_raw_secret_to_the_named_header() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), resolver);

    let request = HttpRequest {
        url: fixture.url("/v1/messages"),
        method: HttpMethod::Post,
        headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
        body: b"{}".to_vec(),
        timeout_ms: 10_000,
        deadline_ms: 10_000,
        max_response_bytes: 4_096,
        credential: Some(CredentialAttachment {
            reference: REFERENCE.to_string(),
            header: "x-api-key".to_string(),
        }),
    };
    let response = AnthropicTransport::send(&transport, &request, &CancelToken::new()).unwrap();

    assert_eq!(response.status, 200);
    let recorded = &fixture.requests()[0];
    assert_eq!(recorded.header("x-api-key"), Some(CANARY));
    assert_eq!(recorded.header("anthropic-version"), Some("2023-06-01"));

    let mut elsewhere = request.clone();
    elsewhere.url = "https://unapproved.invalid/v1/messages".to_string();
    assert_eq!(
        AnthropicTransport::send(&transport, &elsewhere, &CancelToken::new()).unwrap_err(),
        TransportError::Rejected
    );
}

#[test]
fn openai_binding_attaches_a_bearer_secret_and_maps_failures() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, |_| Reply::Hang);
    let ok_fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let policy = approved(&fixture)
        .approve_credential(&ok_fixture.url("/"), REFERENCE)
        .unwrap();
    let transport = transport_for(&authority, policy, resolver);
    let clock = SystemClock::new();

    let (status, body) = transport
        .post(
            &ok_fixture.url("/v1/chat/completions"),
            REFERENCE,
            &[("Content-Type", "application/json")],
            b"{}".to_vec(),
            10_000,
            &CancelToken::new(),
            &clock,
            4_096,
        )
        .unwrap();
    assert_eq!((status, body.as_slice()), (200, &b"{\"ok\":true}"[..]));
    let bearer = format!("Bearer {CANARY}");
    assert_eq!(
        ok_fixture.requests()[0].header("authorization"),
        Some(bearer.as_str())
    );

    let unknown_reference = transport
        .post(
            &ok_fixture.url("/"),
            "unknown-ref",
            &[],
            Vec::new(),
            10_000,
            &CancelToken::new(),
            &clock,
            4_096,
        )
        .unwrap_err();
    assert_eq!(unknown_reference, TransportError::Rejected);

    let manual = ManualClock::new();
    manual.advance_ms(1_000);
    let past_deadline = transport
        .post(
            &fixture.url("/"),
            REFERENCE,
            &[],
            Vec::new(),
            1_000,
            &CancelToken::new(),
            &manual,
            4_096,
        )
        .unwrap_err();
    assert_eq!(past_deadline, TransportError::Timeout);

    let silent = transport
        .post(
            &fixture.url("/"),
            REFERENCE,
            &[],
            Vec::new(),
            300,
            &CancelToken::new(),
            &clock,
            4_096,
        )
        .unwrap_err();
    assert_eq!(silent, TransportError::Timeout);
}

#[test]
fn bindings_share_one_host_effect_across_clones() {
    let authority = TestAuthority::new();
    let fixture = start_fixture(&authority, echo_ok);
    let resolver = RecordingResolver::with_secret(REFERENCE, CANARY);
    let transport = transport_for(&authority, approved(&fixture), Arc::clone(&resolver));
    let counter = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..3)
        .map(|_| {
            let transport = transport.clone();
            let url = fixture.url("/");
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                let request = request_to(url, Some(credential_use()));
                if transport.send(&request, &CancelToken::new(), None).is_ok() {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(resolver.asked().len(), 3);
}
