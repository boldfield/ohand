//! Provider requests over the core handle (V05b).
//!
//! The core decides, the native layer performs the HTTP exchange. A caller names only a job.
//! Everything that matters is derived inside the core from stored state at dispatch: the
//! pinned profile version, the route and capability authorization, the destinations and the
//! opaque credential reference (`jobs::configuration::resolve_execution_target`), and the
//! request text from the item's capture. The native layer receives the finished HTTP request,
//! the origins it may contact and the credential reference to resolve at send time; it never
//! chooses a destination and the core never sees a secret.
//!
//! The exchange runs off the core worker thread, so a slow provider cannot stall capture saves.
//!
//! * `ohand_core_set_provider_transport` registers the native transport callback for a handle
//!   (null clears it). The callback runs on an exchange thread, never on the worker, and must
//!   return promptly (start the request and return). Clearing or replacing the callback waits
//!   for a running invocation, cancels the handle's in-flight exchanges, and must not be called
//!   from inside the callback itself.
//! * `ohand_core_start_provider_exchange` queues the authorization and request build on the
//!   worker. Its operation produces up to two events with the same `operation_id`: a failure
//!   (nothing was sent), or `{"operation_id","phase":"dispatched"}` followed later by the final
//!   event, which is either `{"operation_id","phase":"completed","output":{...}}` or a
//!   normalized failure. A denied or revoked job never reaches the native callback.
//! * The callback receives `OHAND_PROVIDER_COMMAND_SEND` with a JSON description of the
//!   request, and `OHAND_PROVIDER_COMMAND_CANCEL` (no data) when the exchange was cancelled,
//!   ran past its deadline or its core closed. Native code answers a send exactly once with
//!   `ohand_core_complete_provider_exchange` (any HTTP status) or
//!   `ohand_core_fail_provider_exchange` (its own failure, by the snake_case transport error
//!   name); an answer after the exchange ended is `not_found` and is ignored.
//! * `ohand_core_cancel_provider_exchange` cancels one exchange through the provider
//!   `CancelToken`; the late result is discarded and the final event is `cancelled`.

use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::interpretation::instructions::M1_INSTRUCTION_VERSION;
use crate::jobs::configuration::{resolve_execution_target, ExecutionResolution};
use crate::privacy::routing::{DenialReason, JOB_TYPE_INTERPRET};
use crate::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, AnthropicTransport, HttpRequest, HttpResponse,
};
use crate::providers::contracts::{
    dispatch, CancelToken, Clock, DispatchLimits, ErrorClass, InterpretationOutput,
    InterpretationRequest, ProviderAdapter, ProviderProfile, ProviderProtocol, SystemClock,
    TextBasis, TransportError, PROFILE_SCHEMA_VERSION,
};
use crate::providers::openai::{HttpTransport, OpenAiAdapter};
use crate::time::TimeContext;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Native is asked to perform the request described by the JSON `data`.
pub const OHAND_PROVIDER_COMMAND_SEND: u32 = 1;
/// Native should abandon the exchange named by the operation ID; `data` is null.
pub const OHAND_PROVIDER_COMMAND_CANCEL: u32 = 2;
/// Longest start request accepted, in bytes.
pub const OHAND_CORE_MAX_PROVIDER_REQUEST_BYTES: usize = 4096;
/// Longest response header block or body accepted from native, in bytes.
pub const OHAND_CORE_MAX_PROVIDER_RESPONSE_BYTES: usize = 4_194_304;

/// Called on an exchange thread with `command` for the exchange `operation_id` of this handle.
/// `data` is borrowed for the duration of the call; the callback copies what it needs.
pub type OhandProviderTransportCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        operation_id: u64,
        command: u32,
        data: *const u8,
        len: usize,
    ),
>;

type TransportCallbackFn = unsafe extern "C" fn(*mut c_void, u64, u32, *const u8, usize);

const WAIT_SLICE: Duration = Duration::from_millis(25);
const QUEUE_RETRY_PAUSE: Duration = Duration::from_millis(10);
const QUEUE_RETRY_LIMIT: u32 = 500;

struct Registration {
    callback: TransportCallbackFn,
    context: usize,
    active: Mutex<bool>,
}

impl Registration {
    /// Invokes the callback unless the registration was cleared. Holding `active` across the
    /// call is what lets a clear wait for a running invocation.
    fn invoke(&self, operation_id: u64, command: u32, data: &[u8]) -> bool {
        let active = lock(&self.active);
        if !*active {
            return false;
        }
        let pointer = if data.is_empty() {
            std::ptr::null()
        } else {
            data.as_ptr()
        };
        // SAFETY: the registrant guaranteed callback and context stay valid until the
        // registration is cleared or replaced, both of which wait for `active`.
        unsafe {
            (self.callback)(
                self.context as *mut c_void,
                operation_id,
                command,
                pointer,
                data.len(),
            );
        }
        true
    }

    fn deactivate(&self) {
        *lock(&self.active) = false;
    }
}

enum Completion {
    Response(HttpResponse),
    Failure(TransportError),
}

struct Pending {
    sender: Sender<Completion>,
    cancel: CancelToken,
}

static TRANSPORTS: Mutex<BTreeMap<u64, Arc<Registration>>> = Mutex::new(BTreeMap::new());
static EXCHANGES: Mutex<BTreeMap<(u64, u64), Pending>> = Mutex::new(BTreeMap::new());

/// Removes the exchange slot when the exchange ends, however it ends.
struct SlotGuard {
    key: (u64, u64),
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        lock(&EXCHANGES).remove(&self.key);
    }
}

fn cancel_exchanges_of(handle: u64) {
    for ((exchange_handle, _), pending) in lock(&EXCHANGES).iter() {
        if *exchange_handle == handle {
            pending.cancel.cancel();
        }
    }
}

mod failures {
    use super::{AbiFailure, Cow, ErrorClass};

    pub(super) const fn fixed(
        class: ErrorClass,
        code: &'static str,
        message: &'static str,
    ) -> AbiFailure {
        AbiFailure {
            class,
            code,
            message: Cow::Borrowed(message),
        }
    }

    pub(super) const TRANSPORT_NOT_REGISTERED: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "transport_not_registered",
        "no native provider transport is registered for the handle",
    );
    pub(super) const DUPLICATE_OPERATION: AbiFailure = fixed(
        ErrorClass::Permanent,
        "duplicate_operation",
        "a provider exchange with that operation ID is already running",
    );
    pub(super) const NOT_REMOTE: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "not_remote",
        "the job does not use a remote provider",
    );
    pub(super) const UNSUPPORTED_JOB: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "unsupported_job",
        "only interpretation jobs can be sent to a provider",
    );
    pub(super) const UNSUPPORTED_PROTOCOL: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "protocol_unsupported",
        "the pinned profile's protocol has no native transport",
    );
    pub(super) const STALE_SOURCE: AbiFailure = fixed(
        ErrorClass::Permanent,
        "stale_source",
        "the item changed after the job was queued",
    );
    pub(super) const NO_TEXT: AbiFailure = fixed(
        ErrorClass::Permanent,
        "no_text",
        "the capture has no text to interpret",
    );
    pub(super) const INVALID_JOB: AbiFailure = fixed(
        ErrorClass::Permanent,
        "invalid_job",
        "the job or its capture cannot form a valid provider request",
    );
    pub(super) const MALFORMED_PROFILE: AbiFailure = fixed(
        ErrorClass::Permanent,
        "malformed_profile",
        "the pinned profile is not a valid provider profile",
    );
}

fn denial_failure(reason: DenialReason) -> AbiFailure {
    use failures::fixed;
    match reason {
        DenialReason::JobNotFound => AbiFailure::NOT_FOUND,
        DenialReason::JobRetired => fixed(
            ErrorClass::Permanent,
            "job_retired",
            "the job is finished or cancelled and must not be dispatched",
        ),
        DenialReason::UnknownJobType => failures::UNSUPPORTED_JOB,
        DenialReason::LocalOnlyJobHasProfile => failures::INVALID_JOB,
        DenialReason::ProfileUnavailable => fixed(
            ErrorClass::Permanent,
            "profile_unavailable",
            "the job's pinned profile version is unavailable",
        ),
        DenialReason::ProfileRevoked => fixed(
            ErrorClass::Unauthorized,
            "profile_revoked",
            "the job's pinned profile version has been revoked",
        ),
        DenialReason::UnknownProviderType => failures::UNSUPPORTED_PROTOCOL,
        DenialReason::MalformedProfile => failures::MALFORMED_PROFILE,
        DenialReason::RouteNotConfigured => fixed(
            ErrorClass::Unauthorized,
            "route_not_configured",
            "the capture's route has no stored configuration",
        ),
        DenialReason::MalformedPolicy => fixed(
            ErrorClass::Permanent,
            "malformed_policy",
            "stored route policy is malformed",
        ),
        DenialReason::DestinationNotInRoute => fixed(
            ErrorClass::Unauthorized,
            "destination_not_in_route",
            "the route does not permit the provider destination",
        ),
        DenialReason::CapabilityNotAuthorized => fixed(
            ErrorClass::Unauthorized,
            "capability_not_authorized",
            "the route has no authorization for this capability",
        ),
        DenialReason::DestinationNotAuthorizedForCapability => fixed(
            ErrorClass::Unauthorized,
            "destination_not_authorized",
            "the destination is not authorized for this capability",
        ),
    }
}

fn storage_failure(error: impl Into<anyhow::Error>) -> AbiFailure {
    AbiFailure::from_store(&error.into(), AbiFailure::STORAGE_ERROR)
}

fn parse_transport_error(name: &str) -> Option<TransportError> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

fn origin_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    if rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let authority = authority.strip_suffix(":443").unwrap_or(&authority);
    Some(format!("https://{authority}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    job_id: String,
}

#[derive(Serialize)]
struct CredentialJson<'a> {
    reference: &'a str,
    header: &'a str,
    scheme: Option<&'a str>,
}

#[derive(Serialize)]
struct SendCommand<'a> {
    operation_id: u64,
    url: &'a str,
    method: &'static str,
    headers: &'a [(String, String)],
    body: &'a str,
    timeout_ms: u64,
    max_response_bytes: u64,
    credential: Option<CredentialJson<'a>>,
    authorized_origins: &'a [String],
}

struct Outgoing<'a> {
    url: &'a str,
    headers: Vec<(String, String)>,
    body: &'a [u8],
    credential: Option<CredentialJson<'a>>,
    timeout_ms: u64,
    max_response_bytes: u64,
}

struct ExchangeEnd {
    handle: u64,
    operation_id: u64,
    registration: Arc<Registration>,
    receiver: Mutex<Receiver<Completion>>,
    authorized_origins: Vec<String>,
}

/// The core's HTTP effects, both adapter seams, backed by the registered native callback.
#[derive(Clone)]
struct NativeTransport {
    end: Arc<ExchangeEnd>,
}

impl NativeTransport {
    fn exchange(
        &self,
        outgoing: Outgoing<'_>,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let Outgoing {
            url,
            headers,
            body,
            credential,
            timeout_ms,
            max_response_bytes,
        } = outgoing;
        let end = &self.end;
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let authorized = origin_of(url).is_some_and(|origin| {
            end.authorized_origins
                .iter()
                .filter_map(|candidate| origin_of(candidate))
                .any(|candidate| candidate == origin)
        });
        if !authorized {
            return Err(TransportError::Rejected);
        }
        if timeout_ms == 0 {
            return Err(TransportError::Timeout);
        }
        let body = std::str::from_utf8(body).map_err(|_| TransportError::Rejected)?;
        let command = serde_json::to_vec(&SendCommand {
            operation_id: end.operation_id,
            url,
            method: "POST",
            headers: &headers,
            body,
            timeout_ms,
            max_response_bytes,
            credential,
            authorized_origins: &end.authorized_origins,
        })
        .map_err(|_| TransportError::Rejected)?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        if !end
            .registration
            .invoke(end.operation_id, OHAND_PROVIDER_COMMAND_SEND, &command)
        {
            return Err(TransportError::Unavailable);
        }
        let receiver = lock(&end.receiver);
        loop {
            match receiver.recv_timeout(WAIT_SLICE) {
                Ok(Completion::Response(response)) => return Ok(response),
                Ok(Completion::Failure(error)) => return Err(error),
                Err(RecvTimeoutError::Disconnected) => return Err(TransportError::Unavailable),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let abandoned = if cancel.is_cancelled() || instance::lookup(end.handle).is_err() {
                Some(TransportError::Cancelled)
            } else if Instant::now() >= deadline {
                Some(TransportError::Timeout)
            } else {
                None
            };
            if let Some(error) = abandoned {
                end.registration
                    .invoke(end.operation_id, OHAND_PROVIDER_COMMAND_CANCEL, &[]);
                return Err(error);
            }
        }
    }
}

impl AnthropicTransport for NativeTransport {
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let credential = request
            .credential
            .as_ref()
            .map(|attachment| CredentialJson {
                reference: &attachment.reference,
                header: &attachment.header,
                scheme: None,
            });
        self.exchange(
            Outgoing {
                url: &request.url,
                headers: request.headers.clone(),
                body: &request.body,
                credential,
                timeout_ms: request.timeout_ms,
                max_response_bytes: request.max_response_bytes as u64,
            },
            cancel,
        )
    }
}

impl HttpTransport for NativeTransport {
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
        let headers = headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let credential = Some(CredentialJson {
            reference: credential_ref,
            header: "Authorization",
            scheme: Some("Bearer"),
        });
        let response = self.exchange(
            Outgoing {
                url: endpoint,
                headers,
                body: &body,
                credential,
                timeout_ms: deadline_ms.saturating_sub(clock.now_ms()),
                max_response_bytes,
            },
            cancel,
        )?;
        Ok((response.status, response.body))
    }
}

struct Prepared {
    profile: ProviderProfile,
    request: InterpretationRequest,
    destinations: Vec<String>,
}

struct JobRow {
    job_type: String,
    source_revision: i64,
    request_version: Option<String>,
    item_revision: i64,
    capture_id: String,
    text: Option<String>,
    capture_instant: String,
    timezone_id: String,
    utc_offset_minutes: i32,
    locale: String,
    calendar: String,
    route_id: String,
}

fn load_profile(
    connection: &Connection,
    profile_version: &str,
) -> Result<ProviderProfile, AbiFailure> {
    type Row = (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        u32,
        String,
        String,
        String,
        String,
    );
    let row: Row = connection
        .query_row(
            "SELECT profile_id, provider_type, endpoint, model, credential_ref, timeout_seconds, \
             retry_policy, authorized_destinations, capabilities, created_at \
             FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()
        .map_err(storage_failure)?
        .ok_or(failures::MALFORMED_PROFILE)?;
    let (
        profile_id,
        provider_type,
        endpoint,
        model,
        credential_ref,
        timeout_seconds,
        retry_policy,
        authorized_destinations,
        capabilities,
        created_at,
    ) = row;
    let parse = |text: &str| {
        serde_json::from_str::<serde_json::Value>(text).map_err(|_| failures::MALFORMED_PROFILE)
    };
    let record = serde_json::json!({
        "schema_version": PROFILE_SCHEMA_VERSION,
        "profile_version": profile_version,
        "profile_id": profile_id,
        "protocol": provider_type,
        "endpoint": endpoint,
        "model": model,
        "credential_ref": credential_ref.filter(|reference| !reference.is_empty()),
        "timeout_seconds": timeout_seconds,
        "retry_policy": parse(&retry_policy)?,
        "authorized_destinations": parse(&authorized_destinations)?,
        "capabilities": parse(&capabilities)?,
        "created_at": created_at,
    });
    serde_json::from_value(record).map_err(|_| failures::MALFORMED_PROFILE)
}

fn load_job_row(connection: &Connection, job_id: &str) -> Result<JobRow, AbiFailure> {
    connection
        .query_row(
            "SELECT jobs.job_type, jobs.source_revision, jobs.request_version, items.revision, \
             captures.capture_id, captures.text, captures.capture_instant, captures.timezone_id, \
             captures.utc_offset_minutes, captures.locale, captures.calendar, captures.route_id \
             FROM jobs JOIN items ON items.item_id = jobs.item_id \
             JOIN captures ON captures.capture_id = items.capture_id WHERE jobs.job_id = ?",
            [job_id],
            |row| {
                Ok(JobRow {
                    job_type: row.get(0)?,
                    source_revision: row.get(1)?,
                    request_version: row.get(2)?,
                    item_revision: row.get(3)?,
                    capture_id: row.get(4)?,
                    text: row.get(5)?,
                    capture_instant: row.get(6)?,
                    timezone_id: row.get(7)?,
                    utc_offset_minutes: row.get(8)?,
                    locale: row.get(9)?,
                    calendar: row.get(10)?,
                    route_id: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(storage_failure)?
        .ok_or(AbiFailure::NOT_FOUND)
}

/// The job's item's latest user text correction as the contract's correction record identity
/// (the stored id without its `-correction` suffix) and the corrected text. That text, not the
/// immutable capture text, is the item's effective text once a correction exists.
fn latest_text_correction(
    connection: &Connection,
    job_id: &str,
) -> Result<Option<(String, String)>, AbiFailure> {
    let stored: Option<(String, String)> = connection
        .query_row(
            "SELECT corrections.correction_id, corrections.new_value FROM corrections \
             JOIN jobs ON jobs.item_id = corrections.item_id \
             WHERE jobs.job_id = ? AND corrections.kind = 'text' \
             ORDER BY corrections.revision DESC LIMIT 1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_failure)?;
    stored
        .map(|(correction_id, corrected_text)| {
            correction_id
                .strip_suffix("-correction")
                .map(|record_id| (record_id.to_string(), corrected_text))
                .ok_or(failures::INVALID_JOB)
        })
        .transpose()
}

/// Authorizes the job and builds the request, all from stored state. Nothing is sent and no
/// credential is touched; the first denial wins.
fn prepare(connection: &Connection, job_id: &str) -> Result<Prepared, AbiFailure> {
    let target = match resolve_execution_target(connection, job_id).map_err(storage_failure)? {
        ExecutionResolution::Denied(reason) => return Err(denial_failure(reason)),
        ExecutionResolution::Local => return Err(failures::NOT_REMOTE),
        ExecutionResolution::Remote(target) => target,
    };
    let row = load_job_row(connection, job_id)?;
    if row.job_type != JOB_TYPE_INTERPRET {
        return Err(failures::UNSUPPORTED_JOB);
    }
    let profile = load_profile(connection, &target.profile_version)?;
    if profile.protocol() == ProviderProtocol::SelfHosted {
        return Err(failures::UNSUPPORTED_PROTOCOL);
    }
    if target.credential_ref.as_deref() != Some(profile.credential_ref().as_str()) {
        return Err(failures::MALFORMED_PROFILE);
    }
    if row.item_revision != row.source_revision {
        return Err(failures::STALE_SOURCE);
    }
    let source_revision = u64::try_from(row.source_revision).map_err(|_| failures::INVALID_JOB)?;
    let (text, text_basis) = match latest_text_correction(connection, job_id)? {
        Some((correction_record_id, corrected_text)) => (
            corrected_text,
            TextBasis::Correction {
                correction_record_id,
                item_revision: source_revision,
            },
        ),
        None => (
            row.text.ok_or(failures::NO_TEXT)?,
            TextBasis::Original {
                item_revision: source_revision,
            },
        ),
    };
    if text.trim().is_empty() {
        return Err(failures::NO_TEXT);
    }
    let reference_time = DateTime::parse_from_rfc3339(&row.capture_instant)
        .map_err(|_| failures::INVALID_JOB)?
        .with_timezone(&Utc);
    let request = InterpretationRequest::new(
        row.capture_id,
        source_revision,
        text_basis,
        text,
        row.request_version.ok_or(failures::INVALID_JOB)?,
        M1_INSTRUCTION_VERSION,
        &profile,
        row.route_id,
        TimeContext {
            timezone: row.timezone_id,
            locale: row.locale,
            reference_time,
            utc_offset_at_capture: row.utc_offset_minutes.saturating_mul(60),
            calendar: row.calendar,
        },
    )
    .map_err(|_| failures::INVALID_JOB)?;
    Ok(Prepared {
        profile,
        request,
        destinations: target.destinations,
    })
}

#[derive(Serialize)]
struct Dispatched {
    operation_id: u64,
    phase: &'static str,
}

#[derive(Serialize)]
struct Completed<'a> {
    operation_id: u64,
    phase: &'static str,
    output: &'a InterpretationOutput,
}

fn run_exchange(
    prepared: &Prepared,
    end: Arc<ExchangeEnd>,
    cancel: &CancelToken,
) -> Result<Vec<u8>, AbiFailure> {
    let operation_id = end.operation_id;
    let transport = NativeTransport { end };
    let adapter: Box<dyn ProviderAdapter> = match prepared.profile.protocol() {
        ProviderProtocol::Anthropic => Box::new(AnthropicAdapter::new(
            Arc::new(transport),
            AnthropicSettings::default(),
        )),
        ProviderProtocol::OpenAi => Box::new(OpenAiAdapter::new(transport)),
        ProviderProtocol::SelfHosted => return Err(failures::UNSUPPORTED_PROTOCOL),
    };
    let clock = SystemClock::new();
    let output = dispatch(
        adapter.as_ref(),
        &prepared.profile,
        &prepared.request,
        &clock,
        cancel,
        &DispatchLimits::default(),
    )
    .map_err(|failure| AbiFailure::from_provider(&failure))?;
    serde_json::to_vec(&Completed {
        operation_id,
        phase: "completed",
        output: &output,
    })
    .map_err(|_| AbiFailure::INTERNAL)
}

/// Queues the final event for the exchange. The queue is FIFO, so it always follows the
/// `dispatched` event; if the core is gone, nobody is waiting and the result is dropped.
fn deliver_final(handle: u64, operation_id: u64, outcome: Result<Vec<u8>, AbiFailure>) {
    for _ in 0..QUEUE_RETRY_LIMIT {
        let Ok(core) = instance::lookup(handle) else {
            return;
        };
        let attempt = outcome.clone();
        let job: JobFn = Box::new(move |_| attempt);
        match core.submit(operation_id, job) {
            Err(failure) if failure == AbiFailure::QUEUE_FULL => {
                std::thread::sleep(QUEUE_RETRY_PAUSE);
            }
            _ => return,
        }
    }
}

fn start_exchange_job(
    handle: u64,
    operation_id: u64,
    job_id: String,
    registration: Arc<Registration>,
    slot: SlotGuard,
    receiver: Receiver<Completion>,
    cancel: CancelToken,
) -> JobFn {
    Box::new(move |database| {
        let prepared = {
            let database = lock(database);
            prepare(database.conn(), &job_id)?
        };
        let end = Arc::new(ExchangeEnd {
            handle,
            operation_id,
            registration,
            receiver: Mutex::new(receiver),
            authorized_origins: prepared.destinations.clone(),
        });
        std::thread::Builder::new()
            .name("ohand-provider-exchange".to_string())
            .spawn(move || {
                let _slot = slot;
                let outcome =
                    catch_unwind(AssertUnwindSafe(|| run_exchange(&prepared, end, &cancel)))
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

/// Registers `callback` with `context` as the native provider transport of `handle`, replacing
/// any previous registration; a null `callback` clears it. Either way the handle's running
/// exchanges are cancelled and a running invocation is waited for, so the previous context is
/// unused once this returns. Clearing is allowed for a handle that has been closed.
#[no_mangle]
pub extern "C" fn ohand_core_set_provider_transport(
    handle: OhandCoreHandle,
    callback: OhandProviderTransportCallback,
    context: *mut c_void,
) -> OhandCoreResult {
    guarded(|| {
        let previous = match callback {
            Some(callback) => {
                instance::lookup(handle)?;
                let registration = Arc::new(Registration {
                    callback,
                    context: context as usize,
                    active: Mutex::new(true),
                });
                lock(&TRANSPORTS).insert(handle, registration)
            }
            None => lock(&TRANSPORTS).remove(&handle),
        };
        if let Some(previous) = previous {
            cancel_exchanges_of(handle);
            previous.deactivate();
        }
        Ok(())
    })
}

/// Queues the provider exchange for the job named in the JSON `{"job_id"}` at `request`.
/// Authorization, destinations, the credential reference and the request text are all read from
/// stored state on the worker thread. See the module documentation for the events.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_provider_exchange(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        if request_len > OHAND_CORE_MAX_PROVIDER_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if request_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if request.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let start: StartRequest =
            serde_json::from_slice(std::slice::from_raw_parts(request, request_len))
                .map_err(|_| AbiFailure::INVALID_REQUEST)?;
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
        let job = start_exchange_job(
            handle,
            operation_id,
            start.job_id,
            registration,
            slot,
            receiver,
            cancel,
        );
        core.submit(operation_id, job)
    })
}

/// Cancels the running exchange `operation_id` of `handle`: native is told to abandon it, a
/// late answer is discarded and the final event is `cancelled`. Idempotent; an exchange that
/// already ended is not an error.
#[no_mangle]
pub extern "C" fn ohand_core_cancel_provider_exchange(
    handle: OhandCoreHandle,
    operation_id: u64,
) -> OhandCoreResult {
    guarded(|| {
        if let Some(pending) = lock(&EXCHANGES).get(&(handle, operation_id)) {
            pending.cancel.cancel();
        }
        Ok(())
    })
}

fn answer(handle: u64, operation_id: u64, completion: Completion) -> Result<(), AbiFailure> {
    let sender = lock(&EXCHANGES)
        .get(&(handle, operation_id))
        .map(|pending| pending.sender.clone())
        .ok_or(AbiFailure::NOT_FOUND)?;
    sender.send(completion).map_err(|_| AbiFailure::NOT_FOUND)
}

/// Answers the send for exchange `operation_id` with a completed HTTP exchange of any status.
/// `headers` is a JSON array of `[name, value]` pairs (null with length 0 for none) and `body`
/// the response bytes (null with length 0 for none). An answer for an exchange that has ended
/// is `not_found`.
///
/// # Safety
/// `headers` and `body` must each point at as many readable bytes as their length says (a null
/// pointer is allowed only with length 0).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_complete_provider_exchange(
    handle: OhandCoreHandle,
    operation_id: u64,
    http_status: u32,
    headers: *const u8,
    headers_len: usize,
    body: *const u8,
    body_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        if headers_len > OHAND_CORE_MAX_PROVIDER_RESPONSE_BYTES
            || body_len > OHAND_CORE_MAX_PROVIDER_RESPONSE_BYTES
        {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if (headers.is_null() && headers_len > 0) || (body.is_null() && body_len > 0) {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let status = u16::try_from(http_status)
            .ok()
            .filter(|status| (100..=599).contains(status))
            .ok_or(AbiFailure::INVALID_REQUEST)?;
        let header_pairs: Vec<(String, String)> = if headers_len == 0 {
            Vec::new()
        } else {
            serde_json::from_slice(std::slice::from_raw_parts(headers, headers_len))
                .map_err(|_| AbiFailure::INVALID_REQUEST)?
        };
        let body = if body_len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(body, body_len).to_vec()
        };
        answer(
            handle,
            operation_id,
            Completion::Response(HttpResponse {
                status,
                headers: header_pairs,
                body,
            }),
        )
    })
}

/// Answers the send for exchange `operation_id` with a native failure, named by the snake_case
/// core transport error (`timeout`, `cancelled`, `unavailable`, `rate_limited`,
/// `unauthorized`, `rejected`, `invalid_output`). An unknown name is `invalid_request`.
///
/// # Safety
/// `error` must point at `error_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_fail_provider_exchange(
    handle: OhandCoreHandle,
    operation_id: u64,
    error: *const u8,
    error_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        if error_len == 0 || error_len > 64 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if error.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let name = std::str::from_utf8(std::slice::from_raw_parts(error, error_len))
            .map_err(|_| AbiFailure::INVALID_UTF8)?;
        let error = parse_transport_error(name).ok_or(AbiFailure::INVALID_REQUEST)?;
        answer(handle, operation_id, Completion::Failure(error))
    })
}

#[cfg(test)]
mod tests;
