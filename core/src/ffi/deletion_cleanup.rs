//! Per-item deletion cleanup queue over the core handle (L02).
//!
//! `ohand_core_start_deletion_cleanup` exposes exactly what the native cleanup service needs from
//! the L01 deletion queue (`lifecycle::delete_intent`) and the N03 reminder operations, as one
//! tagged JSON command per call. Each command runs on the handle's worker thread and its outcome
//! event for `operation_id` is `{"operation_id", ...}`:
//!
//! * `pending_items` -> `item_ids`: deleted items that still have a pending cleanup row.
//! * `pending_work` (`item_id`) -> `work`: `{work_id, item_id, work_type}` rows, `work_type` being
//!   `remove_audio`, `clear_ingress` or `cancel_notifications`.
//! * `cleanup_target` (`item_id`) -> `target`: `{capture_id, audio_reference}` or null. The audio
//!   reference stays on the tombstoned capture until `remove_audio` is completed.
//! * `reminder_notifications` (`item_id`) -> `notification_ids`: every native identifier,
//!   from any generation, the item's reminder may have installed.
//! * `record_notification_cancelled` (`notification_id`) -> `recorded`: records the confirmed OS
//!   cancel against core's cancel operation; false when core issued no such operation.
//! * `complete_work` / `fail_work` (`work_id`): record the row as completed or terminally failed.
//!   Completing the notification row is refused while a cancel is unrecorded.
//!
//! Reads change nothing. Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::lifecycle::delete_intent::{
    cleanup_target, list_pending_deletion_work, mark_deletion_work_completed,
    mark_deletion_work_failed, DeletionWorkStatus,
};
use crate::providers::contracts::ErrorClass;
use crate::reminders::reconcile::{apply_completion, EffectCompletion, EffectOutcome};
use crate::reminders::state::{get_reminder, list_operations, OperationType};
use crate::store::schema::{Database, SystemClock};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    PendingItems {},
    PendingWork { item_id: String },
    CleanupTarget { item_id: String },
    ReminderNotifications { item_id: String },
    RecordNotificationCancelled { notification_id: String },
    CompleteWork { work_id: String },
    FailWork { work_id: String },
}

impl Command {
    fn validate(&self) -> Result<(), AbiFailure> {
        let identifier = match self {
            Command::PendingItems {} => return Ok(()),
            Command::PendingWork { item_id }
            | Command::CleanupTarget { item_id }
            | Command::ReminderNotifications { item_id } => item_id,
            Command::RecordNotificationCancelled { notification_id } => notification_id,
            Command::CompleteWork { work_id } | Command::FailWork { work_id } => work_id,
        };
        if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_BYTES {
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

/// A queue operation core refused (unknown row, unrecorded notification cancel, terminal state).
const fn refused() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Permanent,
        code: "deletion_work_refused",
        message: Cow::Borrowed("the deletion work row cannot be recorded in its current state"),
    }
}

/// Maps a queue error: a typed SQLite failure keeps its transient/permanent meaning, anything
/// else is core refusing the transition.
fn queue_failure(error: &anyhow::Error) -> AbiFailure {
    let mapped = store_failure(error);
    if mapped == AbiFailure::STORAGE_ERROR {
        refused()
    } else {
        mapped
    }
}

fn pending_items(database: &Database) -> Result<Value, AbiFailure> {
    let mut statement = database
        .conn()
        .prepare("SELECT DISTINCT item_id FROM deletion_work WHERE status = ? ORDER BY item_id")
        .map_err(sqlite_failure)?;
    let item_ids = statement
        .query_map([DeletionWorkStatus::Pending.as_str()], |row| {
            row.get::<_, String>(0)
        })
        .map_err(sqlite_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_failure)?;
    Ok(json!({ "item_ids": item_ids }))
}

fn pending_work(database: &Database, item_id: &str) -> Result<Value, AbiFailure> {
    let work = list_pending_deletion_work(database, item_id).map_err(|e| store_failure(&e))?;
    let rows: Vec<Value> = work
        .iter()
        .map(|row| {
            json!({
                "work_id": row.deletion_work_id,
                "item_id": row.item_id,
                "work_type": row.work_type.as_str(),
            })
        })
        .collect();
    Ok(json!({ "work": rows }))
}

fn target(database: &Database, item_id: &str) -> Result<Value, AbiFailure> {
    let target = cleanup_target(database, item_id).map_err(|e| store_failure(&e))?;
    Ok(json!({
        "target": target.map(|target| json!({
            "capture_id": target.capture_id,
            "audio_reference": target.audio_reference,
        })),
    }))
}

fn reminder_notifications(database: &mut Database, item_id: &str) -> Result<Value, AbiFailure> {
    let tx = database.transaction().map_err(|e| store_failure(&e))?;
    let mut notification_ids: Vec<String> = Vec::new();
    let reminder = get_reminder(&tx, item_id).map_err(|_| AbiFailure::STORAGE_ERROR)?;
    if let Some(reminder) = reminder {
        let operations =
            list_operations(&tx, &reminder.reminder_id).map_err(|_| AbiFailure::STORAGE_ERROR)?;
        let current = reminder.notification_id();
        for notification_id in operations
            .into_iter()
            .map(|operation| operation.notification_id)
            .chain(current)
        {
            if !notification_ids.contains(&notification_id) {
                notification_ids.push(notification_id);
            }
        }
    }
    Ok(json!({ "notification_ids": notification_ids }))
}

fn record_notification_cancelled(
    database: &mut Database,
    notification_id: &str,
) -> Result<Value, AbiFailure> {
    let Some((reminder_id, _generation)) = notification_id.rsplit_once('#') else {
        return Err(AbiFailure::INVALID_REQUEST);
    };
    if reminder_id.is_empty() {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    let effect_identity = format!("{}:{}", OperationType::Cancel.as_str(), notification_id);
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    let issued = list_operations(&tx, reminder_id)
        .map_err(|_| AbiFailure::STORAGE_ERROR)?
        .iter()
        .any(|operation| operation.effect_identity == effect_identity);
    if !issued {
        return Ok(json!({ "recorded": false }));
    }
    apply_completion(
        &tx,
        &SystemClock,
        &EffectCompletion {
            reminder_id: reminder_id.to_string(),
            effect_identity,
            outcome: EffectOutcome::Succeeded,
        },
    )
    .map_err(|_| AbiFailure::STORAGE_ERROR)?;
    tx.commit().map_err(sqlite_failure)?;
    Ok(json!({ "recorded": true }))
}

fn run(database: &mut Database, command: &Command) -> Result<Value, AbiFailure> {
    match command {
        Command::PendingItems {} => pending_items(database),
        Command::PendingWork { item_id } => pending_work(database, item_id),
        Command::CleanupTarget { item_id } => target(database, item_id),
        Command::ReminderNotifications { item_id } => reminder_notifications(database, item_id),
        Command::RecordNotificationCancelled { notification_id } => {
            record_notification_cancelled(database, notification_id)
        }
        Command::CompleteWork { work_id } => {
            mark_deletion_work_completed(database, work_id, Utc::now())
                .map_err(|e| queue_failure(&e))?;
            Ok(json!({}))
        }
        Command::FailWork { work_id } => {
            mark_deletion_work_failed(database, work_id, Utc::now())
                .map_err(|e| queue_failure(&e))?;
            Ok(json!({}))
        }
    }
}

fn cleanup_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one deletion-cleanup command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields are refused as `invalid_request`
/// before anything is queued). The outcome event for `operation_id` is the JSON documented in
/// the module documentation, or a normalized failure. `deletion_work_refused` means core will not
/// record that transition now (for example completing the notification row while a cancel is
/// unrecorded); the row stays pending.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_deletion_cleanup(
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
        core.submit(operation_id, cleanup_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
