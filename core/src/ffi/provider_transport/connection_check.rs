//! Explicit provider connection check (V01c).
//!
//! The user asks, from setup, whether a stored profile version can be reached. The core builds one
//! fixed synthetic request with the profile's own protocol adapter, sends it through the provider
//! transport the host already registered (V05b), and reports a redacted classification. Nothing
//! here runs in the background or on a schedule: the only way to start a check is the export below.
//!
//! * `ohand_core_start_provider_connection_check` queues the check for the profile version named
//!   in the JSON `{"profile_version"}`. A caller names only a stored version. The request text is
//!   a fixed constant, never a capture, query or other user text, and the destinations are
//!   derived from the stored profile exactly as authorization derives them
//!   (`privacy::routing`, `contacted_destinations`); the caller cannot supply or widen them. A
//!   route grant is not required, because the user tests before granting; the check never reads
//!   or writes a route, a job, an item or any other state.
//! * Its events carry the same `operation_id`: a failure (nothing was sent: a missing or revoked
//!   profile, a malformed one, or no registered transport), or `{"phase":"dispatched"}` followed
//!   by `{"phase":"completed","outcome":code}`. A protocol the host has not enabled (T11c) is
//!   not sent: its only event is `completed` with the outcome `protocol_not_enabled`; a self-hosted profile
//!   whose version and model the host has not registered is likewise not sent and completes with
//!   `self_hosted_not_registered`.
//! * The exchange runs off the core worker thread, is answered with the ordinary provider
//!   exchange answers and is cancelled with `ohand_core_cancel_provider_exchange` (the outcome is
//!   then `cancelled`).
//!
//! The outcome is one fixed code. It never carries an endpoint, header, response body, credential
//! reference or secret, and neither do the failure strings. The core classifies from the HTTP
//! status or the native failure name the transport reported. The native layer reports a refused
//! certificate, a refused redirect and an invalid request alike as `rejected`; for this fixed,
//! valid request that is reported as `tls_failure`.

use super::shadow_review::{encode, parse_request};
use super::*;
use crate::providers::contracts::{FailureKind, ProviderFailure};

const CHECK_CAPTURE_ID: &str = "c0c0c0c0-0000-4000-8000-000000000001";
const CHECK_REQUEST_VERSION: &str = "c0c0c0c0-0000-4000-8000-000000000002";
const CHECK_ROUTE_ID: &str = "connection-check";
const CHECK_TEXT: &str = "Connection check. This request carries no user content.";
const MAX_PROFILE_VERSION_BYTES: usize = 512;

/// How a connection check ended. The codes are the fixed set native code may rely on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConnectionOutcome {
    Ok,
    CredentialMissing,
    CredentialRejected,
    Unreachable,
    TlsFailure,
    Timeout,
    CapabilityMismatch,
    ProtocolNotEnabled,
    SelfHostedNotRegistered,
    RateLimited,
    ProviderError,
    UnexpectedResponse,
    Cancelled,
}

/// What the transport reported for the one request, before any adapter mapped it.
#[derive(Debug, Clone, Copy)]
enum Observed {
    Status(u16),
    Failure(TransportError),
}

type ObservedSlot = Arc<Mutex<Option<Observed>>>;

/// Wraps the native transport to remember the raw HTTP status or transport failure, which the
/// adapters collapse into fewer transport errors than the outcome set distinguishes.
#[derive(Clone)]
struct RecordingTransport {
    inner: NativeTransport,
    observed: ObservedSlot,
}

impl RecordingTransport {
    fn note(&self, observation: Observed) {
        *lock(&self.observed) = Some(observation);
    }
}

impl AnthropicTransport for RecordingTransport {
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let result = AnthropicTransport::send(&self.inner, request, cancel);
        self.note(match &result {
            Ok(response) => Observed::Status(response.status),
            Err(error) => Observed::Failure(*error),
        });
        result
    }
}

impl HttpTransport for RecordingTransport {
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &CancelToken,
        clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let result = HttpTransport::post(
            &self.inner,
            endpoint,
            credential_ref,
            headers,
            body,
            deadline_ms,
            cancel,
            clock,
            max_response_bytes,
        );
        self.note(match &result {
            Ok((status, _)) => Observed::Status(*status),
            Err(error) => Observed::Failure(*error),
        });
        result
    }
}

fn outcome_for_status(status: u16) -> ConnectionOutcome {
    match status {
        200..=299 => ConnectionOutcome::UnexpectedResponse,
        401 | 403 => ConnectionOutcome::CredentialRejected,
        400 | 404 | 422 => ConnectionOutcome::CapabilityMismatch,
        408 => ConnectionOutcome::Timeout,
        429 => ConnectionOutcome::RateLimited,
        _ => ConnectionOutcome::ProviderError,
    }
}

fn outcome_for_transport_failure(error: TransportError) -> ConnectionOutcome {
    match error {
        TransportError::Timeout => ConnectionOutcome::Timeout,
        TransportError::Cancelled => ConnectionOutcome::Cancelled,
        TransportError::Unavailable => ConnectionOutcome::Unreachable,
        TransportError::RateLimited => ConnectionOutcome::RateLimited,
        TransportError::Unauthorized => ConnectionOutcome::CredentialMissing,
        TransportError::Rejected => ConnectionOutcome::TlsFailure,
        TransportError::InvalidOutput => ConnectionOutcome::UnexpectedResponse,
    }
}

fn classify(
    result: &Result<InterpretationOutput, ProviderFailure>,
    observed: Option<Observed>,
) -> ConnectionOutcome {
    let failure = match result {
        Ok(_) => return ConnectionOutcome::Ok,
        Err(failure) => failure,
    };
    match failure.kind {
        FailureKind::Cancelled => return ConnectionOutcome::Cancelled,
        FailureKind::Timeout => return ConnectionOutcome::Timeout,
        _ => {}
    }
    match observed {
        Some(Observed::Status(status)) => outcome_for_status(status),
        Some(Observed::Failure(error)) => outcome_for_transport_failure(error),
        None => match failure.kind {
            FailureKind::CapabilityUnavailable | FailureKind::InputTooLarge => {
                ConnectionOutcome::CapabilityMismatch
            }
            _ => ConnectionOutcome::ProviderError,
        },
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckRequest {
    profile_version: String,
}

#[derive(Serialize)]
struct CheckCompleted {
    operation_id: u64,
    phase: &'static str,
    outcome: ConnectionOutcome,
}

fn completed_event(operation_id: u64, outcome: ConnectionOutcome) -> Result<Vec<u8>, AbiFailure> {
    encode(&CheckCompleted {
        operation_id,
        phase: "completed",
        outcome,
    })
}

/// The origins a request for this profile can contact, derived as `privacy::routing` derives
/// them: a hosted profile contacts its declared vendor origins and a self-hosted profile only
/// its own endpoint's origin. The transport refuses any other origin.
fn contacted_origins(profile: &ProviderProfile) -> Result<Vec<String>, AbiFailure> {
    let origins = match profile.protocol() {
        ProviderProtocol::SelfHosted => {
            let authority = profile
                .endpoint()
                .and_then(|endpoint| endpoint.strip_prefix("https://"))
                .and_then(|rest| rest.split(['/', '?', '#']).next())
                .ok_or(failures::MALFORMED_PROFILE)?;
            vec![format!("https://{}", authority.to_ascii_lowercase())]
        }
        ProviderProtocol::Anthropic | ProviderProtocol::OpenAi => {
            profile.authorized_destinations().to_vec()
        }
    };
    if origins.is_empty() || origins.iter().any(|origin| origin_of(origin).is_none()) {
        return Err(failures::MALFORMED_PROFILE);
    }
    Ok(origins)
}

fn synthetic_request(profile: &ProviderProfile) -> Result<InterpretationRequest, AbiFailure> {
    let reference_time = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .map_err(|_| AbiFailure::INTERNAL)?
        .with_timezone(&Utc);
    InterpretationRequest::new(
        CHECK_CAPTURE_ID,
        0,
        TextBasis::Original { item_revision: 0 },
        CHECK_TEXT,
        CHECK_REQUEST_VERSION,
        M1_INSTRUCTION_VERSION,
        profile,
        CHECK_ROUTE_ID,
        TimeContext {
            timezone: "UTC".to_string(),
            locale: "en_US".to_string(),
            reference_time,
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        },
    )
    .map_err(|_| failures::INVALID_JOB)
}

struct CheckPlan {
    profile: ProviderProfile,
    request: InterpretationRequest,
    destinations: Vec<String>,
}

/// Reads the stored profile and builds the fixed request. Nothing is sent and no credential is
/// touched; a missing profile is `not_found` and a revoked one is refused.
fn prepare_check(connection: &Connection, profile_version: &str) -> Result<CheckPlan, AbiFailure> {
    let revoked_at: Option<Option<String>> = connection
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_failure)?;
    match revoked_at {
        None => return Err(AbiFailure::NOT_FOUND),
        Some(Some(_)) => return Err(denial_failure(DenialReason::ProfileRevoked)),
        Some(None) => {}
    }
    let profile = load_profile(connection, profile_version)?;
    let destinations = contacted_origins(&profile)?;
    let request = synthetic_request(&profile)?;
    Ok(CheckPlan {
        profile,
        request,
        destinations,
    })
}

#[cfg(test)]
static BEFORE_SEND: Mutex<Option<fn(u64)>> = Mutex::new(None);

#[cfg(test)]
fn before_send_hook(handle: u64) {
    let hook = *lock(&BEFORE_SEND);
    if let Some(hook) = hook {
        hook(handle);
    }
}

/// The outcome of a check the host's protocol and self-hosted registration settings refuse.
fn refusal_outcome(handle: u64, profile: &ProviderProfile) -> Option<ConnectionOutcome> {
    match protocols::permission(handle, profile) {
        protocols::Permission::Allowed => None,
        protocols::Permission::ProtocolNotEnabled => Some(ConnectionOutcome::ProtocolNotEnabled),
        protocols::Permission::SelfHostedNotRegistered => {
            Some(ConnectionOutcome::SelfHostedNotRegistered)
        }
    }
}

fn run_check(
    plan: &CheckPlan,
    end: Arc<ExchangeEnd>,
    cancel: &CancelToken,
) -> Result<Vec<u8>, AbiFailure> {
    let operation_id = end.operation_id;
    #[cfg(test)]
    before_send_hook(end.handle);
    // The host may disable the protocol between preparation on the worker and this send.
    if let Some(outcome) = refusal_outcome(end.handle, &plan.profile) {
        return completed_event(operation_id, outcome);
    }
    let observed: ObservedSlot = Arc::new(Mutex::new(None));
    let transport = RecordingTransport {
        inner: NativeTransport { end },
        observed: Arc::clone(&observed),
    };
    let adapter: Box<dyn ProviderAdapter> = match plan.profile.protocol() {
        ProviderProtocol::Anthropic => Box::new(AnthropicAdapter::new(
            Arc::new(transport),
            AnthropicSettings::default(),
        )),
        ProviderProtocol::OpenAi => Box::new(OpenAiAdapter::new(transport)),
        ProviderProtocol::SelfHosted => Box::new(SelfHostedAdapter::new(transport)),
    };
    let clock = SystemClock::new();
    let result = dispatch(
        adapter.as_ref(),
        &plan.profile,
        &plan.request,
        &clock,
        cancel,
        &DispatchLimits::default(),
    );
    let observation = *lock(&observed);
    completed_event(operation_id, classify(&result, observation))
}

fn start_check_job(
    handle: u64,
    operation_id: u64,
    profile_version: String,
    registration: Arc<Registration>,
    slot: SlotGuard,
    receiver: Receiver<Completion>,
    cancel: CancelToken,
) -> JobFn {
    Box::new(move |database| {
        let prepared = {
            let database = lock(database);
            prepare_check(database.conn(), &profile_version)?
        };
        if let Some(outcome) = refusal_outcome(handle, &prepared.profile) {
            return completed_event(operation_id, outcome);
        }
        let end = Arc::new(ExchangeEnd {
            handle,
            operation_id,
            registration,
            receiver: Mutex::new(receiver),
            authorized_origins: prepared.destinations.clone(),
            capability: CAPABILITY_TEXT_INTERPRETATION,
        });
        std::thread::Builder::new()
            .name("ohand-provider-connection-check".to_string())
            .spawn(move || {
                let _slot = slot;
                let outcome = catch_unwind(AssertUnwindSafe(|| run_check(&prepared, end, &cancel)))
                    .unwrap_or(Err(AbiFailure::INTERNAL));
                deliver_final(handle, operation_id, outcome);
            })
            .map_err(|_| AbiFailure::INTERNAL)?;
        serde_json::to_vec(&Dispatched {
            operation_id,
            phase: "dispatched",
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues the connection check for the stored profile version named in the JSON
/// `{"profile_version"}` at `request`. See the module documentation for the events.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_provider_connection_check(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let check: CheckRequest = parse_request(request, request_len)?;
        if check.profile_version.is_empty()
            || check.profile_version.len() > MAX_PROFILE_VERSION_BYTES
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let registration = lock(&TRANSPORTS)
            .get(&handle)
            .cloned()
            .ok_or(failures::TRANSPORT_NOT_REGISTERED)?;

        let (sender, receiver) = channel();
        let cancel = CancelToken::new();
        let key = (handle, operation_id);
        {
            let mut exchanges = lock(&EXCHANGES);
            if exchanges.contains_key(&key) {
                return Err(failures::DUPLICATE_OPERATION);
            }
            exchanges.insert(
                key,
                Pending {
                    sender,
                    cancel: cancel.clone(),
                },
            );
        }
        let slot = SlotGuard { key };
        let job = start_check_job(
            handle,
            operation_id,
            check.profile_version,
            registration,
            slot,
            receiver,
            cancel,
        );
        core.submit(operation_id, job)
    })
}

#[cfg(test)]
mod tests;
