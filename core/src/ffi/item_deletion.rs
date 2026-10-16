//! The user's deletion of one item, and its progress, over the core handle (A05).
//!
//! `ohand_core_start_item_deletion` carries one tagged JSON command, runs it on the handle's
//! worker thread, and answers `{"operation_id", "outcome", ...}`:
//!
//! * `mark_deleted` (`item_id`, `expected_revision`, the revision the user saw) records the
//!   deletion intent through `mark_deletion_intent`: in one transaction it tombstones the item,
//!   erases its readable content and search entry, cancels its jobs and reminders and enqueues the
//!   native cleanup rows. The native effects (notification, ingress and audio removal) are not
//!   done here; the client runs the existing cleanup service once the intent is durable, and the
//!   normal lifecycle pass resumes whatever it leaves pending.
//! * `progress` (`item_id`) reads `deletion_progress` and changes nothing.
//!
//! Outcomes:
//! * `deletion_recorded` (`item_id`, `deletion_revision`, `replayed`, `progress`): the intent is
//!   durable. Repeating the command with the revision it was first applied against answers again
//!   with `replayed` true and writes nothing.
//! * `stale_revision` (`current_revision`): the item moved on since the user saw it, or it was
//!   deleted at another revision. Nothing was written.
//! * `progress` (`item_id`, `progress`).
//!
//! `progress` is `{"state", "pending", "failed"}`: `state` is `not_requested`, `in_progress`,
//! `failed` (everything else finished but at least one effect terminally failed) or `complete`,
//! and the lists name the `remove_audio`, `clear_ingress` and `cancel_notifications` effects
//! still pending or failed. Complete is derived only from durable cleanup rows.
//!
//! Unknown items fail as `not_found`. Failures are normalized and never echo identifiers or
//! content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::lifecycle::delete_intent::{
    deletion_progress, mark_deletion_intent, DeletionProgress, DeletionWorkType,
};
use crate::store::schema::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

/// What a deletion command did. `StaleRevision` means nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletionOutcome {
    Recorded {
        deletion_revision: i32,
        replayed: bool,
        progress: DeletionProgress,
    },
    StaleRevision {
        current_revision: i32,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    MarkDeleted {
        item_id: String,
        expected_revision: i64,
    },
    Progress {
        item_id: String,
    },
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

fn valid_identifier(identifier: &str) -> Result<(), AbiFailure> {
    if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_BYTES {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    Ok(())
}

/// The item's lifecycle state and revision, or `None` for an unknown item.
fn stored_state(database: &Database, item_id: &str) -> Result<Option<(String, i32)>, AbiFailure> {
    database
        .conn()
        .query_row(
            "SELECT lifecycle_state, revision FROM items WHERE item_id = ?",
            [item_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?)),
        )
        .optional()
        .map_err(sqlite_failure)
}

/// The revision a deleted item's tombstone event was applied against.
fn deletion_revision(database: &Database, item_id: &str) -> Result<Option<i32>, AbiFailure> {
    database
        .conn()
        .query_row(
            "SELECT revision FROM events WHERE item_id = ? AND event_type = 'deletion'",
            [item_id],
            |row| row.get::<_, i32>(0),
        )
        .optional()
        .map_err(sqlite_failure)
}

/// Records the deletion intent for `item_id` against `expected_revision`, idempotently. The
/// caller must hold the database for the whole call so the checks and the write are one step.
pub fn record_item_deletion(
    database: &mut Database,
    item_id: &str,
    expected_revision: i32,
) -> Result<DeletionOutcome, AbiFailure> {
    let Some((lifecycle_state, revision)) = stored_state(database, item_id)? else {
        return Err(AbiFailure::NOT_FOUND);
    };
    let replayed = lifecycle_state == "deleted";
    let applied_against = if replayed {
        deletion_revision(database, item_id)?
    } else {
        Some(revision)
    };
    if applied_against != Some(expected_revision) {
        return Ok(DeletionOutcome::StaleRevision {
            current_revision: revision,
        });
    }
    let intent = mark_deletion_intent(database, item_id, expected_revision, Utc::now())
        .map_err(|error| store_failure(&error))?;
    let progress = deletion_progress(database, item_id).map_err(|error| store_failure(&error))?;
    Ok(DeletionOutcome::Recorded {
        deletion_revision: intent.deletion_revision,
        replayed,
        progress,
    })
}

/// How far the deletion of `item_id` has progressed. Unknown items are `not_found`.
pub fn read_item_deletion_progress(
    database: &mut Database,
    item_id: &str,
) -> Result<DeletionProgress, AbiFailure> {
    if stored_state(database, item_id)?.is_none() {
        return Err(AbiFailure::NOT_FOUND);
    }
    deletion_progress(database, item_id).map_err(|error| store_failure(&error))
}

fn work_types(types: &[DeletionWorkType]) -> Vec<&'static str> {
    types.iter().map(DeletionWorkType::as_str).collect()
}

fn progress_body(progress: &DeletionProgress) -> Value {
    match progress {
        DeletionProgress::NotRequested => {
            json!({ "state": "not_requested", "pending": [], "failed": [] })
        }
        DeletionProgress::InProgress { pending, failed } => json!({
            "state": "in_progress",
            "pending": work_types(pending),
            "failed": work_types(failed),
        }),
        DeletionProgress::Failed { failed } => json!({
            "state": "failed",
            "pending": [],
            "failed": work_types(failed),
        }),
        DeletionProgress::Complete => json!({ "state": "complete", "pending": [], "failed": [] }),
    }
}

fn recorded_body(item_id: &str, outcome: &DeletionOutcome) -> Value {
    match outcome {
        DeletionOutcome::Recorded {
            deletion_revision,
            replayed,
            progress,
        } => json!({
            "outcome": "deletion_recorded",
            "item_id": item_id,
            "deletion_revision": deletion_revision,
            "replayed": replayed,
            "progress": progress_body(progress),
        }),
        DeletionOutcome::StaleRevision { current_revision } => json!({
            "outcome": "stale_revision",
            "current_revision": current_revision,
        }),
    }
}

fn finish(mut body: Value, operation_id: u64) -> Result<Vec<u8>, AbiFailure> {
    if let Value::Object(fields) = &mut body {
        fields.insert("operation_id".to_string(), json!(operation_id));
    }
    serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
}

fn deletion_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let body = match command {
            Command::MarkDeleted {
                item_id,
                expected_revision,
            } => {
                let expected_revision =
                    i32::try_from(expected_revision).map_err(|_| AbiFailure::INVALID_REQUEST)?;
                let outcome = record_item_deletion(&mut database, &item_id, expected_revision)?;
                recorded_body(&item_id, &outcome)
            }
            Command::Progress { item_id } => {
                let progress = read_item_deletion_progress(&mut database, &item_id)?;
                json!({
                    "outcome": "progress",
                    "item_id": item_id,
                    "progress": progress_body(&progress),
                })
            }
        };
        finish(body, operation_id)
    })
}

impl Command {
    /// Refuse malformed commands before anything is queued.
    fn validate(&self) -> Result<(), AbiFailure> {
        match self {
            Command::MarkDeleted {
                item_id,
                expected_revision,
            } => {
                valid_identifier(item_id)?;
                let in_range = i32::try_from(*expected_revision).is_ok_and(|value| value >= 0);
                if !in_range {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                Ok(())
            }
            Command::Progress { item_id } => valid_identifier(item_id),
        }
    }
}

/// Queues one deletion command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields, empty identifiers and out-of-range
/// revisions are refused as `invalid_request` before anything is queued). The outcome event for
/// `operation_id` is the JSON documented in the module documentation, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_item_deletion(
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
        core.submit(operation_id, deletion_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
