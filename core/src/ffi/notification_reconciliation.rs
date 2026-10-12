//! Reminder reconciliation over the core handle (N04b).
//!
//! The core's capacity pass (`reminders::capacity::reconcile_with_capacity`) and the explicit
//! re-arm of OS-rejected schedules (`reminders::reconcile::retry_rejected_schedule`) run on the
//! handle's worker thread. The native layer supplies only what the core cannot know: whether the
//! notification permission now lets the OS alert, and the OS itself as the injected
//! `NotificationEffects`.
//!
//! * `ohand_core_set_notification_effects` registers the native effect callback of a handle (null
//!   clears it; a null callback with a non-null context clears it only if that context is still
//!   the registered one). The callback is invoked on the worker thread with one command at a
//!   time and must return promptly: start the OS call and return. Native answers each command
//!   exactly once with `ohand_core_complete_notification_effect`. A command that is not answered
//!   within `EFFECT_DEADLINE` is abandoned (`OHAND_NOTIFICATION_COMMAND_ABANDON`) and counts as a
//!   transient failure, which the next pass retries; a late answer is `not_found` and ignored.
//! * `ohand_core_start_reconcile_reminders` queues one pass. The request is
//!   `{"rearm_rejected_schedules","allow_optional_prompts","prompts":[{"notification_id",
//!   "fire_at"}],"facts_only"}`. `rearm_rejected_schedules` re-opens the current generation's
//!   `rejected` schedules before the pass, which a regained permission (or first observation)
//!   asks for; re-arming an already re-armed schedule changes nothing, so a repeated request
//!   cannot duplicate it. `allow_optional_prompts` false withholds every prompt candidate from the
//!   pass; explicit reminders never depend on it. `facts_only` reads the reminders and runs no
//!   pass, so status stays readable while the OS or the pass is unavailable.
//! * The outcome event is `{"operation_id","rearmed","converged","installed","already_installed",
//!   "removed","withheld","failures","demoted","refilled","expired","prompts_granted",
//!   "prompts_deferred","prompts_withheld","reminders":[{"item_id","request_state",
//!   "schedule_state","delivery_state","acknowledgment_state","unschedulable_reason"}]}` or a
//!   normalized failure. Counts and machine labels only: no text, no native identifier.
//!   `reminders` are the stored facts of active items after the pass. Delivery and acknowledgment
//!   are read, never written: a revoked permission, Focus or elapsed time records neither.
//! * A listing answer may add `"unattributable": n`, the number of pending requests whose
//!   identifiers native cannot carry (empty, or longer than `MAX_IDENTIFIER_BYTES`). Each one
//!   still occupies an OS slot, so the core lists it under a placeholder identifier longer than
//!   any identifier a prompt candidate or a reminder can have: no placeholder can alias an
//!   offered prompt or a known reminder, so each is charged against the budget as foreign and
//!   never cancelled.
//! * The pass holds the worker thread while it waits for native answers, so the native layer
//!   answers promptly; each command is bounded by `EFFECT_DEADLINE`.

use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::providers::contracts::ErrorClass;
use crate::reminders::capacity::{reconcile_with_capacity, CapacityPolicy, PromptDemand};
use crate::reminders::reconcile::{
    retry_rejected_schedule, NotificationEffectError, NotificationEffects, PendingRequest,
    ReconcileError, ScheduleRequest,
};
use crate::store::schema::{Database, SystemClock};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Native is asked to schedule the request described by the JSON `data`.
pub const OHAND_NOTIFICATION_COMMAND_SCHEDULE: u32 = 1;
/// Native is asked to cancel the pending request named by the JSON `data`.
pub const OHAND_NOTIFICATION_COMMAND_CANCEL: u32 = 2;
/// Native is asked to list its pending requests; `data` is null.
pub const OHAND_NOTIFICATION_COMMAND_LIST_PENDING: u32 = 3;
/// Native should stop working on the command named by the effect ID; `data` is null.
pub const OHAND_NOTIFICATION_COMMAND_ABANDON: u32 = 4;
/// Longest reconcile request accepted, in bytes.
pub const OHAND_CORE_MAX_RECONCILE_REQUEST_BYTES: usize = 65_536;
/// Longest native answer accepted, in bytes.
pub const OHAND_CORE_MAX_NOTIFICATION_ANSWER_BYTES: usize = 262_144;

/// Called on the worker thread with `command` for the effect `effect_id` of this handle. `data`
/// is borrowed for the duration of the call; the callback copies what it needs.
pub type OhandNotificationEffectCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        effect_id: u64,
        command: u32,
        data: *const u8,
        len: usize,
    ),
>;

type EffectCallbackFn = unsafe extern "C" fn(*mut c_void, u64, u32, *const u8, usize);

/// How long one native command may stay unanswered.
const EFFECT_DEADLINE: Duration = Duration::from_secs(60);
const WAIT_SLICE: Duration = Duration::from_millis(25);
const MAX_PROMPT_CANDIDATES: usize = 256;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_CODE_BYTES: usize = 64;
/// Most unattributable requests carried per listing. A count above the OS pending limit
/// exhausts the budget the same way, so a larger count is charged as this many.
const MAX_UNATTRIBUTABLE_PENDING: usize = 1_024;

struct Registration {
    callback: EffectCallbackFn,
    context: usize,
    active: Mutex<bool>,
}

impl Registration {
    /// Invokes the callback unless the registration was cleared. Holding `active` across the
    /// call is what lets a clear wait for a running invocation.
    fn invoke(&self, effect_id: u64, command: u32, data: &[u8]) -> bool {
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
                effect_id,
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

enum Answer {
    Done,
    Failed(NotificationEffectError),
    Pending(Vec<PendingRequest>),
}

static REGISTRATIONS: Mutex<BTreeMap<u64, Arc<Registration>>> = Mutex::new(BTreeMap::new());
static WAITING: Mutex<BTreeMap<(u64, u64), Sender<Answer>>> = Mutex::new(BTreeMap::new());
static NEXT_EFFECT_ID: AtomicU64 = AtomicU64::new(1);

/// Removes the waiting slot when the command ends, however it ends.
struct SlotGuard {
    key: (u64, u64),
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        lock(&WAITING).remove(&self.key);
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

    pub(super) const EFFECTS_NOT_REGISTERED: AbiFailure = fixed(
        ErrorClass::Unsupported,
        "notification_effects_not_registered",
        "no native notification effects are registered for the handle",
    );
    pub(super) const LIST_PENDING_FAILED: AbiFailure = fixed(
        ErrorClass::Transient,
        "pending_list_unavailable",
        "the pending notification requests could not be listed; no pass ran",
    );
    pub(super) const RECONCILE_FAILED: AbiFailure = fixed(
        ErrorClass::Permanent,
        "reconcile_failed",
        "the reminder reconciliation could not complete",
    );
}

fn storage_failure(error: impl Into<anyhow::Error>) -> AbiFailure {
    AbiFailure::from_store(&error.into(), AbiFailure::STORAGE_ERROR)
}

fn reconcile_failure(error: ReconcileError) -> AbiFailure {
    match error {
        ReconcileError::ListPending(cause) => {
            let mut failure = failures::LIST_PENDING_FAILED;
            failure.class = cause.class;
            failure
        }
        ReconcileError::Storage(cause) => storage_failure(cause),
        ReconcileError::Database(_) | ReconcileError::State(_) => AbiFailure::STORAGE_ERROR,
        ReconcileError::UnknownReminder(_) | ReconcileError::UnknownEffect { .. } => {
            failures::RECONCILE_FAILED
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptCandidate {
    notification_id: String,
    fire_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconcileRequest {
    #[serde(default)]
    rearm_rejected_schedules: bool,
    #[serde(default)]
    allow_optional_prompts: bool,
    #[serde(default)]
    prompts: Vec<PromptCandidate>,
    #[serde(default)]
    facts_only: bool,
}

struct ValidatedRequest {
    rearm_rejected_schedules: bool,
    allow_optional_prompts: bool,
    prompts: Vec<PromptDemand>,
    facts_only: bool,
}

fn validate(request: ReconcileRequest) -> Result<ValidatedRequest, AbiFailure> {
    if request.prompts.len() > MAX_PROMPT_CANDIDATES {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    let mut prompts = Vec::with_capacity(request.prompts.len());
    for candidate in request.prompts {
        if candidate.notification_id.is_empty()
            || candidate.notification_id.len() > MAX_IDENTIFIER_BYTES
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let fire_at = DateTime::parse_from_rfc3339(&candidate.fire_at)
            .map_err(|_| AbiFailure::INVALID_REQUEST)?
            .with_timezone(&Utc);
        prompts.push(PromptDemand {
            notification_id: candidate.notification_id,
            fire_at,
        });
    }
    Ok(ValidatedRequest {
        rearm_rejected_schedules: request.rearm_rejected_schedules,
        allow_optional_prompts: request.allow_optional_prompts,
        prompts,
        facts_only: request.facts_only,
    })
}

#[derive(Serialize)]
struct ScheduleCommand<'a> {
    effect_id: u64,
    notification_id: &'a str,
    fire_at: String,
    payload_kind: &'static str,
    opaque_target_id: &'a str,
}

#[derive(Serialize)]
struct CancelCommand<'a> {
    effect_id: u64,
    notification_id: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorAnswer {
    class: ErrorClass,
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingAnswer {
    notification_id: String,
    #[serde(default)]
    fire_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerJson {
    #[serde(default)]
    error: Option<ErrorAnswer>,
    #[serde(default)]
    pending: Option<Vec<PendingAnswer>>,
    #[serde(default)]
    unattributable: Option<usize>,
}

/// The placeholder identifier of the `index`th unattributable pending request. It is longer than
/// `MAX_IDENTIFIER_BYTES` and has no `#`, so it is distinct from every prompt candidate (bounded
/// by `validate`), every reminder identifier and every other placeholder.
fn unattributable_identifier(index: usize) -> String {
    format!(
        "{:.<width$}{index}",
        "unattributable",
        width = MAX_IDENTIFIER_BYTES + 1
    )
}

fn sanitized_code(code: &str) -> String {
    let allowed = !code.is_empty()
        && code.len() <= MAX_CODE_BYTES
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if allowed {
        code.to_string()
    } else {
        "native_failure".to_string()
    }
}

fn parse_answer(answer: AnswerJson) -> Result<Answer, AbiFailure> {
    if let Some(error) = answer.error {
        return Ok(Answer::Failed(NotificationEffectError::new(
            error.class,
            sanitized_code(&error.code),
        )));
    }
    let Some(pending) = answer.pending else {
        if answer.unattributable.is_some() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        return Ok(Answer::Done);
    };
    let mut requests = Vec::with_capacity(pending.len());
    for entry in pending {
        if entry.notification_id.is_empty() || entry.notification_id.len() > MAX_IDENTIFIER_BYTES {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let fire_at = match entry.fire_at {
            Some(text) => Some(
                DateTime::parse_from_rfc3339(&text)
                    .map_err(|_| AbiFailure::INVALID_REQUEST)?
                    .with_timezone(&Utc),
            ),
            None => None,
        };
        requests.push(PendingRequest {
            notification_id: entry.notification_id,
            fire_at,
        });
    }
    let unattributable = answer
        .unattributable
        .unwrap_or(0)
        .min(MAX_UNATTRIBUTABLE_PENDING);
    requests.extend((0..unattributable).map(|index| PendingRequest {
        notification_id: unattributable_identifier(index),
        fire_at: None,
    }));
    Ok(Answer::Pending(requests))
}

/// The core's notification effects, backed by the registered native callback.
struct NativeEffects {
    handle: u64,
    registration: Arc<Registration>,
    deadline: Duration,
}

impl NativeEffects {
    fn command(&self, command: u32, payload: &dyn Fn(u64) -> Vec<u8>) -> AnswerResult {
        let effect_id = NEXT_EFFECT_ID.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        let key = (self.handle, effect_id);
        lock(&WAITING).insert(key, sender);
        let _slot = SlotGuard { key };
        let data = payload(effect_id);
        if !self.registration.invoke(effect_id, command, &data) {
            return Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "effects_unavailable",
            ));
        }
        let deadline = Instant::now() + self.deadline;
        loop {
            match receiver.recv_timeout(WAIT_SLICE) {
                Ok(Answer::Failed(error)) => return Err(error),
                Ok(answer) => return Ok(answer),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(NotificationEffectError::new(
                        ErrorClass::Transient,
                        "effects_unavailable",
                    ))
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            let abandoned = if instance::lookup(self.handle).is_err() {
                Some("effects_cancelled")
            } else if Instant::now() >= deadline {
                Some("effect_timed_out")
            } else {
                None
            };
            if let Some(code) = abandoned {
                self.registration
                    .invoke(effect_id, OHAND_NOTIFICATION_COMMAND_ABANDON, &[]);
                let class = if code == "effects_cancelled" {
                    ErrorClass::Cancelled
                } else {
                    ErrorClass::Transient
                };
                return Err(NotificationEffectError::new(class, code));
            }
        }
    }
}

type AnswerResult = Result<Answer, NotificationEffectError>;

impl NotificationEffects for NativeEffects {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        let fire_at = request.fire_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        self.command(OHAND_NOTIFICATION_COMMAND_SCHEDULE, &|effect_id| {
            serde_json::to_vec(&ScheduleCommand {
                effect_id,
                notification_id: &request.notification_id,
                fire_at: fire_at.clone(),
                payload_kind: request.payload_kind.as_str(),
                opaque_target_id: &request.opaque_target_id,
            })
            .unwrap_or_default()
        })
        .map(|_| ())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.command(OHAND_NOTIFICATION_COMMAND_CANCEL, &|effect_id| {
            serde_json::to_vec(&CancelCommand {
                effect_id,
                notification_id,
            })
            .unwrap_or_default()
        })
        .map(|_| ())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        match self.command(OHAND_NOTIFICATION_COMMAND_LIST_PENDING, &|_| Vec::new())? {
            Answer::Pending(requests) => Ok(requests),
            Answer::Done | Answer::Failed(_) => Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "malformed_native_answer",
            )),
        }
    }
}

#[derive(Serialize)]
struct ReminderFacts {
    item_id: String,
    request_state: String,
    schedule_state: Option<String>,
    delivery_state: Option<String>,
    acknowledgment_state: Option<String>,
    unschedulable_reason: Option<String>,
}

#[derive(Serialize, Default)]
struct PassSummary {
    rearmed: usize,
    converged: bool,
    installed: usize,
    already_installed: usize,
    removed: usize,
    withheld: usize,
    failures: usize,
    demoted: usize,
    refilled: usize,
    expired: usize,
    prompts_granted: usize,
    prompts_deferred: usize,
    prompts_withheld: usize,
}

#[derive(Serialize)]
struct Outcome {
    operation_id: u64,
    #[serde(flatten)]
    summary: PassSummary,
    reminders: Vec<ReminderFacts>,
}

fn read_facts(database: &Database) -> Result<Vec<ReminderFacts>, AbiFailure> {
    let mut statement = database
        .conn()
        .prepare(
            "SELECT reminders.item_id, reminders.request_state, reminders.schedule_state,
                    reminders.delivery_state, reminders.acknowledgment_state,
                    reminders.unschedulable_reason
             FROM reminders JOIN items ON items.item_id = reminders.item_id
             WHERE items.lifecycle_state = 'active'
             ORDER BY reminders.created_at, reminders.reminder_id",
        )
        .map_err(storage_failure)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ReminderFacts {
                item_id: row.get(0)?,
                request_state: row.get(1)?,
                schedule_state: row.get(2)?,
                delivery_state: row.get(3)?,
                acknowledgment_state: row.get(4)?,
                unschedulable_reason: row.get(5)?,
            })
        })
        .map_err(storage_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage_failure)?;
    Ok(rows)
}

/// Re-opens every current-generation `rejected` schedule of an active item. Returns how many
/// were re-armed; a schedule that is not rejected is left untouched, so repeating it re-arms
/// nothing.
fn rearm_rejected(database: &mut Database) -> Result<usize, AbiFailure> {
    let item_ids: Vec<String> = {
        let mut statement = database
            .conn()
            .prepare(
                "SELECT reminders.item_id FROM reminders
                 JOIN items ON items.item_id = reminders.item_id
                 WHERE reminders.request_state = 'resolved'
                   AND items.lifecycle_state = 'active'
                 ORDER BY reminders.created_at, reminders.reminder_id",
            )
            .map_err(storage_failure)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_failure)?;
        rows
    };
    if item_ids.is_empty() {
        return Ok(0);
    }
    let clock = SystemClock;
    let transaction = database.immediate_transaction().map_err(storage_failure)?;
    let mut rearmed = 0;
    for item_id in &item_ids {
        if retry_rejected_schedule(&transaction, &clock, item_id).map_err(reconcile_failure)? {
            rearmed += 1;
        }
    }
    transaction.commit().map_err(storage_failure)?;
    Ok(rearmed)
}

fn reconcile_job(
    handle: u64,
    operation_id: u64,
    request: ValidatedRequest,
    registration: Option<Arc<Registration>>,
    deadline: Duration,
) -> JobFn {
    Box::new(move |database| {
        let mut database = lock(database);
        let mut summary = PassSummary::default();
        if !request.facts_only {
            let registration = registration.ok_or(failures::EFFECTS_NOT_REGISTERED)?;
            if request.rearm_rejected_schedules {
                summary.rearmed = rearm_rejected(&mut database)?;
            }
            let offered: &[PromptDemand] = if request.allow_optional_prompts {
                &request.prompts
            } else {
                summary.prompts_withheld = request.prompts.len();
                &[]
            };
            let mut effects = NativeEffects {
                handle,
                registration,
                deadline,
            };
            let report = reconcile_with_capacity(
                &mut database,
                &SystemClock,
                &mut effects,
                &CapacityPolicy::default(),
                offered,
            )
            .map_err(reconcile_failure)?;
            summary.converged = report.converged();
            summary.installed = report.reconcile.installed.len();
            summary.already_installed = report.reconcile.already_installed.len();
            summary.removed = report.reconcile.removed.len();
            summary.withheld = report.reconcile.withheld.len();
            summary.failures =
                report.reconcile.failures.len() + report.prompt_cancel_failures.len();
            summary.demoted = report.demoted.len();
            summary.refilled = report.refilled.len();
            summary.expired = report.expired.len() + report.reconcile.expired.len();
            summary.prompts_granted = report.allocation.granted_prompts.len();
            summary.prompts_deferred = report.allocation.deferred_prompts.len();
        }
        let reminders = read_facts(&database)?;
        serde_json::to_vec(&Outcome {
            operation_id,
            summary,
            reminders,
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Registers `callback` with `context` as the native notification effects of `handle`, replacing
/// any previous registration; a null `callback` clears it. A null `callback` with a non-null
/// `context` clears only if that context is still the registered one and otherwise does nothing,
/// so a superseded registrant can release itself without removing its replacement. A running
/// invocation is waited for, so the previous context is unused once this returns. Clearing is
/// allowed for a handle that has been closed.
#[no_mangle]
pub extern "C" fn ohand_core_set_notification_effects(
    handle: OhandCoreHandle,
    callback: OhandNotificationEffectCallback,
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
                lock(&REGISTRATIONS).insert(handle, registration)
            }
            None => {
                let mut registrations = lock(&REGISTRATIONS);
                let still_current = context.is_null()
                    || registrations
                        .get(&handle)
                        .is_some_and(|current| current.context == context as usize);
                if still_current {
                    registrations.remove(&handle)
                } else {
                    None
                }
            }
        };
        if let Some(previous) = previous {
            previous.deactivate();
        }
        Ok(())
    })
}

/// Queues one reconciliation pass described by the JSON at `request` (see the module
/// documentation). The outcome event for `operation_id` is the summary and the reminders' stored
/// facts, or a normalized failure. A pass needs registered effects unless `facts_only`.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_reconcile_reminders(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        if request_len > OHAND_CORE_MAX_RECONCILE_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if request_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if request.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let parsed: ReconcileRequest =
            serde_json::from_slice(std::slice::from_raw_parts(request, request_len))
                .map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let validated = validate(parsed)?;
        let registration = lock(&REGISTRATIONS).get(&handle).cloned();
        if registration.is_none() && !validated.facts_only {
            return Err(failures::EFFECTS_NOT_REGISTERED);
        }
        core.submit(
            operation_id,
            reconcile_job(
                handle,
                operation_id,
                validated,
                registration,
                EFFECT_DEADLINE,
            ),
        )
    })
}

/// Answers the native command `effect_id` of `handle` with the JSON at `answer`: `{}` for a
/// completed schedule or cancel, `{"error":{"class","code"}}` for a failure (`class` is one of
/// the normalized classes, `code` lowercase letters, digits and underscores), or
/// `{"pending":[{"notification_id","fire_at"}],"unattributable":n}` for a listing (`n` optional,
/// see the module documentation). An answer for a command that has
/// ended is `not_found`.
///
/// # Safety
/// `answer` must point at `answer_len` readable bytes (it may be null only when `answer_len` is
/// 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_complete_notification_effect(
    handle: OhandCoreHandle,
    effect_id: u64,
    answer: *const u8,
    answer_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        if answer_len > OHAND_CORE_MAX_NOTIFICATION_ANSWER_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if answer_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if answer.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let parsed: AnswerJson =
            serde_json::from_slice(std::slice::from_raw_parts(answer, answer_len))
                .map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let parsed = parse_answer(parsed)?;
        let sender = lock(&WAITING)
            .get(&(handle, effect_id))
            .cloned()
            .ok_or(AbiFailure::NOT_FOUND)?;
        sender.send(parsed).map_err(|_| AbiFailure::NOT_FOUND)
    })
}

#[cfg(test)]
mod tests;
