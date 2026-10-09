//! Job drain over the core handle (J02b).
//!
//! `ohand_core_start_job_drain` runs the J02a [`JobRunner`] once: it works through the jobs that
//! are ready (queued jobs whose backoff passed, running jobs whose lease expired) and reports what
//! happened. Retry budgets, backoff, lease recovery and fencing stay in the core; this module only
//! lends the runner the native effects it needs and delivers the report.
//!
//! * **Capture is never blocked.** The drain runs on its own thread over its own connection to the
//!   store at the path the handle was opened with, so a slow provider call never holds the
//!   connection the core worker uses to save captures. A private in-memory store cannot be shared
//!   and is refused (`store_not_shareable`).
//! * **One drain per handle.** A second start while one runs is refused (`drain_in_progress`), so
//!   repeated or overlapping activation cannot create competing drainers. The refusal changes
//!   nothing; the caller decides whether to run again after the final event.
//! * **Offline is not a drain stopper.** `ohand_core_set_job_network_reachable` tells the core
//!   whether provider calls can succeed. The gate sits at the provider-send boundary: a request
//!   that would leave the device while the host has no network is refused there, and its job is
//!   deferred without cost while the drain moves on to on-device and local jobs. Work that never
//!   reaches the transport, such as a fast-path interpretation of a remotely routed capture,
//!   completes offline.
//! * **Cancellation is a checkpoint.** `ohand_core_cancel_job_drain` fires the runner's
//!   `CancelToken`. A provider call in flight is abandoned and its job is re-queued at once
//!   without spending retry budget; the drain ends with `stop: "cancelled"` (or `"interrupted"`).
//!   Clearing the host, and closing the core, cancel the drain the same way.
//! * **The host** is one native callback per handle (`ohand_core_set_job_host`). The drain runs on
//!   its own thread and calls it there, never on the worker, and the callback must return
//!   promptly. It carries two kinds of request, each answered exactly once:
//!   - `OHAND_JOB_HOST_COMMAND_SEND`: perform the provider HTTP request in the JSON description
//!     (destination, headers, credential *reference* and authorized origins all come from stored
//!     policy at dispatch; the core never sees a secret). Answer with
//!     `ohand_core_complete_job_exchange` (any HTTP status) or `ohand_core_fail_job_exchange`.
//!   - `OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY`: run a job of one of the job types the host
//!     declared when it registered (for example transcription). The JSON holds `job_id`,
//!     `job_type` and `attempt`, the lease token. The host applies its result through the core's
//!     own operations (which settle the job under that lease) and then answers with
//!     `ohand_core_finish_job_capability`. A job type nobody registered is not run and not failed:
//!     the runner defers it as `capability_unavailable`.
//!
//!   `OHAND_JOB_HOST_COMMAND_CANCEL_EXCHANGE` and `OHAND_JOB_HOST_COMMAND_CANCEL_CAPABILITY` tell
//!   the host to abandon the named request; an answer that arrives afterwards is `not_found`.
//! * The drain's final event carries the same `operation_id`:
//!   `{"operation_id","stop","jobs":[{"job_id","job_type","result","settlement"}]}`, content-free
//!   labels only. A drain that could not start working at all is a normalized failure.

use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::interpretation::dispatch::{AdapterRegistry, InterpretationDispatcher};
use crate::jobs::configuration::{resolve_execution_target, ExecutionResolution};
use crate::jobs::queue::{get_job, Job, JobStatus};
use crate::jobs::runner::{
    CapabilityError, CapabilityOutcome, DrainReport, InterpretationCapability, JobCapabilities,
    JobCapability, JobResult, JobRunner, RunnerConfig, Settlement, StopReason,
};
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use crate::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, AnthropicTransport, HttpRequest, HttpResponse,
};
use crate::providers::contracts::{
    CancelToken, Clock, ErrorClass, ProviderProtocol, SystemClock, TransportError,
};
use crate::providers::openai::{HttpTransport, OpenAiAdapter};
use crate::store::schema::{Database, SystemClock as StoreClock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The host is asked to perform the provider HTTP request described by the JSON `data`.
pub const OHAND_JOB_HOST_COMMAND_SEND: u32 = 1;
/// The host should abandon the exchange named by the request ID; `data` is null.
pub const OHAND_JOB_HOST_COMMAND_CANCEL_EXCHANGE: u32 = 2;
/// The host is asked to run the job described by the JSON `data` (`job_id`, `job_type`, `attempt`).
pub const OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY: u32 = 3;
/// The host should stop the capability run named by the request ID; `data` is null.
pub const OHAND_JOB_HOST_COMMAND_CANCEL_CAPABILITY: u32 = 4;

/// The host settled the job through the core's own operations.
pub const OHAND_JOB_CAPABILITY_SETTLED: u32 = 0;
/// A fault expected to pass; the runner counts it against the job's retry budget.
pub const OHAND_JOB_CAPABILITY_TRANSIENT: u32 = 1;
/// A fault retrying cannot fix; the job ends `failed`.
pub const OHAND_JOB_CAPABILITY_PERMANENT: u32 = 2;
/// The host stopped at a checkpoint; the job is re-queued at once and nothing is spent.
pub const OHAND_JOB_CAPABILITY_INTERRUPTED: u32 = 3;

/// Longest store path or job type list accepted, in bytes.
pub const OHAND_CORE_MAX_JOB_RUNNER_REQUEST_BYTES: usize = 4096;
/// Longest response header block or body accepted from the host, in bytes.
pub const OHAND_CORE_MAX_JOB_EXCHANGE_RESPONSE_BYTES: usize = 4_194_304;

/// Called on a drain thread with `command` for request `request_id` of this handle. `data` is
/// borrowed for the duration of the call; the callback copies what it needs.
pub type OhandJobHostCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        request_id: u64,
        command: u32,
        data: *const u8,
        len: usize,
    ),
>;

type HostCallbackFn = unsafe extern "C" fn(*mut c_void, u64, u32, *const u8, usize);

const WAIT_SLICE: Duration = Duration::from_millis(25);
const QUEUE_RETRY_PAUSE: Duration = Duration::from_millis(10);
const QUEUE_RETRY_LIMIT: u32 = 500;
/// A native capability run is abandoned before the runner's lease would lapse.
const CAPABILITY_DEADLINE: Duration = Duration::from_secs(240);
/// Recorded on a provider job put back because the host reported no network.
const OFFLINE_DEFERRED_REASON: &str = "offline_deferred";
/// What the dispatcher records on a job whose provider request was cancelled.
const CANCELLED_REASON: &str = "cancelled";
/// How long an offline-deferred job waits; the next drain that starts with a network releases it
/// at once, so this only bounds the wait if that release could not be written.
const OFFLINE_DEFERRAL: chrono::Duration = chrono::Duration::seconds(30);
const MAX_REASON_BYTES: usize = 64;
const FALLBACK_REASON: &str = "native_failure";

struct Registration {
    callback: HostCallbackFn,
    context: usize,
    native_job_types: Vec<String>,
    active: Mutex<bool>,
    /// Whether provider work can reach the network. Only the host knows; it starts `true`.
    network_available: AtomicBool,
}

impl Registration {
    /// Invokes the callback unless the registration was cleared. Holding `active` across the call
    /// is what lets a clear wait for a running invocation.
    fn invoke(&self, request_id: u64, command: u32, data: &[u8]) -> bool {
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
                request_id,
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

enum ExchangeAnswer {
    Response(HttpResponse),
    Failure(TransportError),
}

struct CapabilityAnswer {
    outcome: u32,
    reason: String,
}

static HOSTS: Mutex<BTreeMap<u64, Arc<Registration>>> = Mutex::new(BTreeMap::new());
static DRAINS: Mutex<BTreeMap<u64, CancelToken>> = Mutex::new(BTreeMap::new());
static EXCHANGES: Mutex<BTreeMap<(u64, u64), Sender<ExchangeAnswer>>> = Mutex::new(BTreeMap::new());
static CAPABILITIES: Mutex<BTreeMap<(u64, u64), Sender<CapabilityAnswer>>> =
    Mutex::new(BTreeMap::new());
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Removes the drain slot when the drain ends, however it ends.
struct DrainSlot {
    handle: u64,
}

impl Drop for DrainSlot {
    fn drop(&mut self) {
        lock(&DRAINS).remove(&self.handle);
    }
}

/// Removes a request's answer slot when its wait ends, however it ends.
struct RequestSlot<'a, T> {
    map: &'a Mutex<BTreeMap<(u64, u64), Sender<T>>>,
    key: (u64, u64),
}

impl<T> Drop for RequestSlot<'_, T> {
    fn drop(&mut self) {
        lock(self.map).remove(&self.key);
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

    pub(super) const HOST_NOT_REGISTERED: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "host_not_registered",
        "no native job host is registered for the handle",
    );
    pub(super) const DRAIN_IN_PROGRESS: AbiFailure = fixed(
        ErrorClass::Transient,
        "drain_in_progress",
        "a job drain is already running for the handle",
    );
    pub(super) const STORE_NOT_SHAREABLE: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "store_not_shareable",
        "an in-memory store cannot be opened by a second connection",
    );
    pub(super) const INVALID_JOB_TYPE: AbiFailure = fixed(
        ErrorClass::Permanent,
        "invalid_job_type",
        "a native job type is malformed or is owned by the core",
    );
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

fn is_valid_job_type(job_type: &str) -> bool {
    !job_type.is_empty()
        && job_type.len() <= MAX_REASON_BYTES
        && job_type != JOB_TYPE_INTERPRET
        && job_type
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// A reason is stored on the job and shown to the user, so it is a short label, never content.
fn sanitized_reason(reason: &str) -> String {
    let valid = !reason.is_empty()
        && reason.len() <= MAX_REASON_BYTES
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        reason.to_string()
    } else {
        FALLBACK_REASON.to_string()
    }
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

/// The core's provider HTTP effects, both adapter seams, backed by the registered host. The
/// origins a request may reach are set by [`ScopedInterpretation`] from the job's stored route
/// authorization before each job runs; with none set, nothing is sent.
#[derive(Clone)]
struct HostTransport {
    handle: u64,
    host: Arc<Registration>,
    authorized_origins: Arc<Mutex<Vec<String>>>,
    /// Set when a request was refused because the host has no network, so the job that made it
    /// can be deferred instead of ending the drain.
    refused_offline: Arc<AtomicBool>,
}

impl HostTransport {
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
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if !self.host.network_available.load(Ordering::SeqCst) {
            self.refused_offline.store(true, Ordering::SeqCst);
            return Err(TransportError::Cancelled);
        }
        let authorized_origins = lock(&self.authorized_origins).clone();
        let authorized = origin_of(url).is_some_and(|origin| {
            authorized_origins
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

        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        let key = (self.handle, request_id);
        lock(&EXCHANGES).insert(key, sender);
        let _slot = RequestSlot {
            map: &EXCHANGES,
            key,
        };
        let command = serde_json::to_vec(&SendCommand {
            operation_id: request_id,
            url,
            method: "POST",
            headers: &headers,
            body,
            timeout_ms,
            max_response_bytes,
            credential,
            authorized_origins: &authorized_origins,
        })
        .map_err(|_| TransportError::Rejected)?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        if !self
            .host
            .invoke(request_id, OHAND_JOB_HOST_COMMAND_SEND, &command)
        {
            return Err(TransportError::Unavailable);
        }
        loop {
            match receiver.recv_timeout(WAIT_SLICE) {
                Ok(ExchangeAnswer::Response(response)) => return Ok(response),
                Ok(ExchangeAnswer::Failure(error)) => return Err(error),
                Err(RecvTimeoutError::Disconnected) => return Err(TransportError::Unavailable),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let abandoned = if cancel.is_cancelled() || instance::lookup(self.handle).is_err() {
                Some(TransportError::Cancelled)
            } else if Instant::now() >= deadline {
                Some(TransportError::Timeout)
            } else {
                None
            };
            if let Some(error) = abandoned {
                self.host
                    .invoke(request_id, OHAND_JOB_HOST_COMMAND_CANCEL_EXCHANGE, &[]);
                return Err(error);
            }
        }
    }
}

impl AnthropicTransport for HostTransport {
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

impl HttpTransport for HostTransport {
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

/// Interpretation jobs, with the origins the provider may be sent to taken from the job's stored
/// route authorization immediately before it runs. Fast-path and local jobs never reach the
/// transport, so they leave the set unused and run whether or not the device is online. Only a
/// request that actually reaches the transport while the host has no network is refused there.
struct ScopedInterpretation<'a> {
    handle: u64,
    inner: InterpretationCapability<'a>,
    authorized_origins: Arc<Mutex<Vec<String>>>,
    refused_offline: Arc<AtomicBool>,
}

impl JobCapability for ScopedInterpretation<'_> {
    fn job_type(&self) -> &str {
        self.inner.job_type()
    }

    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError> {
        if instance::lookup(self.handle).is_err() {
            cancel.cancel();
            return Ok(CapabilityOutcome::Interrupted);
        }
        let destinations = match resolve_execution_target(db.conn(), &claimed.job_id) {
            Ok(ExecutionResolution::Remote(target)) => target.destinations,
            _ => Vec::new(),
        };
        *lock(&self.authorized_origins) = destinations;
        self.refused_offline.store(false, Ordering::SeqCst);
        let outcome = self.inner.run(db, claimed, cancel, now)?;
        if self.refused_offline.swap(false, Ordering::SeqCst)
            && matches!(outcome, CapabilityOutcome::Settled(Settlement::Interrupted))
        {
            return defer_until_network(db, claimed, now)
                .map(|deferred| deferred.unwrap_or(outcome));
        }
        Ok(outcome)
    }
}

/// The dispatcher put a job back at once after its provider request was refused for lack of a
/// network (a cancelled request costs nothing but would end the drain). Move that job to a short
/// wait instead, so on-device and local jobs queued behind it still run while the device is
/// offline. `None` when the job is no longer in that state, which leaves the dispatcher's result
/// to stand.
fn defer_until_network(
    db: &mut Database,
    claimed: &Job,
    now: DateTime<Utc>,
) -> Result<Option<CapabilityOutcome>, CapabilityError> {
    let retry_at = now + OFFLINE_DEFERRAL;
    let affected = db
        .conn()
        .execute(
            "UPDATE jobs SET failure_reason = ?, next_attempt_at = ?
              WHERE job_id = ? AND status = ? AND failure_reason = ? AND attempt_count = ?",
            rusqlite::params![
                OFFLINE_DEFERRED_REASON,
                retry_at.to_rfc3339(),
                claimed.job_id,
                JobStatus::Queued.as_str(),
                CANCELLED_REASON,
                claimed.attempt_count
            ],
        )
        .map_err(|error| CapabilityError::Internal(error.to_string()))?;
    Ok(
        (affected == 1).then_some(CapabilityOutcome::Settled(Settlement::BackedOff {
            retry_at,
        })),
    )
}

/// Makes the jobs an offline drain deferred ready again; called when a drain starts with a network.
fn release_offline_deferred(db: &Database, now: DateTime<Utc>) {
    let _ = db.conn().execute(
        "UPDATE jobs SET next_attempt_at = ? WHERE status = ? AND failure_reason = ?",
        rusqlite::params![
            now.to_rfc3339(),
            JobStatus::Queued.as_str(),
            OFFLINE_DEFERRED_REASON
        ],
    );
}

/// A job type the host declared it can run. The host applies its own result through the core's
/// operations; this only hands over the job and waits for the host's answer.
struct HostCapability {
    job_type: String,
    handle: u64,
    host: Arc<Registration>,
}

#[derive(Serialize)]
struct RunCommand<'a> {
    job_id: &'a str,
    job_type: &'a str,
    attempt: i32,
}

impl JobCapability for HostCapability {
    fn job_type(&self) -> &str {
        &self.job_type
    }

    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError> {
        if instance::lookup(self.handle).is_err() {
            cancel.cancel();
        }
        if cancel.is_cancelled() {
            return Ok(CapabilityOutcome::Interrupted);
        }
        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        let key = (self.handle, request_id);
        lock(&CAPABILITIES).insert(key, sender);
        let _slot = RequestSlot {
            map: &CAPABILITIES,
            key,
        };
        let command = serde_json::to_vec(&RunCommand {
            job_id: &claimed.job_id,
            job_type: &claimed.job_type,
            attempt: claimed.attempt_count,
        })
        .map_err(|error| CapabilityError::Internal(error.to_string()))?;
        if !self
            .host
            .invoke(request_id, OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY, &command)
        {
            return Ok(CapabilityOutcome::Interrupted);
        }
        let deadline = Instant::now() + CAPABILITY_DEADLINE;
        loop {
            match receiver.recv_timeout(WAIT_SLICE) {
                Ok(answer) => return Ok(capability_outcome(answer, db, claimed, now)),
                Err(RecvTimeoutError::Disconnected) => return Ok(CapabilityOutcome::Interrupted),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let stop = if cancel.is_cancelled() || instance::lookup(self.handle).is_err() {
                Some(CapabilityOutcome::Interrupted)
            } else if Instant::now() >= deadline {
                Some(CapabilityOutcome::TransientFailure {
                    reason: "timeout".to_string(),
                })
            } else {
                None
            };
            if let Some(outcome) = stop {
                self.host
                    .invoke(request_id, OHAND_JOB_HOST_COMMAND_CANCEL_CAPABILITY, &[]);
                return Ok(outcome);
            }
        }
    }
}

/// What the host's settlement did to the job, read back from the store so the report says
/// `failed` or `retired` rather than `completed` when that is what the host recorded.
fn settlement_of_host_answer(db: &Database, claimed: &Job, now: DateTime<Utc>) -> Settlement {
    match get_job(db, &claimed.job_id) {
        Ok(Some(job)) => match job.status {
            JobStatus::Failed => Settlement::Failed,
            JobStatus::Cancelled => Settlement::Retired,
            JobStatus::Queued => Settlement::BackedOff {
                retry_at: job.next_attempt_at.unwrap_or(now),
            },
            JobStatus::Completed | JobStatus::Running => Settlement::Completed,
        },
        _ => Settlement::Completed,
    }
}

fn capability_outcome(
    answer: CapabilityAnswer,
    db: &Database,
    claimed: &Job,
    now: DateTime<Utc>,
) -> CapabilityOutcome {
    match answer.outcome {
        OHAND_JOB_CAPABILITY_SETTLED => {
            CapabilityOutcome::Settled(settlement_of_host_answer(db, claimed, now))
        }
        OHAND_JOB_CAPABILITY_TRANSIENT => CapabilityOutcome::TransientFailure {
            reason: answer.reason,
        },
        OHAND_JOB_CAPABILITY_PERMANENT => CapabilityOutcome::PermanentFailure {
            reason: answer.reason,
        },
        _ => CapabilityOutcome::Interrupted,
    }
}

#[derive(Serialize)]
struct JobSummary {
    job_id: String,
    job_type: String,
    result: &'static str,
    settlement: Option<&'static str>,
}

#[derive(Serialize)]
struct DrainSummary {
    operation_id: u64,
    stop: &'static str,
    jobs: Vec<JobSummary>,
}

fn settlement_label(settlement: &Settlement) -> &'static str {
    match settlement {
        Settlement::Completed => "completed",
        Settlement::Failed => "failed",
        Settlement::BackedOff { .. } => "backed_off",
        Settlement::Interrupted => "interrupted",
        Settlement::Retired => "retired",
    }
}

fn result_labels(result: &JobResult) -> (&'static str, Option<&'static str>) {
    match result {
        JobResult::Settled(settlement) => ("settled", Some(settlement_label(settlement))),
        JobResult::RetryScheduled { .. } => ("retry_scheduled", None),
        JobResult::RetriesExhausted => ("retries_exhausted", None),
        JobResult::FailedPermanently { .. } => ("failed_permanently", None),
        JobResult::Interrupted => ("interrupted", None),
        JobResult::CapabilityUnavailable { .. } => ("capability_unavailable", None),
        JobResult::LeaseLost => ("lease_lost", None),
        JobResult::Unsettled { .. } => ("unsettled", None),
    }
}

fn stop_label(stop: &StopReason) -> &'static str {
    match stop {
        StopReason::Idle => "idle",
        StopReason::JobLimit => "job_limit",
        StopReason::TimeBudget => "time_budget",
        StopReason::Cancelled => "cancelled",
        StopReason::Interrupted => "interrupted",
        StopReason::ClaimFailed(_) => "claim_failed",
    }
}

fn summarize(operation_id: u64, report: &DrainReport) -> DrainSummary {
    DrainSummary {
        operation_id,
        stop: stop_label(&report.stop),
        jobs: report
            .jobs
            .iter()
            .map(|job| {
                let (result, settlement) = result_labels(&job.result);
                JobSummary {
                    job_id: job.job_id.clone(),
                    job_type: job.job_type.clone(),
                    result,
                    settlement,
                }
            })
            .collect(),
    }
}

fn run_drain(
    handle: u64,
    operation_id: u64,
    path: &str,
    host: Arc<Registration>,
    cancel: &CancelToken,
) -> Result<Vec<u8>, AbiFailure> {
    let store_clock = Arc::new(StoreClock);
    let mut database = Database::open(path, store_clock.clone())
        .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORE_UNAVAILABLE))?;
    if host.network_available.load(Ordering::SeqCst) {
        release_offline_deferred(&database, Utc::now());
    }

    let authorized_origins = Arc::new(Mutex::new(Vec::new()));
    let refused_offline = Arc::new(AtomicBool::new(false));
    let transport = HostTransport {
        handle,
        host: Arc::clone(&host),
        authorized_origins: Arc::clone(&authorized_origins),
        refused_offline: Arc::clone(&refused_offline),
    };
    let registry = AdapterRegistry::new()
        .with_adapter(
            ProviderProtocol::Anthropic,
            AnthropicAdapter::new(Arc::new(transport.clone()), AnthropicSettings::default()),
        )
        .with_adapter(ProviderProtocol::OpenAi, OpenAiAdapter::new(transport));
    let provider_clock = SystemClock::new();
    let mut capabilities = JobCapabilities::new().with(ScopedInterpretation {
        handle,
        inner: InterpretationCapability::new(InterpretationDispatcher::new(
            &registry,
            &provider_clock,
        )),
        authorized_origins,
        refused_offline,
    });
    for job_type in &host.native_job_types {
        capabilities = capabilities.with(HostCapability {
            job_type: job_type.clone(),
            handle,
            host: Arc::clone(&host),
        });
    }
    let runner = JobRunner::new(capabilities, store_clock.as_ref(), RunnerConfig::default());
    let report = runner.drain(&mut database, cancel);
    serde_json::to_vec(&summarize(operation_id, &report)).map_err(|_| AbiFailure::INTERNAL)
}

/// Queues the final event for the drain. The queue is FIFO; if the core is gone, nobody is
/// waiting and the result is dropped.
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

#[derive(Deserialize)]
#[serde(transparent)]
struct JobTypeList(Vec<String>);

/// Registers `callback` with `context` as the native job host of `handle`, replacing any previous
/// registration. `job_types` is a JSON array of the job types the host can run itself (null with
/// length 0 for none); `interpret` belongs to the core and is refused. A null `callback` clears
/// the registration; with a non-null `context` it clears only if that context is still the
/// registered one, so a superseded registrant can release itself without removing its
/// replacement. Replacing or clearing cancels the handle's running drain and waits for a running
/// invocation, so the previous context is unused once this returns. Clearing is allowed for a
/// handle that has been closed. Must not be called from inside the callback itself.
///
/// # Safety
/// `job_types` must point at `job_types_len` readable bytes (null only with length 0).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_set_job_host(
    handle: OhandCoreHandle,
    callback: OhandJobHostCallback,
    context: *mut c_void,
    job_types: *const u8,
    job_types_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let previous = match callback {
            Some(callback) => {
                instance::lookup(handle)?;
                if job_types_len > OHAND_CORE_MAX_JOB_RUNNER_REQUEST_BYTES {
                    return Err(AbiFailure::REQUEST_TOO_LARGE);
                }
                if job_types.is_null() && job_types_len > 0 {
                    return Err(AbiFailure::NULL_ARGUMENT);
                }
                let mut native_job_types = if job_types_len == 0 {
                    Vec::new()
                } else {
                    serde_json::from_slice::<JobTypeList>(std::slice::from_raw_parts(
                        job_types,
                        job_types_len,
                    ))
                    .map_err(|_| AbiFailure::INVALID_REQUEST)?
                    .0
                };
                if !native_job_types.iter().all(|name| is_valid_job_type(name)) {
                    return Err(failures::INVALID_JOB_TYPE);
                }
                native_job_types.sort();
                native_job_types.dedup();
                let registration = Arc::new(Registration {
                    callback,
                    context: context as usize,
                    native_job_types,
                    active: Mutex::new(true),
                    network_available: AtomicBool::new(true),
                });
                lock(&HOSTS).insert(handle, registration)
            }
            None => {
                let mut hosts = lock(&HOSTS);
                let still_current = context.is_null()
                    || hosts
                        .get(&handle)
                        .is_some_and(|current| current.context == context as usize);
                if still_current {
                    hosts.remove(&handle)
                } else {
                    None
                }
            }
        };
        if let Some(previous) = previous {
            if let Some(token) = lock(&DRAINS).get(&handle) {
                token.cancel();
            }
            previous.deactivate();
        }
        Ok(())
    })
}

/// Tells the core whether the host can reach the network. While it cannot, a job that would call
/// a provider is put back for a while without spending retry budget and without ending the drain,
/// so on-device and local jobs keep running; a drain that starts with a network releases those
/// jobs at once. A registration starts out reachable. Refused with `host_not_registered` when no
/// host is registered; the flag belongs to the registration, so re-register, then set it again.
#[no_mangle]
pub extern "C" fn ohand_core_set_job_network_reachable(
    handle: OhandCoreHandle,
    reachable: u32,
) -> OhandCoreResult {
    guarded(|| {
        let host = lock(&HOSTS)
            .get(&handle)
            .cloned()
            .ok_or(failures::HOST_NOT_REGISTERED)?;
        host.network_available
            .store(reachable != 0, Ordering::SeqCst);
        Ok(())
    })
}

/// Starts one drain of the ready jobs for `handle` on its own thread and returns at once; the
/// outcome arrives as an event for `operation_id` (see the module documentation).
/// `store_path` must be the path the handle was opened with: the drain opens its own connection to
/// it, and the core does not verify the caller's claim.
/// Refused with `host_not_registered` when no host is registered, `drain_in_progress` when a drain
/// of this handle is already running, and `store_not_shareable` for an in-memory store.
///
/// # Safety
/// `store_path` must point at `store_path_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_job_drain(
    handle: OhandCoreHandle,
    operation_id: u64,
    store_path: *const u8,
    store_path_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        instance::lookup(handle)?;
        if store_path_len > OHAND_CORE_MAX_JOB_RUNNER_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if store_path_len == 0 {
            return Err(AbiFailure::INVALID_PATH);
        }
        if store_path.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let path = std::str::from_utf8(std::slice::from_raw_parts(store_path, store_path_len))
            .map_err(|_| AbiFailure::INVALID_UTF8)?
            .to_string();
        if path == ":memory:" || path.starts_with("file::memory:") {
            return Err(failures::STORE_NOT_SHAREABLE);
        }
        let host = lock(&HOSTS)
            .get(&handle)
            .cloned()
            .ok_or(failures::HOST_NOT_REGISTERED)?;

        let cancel = CancelToken::new();
        {
            let mut drains = lock(&DRAINS);
            if drains.contains_key(&handle) {
                return Err(failures::DRAIN_IN_PROGRESS);
            }
            drains.insert(handle, cancel.clone());
        }
        let slot = DrainSlot { handle };
        std::thread::Builder::new()
            .name("ohand-job-drain".to_string())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    run_drain(handle, operation_id, &path, host, &cancel)
                }))
                .unwrap_or(Err(AbiFailure::INTERNAL));
                drop(slot);
                deliver_final(handle, operation_id, outcome);
            })
            .map(|_| ())
            .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Asks the running drain of `handle` to stop at a checkpoint: a provider call in flight is
/// abandoned and its job re-queued at once without spending retry budget. Idempotent; a handle
/// with no drain running is not an error.
#[no_mangle]
pub extern "C" fn ohand_core_cancel_job_drain(handle: OhandCoreHandle) -> OhandCoreResult {
    guarded(|| {
        if let Some(token) = lock(&DRAINS).get(&handle) {
            token.cancel();
        }
        Ok(())
    })
}

fn answer_exchange(handle: u64, request_id: u64, answer: ExchangeAnswer) -> Result<(), AbiFailure> {
    let sender = lock(&EXCHANGES)
        .get(&(handle, request_id))
        .cloned()
        .ok_or(AbiFailure::NOT_FOUND)?;
    sender.send(answer).map_err(|_| AbiFailure::NOT_FOUND)
}

/// Answers the send for request `request_id` with a completed HTTP exchange of any status.
/// `headers` is a JSON array of `[name, value]` pairs (null with length 0 for none) and `body`
/// the response bytes (null with length 0 for none). An answer for a request that has ended is
/// `not_found`.
///
/// # Safety
/// `headers` and `body` must each point at as many readable bytes as their length says (a null
/// pointer is allowed only with length 0).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_complete_job_exchange(
    handle: OhandCoreHandle,
    request_id: u64,
    http_status: u32,
    headers: *const u8,
    headers_len: usize,
    body: *const u8,
    body_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        if headers_len > OHAND_CORE_MAX_JOB_EXCHANGE_RESPONSE_BYTES
            || body_len > OHAND_CORE_MAX_JOB_EXCHANGE_RESPONSE_BYTES
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
        answer_exchange(
            handle,
            request_id,
            ExchangeAnswer::Response(HttpResponse {
                status,
                headers: header_pairs,
                body,
            }),
        )
    })
}

/// Answers the send for request `request_id` with a native failure, named by the snake_case core
/// transport error (`timeout`, `cancelled`, `unavailable`, `rate_limited`, `unauthorized`,
/// `rejected`, `invalid_output`). An unknown name is `invalid_request`.
///
/// # Safety
/// `error` must point at `error_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_fail_job_exchange(
    handle: OhandCoreHandle,
    request_id: u64,
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
        answer_exchange(handle, request_id, ExchangeAnswer::Failure(error))
    })
}

/// Answers the capability run `request_id` with `outcome` (one of the
/// `OHAND_JOB_CAPABILITY_*` values). `reason` is a short lowercase label (`[a-z0-9_]`, at most 64
/// bytes) stored on the job for a transient or permanent failure; it must never contain captured
/// content, and anything else is replaced by `native_failure`. An answer for a run that has ended
/// is `not_found`; an `outcome` that is not one of the values is `invalid_request`.
///
/// # Safety
/// `reason` must point at `reason_len` readable bytes (null only with length 0).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_finish_job_capability(
    handle: OhandCoreHandle,
    request_id: u64,
    outcome: u32,
    reason: *const u8,
    reason_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        if outcome > OHAND_JOB_CAPABILITY_INTERRUPTED {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if reason_len > OHAND_CORE_MAX_JOB_RUNNER_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if reason.is_null() && reason_len > 0 {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let reason = if reason_len == 0 {
            FALLBACK_REASON.to_string()
        } else {
            sanitized_reason(
                std::str::from_utf8(std::slice::from_raw_parts(reason, reason_len))
                    .unwrap_or_default(),
            )
        };
        let sender = lock(&CAPABILITIES)
            .get(&(handle, request_id))
            .cloned()
            .ok_or(AbiFailure::NOT_FOUND)?;
        sender
            .send(CapabilityAnswer { outcome, reason })
            .map_err(|_| AbiFailure::NOT_FOUND)
    })
}

#[cfg(test)]
mod tests;
