//! Notification taps and actions over the core handle (N07).
//!
//! `ohand_core_start_notification_action` carries what the user, or the OS on their behalf, did
//! with a delivered reminder into the existing revisioned records. Each call is one tagged JSON
//! command, runs on the handle's worker thread in one transaction, and answers
//! `{"operation_id", "outcome", ...}`. `notification_id` is the core-derived
//! `reminder_id#generation`; an optional `item_id` is the opaque target the notification carried
//! and must agree with the item the reminder belongs to.
//!
//! * `record_evidence` (`kind` `delivered` | `opened`, `observed_at_unix_ms`): store OS evidence.
//! * `acknowledge`: store the user's explicit acknowledgment. Does not touch the item.
//! * `complete`: record a completion event at the item's current revision and cancel its
//!   reminder in the same transaction. Acknowledgment is a different command and a different
//!   fact; completing never acknowledges and acknowledging never completes.
//! * `resolve`: name the item a tap opens. It reads stored state, so the native side asks only
//!   while the session may read private content.
//!
//! Outcomes that mean "nothing was changed" are answers, not failures: `unknown_notification`,
//! `not_installed`, `item_mismatch`, `item_deleted`, `future_observation`, `stale_notification`
//! (a `complete` for an older schedule generation than the reminder's current one) and
//! `already_ended` (the item was completed or cancelled before). A repeated callback therefore
//! answers `already_recorded` or `already_ended` and mutates nothing. Failures are normalized
//! and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::reminders::history::{
    acknowledge_reminder, record_delivery_evidence, DeliveryEvidence, DeliveryEvidenceKind,
    HistoryError, RecordOutcome, UserAcknowledgment,
};
use crate::reminders::state::{end_item_with_event, notification_identifier, OperationState};
use crate::store::events::{get_item_snapshot, Event, EventError, EventPayload, EventType};
use crate::store::schema::{Database, SystemClock};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use rusqlite::{OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceKind {
    Delivered,
    Opened,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    RecordEvidence {
        notification_id: String,
        kind: EvidenceKind,
        observed_at_unix_ms: i64,
        item_id: Option<String>,
    },
    Acknowledge {
        notification_id: String,
        item_id: Option<String>,
    },
    Complete {
        notification_id: String,
        item_id: Option<String>,
    },
    Resolve {
        notification_id: String,
    },
}

impl Command {
    fn validate(&self) -> Result<(), AbiFailure> {
        let (notification_id, item_id) = match self {
            Command::RecordEvidence {
                notification_id,
                item_id,
                ..
            }
            | Command::Acknowledge {
                notification_id,
                item_id,
            }
            | Command::Complete {
                notification_id,
                item_id,
            } => (notification_id, item_id.as_ref()),
            Command::Resolve { notification_id } => (notification_id, None),
        };
        let acceptable = |identifier: &String| {
            !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES
        };
        if !acceptable(notification_id) || item_id.is_some_and(|id| !acceptable(id)) {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(())
    }
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

/// The reminder and item a notification identifier names.
struct Located {
    reminder_id: String,
    item_id: String,
    generation: i64,
    current_generation: i64,
    lifecycle_state: String,
}

fn locate(tx: &Transaction<'_>, notification_id: &str) -> Result<Option<Located>, AbiFailure> {
    let Some((reminder_id, generation)) =
        notification_id
            .rsplit_once('#')
            .and_then(|(reminder_id, generation)| {
                let generation: i64 = generation.parse().ok()?;
                (!reminder_id.is_empty() && generation > 0).then_some((reminder_id, generation))
            })
    else {
        return Ok(None);
    };
    if notification_identifier(reminder_id, generation) != notification_id {
        return Ok(None);
    }
    tx.query_row(
        "SELECT reminders.item_id, reminders.schedule_generation, items.lifecycle_state
         FROM reminders JOIN items ON items.item_id = reminders.item_id
         WHERE reminders.reminder_id = ?",
        [reminder_id],
        |row| {
            Ok(Located {
                reminder_id: reminder_id.to_string(),
                item_id: row.get(0)?,
                generation,
                current_generation: row.get(1)?,
                lifecycle_state: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_failure)
}

/// The state of the schedule operation core recorded for this exact request, if it recorded one.
fn schedule_state(
    tx: &Transaction<'_>,
    located: &Located,
    notification_id: &str,
) -> Result<Option<OperationState>, AbiFailure> {
    let state: Option<String> = tx
        .query_row(
            "SELECT operation_state FROM reminder_operations
             WHERE reminder_id = ? AND effect_identity = ?",
            rusqlite::params![located.reminder_id, format!("schedule:{notification_id}")],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_failure)?;
    state
        .map(|state| state.parse().map_err(|_| AbiFailure::STORAGE_ERROR))
        .transpose()
}

/// Whether core attempted to hand the request to the OS, the only case in which the OS can hold a
/// notification that returns an action.
fn attempted(state: OperationState) -> bool {
    matches!(state, OperationState::Failed | OperationState::Acknowledged)
}

fn answer(outcome: &str) -> Value {
    json!({ "outcome": outcome })
}

fn recorded(outcome: Result<RecordOutcome, HistoryError>) -> Result<Value, AbiFailure> {
    match outcome {
        Ok(RecordOutcome::Recorded {
            current_generation: true,
        }) => Ok(answer("recorded")),
        Ok(RecordOutcome::Recorded {
            current_generation: false,
        }) => Ok(answer("recorded_superseded")),
        Ok(RecordOutcome::AlreadyRecorded) => Ok(answer("already_recorded")),
        Err(HistoryError::UnknownNotification(_)) => Ok(answer("unknown_notification")),
        Err(HistoryError::NotInstalled(_)) => Ok(answer("not_installed")),
        Err(HistoryError::ItemMismatch { .. }) => Ok(answer("item_mismatch")),
        Err(HistoryError::ItemDeleted(_)) => Ok(answer("item_deleted")),
        Err(HistoryError::FutureObservation(_)) => Ok(answer("future_observation")),
        Err(HistoryError::Database(error)) => Err(sqlite_failure(error)),
        Err(_) => Err(AbiFailure::STORAGE_ERROR),
    }
}

fn mismatch(located: &Located, item_id: &Option<String>) -> bool {
    item_id
        .as_ref()
        .is_some_and(|stated| *stated != located.item_id)
}

fn record_evidence(
    database: &mut Database,
    notification_id: &str,
    kind: &EvidenceKind,
    observed_at_unix_ms: i64,
    item_id: &Option<String>,
) -> Result<Value, AbiFailure> {
    let observed_at: DateTime<Utc> = Utc
        .timestamp_millis_opt(observed_at_unix_ms)
        .single()
        .ok_or(AbiFailure::INVALID_REQUEST)?;
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    if let Some(located) = locate(&tx, notification_id)? {
        if mismatch(&located, item_id) {
            return Ok(answer("item_mismatch"));
        }
    }
    let outcome = record_delivery_evidence(
        &tx,
        &SystemClock,
        &DeliveryEvidence {
            notification_id: notification_id.to_string(),
            kind: match kind {
                EvidenceKind::Delivered => DeliveryEvidenceKind::Delivered,
                EvidenceKind::Opened => DeliveryEvidenceKind::Opened,
            },
            observed_at,
        },
    );
    let body = recorded(outcome)?;
    tx.commit().map_err(sqlite_failure)?;
    Ok(body)
}

fn acknowledge(
    database: &mut Database,
    notification_id: &str,
    item_id: &Option<String>,
) -> Result<Value, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    let Some(located) = locate(&tx, notification_id)? else {
        return Ok(answer("unknown_notification"));
    };
    if mismatch(&located, item_id) {
        return Ok(answer("item_mismatch"));
    }
    let outcome = acknowledge_reminder(
        &tx,
        &SystemClock,
        &UserAcknowledgment {
            item_id: located.item_id,
            notification_id: notification_id.to_string(),
        },
    );
    let body = recorded(outcome)?;
    tx.commit().map_err(sqlite_failure)?;
    Ok(body)
}

fn complete(
    database: &mut Database,
    notification_id: &str,
    item_id: &Option<String>,
) -> Result<Value, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    let Some(located) = locate(&tx, notification_id)? else {
        return Ok(answer("unknown_notification"));
    };
    if mismatch(&located, item_id) {
        return Ok(answer("item_mismatch"));
    }
    match located.lifecycle_state.as_str() {
        "deleted" => return Ok(answer("item_deleted")),
        "completed" | "cancelled" => return Ok(answer("already_ended")),
        _ => {}
    }
    let Some(state) = schedule_state(&tx, &located, notification_id)? else {
        return Ok(answer("unknown_notification"));
    };
    if located.generation != located.current_generation {
        return Ok(answer("stale_notification"));
    }
    if !attempted(state) {
        return Ok(answer("not_installed"));
    }
    let snapshot = match get_item_snapshot(&tx, &located.item_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(answer("unknown_notification")),
        Err(EventError::Storage(error)) => return Err(sqlite_failure(error)),
        Err(_) => return Err(AbiFailure::STORAGE_ERROR),
    };
    let event = Event::new(
        format!("notification-complete:{notification_id}"),
        located.item_id.clone(),
        snapshot.revision,
        EventType::Completion,
        EventPayload::Completion,
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    )
    .map_err(|_| AbiFailure::INTERNAL)?;
    match end_item_with_event(&tx, &SystemClock, &event, snapshot.revision) {
        Ok(_) => {}
        Err(crate::reminders::state::ReminderStateError::Event(
            EventError::NotAllowedInState { .. },
        )) => return Ok(answer("already_ended")),
        Err(crate::reminders::state::ReminderStateError::Database(error)) => {
            return Err(sqlite_failure(error))
        }
        Err(_) => return Err(AbiFailure::STORAGE_ERROR),
    }
    tx.commit().map_err(sqlite_failure)?;
    Ok(answer("completed"))
}

fn resolve(database: &mut Database, notification_id: &str) -> Result<Value, AbiFailure> {
    let tx = database.transaction().map_err(|e| store_failure(&e))?;
    let Some(located) = locate(&tx, notification_id)? else {
        return Ok(answer("unknown_notification"));
    };
    if located.lifecycle_state == "deleted" {
        return Ok(answer("item_deleted"));
    }
    Ok(json!({
        "outcome": "item",
        "item_id": located.item_id,
        "lifecycle": located.lifecycle_state,
        "current_generation": located.generation == located.current_generation,
    }))
}

fn run(database: &mut Database, command: &Command) -> Result<Value, AbiFailure> {
    match command {
        Command::RecordEvidence {
            notification_id,
            kind,
            observed_at_unix_ms,
            item_id,
        } => record_evidence(
            database,
            notification_id,
            kind,
            *observed_at_unix_ms,
            item_id,
        ),
        Command::Acknowledge {
            notification_id,
            item_id,
        } => acknowledge(database, notification_id, item_id),
        Command::Complete {
            notification_id,
            item_id,
        } => complete(database, notification_id, item_id),
        Command::Resolve { notification_id } => resolve(database, notification_id),
    }
}

fn action_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one notification action command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields are refused as `invalid_request`
/// before anything is queued). The outcome event for `operation_id` is the JSON documented in the
/// module documentation, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_notification_action(
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
        command.validate()?;
        core.submit(operation_id, action_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
