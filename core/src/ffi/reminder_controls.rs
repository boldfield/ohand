//! Reading, editing, cancelling and reviewing one item's reminder over the core handle (A04).
//!
//! Two exports share this module. Reads carry the caller's read scopes and are meant to be issued
//! only through the native read gate; writes are user commands that native issues only for an
//! authenticated session. Neither touches the OS: a write changes the reminder's desired state and
//! its pending operations, and the next reminder pass installs or cancels the notification.
//!
//! # Reads: `ohand_core_start_reminder_read`
//!
//! One tagged JSON object, `{"command": "get_reminder" | "reminder_history", "item_id", "read_scopes"}`.
//! `read_scopes` is required and non-empty (`personal` and/or `work`). An item that is unknown,
//! deleted or outside the scopes fails as `not_found`, indistinguishably.
//!
//! * `get_reminder` answers `{"operation_id", "item_id", "item_revision", "item_type",
//!   "lifecycle_state", "reminder"}`. `reminder` is `null` when the item has none, else
//!   `{"reminder_id", "source_phrase", "request_state", "unschedulable_reason", "schedule_state",
//!   "delivery_state", "acknowledgment_state", "resolved_instant", "timezone_id",
//!   "ambiguity_reason", "unsupported_reason", "schedule_generation", "state_version",
//!   "updated_at"}`. The state names are those the item status reports. `source_phrase` is the
//!   request as made and `resolved_instant` what it resolved to (kept for an unschedulable
//!   `time_in_past` request, so the expired opportunity stays visible). A caller edits with
//!   `item_revision` and `state_version` (0 when there is no reminder).
//! * `reminder_history` answers `{"operation_id", "item_id", "reminder_id", "summary", "entries",
//!   "total_entries", "truncated"}`. `summary` is the live generation's standing (request,
//!   schedule, delivery and acknowledgment states, `due_at`, `timezone_id`, `due`:
//!   `not_applicable` | `upcoming` | `elapsed`, and `elapsed_without_delivery_evidence`). `entries`
//!   are the newest [`OHAND_CORE_MAX_REMINDER_HISTORY_ENTRIES`] recorded facts, newest first; when
//!   there are more, `truncated` is true and `total_entries` counts them all. An item without a
//!   reminder answers `reminder_id` `null`, `summary` `null` and no entries.
//!
//! # Writes: `ohand_core_start_reminder_control`
//!
//! `correct_time` (`command_id`, `item_id`, `expected_revision`, `expected_reminder_version`,
//! `instant`, `timezone_id`) and `cancel` (the same without `instant` and `timezone_id`). Every
//! command is idempotent by `command_id`: repeating one answers its first result and writes
//! nothing. `instant` is an RFC 3339 instant on a whole second, in the future when the command is
//! first applied; a fractional second or an unknown IANA zone is `invalid_request`. A time that is
//! not in the future is refused as `time_in_past` and never stored as a backdated reminder, and
//! the stored `resolved_instant` is always exactly the instant the user gave.
//!
//! Outcomes: `corrected` / `cancelled` (`item_id`, `reminder`, `replayed`); `no_reminder` (a cancel
//! with nothing to cancel); `stale_revision` (`current_revision`); `stale_reminder_version`
//! (`current_reminder_version`); `command_conflict` (the id was used for a different command);
//! `item_deleted`; `item_not_active` (`lifecycle_state`: `completed` | `cancelled`);
//! `item_cannot_carry_reminder` (the item is not an action). Every refusal wrote nothing. An
//! unknown item fails as `not_found`. Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::domain::items::{load_item_state, ItemState, LifecycleState};
use crate::reminders::history::{
    reminder_history, DueStatus, HistoryEntry, HistoryEntryKind, HistoryError, NativeEffectStatus,
    ReminderHistory,
};
use crate::reminders::state::{
    apply_user_time_correction, cancel_reminder, get_reminder, AcknowledgmentState, DeliveryState,
    ReminderCancellation, ReminderRecord, ReminderStateError, RequestState, UserTimeCorrection,
};
use crate::store::events::ItemScope;
use crate::store::schema::{Clock, Database, SystemClock};
use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::Transaction;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

/// Most history entries one `reminder_history` answer carries.
pub const OHAND_CORE_MAX_REMINDER_HISTORY_ENTRIES: usize = 50;

const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_TIMEZONE_BYTES: usize = 128;

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

fn state_failure(error: ReminderStateError) -> AbiFailure {
    match error {
        ReminderStateError::ItemNotFound(_) => AbiFailure::NOT_FOUND,
        ReminderStateError::InvalidTimezone(_) => AbiFailure::INVALID_REQUEST,
        ReminderStateError::Database(error) => sqlite_failure(error),
        ReminderStateError::Store(error) => store_failure(&error),
        ReminderStateError::Corrupt(_) | ReminderStateError::Event(_) => AbiFailure::STORAGE_ERROR,
        _ => AbiFailure::INTERNAL,
    }
}

fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn acceptable_identifier(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES
}

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

fn request_state_name(state: &RequestState) -> &'static str {
    match state {
        RequestState::NotRequested => "not_requested",
        RequestState::Resolved => "resolved",
        RequestState::NotScheduledYet => "not_scheduled_yet",
        RequestState::UnsupportedRecurrence => "unsupported_recurrence",
        RequestState::Unschedulable(_) => "unschedulable",
        RequestState::Cancelled => "cancelled",
    }
}

fn unschedulable_reason(state: &RequestState) -> Option<&'static str> {
    match state {
        RequestState::Unschedulable(reason) => Some(reason.as_str()),
        _ => None,
    }
}

fn delivery_state_name(state: &DeliveryState) -> &'static str {
    match state {
        DeliveryState::Unknown => "unknown",
        DeliveryState::Delivered => "delivered",
        DeliveryState::Opened => "opened",
    }
}

fn acknowledgment_state_name(state: &AcknowledgmentState) -> &'static str {
    match state {
        AcknowledgmentState::NotAcknowledged => "not_acknowledged",
        AcknowledgmentState::Acknowledged => "acknowledged",
    }
}

fn due_name(due: &DueStatus) -> &'static str {
    match due {
        DueStatus::NotApplicable => "not_applicable",
        DueStatus::Upcoming => "upcoming",
        DueStatus::Elapsed => "elapsed",
    }
}

fn native_effect_name(status: &NativeEffectStatus) -> &'static str {
    match status {
        NativeEffectStatus::NotAttempted => "not_attempted",
        NativeEffectStatus::AttemptedUnconfirmed => "attempted_unconfirmed",
        NativeEffectStatus::Rejected => "rejected",
        NativeEffectStatus::Confirmed => "confirmed",
        NativeEffectStatus::Superseded => "superseded",
    }
}

fn reminder_json(record: &ReminderRecord) -> Value {
    json!({
        "reminder_id": record.reminder_id,
        "source_phrase": record.source_phrase,
        "request_state": request_state_name(&record.request_state),
        "unschedulable_reason": unschedulable_reason(&record.request_state),
        "schedule_state": record.schedule_state.as_str(),
        "delivery_state": delivery_state_name(&record.delivery_state),
        "acknowledgment_state": acknowledgment_state_name(&record.acknowledgment_state),
        "resolved_instant": record.resolved_instant.map(format_instant),
        "timezone_id": record.timezone_id,
        "ambiguity_reason": record.ambiguity_reason,
        "unsupported_reason": record.unsupported_reason,
        "schedule_generation": record.schedule_generation,
        "state_version": record.state_version,
        "updated_at": format_instant(record.updated_at),
    })
}

fn history_entry_json(entry: &HistoryEntry) -> Value {
    let mut body = json!({ "occurred_at": format_instant(entry.occurred_at) });
    let fields = match &entry.kind {
        HistoryEntryKind::TimeRequested {
            generation,
            due_at,
            installation,
        } => json!({
            "kind": "time_requested",
            "generation": generation,
            "due_at": format_instant(*due_at),
            "installation": native_effect_name(installation),
        }),
        HistoryEntryKind::RepeatRequested { reason } => json!({
            "kind": "repeat_requested",
            "reason": reason,
        }),
        HistoryEntryKind::NotSchedulable {
            request_state,
            due_at,
        } => json!({
            "kind": "not_schedulable",
            "request_state": request_state_name(request_state),
            "unschedulable_reason": unschedulable_reason(request_state),
            "due_at": due_at.map(format_instant),
        }),
        HistoryEntryKind::Cancelled {
            generation,
            native_cancel,
        } => json!({
            "kind": "cancelled",
            "generation": generation,
            "native_cancel": native_cancel.as_ref().map(native_effect_name),
        }),
        HistoryEntryKind::Delivered { generation } => {
            json!({ "kind": "delivered", "generation": generation })
        }
        HistoryEntryKind::Opened { generation } => {
            json!({ "kind": "opened", "generation": generation })
        }
        HistoryEntryKind::Acknowledged { generation } => {
            json!({ "kind": "acknowledged", "generation": generation })
        }
    };
    if let (Value::Object(body), Value::Object(fields)) = (&mut body, fields) {
        body.extend(fields);
    }
    body
}

fn history_json(item_id: &str, history: Option<&ReminderHistory>) -> Value {
    let Some(history) = history else {
        return json!({
            "item_id": item_id,
            "reminder_id": null,
            "summary": null,
            "entries": [],
            "total_entries": 0,
            "truncated": false,
        });
    };
    let summary = &history.summary;
    let entries: Vec<Value> = history
        .entries
        .iter()
        .rev()
        .take(OHAND_CORE_MAX_REMINDER_HISTORY_ENTRIES)
        .map(history_entry_json)
        .collect();
    json!({
        "item_id": history.item_id,
        "reminder_id": history.reminder_id,
        "summary": {
            "request_state": request_state_name(&summary.request_state),
            "unschedulable_reason": unschedulable_reason(&summary.request_state),
            "schedule_state": summary.schedule_state.as_str(),
            "due_at": summary.due_at.map(format_instant),
            "timezone_id": summary.timezone_id,
            "due": due_name(&summary.due),
            "delivery_state": delivery_state_name(&summary.delivery),
            "acknowledgment_state": acknowledgment_state_name(&summary.acknowledgment),
            "elapsed_without_delivery_evidence": summary.elapsed_without_delivery_evidence(),
        },
        "truncated": history.entries.len() > entries.len(),
        "total_entries": history.entries.len(),
        "entries": entries,
    })
}

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

/// What a read asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderReadKind {
    Reminder,
    History,
}

/// One read of an item's reminder, limited to the scopes the caller may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderRead {
    pub kind: ReminderReadKind,
    pub item_id: String,
    pub read_scopes: Vec<ItemScope>,
}

fn readable_item(
    tx: &Transaction<'_>,
    item_id: &str,
    read_scopes: &[ItemScope],
) -> Result<ItemState, AbiFailure> {
    load_item_state(tx, item_id)
        .map_err(|error| store_failure(&error))?
        .filter(|item| {
            item.lifecycle_state != LifecycleState::Deleted && read_scopes.contains(&item.scope)
        })
        .ok_or(AbiFailure::NOT_FOUND)
}

/// Reads one item's reminder or reminder history. An unknown, deleted or out-of-scope item is
/// `NOT_FOUND`. Writes nothing.
pub fn read_item_reminder(
    database: &mut Database,
    clock: &dyn Clock,
    read: &ReminderRead,
) -> Result<Value, AbiFailure> {
    let tx = database.transaction().map_err(|e| store_failure(&e))?;
    let item = readable_item(&tx, &read.item_id, &read.read_scopes)?;
    match read.kind {
        ReminderReadKind::Reminder => {
            let reminder = get_reminder(&tx, &read.item_id).map_err(state_failure)?;
            Ok(json!({
                "item_id": item.item_id,
                "item_revision": item.revision,
                "item_type": item.item_type.map(|item_type| item_type.as_str()),
                "lifecycle_state": item.lifecycle_state.as_str(),
                "reminder": reminder.as_ref().map(reminder_json),
            }))
        }
        ReminderReadKind::History => {
            let history =
                reminder_history(&tx, clock, &read.item_id).map_err(|error| match error {
                    HistoryError::State(error) => state_failure(error),
                    HistoryError::Database(error) => sqlite_failure(error),
                    _ => AbiFailure::STORAGE_ERROR,
                })?;
            Ok(history_json(&item.item_id, history.as_ref()))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum ReadCommand {
    #[serde(rename = "get_reminder")]
    Reminder {
        item_id: String,
        read_scopes: Vec<String>,
    },
    #[serde(rename = "reminder_history")]
    History {
        item_id: String,
        read_scopes: Vec<String>,
    },
}

impl ReadCommand {
    fn into_read(self) -> Result<ReminderRead, AbiFailure> {
        let (kind, item_id, scope_names) = match self {
            ReadCommand::Reminder {
                item_id,
                read_scopes,
            } => (ReminderReadKind::Reminder, item_id, read_scopes),
            ReadCommand::History {
                item_id,
                read_scopes,
            } => (ReminderReadKind::History, item_id, read_scopes),
        };
        let mut read_scopes: Vec<ItemScope> = Vec::new();
        for name in &scope_names {
            let scope = ItemScope::from_str(name).map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if !read_scopes.contains(&scope) {
                read_scopes.push(scope);
            }
        }
        if read_scopes.is_empty() || !acceptable_identifier(&item_id) {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(ReminderRead {
            kind,
            item_id,
            read_scopes,
        })
    }
}

fn with_operation_id(mut body: Value, operation_id: u64) -> Result<Vec<u8>, AbiFailure> {
    if let Value::Object(fields) = &mut body {
        fields.insert("operation_id".to_string(), json!(operation_id));
    }
    serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
}

fn read_job(read: ReminderRead, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let body = read_item_reminder(&mut database, &SystemClock, &read)?;
        with_operation_id(body, operation_id)
    })
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

/// The user's chosen reminder time. `instant` is a whole second.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderTimeEdit {
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    pub expected_reminder_version: i64,
    pub instant: DateTime<Utc>,
    pub timezone_id: String,
}

/// The user cancelling the reminder while keeping the item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderCancel {
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    pub expected_reminder_version: i64,
}

/// What a write did. Every variant except `Applied` and `NoReminder` means nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReminderWriteOutcome {
    Applied {
        reminder: ReminderRecord,
        replayed: bool,
    },
    NoReminder,
    StaleRevision {
        current_revision: i32,
    },
    StaleReminderVersion {
        current_reminder_version: i64,
    },
    CommandConflict,
    ItemDeleted,
    ItemNotActive {
        lifecycle_state: LifecycleState,
    },
    ItemCannotCarryReminder,
    TimeInPast,
}

fn command_already_applied(tx: &Transaction<'_>, command_id: &str) -> Result<bool, AbiFailure> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM reminder_commands WHERE command_id = ?)",
        [command_id],
        |row| row.get(0),
    )
    .map_err(sqlite_failure)
}

/// A state error that is an answer rather than a failure, as an outcome.
fn refusal(error: &ReminderStateError) -> Option<ReminderWriteOutcome> {
    Some(match error {
        ReminderStateError::StaleRevision { current, .. } => ReminderWriteOutcome::StaleRevision {
            current_revision: *current,
        },
        ReminderStateError::StaleReminderVersion { current, .. } => {
            ReminderWriteOutcome::StaleReminderVersion {
                current_reminder_version: *current,
            }
        }
        ReminderStateError::CommandIdReused { .. } => ReminderWriteOutcome::CommandConflict,
        ReminderStateError::ItemNotActive {
            lifecycle_state: LifecycleState::Deleted,
            ..
        } => ReminderWriteOutcome::ItemDeleted,
        ReminderStateError::ItemNotActive {
            lifecycle_state, ..
        } => ReminderWriteOutcome::ItemNotActive {
            lifecycle_state: *lifecycle_state,
        },
        ReminderStateError::ItemCannotCarryReminder(_) => {
            ReminderWriteOutcome::ItemCannotCarryReminder
        }
        _ => return None,
    })
}

fn settle(
    tx: Transaction<'_>,
    result: Result<ReminderWriteOutcome, ReminderStateError>,
) -> Result<ReminderWriteOutcome, AbiFailure> {
    match result {
        Ok(outcome) => {
            tx.commit().map_err(sqlite_failure)?;
            Ok(outcome)
        }
        Err(error) => match refusal(&error) {
            Some(outcome) => Ok(outcome),
            None => Err(state_failure(error)),
        },
    }
}

/// Applies the user's time correction in one immediate transaction. The refusals that need no
/// state module (unknown, deleted, inactive, not an action, stale, in the past) are decided here
/// from the same item state the module uses, so a refusal never depends on write order.
pub fn apply_reminder_time_edit(
    database: &mut Database,
    clock: &dyn Clock,
    edit: &ReminderTimeEdit,
) -> Result<ReminderWriteOutcome, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    let replayed = command_already_applied(&tx, &edit.command_id)?;
    if !replayed {
        let item = load_item_state(&tx, &edit.item_id)
            .map_err(|error| store_failure(&error))?
            .ok_or(AbiFailure::NOT_FOUND)?;
        match item.lifecycle_state {
            LifecycleState::Active => {}
            LifecycleState::Deleted => return Ok(ReminderWriteOutcome::ItemDeleted),
            lifecycle_state => return Ok(ReminderWriteOutcome::ItemNotActive { lifecycle_state }),
        }
        if !item
            .item_type
            .is_some_and(|item_type| item_type.can_carry_obligation())
        {
            return Ok(ReminderWriteOutcome::ItemCannotCarryReminder);
        }
        if item.revision != edit.expected_revision {
            return Ok(ReminderWriteOutcome::StaleRevision {
                current_revision: item.revision,
            });
        }
        let current_version = get_reminder(&tx, &edit.item_id)
            .map_err(state_failure)?
            .map_or(0, |record| record.state_version);
        if current_version != edit.expected_reminder_version {
            return Ok(ReminderWriteOutcome::StaleReminderVersion {
                current_reminder_version: current_version,
            });
        }
        if edit.instant <= clock.now() {
            return Ok(ReminderWriteOutcome::TimeInPast);
        }
    }
    let result = apply_user_time_correction(
        &tx,
        clock,
        &UserTimeCorrection {
            command_id: edit.command_id.clone(),
            item_id: edit.item_id.clone(),
            expected_revision: edit.expected_revision,
            expected_reminder_version: edit.expected_reminder_version,
            instant: edit.instant,
            timezone_id: edit.timezone_id.clone(),
        },
    )
    .map(|reminder| ReminderWriteOutcome::Applied { reminder, replayed });
    settle(tx, result)
}

/// Cancels the item's reminder, keeping the item, in one immediate transaction.
pub fn apply_reminder_cancel(
    database: &mut Database,
    clock: &dyn Clock,
    cancel: &ReminderCancel,
) -> Result<ReminderWriteOutcome, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    let replayed = command_already_applied(&tx, &cancel.command_id)?;
    let result = cancel_reminder(
        &tx,
        clock,
        &ReminderCancellation {
            command_id: cancel.command_id.clone(),
            item_id: cancel.item_id.clone(),
            expected_revision: cancel.expected_revision,
            expected_reminder_version: cancel.expected_reminder_version,
        },
    )
    .map(|reminder| match reminder {
        Some(reminder) => ReminderWriteOutcome::Applied { reminder, replayed },
        None => ReminderWriteOutcome::NoReminder,
    });
    settle(tx, result)
}

fn write_answer(item_id: &str, command: &WriteKind, outcome: &ReminderWriteOutcome) -> Value {
    match outcome {
        ReminderWriteOutcome::Applied { reminder, replayed } => json!({
            "outcome": match command {
                WriteKind::Edit => "corrected",
                WriteKind::Cancel => "cancelled",
            },
            "item_id": item_id,
            "reminder": reminder_json(reminder),
            "replayed": replayed,
        }),
        ReminderWriteOutcome::NoReminder => json!({ "outcome": "no_reminder" }),
        ReminderWriteOutcome::StaleRevision { current_revision } => json!({
            "outcome": "stale_revision",
            "current_revision": current_revision,
        }),
        ReminderWriteOutcome::StaleReminderVersion {
            current_reminder_version,
        } => json!({
            "outcome": "stale_reminder_version",
            "current_reminder_version": current_reminder_version,
        }),
        ReminderWriteOutcome::CommandConflict => json!({ "outcome": "command_conflict" }),
        ReminderWriteOutcome::ItemDeleted => json!({ "outcome": "item_deleted" }),
        ReminderWriteOutcome::ItemNotActive { lifecycle_state } => json!({
            "outcome": "item_not_active",
            "lifecycle_state": lifecycle_state.as_str(),
        }),
        ReminderWriteOutcome::ItemCannotCarryReminder => {
            json!({ "outcome": "item_cannot_carry_reminder" })
        }
        ReminderWriteOutcome::TimeInPast => json!({ "outcome": "time_in_past" }),
    }
}

#[derive(Clone, Debug)]
enum WriteKind {
    Edit,
    Cancel,
}

#[derive(Clone, Debug)]
enum WriteCommand {
    Edit(ReminderTimeEdit),
    Cancel(ReminderCancel),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum ControlCommand {
    #[serde(rename = "correct_time")]
    CorrectTime {
        command_id: String,
        item_id: String,
        expected_revision: i64,
        expected_reminder_version: i64,
        instant: String,
        timezone_id: String,
    },
    #[serde(rename = "cancel")]
    Cancel {
        command_id: String,
        item_id: String,
        expected_revision: i64,
        expected_reminder_version: i64,
    },
}

impl ControlCommand {
    /// Refuse malformed commands before anything is queued.
    fn into_write(self) -> Result<WriteCommand, AbiFailure> {
        let valid_numbers = |revision: i64, version: i64| -> Result<(i32, i64), AbiFailure> {
            let revision = i32::try_from(revision).map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if revision < 0 || version < 0 {
                return Err(AbiFailure::INVALID_REQUEST);
            }
            Ok((revision, version))
        };
        match self {
            ControlCommand::CorrectTime {
                command_id,
                item_id,
                expected_revision,
                expected_reminder_version,
                instant,
                timezone_id,
            } => {
                let (expected_revision, expected_reminder_version) =
                    valid_numbers(expected_revision, expected_reminder_version)?;
                if !acceptable_identifier(&command_id)
                    || !acceptable_identifier(&item_id)
                    || timezone_id.is_empty()
                    || timezone_id.len() > MAX_TIMEZONE_BYTES
                    || timezone_id.parse::<Tz>().is_err()
                {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                let instant = DateTime::parse_from_rfc3339(&instant)
                    .map_err(|_| AbiFailure::INVALID_REQUEST)?
                    .with_timezone(&Utc);
                if instant.nanosecond() != 0 {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                Ok(WriteCommand::Edit(ReminderTimeEdit {
                    command_id,
                    item_id,
                    expected_revision,
                    expected_reminder_version,
                    instant,
                    timezone_id,
                }))
            }
            ControlCommand::Cancel {
                command_id,
                item_id,
                expected_revision,
                expected_reminder_version,
            } => {
                let (expected_revision, expected_reminder_version) =
                    valid_numbers(expected_revision, expected_reminder_version)?;
                if !acceptable_identifier(&command_id) || !acceptable_identifier(&item_id) {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                Ok(WriteCommand::Cancel(ReminderCancel {
                    command_id,
                    item_id,
                    expected_revision,
                    expected_reminder_version,
                }))
            }
        }
    }
}

fn control_job(command: WriteCommand, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let (item_id, kind, outcome) = match &command {
            WriteCommand::Edit(edit) => (
                edit.item_id.as_str(),
                WriteKind::Edit,
                apply_reminder_time_edit(&mut database, &SystemClock, edit)?,
            ),
            WriteCommand::Cancel(cancel) => (
                cancel.item_id.as_str(),
                WriteKind::Cancel,
                apply_reminder_cancel(&mut database, &SystemClock, cancel)?,
            ),
        };
        with_operation_id(write_answer(item_id, &kind, &outcome), operation_id)
    })
}

fn request_bytes<'a>(request: *const u8, request_len: usize) -> Result<&'a [u8], AbiFailure> {
    if request_len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if request_len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if request.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    // SAFETY: the caller of each export guarantees `request` points at `request_len` readable
    // bytes for the duration of the call, and the bytes are parsed before the call returns.
    Ok(unsafe { std::slice::from_raw_parts(request, request_len) })
}

/// Queues one read of an item's reminder or reminder history described by the `request_len` JSON
/// bytes at `request` (see the module documentation). A malformed request, one with no read
/// scope, or with unknown fields is refused as `invalid_request` before anything is queued. Native
/// must issue it only through the read gate, passing only scopes the authenticated session may
/// read. The outcome event for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_reminder_read(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = request_bytes(request, request_len)?;
        let command: ReadCommand =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(operation_id, read_job(command.into_read()?, operation_id))
    })
}

/// Queues one user command on an item's reminder (`correct_time` or `cancel`) described by the
/// `request_len` JSON bytes at `request` (see the module documentation). Unknown commands and
/// fields, empty identifiers, a fractional-second instant and an unknown zone are refused as
/// `invalid_request` before anything is queued. Native must issue it only for an authenticated
/// session. The outcome event for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_reminder_control(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = request_bytes(request, request_len)?;
        let command: ControlCommand =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(
            operation_id,
            control_job(command.into_write()?, operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
