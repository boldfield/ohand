// Reminder desired state and durable schedule/cancel operations (N01).
//
// Desired state lives in the `reminders` table (one row per item) and the native effects core
// wants are recorded in `reminder_operations`, both written inside the caller's transaction so
// a crash can never leave state without its operation or the reverse. Delivery and
// acknowledgment facts are separate columns that only OS evidence or an explicit user action
// changes; the passage of a due time never touches them.
//
// Authority rules enforced here:
// - A time derived from text is never accepted as an instant. Callers pass the source phrase;
//   core checks that the phrase occurs in the item's own text and resolves it with the
//   deterministic resolver against the stored capture context. A phrase the item does not
//   contain is a fabricated deadline and is rejected without mutation.
// - Derived output can create a reminder or refine one that has no committed time. It can
//   never replace a committed time (user-set or earlier derived); only an explicit user time
//   correction changes that.
// - Every write carries the expected item revision (compare-and-set). Reminder changes do not
//   bump the item revision; they bump the reminder's own `state_version` instead.
// - User reminder commands (time correction, cancel) carry an immutable command ID and the
//   reminder `state_version` they were issued against. Each applied command is recorded in
//   `reminder_commands` with its original result in the same transaction, so a delayed retry
//   returns that result without touching newer state, and a distinct command issued against an
//   older version is rejected as stale.
// - Recurring requests are stored as `unsupported_recurrence` and are never reduced to one shot:
//   a derived phrase is refused (stored as recurrence) when the item's text carries a repeat
//   marker anywhere, and derived output can never leave `unsupported_recurrence`; only an
//   explicit user time correction does.
// - Completing or cancelling an item cancels its reminder in the same transaction as the
//   lifecycle event. `reconcile_inactive_items` repairs any reminder left behind by an item that
//   became inactive through another path, and `desired_notifications` never includes the
//   reminder of an inactive item.
//
// Native identity: `reminder_id#schedule_generation`, used verbatim as the OS request
// identifier. The generation increases every time the reminder's time changes, so a retry
// replaces and a reschedule never reuses an old identifier. The schema's uniqueness key is
// (reminder_id, effect_identity); the effect identity names the operation type as well as the
// native identifier, so create and cancel of one generation are distinct, replayable records.

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

use crate::domain::items::{load_item_state, ItemState, LifecycleState};
use crate::store::events::{save_event_in_tx, Event, EventError, EventType};
use crate::store::schema::Clock;
use crate::time::{AmbiguityKind, ResolutionError, TimeContext, TimeResolver};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnschedulableReason {
    TimeInPast,
    PermissionDenied,
    CapacityExceeded,
}

impl UnschedulableReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            UnschedulableReason::TimeInPast => "time_in_past",
            UnschedulableReason::PermissionDenied => "permission_denied",
            UnschedulableReason::CapacityExceeded => "capacity_exceeded",
        }
    }
}

impl FromStr for UnschedulableReason {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "time_in_past" => Ok(UnschedulableReason::TimeInPast),
            "permission_denied" => Ok(UnschedulableReason::PermissionDenied),
            "capacity_exceeded" => Ok(UnschedulableReason::CapacityExceeded),
            other => Err(corrupt("unschedulable reason", other)),
        }
    }
}

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestState {
    NotRequested,
    Resolved,
    NotScheduledYet,
    UnsupportedRecurrence,
    Unschedulable(UnschedulableReason),
    Cancelled,
}

impl RequestState {
    fn column_value(&self) -> &'static str {
        match self {
            RequestState::NotRequested => "not_requested",
            RequestState::Resolved => "resolved",
            RequestState::NotScheduledYet => "not_scheduled_yet",
            RequestState::UnsupportedRecurrence => "unsupported_recurrence",
            RequestState::Unschedulable(_) => "unschedulable",
            RequestState::Cancelled => "cancelled",
        }
    }

    fn decode(state: &str, reason: Option<&str>) -> Result<Self, ReminderStateError> {
        match state {
            "not_requested" => Ok(RequestState::NotRequested),
            "resolved" => Ok(RequestState::Resolved),
            "not_scheduled_yet" => Ok(RequestState::NotScheduledYet),
            "unsupported_recurrence" => Ok(RequestState::UnsupportedRecurrence),
            "cancelled" => Ok(RequestState::Cancelled),
            "unschedulable" => {
                let reason = reason.ok_or_else(|| corrupt("unschedulable reason", "<missing>"))?;
                Ok(RequestState::Unschedulable(reason.parse()?))
            }
            other => Err(corrupt("request state", other)),
        }
    }
}

/// Native installation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleState {
    NotScheduled,
    PendingSchedule,
    Scheduled,
    ScheduleFailed,
}

impl ScheduleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScheduleState::NotScheduled => "not_scheduled",
            ScheduleState::PendingSchedule => "pending_schedule",
            ScheduleState::Scheduled => "scheduled",
            ScheduleState::ScheduleFailed => "schedule_failed",
        }
    }
}

impl FromStr for ScheduleState {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "not_scheduled" => Ok(ScheduleState::NotScheduled),
            "pending_schedule" => Ok(ScheduleState::PendingSchedule),
            "scheduled" => Ok(ScheduleState::Scheduled),
            "schedule_failed" => Ok(ScheduleState::ScheduleFailed),
            other => Err(corrupt("schedule state", other)),
        }
    }
}

/// OS evidence only; the passage of the due time never changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryState {
    Unknown,
    Delivered,
    Opened,
}

impl FromStr for DeliveryState {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "unknown" => Ok(DeliveryState::Unknown),
            "delivered" => Ok(DeliveryState::Delivered),
            "opened" => Ok(DeliveryState::Opened),
            other => Err(corrupt("delivery state", other)),
        }
    }
}

/// Explicit user acknowledgment, independent of delivery and of item completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcknowledgmentState {
    NotAcknowledged,
    Acknowledged,
}

impl FromStr for AcknowledgmentState {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "not_acknowledged" => Ok(AcknowledgmentState::NotAcknowledged),
            "acknowledged" => Ok(AcknowledgmentState::Acknowledged),
            other => Err(corrupt("acknowledgment state", other)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationType {
    Schedule,
    Cancel,
}

impl OperationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationType::Schedule => "schedule",
            OperationType::Cancel => "cancel",
        }
    }
}

impl FromStr for OperationType {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "schedule" => Ok(OperationType::Schedule),
            "cancel" => Ok(OperationType::Cancel),
            other => Err(corrupt("operation type", other)),
        }
    }
}

/// `Pending` until a native acknowledgment is recorded (N03); `Superseded` once a newer
/// desired state made the operation obsolete before it was acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Acknowledged,
    Failed,
    Superseded,
}

impl OperationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationState::Pending => "pending",
            OperationState::Acknowledged => "acknowledged",
            OperationState::Failed => "failed",
            OperationState::Superseded => "superseded",
        }
    }
}

impl FromStr for OperationState {
    type Err = ReminderStateError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "pending" => Ok(OperationState::Pending),
            "acknowledged" => Ok(OperationState::Acknowledged),
            "failed" => Ok(OperationState::Failed),
            "superseded" => Ok(OperationState::Superseded),
            other => Err(corrupt("operation state", other)),
        }
    }
}

/// Durable desired state of one item's reminder, plus delivery and acknowledgment facts that
/// are kept apart from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderRecord {
    pub reminder_id: String,
    pub item_id: String,
    pub request_state: RequestState,
    pub schedule_state: ScheduleState,
    pub delivery_state: DeliveryState,
    pub acknowledgment_state: AcknowledgmentState,
    /// The resolved instant. Kept for `Unschedulable(TimeInPast)` so the expired opportunity
    /// stays inspectable.
    pub resolved_instant: Option<DateTime<Utc>>,
    /// IANA zone for display.
    pub timezone_id: Option<String>,
    /// Why the time is incomplete or ambiguous (`NotScheduledYet`).
    pub ambiguity_reason: Option<String>,
    /// Why recurrence is unsupported (`UnsupportedRecurrence`). Never holds an unschedulable
    /// reason; that lives in `RequestState::Unschedulable`.
    pub unsupported_reason: Option<String>,
    /// The phrase the reminder was requested with, preserved so an ambiguous or unscheduled
    /// request stays inspectable after the item's text is corrected.
    pub source_phrase: Option<String>,
    pub schedule_generation: i64,
    /// Increases on every write to this reminder. User commands name the version they were
    /// issued against; a command against an older version is stale and rejected.
    pub state_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ReminderRecord {
    /// Native notification identifier of the current generation; `None` before the first
    /// schedulable time.
    pub fn notification_id(&self) -> Option<String> {
        (self.schedule_generation > 0)
            .then(|| notification_identifier(&self.reminder_id, self.schedule_generation))
    }

    /// True when a resolved or expired due time lies at or before `now`. Purely informational:
    /// it says nothing about delivery.
    pub fn due_time_passed(&self, now: DateTime<Utc>) -> bool {
        self.resolved_instant.is_some_and(|instant| instant <= now)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderOperation {
    pub operation_id: String,
    pub reminder_id: String,
    pub operation_type: OperationType,
    pub operation_state: OperationState,
    pub effect_identity: String,
    /// The native notification request identifier this operation targets.
    pub notification_id: String,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// One entry of the set the OS should hold pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredNotification {
    pub reminder_id: String,
    pub item_id: String,
    pub notification_id: String,
    pub fire_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum ReminderStateError {
    #[error("item {0} not found")]
    ItemNotFound(String),
    #[error("item {item_id} is {lifecycle_state}, not active")]
    ItemNotActive {
        item_id: String,
        lifecycle_state: LifecycleState,
    },
    #[error("item {0} is not an action and cannot carry a reminder")]
    ItemCannotCarryReminder(String),
    #[error("item {item_id} is still active; its reminder is not cancelled by lifecycle")]
    ItemStillActive { item_id: String },
    #[error("only completion and cancellation events end an item's reminder")]
    NotALifecycleEvent,
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("stale revision on item {item_id}: expected {expected}, current {current}")]
    StaleRevision {
        item_id: String,
        expected: i32,
        current: i32,
    },
    #[error("fabricated deadline rejected: {0}")]
    FabricatedDeadline(String),
    #[error("a committed reminder time can only be changed by an explicit user correction")]
    CommittedTimeProtected,
    #[error("the request is a recurring reminder; only an explicit user time correction can replace it with a one-shot")]
    RecurrenceProtected,
    #[error("the reminder was cancelled; only an explicit user time correction can restore it")]
    ReminderCancelled,
    #[error("unknown timezone {0}")]
    InvalidTimezone(String),
    #[error("time resolution failed: {0}")]
    Resolution(ResolutionError),
    #[error("stale reminder version on item {item_id}: expected {expected}, current {current}")]
    StaleReminderVersion {
        item_id: String,
        expected: i64,
        current: i64,
    },
    #[error("command {command_id} was already applied with different content")]
    CommandIdReused { command_id: String },
    #[error("operation {effect_identity} was already recorded with different content")]
    ConflictingReplay { effect_identity: String },
    #[error("stored reminder data is invalid: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

fn corrupt(field: &str, value: &str) -> ReminderStateError {
    ReminderStateError::Corrupt(format!("{field} '{value}'"))
}

/// A time given explicitly by the user. The only way to change a committed time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserTimeCorrection {
    /// Immutable identity of this user action, reused unchanged by every retry of it.
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    /// `state_version` of the reminder the user saw; 0 when the item had no reminder.
    pub expected_reminder_version: i64,
    pub instant: DateTime<Utc>,
    pub timezone_id: String,
}

/// The user cancelling an item's reminder while keeping the item active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderCancellation {
    /// Immutable identity of this user action, reused unchanged by every retry of it.
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    /// `state_version` of the reminder the user saw; 0 when the item had no reminder.
    pub expected_reminder_version: i64,
}

/// A reminder phrase found in the item's text by the fast path or an interpretation result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedReminderRequest {
    pub item_id: String,
    /// Item revision the phrase was derived from.
    pub source_revision: i32,
    pub phrase: String,
}

pub fn notification_identifier(reminder_id: &str, schedule_generation: i64) -> String {
    format!("{reminder_id}#{schedule_generation}")
}

fn effect_identity(operation_type: OperationType, notification_id: &str) -> String {
    format!("{}:{}", operation_type.as_str(), notification_id)
}

/// Outcome of evaluating a requested time, before it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TimeOutcome {
    Resolved {
        instant: DateTime<Utc>,
        timezone_id: String,
    },
    InPast {
        instant: DateTime<Utc>,
        timezone_id: String,
    },
    NeedsCorrection {
        reason: String,
        timezone_id: String,
    },
    UnsupportedRecurrence {
        reason: String,
        timezone_id: String,
    },
}

impl TimeOutcome {
    fn for_instant(instant: DateTime<Utc>, timezone_id: String, now: DateTime<Utc>) -> Self {
        if instant <= now {
            TimeOutcome::InPast {
                instant,
                timezone_id,
            }
        } else {
            TimeOutcome::Resolved {
                instant,
                timezone_id,
            }
        }
    }

    fn committed_instant(&self) -> Option<DateTime<Utc>> {
        match self {
            TimeOutcome::Resolved { instant, .. } | TimeOutcome::InPast { instant, .. } => {
                Some(*instant)
            }
            _ => None,
        }
    }
}

/// Request a reminder from a phrase in the item's text. Idempotent for the same phrase.
pub fn apply_derived_request(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    request: &DerivedReminderRequest,
) -> Result<ReminderRecord, ReminderStateError> {
    let item = load_reminder_capable_item(tx, &request.item_id)?;
    check_revision(&item, request.source_revision)?;
    let context = load_capture_time_context(tx, &item.capture_id)?;
    require_phrase_in_text(&item, &request.phrase)?;

    let outcome = match text_repeat_reason(&item, &context) {
        Some(reason) => TimeOutcome::UnsupportedRecurrence {
            reason,
            timezone_id: context.timezone.clone(),
        },
        None => resolve_phrase(&request.phrase, &context, clock.now())?,
    };

    let existing = load_reminder_by_item(tx, &request.item_id)?;
    if let Some(record) = &existing {
        match record.request_state {
            RequestState::Cancelled => return Err(ReminderStateError::ReminderCancelled),
            RequestState::UnsupportedRecurrence => {
                return if matches!(outcome, TimeOutcome::UnsupportedRecurrence { .. }) {
                    Ok(record.clone())
                } else {
                    Err(ReminderStateError::RecurrenceProtected)
                };
            }
            RequestState::Resolved | RequestState::Unschedulable(_) => {
                return if outcome.committed_instant() == record.resolved_instant {
                    Ok(record.clone())
                } else {
                    Err(ReminderStateError::CommittedTimeProtected)
                };
            }
            RequestState::NotRequested | RequestState::NotScheduledYet => {}
        }
    }
    commit_outcome(
        tx,
        clock.now(),
        &request.item_id,
        existing,
        outcome,
        Some(request.phrase.trim()),
    )
}

fn resolve_phrase(
    phrase: &str,
    context: &TimeContext,
    now: DateTime<Utc>,
) -> Result<TimeOutcome, ReminderStateError> {
    Ok(match TimeResolver::resolve(phrase, context) {
        Ok(resolution) => match resolution.resolved_time {
            Some(instant) if resolution.ambiguity_kind != Some(AmbiguityKind::Past) => {
                TimeOutcome::for_instant(instant, context.timezone.clone(), now)
            }
            Some(instant) => TimeOutcome::InPast {
                instant,
                timezone_id: context.timezone.clone(),
            },
            None => TimeOutcome::NeedsCorrection {
                reason: resolution
                    .ambiguity_reason
                    .unwrap_or_else(|| "time is ambiguous or incomplete".to_string()),
                timezone_id: context.timezone.clone(),
            },
        },
        Err(ResolutionError::UnsupportedRepeat(reason)) => TimeOutcome::UnsupportedRecurrence {
            reason,
            timezone_id: context.timezone.clone(),
        },
        Err(ResolutionError::InvalidDateFormat(_)) => TimeOutcome::NeedsCorrection {
            reason: "the time phrase could not be understood".to_string(),
            timezone_id: context.timezone.clone(),
        },
        Err(other) => return Err(ReminderStateError::Resolution(other)),
    })
}

/// The resolver's own repeat detection applied to the whole item text (punctuation treated as
/// whitespace), so a one-shot phrase beside "every day" is not mistaken for a one-shot request.
fn text_repeat_reason(item: &ItemState, context: &TimeContext) -> Option<String> {
    let text = item.current_text.text()?;
    let spaced: String = text
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect();
    match TimeResolver::resolve(&spaced, context) {
        Err(ResolutionError::UnsupportedRepeat(reason)) => Some(reason),
        _ => None,
    }
}

/// Set or change the reminder time from an explicit user choice. A new instant starts a new
/// schedule generation; the same instant with a different timezone only updates the display
/// timezone.
///
/// Retries are keyed by `command_id`: replaying an applied command returns its original result
/// and changes nothing, even if later commands have changed the reminder since. A distinct
/// command issued against an older `expected_reminder_version` is stale and rejected.
pub fn apply_user_time_correction(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    correction: &UserTimeCorrection,
) -> Result<ReminderRecord, ReminderStateError> {
    // Persisted instants carry whole seconds, so normalize once here; comparison, storage and
    // the command fingerprint all use the same precision.
    let instant = parse_instant(&format_instant(correction.instant))?;
    let fingerprint = format!(
        "user_time|{}|{}|{}|{}|{}",
        correction.item_id,
        correction.expected_revision,
        correction.expected_reminder_version,
        format_instant(instant),
        correction.timezone_id
    );
    if let Some(original) = replayed_result::<ReminderRecord>(
        tx,
        &correction.command_id,
        &correction.item_id,
        &fingerprint,
    )? {
        return Ok(original);
    }

    let item = load_reminder_capable_item(tx, &correction.item_id)?;
    check_revision(&item, correction.expected_revision)?;
    correction
        .timezone_id
        .parse::<Tz>()
        .map_err(|_| ReminderStateError::InvalidTimezone(correction.timezone_id.clone()))?;
    let existing = load_reminder_by_item(tx, &correction.item_id)?;
    check_reminder_version(
        &correction.item_id,
        existing.as_ref(),
        correction.expected_reminder_version,
    )?;

    let result = apply_user_instant(
        tx,
        clock.now(),
        &correction.item_id,
        existing,
        instant,
        &correction.timezone_id,
    )?;
    record_command(
        tx,
        clock.now(),
        &correction.command_id,
        &correction.item_id,
        &fingerprint,
        &result,
    )?;
    Ok(result)
}

fn apply_user_instant(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    item_id: &str,
    existing: Option<ReminderRecord>,
    instant: DateTime<Utc>,
    timezone_id: &str,
) -> Result<ReminderRecord, ReminderStateError> {
    if let Some(record) = &existing {
        let committed = matches!(
            record.request_state,
            RequestState::Resolved | RequestState::Unschedulable(_)
        );
        if committed && record.resolved_instant == Some(instant) {
            if record.timezone_id.as_deref() == Some(timezone_id) {
                return Ok(record.clone());
            }
            tx.execute(
                "UPDATE reminders SET timezone_id = ?, state_version = state_version + 1,
                        updated_at = ?
                 WHERE reminder_id = ?",
                rusqlite::params![timezone_id, format_instant(now), &record.reminder_id],
            )?;
            return load_reminder_by_item(tx, item_id)?
                .ok_or_else(|| corrupt("reminder", &record.reminder_id));
        }
    }
    let outcome = TimeOutcome::for_instant(instant, timezone_id.to_string(), now);
    commit_outcome(tx, now, item_id, existing, outcome, None)
}

/// Cancel the reminder only; the item stays active. Returns `None` when the item has no
/// reminder. Retries are keyed by `command_id` exactly as for `apply_user_time_correction`, so
/// a delayed cancel can never undo a later restore.
pub fn cancel_reminder(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    cancellation: &ReminderCancellation,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    let fingerprint = format!(
        "cancel|{}|{}|{}",
        cancellation.item_id,
        cancellation.expected_revision,
        cancellation.expected_reminder_version
    );
    if let Some(original) = replayed_result::<Option<ReminderRecord>>(
        tx,
        &cancellation.command_id,
        &cancellation.item_id,
        &fingerprint,
    )? {
        return Ok(original);
    }

    let item = load_item(tx, &cancellation.item_id)?;
    check_revision(&item, cancellation.expected_revision)?;
    require_active(&item)?;
    let existing = load_reminder_by_item(tx, &cancellation.item_id)?;
    check_reminder_version(
        &cancellation.item_id,
        existing.as_ref(),
        cancellation.expected_reminder_version,
    )?;

    let result = cancel_existing(tx, clock.now(), &cancellation.item_id)?;
    record_command(
        tx,
        clock.now(),
        &cancellation.command_id,
        &cancellation.item_id,
        &fingerprint,
        &result,
    )?;
    Ok(result)
}

fn check_reminder_version(
    item_id: &str,
    existing: Option<&ReminderRecord>,
    expected: i64,
) -> Result<(), ReminderStateError> {
    let current = existing.map_or(0, |record| record.state_version);
    if current != expected {
        return Err(ReminderStateError::StaleReminderVersion {
            item_id: item_id.to_string(),
            expected,
            current,
        });
    }
    Ok(())
}

/// The original result of an already-applied command, or `None` when `command_id` is new.
/// Reusing a command ID for a different command is a conflict, never a silent replay.
fn replayed_result<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    command_id: &str,
    item_id: &str,
    fingerprint: &str,
) -> Result<Option<T>, ReminderStateError> {
    let stored: Option<(String, String, String)> = tx
        .query_row(
            "SELECT item_id, command_fingerprint, result_json
             FROM reminder_commands WHERE command_id = ?",
            [command_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_item_id, stored_fingerprint, result_json)) = stored else {
        return Ok(None);
    };
    if stored_item_id != item_id || stored_fingerprint != fingerprint {
        return Err(ReminderStateError::CommandIdReused {
            command_id: command_id.to_string(),
        });
    }
    serde_json::from_str(&result_json)
        .map(Some)
        .map_err(|_| corrupt("command result", command_id))
}

fn record_command<T: Serialize>(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    command_id: &str,
    item_id: &str,
    fingerprint: &str,
    result: &T,
) -> Result<(), ReminderStateError> {
    let result_json =
        serde_json::to_string(result).map_err(|_| corrupt("command result", command_id))?;
    tx.execute(
        "INSERT INTO reminder_commands
            (command_id, item_id, command_fingerprint, result_json, created_at)
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            command_id,
            item_id,
            fingerprint,
            result_json,
            format_instant(now)
        ],
    )?;
    Ok(())
}

/// Cancel the reminder because its item was completed, cancelled or deleted. Idempotent, and
/// safe to replay after a crash. Fails if the item is still active.
pub fn cancel_for_inactive_item(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    item_id: &str,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    let item = load_item(tx, item_id)?;
    if item.lifecycle_state == LifecycleState::Active {
        return Err(ReminderStateError::ItemStillActive {
            item_id: item_id.to_string(),
        });
    }
    cancel_existing(tx, clock.now(), item_id)
}

/// Record a completion or cancellation event for an item and cancel its reminder in the same
/// transaction, so the item can never be inactive with a live reminder after a crash.
/// Idempotent: replaying the identical event cancels nothing twice.
pub fn end_item_with_event(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    event: &Event,
    expected_revision: i32,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    if !matches!(
        event.event_type,
        EventType::Completion | EventType::Cancellation
    ) {
        return Err(ReminderStateError::NotALifecycleEvent);
    }
    save_event_in_tx(tx, event, expected_revision)?;
    cancel_existing(tx, clock.now(), &event.item_id)
}

/// Cancel the reminder of every item that is no longer active (completed, cancelled or
/// deleted by any path) and whose reminder is not yet cancelled. Run at startup or before
/// reconciling with the OS; returns the reminders it cancelled.
pub fn reconcile_inactive_items(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
) -> Result<Vec<ReminderRecord>, ReminderStateError> {
    let mut statement = tx.prepare(
        "SELECT reminders.item_id FROM reminders
         JOIN items ON items.item_id = reminders.item_id
         WHERE items.lifecycle_state != 'active' AND reminders.request_state != 'cancelled'
         ORDER BY reminders.created_at, reminders.reminder_id",
    )?;
    let item_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut cancelled = Vec::new();
    for item_id in item_ids {
        if let Some(record) = cancel_existing(tx, clock.now(), &item_id)? {
            cancelled.push(record);
        }
    }
    Ok(cancelled)
}

pub fn get_reminder(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    load_reminder_by_item(tx, item_id)
}

/// Operations for a reminder in creation order.
pub fn list_operations(
    tx: &Transaction<'_>,
    reminder_id: &str,
) -> Result<Vec<ReminderOperation>, ReminderStateError> {
    let mut statement = tx.prepare(
        "SELECT operation_id, reminder_id, operation_type, operation_state, effect_identity,
                external_id, scheduled_for, created_at
         FROM reminder_operations WHERE reminder_id = ? ORDER BY created_at, rowid",
    )?;
    let rows = statement
        .query_map([reminder_id], read_operation_columns)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(decode_operation).collect()
}

/// The native set core wants installed: every `Resolved` reminder of an active item at its
/// current generation.
pub fn desired_notifications(
    tx: &Transaction<'_>,
) -> Result<Vec<DesiredNotification>, ReminderStateError> {
    let mut statement = tx.prepare(
        "SELECT reminders.reminder_id, reminders.item_id, reminders.schedule_generation,
                reminders.resolved_instant
         FROM reminders JOIN items ON items.item_id = reminders.item_id
         WHERE reminders.request_state = 'resolved' AND items.lifecycle_state = 'active'
         ORDER BY reminders.resolved_instant, reminders.reminder_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(reminder_id, item_id, generation, instant)| {
            let fire_at = parse_instant(
                instant
                    .as_deref()
                    .ok_or_else(|| corrupt("resolved_instant", "<missing>"))?,
            )?;
            Ok(DesiredNotification {
                notification_id: notification_identifier(&reminder_id, generation),
                reminder_id,
                item_id,
                fire_at,
            })
        })
        .collect()
}

fn load_item(tx: &Transaction<'_>, item_id: &str) -> Result<ItemState, ReminderStateError> {
    load_item_state(tx, item_id)?
        .ok_or_else(|| ReminderStateError::ItemNotFound(item_id.to_string()))
}

fn require_active(item: &ItemState) -> Result<(), ReminderStateError> {
    if item.lifecycle_state != LifecycleState::Active {
        return Err(ReminderStateError::ItemNotActive {
            item_id: item.item_id.clone(),
            lifecycle_state: item.lifecycle_state,
        });
    }
    Ok(())
}

fn load_reminder_capable_item(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<ItemState, ReminderStateError> {
    let item = load_item(tx, item_id)?;
    require_active(&item)?;
    let can_carry = item
        .item_type
        .is_some_and(|item_type| item_type.can_carry_obligation());
    if !can_carry {
        return Err(ReminderStateError::ItemCannotCarryReminder(
            item_id.to_string(),
        ));
    }
    Ok(item)
}

fn check_revision(item: &ItemState, expected: i32) -> Result<(), ReminderStateError> {
    if item.revision != expected {
        return Err(ReminderStateError::StaleRevision {
            item_id: item.item_id.clone(),
            expected,
            current: item.revision,
        });
    }
    Ok(())
}

fn normalize_phrase(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn require_phrase_in_text(item: &ItemState, phrase: &str) -> Result<(), ReminderStateError> {
    let normalized_phrase = normalize_phrase(phrase);
    if normalized_phrase.is_empty() {
        return Err(ReminderStateError::FabricatedDeadline(
            "empty phrase".to_string(),
        ));
    }
    let contained = item
        .current_text
        .text()
        .is_some_and(|text| normalize_phrase(text).contains(&normalized_phrase));
    if !contained {
        return Err(ReminderStateError::FabricatedDeadline(format!(
            "phrase '{}' does not occur in the item's text",
            phrase.trim()
        )));
    }
    Ok(())
}

fn load_capture_time_context(
    tx: &Transaction<'_>,
    capture_id: &str,
) -> Result<TimeContext, ReminderStateError> {
    let (capture_instant, timezone_id, utc_offset_minutes, locale, calendar): (
        String,
        String,
        i32,
        String,
        String,
    ) = tx.query_row(
        "SELECT capture_instant, timezone_id, utc_offset_minutes, locale, calendar
         FROM captures WHERE capture_id = ?",
        [capture_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    Ok(TimeContext {
        timezone: timezone_id,
        locale,
        reference_time: parse_instant(&capture_instant)?,
        utc_offset_at_capture: utc_offset_minutes * 60,
        calendar,
    })
}

fn parse_instant(text: &str) -> Result<DateTime<Utc>, ReminderStateError> {
    DateTime::parse_from_rfc3339(text)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| corrupt("timestamp", text))
}

fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

type ReminderColumns = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
);

fn load_reminder_by_item(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    let columns: Option<ReminderColumns> = tx
        .query_row(
            "SELECT reminder_id, item_id, request_state, schedule_state, delivery_state,
                    acknowledgment_state, resolved_instant, timezone_id, ambiguity_reason,
                    unsupported_reason, schedule_generation, created_at, updated_at,
                    unschedulable_reason, source_phrase, state_version
             FROM reminders WHERE item_id = ?",
            [item_id],
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
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                ))
            },
        )
        .optional()?;
    columns.map(decode_reminder).transpose()
}

fn decode_reminder(columns: ReminderColumns) -> Result<ReminderRecord, ReminderStateError> {
    let (
        reminder_id,
        item_id,
        request_state,
        schedule_state,
        delivery_state,
        acknowledgment_state,
        resolved_instant,
        timezone_id,
        ambiguity_reason,
        unsupported_reason,
        schedule_generation,
        created_at,
        updated_at,
        unschedulable_reason,
        source_phrase,
        state_version,
    ) = columns;
    let request_state = RequestState::decode(&request_state, unschedulable_reason.as_deref())?;
    let resolved_instant = resolved_instant.as_deref().map(parse_instant).transpose()?;
    if request_state == RequestState::Resolved && resolved_instant.is_none() {
        return Err(corrupt("resolved reminder without instant", &reminder_id));
    }
    Ok(ReminderRecord {
        reminder_id,
        item_id,
        request_state,
        schedule_state: schedule_state.parse()?,
        delivery_state: delivery_state.parse()?,
        acknowledgment_state: acknowledgment_state.parse()?,
        resolved_instant,
        timezone_id,
        ambiguity_reason,
        unsupported_reason,
        source_phrase,
        schedule_generation,
        state_version,
        created_at: parse_instant(&created_at)?,
        updated_at: parse_instant(&updated_at)?,
    })
}

type OperationColumns = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

fn read_operation_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn decode_operation(columns: OperationColumns) -> Result<ReminderOperation, ReminderStateError> {
    let (
        operation_id,
        reminder_id,
        operation_type,
        operation_state,
        effect_identity,
        external_id,
        scheduled_for,
        created_at,
    ) = columns;
    Ok(ReminderOperation {
        operation_id,
        reminder_id,
        operation_type: operation_type.parse()?,
        operation_state: operation_state.parse()?,
        effect_identity,
        notification_id: external_id
            .ok_or_else(|| corrupt("operation notification id", "<missing>"))?,
        scheduled_for: scheduled_for.as_deref().map(parse_instant).transpose()?,
        created_at: parse_instant(&created_at)?,
    })
}

/// Record a desired operation, or return the identical one already recorded. Reusing an
/// effect identity with different content is a conflict, never a silent overwrite.
fn record_operation(
    tx: &Transaction<'_>,
    reminder_id: &str,
    operation_type: OperationType,
    notification_id: &str,
    scheduled_for: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<ReminderOperation, ReminderStateError> {
    let identity = effect_identity(operation_type, notification_id);
    let existing: Option<OperationColumns> = tx
        .query_row(
            "SELECT operation_id, reminder_id, operation_type, operation_state, effect_identity,
                    external_id, scheduled_for, created_at
             FROM reminder_operations WHERE reminder_id = ? AND effect_identity = ?",
            rusqlite::params![reminder_id, &identity],
            read_operation_columns,
        )
        .optional()?;
    if let Some(columns) = existing {
        let operation = decode_operation(columns)?;
        if operation.operation_type != operation_type
            || operation.notification_id != notification_id
            || operation.scheduled_for != scheduled_for
        {
            return Err(ReminderStateError::ConflictingReplay {
                effect_identity: identity,
            });
        }
        return Ok(operation);
    }
    tx.execute(
        "INSERT INTO reminder_operations
            (operation_id, reminder_id, operation_type, operation_state, effect_identity,
             external_id, scheduled_for, created_at)
         VALUES (?, ?, ?, 'pending', ?, ?, ?, ?)",
        rusqlite::params![
            &identity,
            reminder_id,
            operation_type.as_str(),
            &identity,
            notification_id,
            scheduled_for.map(format_instant),
            format_instant(now),
        ],
    )?;
    Ok(ReminderOperation {
        operation_id: identity.clone(),
        reminder_id: reminder_id.to_string(),
        operation_type,
        operation_state: OperationState::Pending,
        effect_identity: identity,
        notification_id: notification_id.to_string(),
        scheduled_for,
        created_at: now,
    })
}

/// Retire the schedule of `generation` if one was ever requested: mark an unacknowledged
/// schedule operation superseded and record a cancel for its native identifier. Returns
/// whether a schedule existed.
fn retire_generation(
    tx: &Transaction<'_>,
    reminder_id: &str,
    generation: i64,
    now: DateTime<Utc>,
) -> Result<bool, ReminderStateError> {
    if generation <= 0 {
        return Ok(false);
    }
    let notification_id = notification_identifier(reminder_id, generation);
    let schedule_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM reminder_operations
                       WHERE reminder_id = ? AND effect_identity = ?)",
        rusqlite::params![
            reminder_id,
            effect_identity(OperationType::Schedule, &notification_id)
        ],
        |row| row.get(0),
    )?;
    if !schedule_exists {
        return Ok(false);
    }
    tx.execute(
        "UPDATE reminder_operations SET operation_state = 'superseded'
         WHERE reminder_id = ? AND effect_identity = ? AND operation_state = 'pending'",
        rusqlite::params![
            reminder_id,
            effect_identity(OperationType::Schedule, &notification_id)
        ],
    )?;
    record_operation(
        tx,
        reminder_id,
        OperationType::Cancel,
        &notification_id,
        None,
        now,
    )?;
    Ok(true)
}

fn cancel_existing(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    item_id: &str,
) -> Result<Option<ReminderRecord>, ReminderStateError> {
    let Some(record) = load_reminder_by_item(tx, item_id)? else {
        return Ok(None);
    };
    if record.request_state == RequestState::Cancelled {
        return Ok(Some(record));
    }
    retire_generation(tx, &record.reminder_id, record.schedule_generation, now)?;
    tx.execute(
        "UPDATE reminders SET request_state = 'cancelled', unschedulable_reason = NULL,
                state_version = state_version + 1, updated_at = ? WHERE reminder_id = ?",
        rusqlite::params![format_instant(now), &record.reminder_id],
    )?;
    load_reminder_by_item(tx, item_id)
}

/// Write a new desired state and the operations it implies, in the caller's transaction.
fn commit_outcome(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    item_id: &str,
    existing: Option<ReminderRecord>,
    outcome: TimeOutcome,
    requested_phrase: Option<&str>,
) -> Result<ReminderRecord, ReminderStateError> {
    let source_phrase = requested_phrase.map(str::to_string).or_else(|| {
        existing
            .as_ref()
            .and_then(|record| record.source_phrase.clone())
    });
    let (reminder_id, previous_generation, created_at, previous_schedule_state) = match &existing {
        Some(record) => (
            record.reminder_id.clone(),
            record.schedule_generation,
            record.created_at,
            record.schedule_state,
        ),
        None => (
            uuid::Uuid::new_v4().to_string(),
            0,
            now,
            ScheduleState::NotScheduled,
        ),
    };

    let retired = retire_generation(tx, &reminder_id, previous_generation, now)?;

    let (
        request_state,
        resolved_instant,
        timezone_id,
        ambiguity_reason,
        unsupported_reason,
        unschedulable_reason,
    ) = match &outcome {
        TimeOutcome::Resolved {
            instant,
            timezone_id,
        } => (
            RequestState::Resolved,
            Some(*instant),
            Some(timezone_id.clone()),
            None,
            None,
            None,
        ),
        TimeOutcome::InPast {
            instant,
            timezone_id,
        } => (
            RequestState::Unschedulable(UnschedulableReason::TimeInPast),
            Some(*instant),
            Some(timezone_id.clone()),
            None,
            None,
            Some(UnschedulableReason::TimeInPast.as_str().to_string()),
        ),
        TimeOutcome::NeedsCorrection {
            reason,
            timezone_id,
        } => (
            RequestState::NotScheduledYet,
            None,
            Some(timezone_id.clone()),
            Some(reason.clone()),
            None,
            None,
        ),
        TimeOutcome::UnsupportedRecurrence {
            reason,
            timezone_id,
        } => (
            RequestState::UnsupportedRecurrence,
            None,
            Some(timezone_id.clone()),
            None,
            Some(reason.clone()),
            None,
        ),
    };

    let (schedule_generation, schedule_state) = match request_state {
        RequestState::Resolved => (previous_generation + 1, ScheduleState::PendingSchedule),
        _ if retired => (previous_generation, previous_schedule_state),
        _ => (previous_generation, ScheduleState::NotScheduled),
    };

    if existing.is_none() {
        tx.execute(
            "INSERT INTO reminders
                (reminder_id, item_id, request_state, schedule_state, delivery_state,
                 acknowledgment_state, resolved_instant, timezone_id, ambiguity_reason,
                 unsupported_reason, unschedulable_reason, source_phrase, schedule_generation,
                 state_version, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'unknown', 'not_acknowledged', ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
            rusqlite::params![
                &reminder_id,
                item_id,
                request_state.column_value(),
                schedule_state.as_str(),
                resolved_instant.map(format_instant),
                &timezone_id,
                &ambiguity_reason,
                &unsupported_reason,
                &unschedulable_reason,
                &source_phrase,
                schedule_generation,
                format_instant(created_at),
                format_instant(now),
            ],
        )?;
    } else {
        tx.execute(
            "UPDATE reminders SET request_state = ?, schedule_state = ?, resolved_instant = ?,
                timezone_id = ?, ambiguity_reason = ?, unsupported_reason = ?,
                unschedulable_reason = ?, source_phrase = ?, schedule_generation = ?,
                state_version = state_version + 1, updated_at = ?
             WHERE reminder_id = ?",
            rusqlite::params![
                request_state.column_value(),
                schedule_state.as_str(),
                resolved_instant.map(format_instant),
                &timezone_id,
                &ambiguity_reason,
                &unsupported_reason,
                &unschedulable_reason,
                &source_phrase,
                schedule_generation,
                format_instant(now),
                &reminder_id,
            ],
        )?;
    }

    if request_state == RequestState::Resolved {
        record_operation(
            tx,
            &reminder_id,
            OperationType::Schedule,
            &notification_identifier(&reminder_id, schedule_generation),
            resolved_instant,
            now,
        )?;
    }

    load_reminder_by_item(tx, item_id)?
        .ok_or_else(|| corrupt("reminder missing after write", &reminder_id))
}
