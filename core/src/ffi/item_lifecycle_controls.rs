//! The user's controls over an item's lifecycle and suggestions over the core handle (A03).
//!
//! `ohand_core_start_item_lifecycle_control` carries one tagged JSON command, runs it on the
//! handle's worker thread in one transaction, and answers `{"operation_id", "outcome", ...}`:
//!
//! * `complete_item` and `cancel_item`: end the item.
//! * `not_now`: leave the item out of suggestions for the stated cooldown.
//! * `stop_suggesting`: mark the item pull-only; it stays searchable and is never offered again.
//!
//! Every command also carries `command_id` (the immutable identity of the user action, reused
//! unchanged by each retry), `item_id` and `expected_revision` (the revision the user saw). They
//! are keyed by item, not by a notification identifier, and otherwise do exactly what the
//! notification action path (`complete`) and the prompt response path do:
//!
//! * Completion and cancellation are saved by `end_item_with_event`, which stores the event and
//!   cancels the item's reminder in the same transaction. The OS notification that reminder
//!   installed is removed by the next reminder pass, never by a native effect from here.
//! * Not-now is `record_not_now`: the control event plus the cooldown.
//! * Stop-suggesting is the control event plus `set_pull_only`.
//!
//! Outcomes:
//! * `applied` (`item_id`, `revision` the item now has, `replayed`).
//! * `stale_revision` (`current_revision`): the item moved on since the user saw it. Nothing was
//!   written. The failure shape of the boundary cannot carry the current revision and the
//!   caller needs it to refresh, so, like every other "nothing changed" outcome of this
//!   boundary, it is an answer.
//! * `command_conflict`: `command_id` was already used for a different command.
//! * `item_deleted`: the item was deleted.
//! * `item_ended` (`lifecycle`: `completed` | `cancelled`): the item was already ended, which
//!   takes no further lifecycle or suggestion command.
//!
//! Unknown items fail as `not_found`. A repeated `command_id` answers `applied` again with the
//! revision the first call produced and `replayed` true; it writes nothing and ignores
//! `expected_revision`, which the first call already consumed. Failures are normalized and never
//! echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::reminders::state::{end_item_with_event, ReminderStateError};
use crate::store::events::{
    get_event, get_item_snapshot, save_event_in_tx, Event, EventError, EventPayload, EventType,
    SuggestionControlKind, SuggestionControlPayload,
};
use crate::store::schema::{Database, SystemClock};
use crate::suggestions::eligibility::{record_not_now, set_pull_only};
use chrono::{SecondsFormat, Utc};
use rusqlite::Transaction;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleControl {
    Complete,
    Cancel,
    NotNow,
    StopSuggesting,
}

impl LifecycleControl {
    fn event_type(self) -> EventType {
        match self {
            LifecycleControl::Complete => EventType::Completion,
            LifecycleControl::Cancel => EventType::Cancellation,
            LifecycleControl::NotNow | LifecycleControl::StopSuggesting => {
                EventType::SuggestionControl
            }
        }
    }

    fn payload(self) -> EventPayload {
        match self {
            LifecycleControl::Complete => EventPayload::Completion,
            LifecycleControl::Cancel => EventPayload::Cancellation,
            LifecycleControl::NotNow => EventPayload::SuggestionControl(SuggestionControlPayload {
                kind: SuggestionControlKind::NotNow,
            }),
            LifecycleControl::StopSuggesting => {
                EventPayload::SuggestionControl(SuggestionControlPayload {
                    kind: SuggestionControlKind::StopSuggesting,
                })
            }
        }
    }
}

/// One user control, compare-and-set on the item's revision and idempotent by `command_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemLifecycleControl {
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    pub control: LifecycleControl,
}

/// What a control did. Every variant except `Applied` means nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleControlOutcome {
    Applied {
        /// The item's revision after the control.
        revision: i32,
        replayed: bool,
    },
    StaleRevision {
        current_revision: i32,
    },
    CommandConflict,
    ItemDeleted,
    ItemEnded {
        /// `completed` or `cancelled`.
        lifecycle: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    CompleteItem {
        command_id: String,
        item_id: String,
        expected_revision: i64,
    },
    CancelItem {
        command_id: String,
        item_id: String,
        expected_revision: i64,
    },
    NotNow {
        command_id: String,
        item_id: String,
        expected_revision: i64,
    },
    StopSuggesting {
        command_id: String,
        item_id: String,
        expected_revision: i64,
    },
}

impl Command {
    /// Refuse malformed commands before anything is queued.
    fn into_control(self) -> Result<ItemLifecycleControl, AbiFailure> {
        let (control, command_id, item_id, expected_revision) = match self {
            Command::CompleteItem {
                command_id,
                item_id,
                expected_revision,
            } => (
                LifecycleControl::Complete,
                command_id,
                item_id,
                expected_revision,
            ),
            Command::CancelItem {
                command_id,
                item_id,
                expected_revision,
            } => (
                LifecycleControl::Cancel,
                command_id,
                item_id,
                expected_revision,
            ),
            Command::NotNow {
                command_id,
                item_id,
                expected_revision,
            } => (
                LifecycleControl::NotNow,
                command_id,
                item_id,
                expected_revision,
            ),
            Command::StopSuggesting {
                command_id,
                item_id,
                expected_revision,
            } => (
                LifecycleControl::StopSuggesting,
                command_id,
                item_id,
                expected_revision,
            ),
        };
        let acceptable = |identifier: &String| {
            !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES
        };
        if !acceptable(&command_id) || !acceptable(&item_id) {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let expected_revision =
            i32::try_from(expected_revision).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        if expected_revision < 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(ItemLifecycleControl {
            command_id,
            item_id,
            expected_revision,
            control,
        })
    }
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

fn event_id_for(command_id: &str) -> String {
    format!("item-control:{command_id}")
}

/// The outcome an event failure means, or the failure to report.
fn event_failure(error: EventError) -> Result<LifecycleControlOutcome, AbiFailure> {
    match error {
        EventError::StaleRevision { current, .. } => Ok(LifecycleControlOutcome::StaleRevision {
            current_revision: current.revision,
        }),
        EventError::Conflict { .. } => Ok(LifecycleControlOutcome::CommandConflict),
        EventError::NotAllowedInState {
            lifecycle_state, ..
        } => Ok(match lifecycle_state.as_str() {
            "completed" | "cancelled" => LifecycleControlOutcome::ItemEnded {
                lifecycle: lifecycle_state,
            },
            _ => LifecycleControlOutcome::ItemDeleted,
        }),
        EventError::ItemNotFound { .. } => Err(AbiFailure::NOT_FOUND),
        EventError::Invalid(_) | EventError::RevisionMismatch { .. } => {
            Err(AbiFailure::INVALID_REQUEST)
        }
        EventError::Storage(error) => Err(sqlite_failure(error)),
        EventError::Database(error) => Err(store_failure(&error)),
        EventError::OldValueMismatch { .. } | EventError::Corrupt(_) => {
            Err(AbiFailure::STORAGE_ERROR)
        }
    }
}

fn reminder_state_failure(
    error: ReminderStateError,
) -> Result<LifecycleControlOutcome, AbiFailure> {
    match error {
        ReminderStateError::Event(error) => event_failure(error),
        ReminderStateError::Database(error) => Err(sqlite_failure(error)),
        ReminderStateError::Store(error) => Err(store_failure(&error)),
        _ => Err(AbiFailure::STORAGE_ERROR),
    }
}

/// The outcome the first call with this `command_id` produced, if there was one.
fn replay(
    tx: &Transaction<'_>,
    request: &ItemLifecycleControl,
) -> Result<Option<LifecycleControlOutcome>, AbiFailure> {
    let Some(stored) =
        get_event(tx, &event_id_for(&request.command_id)).map_err(|e| store_failure(&e))?
    else {
        return Ok(None);
    };
    let same_control = stored.item_id == request.item_id
        && stored.event_type == request.control.event_type()
        && stored.payload == request.control.payload();
    if !same_control {
        return Ok(Some(LifecycleControlOutcome::CommandConflict));
    }
    Ok(Some(LifecycleControlOutcome::Applied {
        revision: stored.revision + 1,
        replayed: true,
    }))
}

/// Applies one control in its own immediate transaction. See the module documentation for the
/// outcomes; an `Applied` result is durable when this returns.
pub fn apply_item_lifecycle_control(
    database: &mut Database,
    request: &ItemLifecycleControl,
) -> Result<LifecycleControlOutcome, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    if let Some(replayed) = replay(&tx, request)? {
        return Ok(replayed);
    }
    let snapshot = match get_item_snapshot(&tx, &request.item_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Err(AbiFailure::NOT_FOUND),
        Err(error) => return event_failure(error),
    };
    match snapshot.lifecycle_state.as_str() {
        "deleted" => return Ok(LifecycleControlOutcome::ItemDeleted),
        "completed" | "cancelled" => {
            return Ok(LifecycleControlOutcome::ItemEnded {
                lifecycle: snapshot.lifecycle_state,
            })
        }
        _ => {}
    }
    if snapshot.revision != request.expected_revision {
        return Ok(LifecycleControlOutcome::StaleRevision {
            current_revision: snapshot.revision,
        });
    }

    let event_id = event_id_for(&request.command_id);
    let now = Utc::now();
    let revision = request.expected_revision;
    match request.control {
        LifecycleControl::Complete | LifecycleControl::Cancel => {
            let event = Event::new(
                event_id,
                request.item_id.clone(),
                revision,
                request.control.event_type(),
                request.control.payload(),
                now.to_rfc3339_opts(SecondsFormat::Secs, true),
            )
            .map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if let Err(error) = end_item_with_event(&tx, &SystemClock, &event, revision) {
                return reminder_state_failure(error);
            }
        }
        LifecycleControl::NotNow => {
            record_not_now(&tx, &request.item_id, &event_id, revision, now)
                .map_err(|e| store_failure(&e))?;
        }
        LifecycleControl::StopSuggesting => {
            let event = Event::new(
                event_id,
                request.item_id.clone(),
                revision,
                request.control.event_type(),
                request.control.payload(),
                now.to_rfc3339_opts(SecondsFormat::Secs, true),
            )
            .map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if let Err(error) = save_event_in_tx(&tx, &event, revision) {
                return event_failure(error);
            }
            set_pull_only(&tx, &request.item_id, now).map_err(|e| store_failure(&e))?;
        }
    }
    tx.commit().map_err(sqlite_failure)?;
    Ok(LifecycleControlOutcome::Applied {
        revision: revision + 1,
        replayed: false,
    })
}

fn answer(item_id: &str, outcome: &LifecycleControlOutcome) -> Value {
    match outcome {
        LifecycleControlOutcome::Applied { revision, replayed } => json!({
            "outcome": "applied",
            "item_id": item_id,
            "revision": revision,
            "replayed": replayed,
        }),
        LifecycleControlOutcome::StaleRevision { current_revision } => json!({
            "outcome": "stale_revision",
            "current_revision": current_revision,
        }),
        LifecycleControlOutcome::CommandConflict => json!({ "outcome": "command_conflict" }),
        LifecycleControlOutcome::ItemDeleted => json!({ "outcome": "item_deleted" }),
        LifecycleControlOutcome::ItemEnded { lifecycle } => json!({
            "outcome": "item_ended",
            "lifecycle": lifecycle,
        }),
    }
}

fn control_job(request: ItemLifecycleControl, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let outcome = apply_item_lifecycle_control(&mut database, &request)?;
        let mut body = answer(&request.item_id, &outcome);
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one lifecycle control command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields, empty identifiers and out-of-range
/// revisions are refused as `invalid_request` before anything is queued). The outcome event for
/// `operation_id` is the JSON documented in the module documentation, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_item_lifecycle_control(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        if request_len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if request_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if request.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let bytes = std::slice::from_raw_parts(request, request_len);
        let command: Command =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let control = command.into_control()?;
        core.submit(operation_id, control_job(control, operation_id))
    })
}

#[cfg(test)]
mod tests;
