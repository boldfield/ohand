//! The user's corrections of an item's text, type and session topic over the core handle (A02).
//!
//! `ohand_core_start_item_correction` carries one tagged JSON command, runs it on the handle's
//! worker thread in one transaction, and answers `{"operation_id", "outcome", ...}`:
//!
//! * `correct_text` (`text`), `correct_type` (`item_type`: `broad_intention` | `note` | `idea` |
//!   `action`) and `correct_session_topic` (`session_topic`). Every command also carries
//!   `command_id` (the immutable identity of the user action, reused unchanged by each retry),
//!   `item_id` and `expected_revision` (the revision the user saw).
//!
//! Each correction is a correction event saved through `save_event_in_tx`, the one event path:
//! it compares the expected revision with the item's, stores the event and the correction
//! record, makes type and topic the item's effective values, re-syncs the search index for text,
//! and bumps the revision. The captured source is never touched. The replaced value the event
//! states is read inside the same transaction, so the caller never supplies it.
//!
//! Outcomes:
//! * `corrected` (`item_id`, `revision` the item now has, `replayed`, `reminder_cancelled`).
//! * `stale_revision` (`current_revision`): the item moved on since the user saw it. Nothing was
//!   written. The failure shape of the boundary cannot carry the current revision and the
//!   caller needs it to refresh, so, like every other "nothing changed" outcome of this
//!   boundary, it is an answer.
//! * `command_conflict`: `command_id` was already used for a different correction.
//! * `item_deleted`: the item was deleted. Unknown items fail as `not_found`.
//!
//! A repeated `command_id` answers `corrected` again with the revision, and reminder effect, the
//! first call produced and `replayed` true; it writes nothing and ignores `expected_revision`,
//! which the first call already consumed.
//!
//! Correcting the type of an item away from `action` cancels its live reminder in the same
//! transaction through the reminder state path (`cancel_reminder`), because DESIGN.md and
//! `docs/architecture/m1-contracts.md` ("reclassifying an item cancels pending notifications")
//! allow only an explicit action to carry an obligation. The core is the one place that knows
//! both facts, so the choice is made here rather than left to a native caller to remember.
//!
//! Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::reminders::state::{
    cancel_reminder, get_reminder, ReminderCancellation, ReminderStateError, RequestState,
};
use crate::store::events::{
    get_event, get_item_snapshot, Correction, CorrectionKind, Event, EventError, EventPayload,
    EventType, ItemType,
};
use crate::store::schema::{Database, SystemClock};
use chrono::{SecondsFormat, Utc};
use rusqlite::{OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;
/// Longest session topic accepted, in bytes.
const MAX_SESSION_TOPIC_BYTES: usize = 1024;

/// What the user changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrectionChange {
    Text(String),
    Type(ItemType),
    SessionTopic(String),
}

impl CorrectionChange {
    fn kind(&self) -> CorrectionKind {
        match self {
            CorrectionChange::Text(_) => CorrectionKind::Text,
            CorrectionChange::Type(_) => CorrectionKind::Type,
            CorrectionChange::SessionTopic(_) => CorrectionKind::SessionTopic,
        }
    }

    fn new_value(&self) -> String {
        match self {
            CorrectionChange::Text(text) => text.clone(),
            CorrectionChange::Type(item_type) => item_type.as_str().to_string(),
            CorrectionChange::SessionTopic(topic) => topic.clone(),
        }
    }
}

/// One user correction, compare-and-set on the item's revision and idempotent by `command_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCorrection {
    pub command_id: String,
    pub item_id: String,
    pub expected_revision: i32,
    pub change: CorrectionChange,
}

/// What a correction did. Every variant except `Corrected` means nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrectionOutcome {
    Corrected {
        /// The item's revision after the correction.
        revision: i32,
        replayed: bool,
        reminder_cancelled: bool,
    },
    StaleRevision {
        current_revision: i32,
    },
    CommandConflict,
    ItemDeleted,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
enum Command {
    #[serde(rename = "correct_text")]
    Text {
        command_id: String,
        item_id: String,
        expected_revision: i64,
        text: String,
    },
    #[serde(rename = "correct_type")]
    Type {
        command_id: String,
        item_id: String,
        expected_revision: i64,
        item_type: String,
    },
    #[serde(rename = "correct_session_topic")]
    SessionTopic {
        command_id: String,
        item_id: String,
        expected_revision: i64,
        session_topic: String,
    },
}

impl Command {
    /// Refuse malformed commands before anything is queued.
    fn into_correction(self) -> Result<ItemCorrection, AbiFailure> {
        let (command_id, item_id, expected_revision, change) = match self {
            Command::Text {
                command_id,
                item_id,
                expected_revision,
                text,
            } => {
                if text.trim().is_empty() {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                (
                    command_id,
                    item_id,
                    expected_revision,
                    CorrectionChange::Text(text),
                )
            }
            Command::Type {
                command_id,
                item_id,
                expected_revision,
                item_type,
            } => {
                let item_type =
                    ItemType::from_str(&item_type).map_err(|_| AbiFailure::INVALID_REQUEST)?;
                (
                    command_id,
                    item_id,
                    expected_revision,
                    CorrectionChange::Type(item_type),
                )
            }
            Command::SessionTopic {
                command_id,
                item_id,
                expected_revision,
                session_topic,
            } => {
                if session_topic.trim().is_empty() || session_topic.len() > MAX_SESSION_TOPIC_BYTES
                {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                (
                    command_id,
                    item_id,
                    expected_revision,
                    CorrectionChange::SessionTopic(session_topic),
                )
            }
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
        Ok(ItemCorrection {
            command_id,
            item_id,
            expected_revision,
            change,
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
    format!("user-correction:{command_id}")
}

fn reminder_command_id_for(command_id: &str) -> String {
    format!("user-correction:{command_id}:cancel-reminder")
}

/// The value a correction of this kind replaces, exactly as the event path compares it: type is
/// the item's column, topic falls back to the one stated at capture, and text is the latest text
/// correction, else the attached transcript, else the capture text.
fn replaced_value(
    tx: &Transaction<'_>,
    item_id: &str,
    kind: &CorrectionKind,
) -> Result<Option<String>, AbiFailure> {
    let sql = match kind {
        CorrectionKind::Type => "SELECT item_type FROM items WHERE item_id = ?1",
        CorrectionKind::SessionTopic => {
            "SELECT COALESCE(i.current_session_topic, c.session_topic) FROM items i
             JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?1"
        }
        CorrectionKind::Text => {
            "SELECT COALESCE(
                 (SELECT new_value FROM corrections
                  WHERE item_id = ?1 AND kind = 'text' ORDER BY revision DESC LIMIT 1),
                 (SELECT text FROM transcript_attachments WHERE item_id = ?1),
                 c.text)
             FROM items i JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?1"
        }
        CorrectionKind::Scope => return Err(AbiFailure::INTERNAL),
    };
    let value: Option<Option<String>> = tx
        .query_row(sql, [item_id], |row| row.get(0))
        .optional()
        .map_err(sqlite_failure)?;
    Ok(value.flatten())
}

fn event_failure(error: EventError) -> Result<CorrectionOutcome, AbiFailure> {
    match error {
        EventError::StaleRevision { current, .. } => Ok(CorrectionOutcome::StaleRevision {
            current_revision: current.revision,
        }),
        EventError::Conflict { .. } => Ok(CorrectionOutcome::CommandConflict),
        EventError::NotAllowedInState { .. } => Ok(CorrectionOutcome::ItemDeleted),
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

/// The outcome the first call with this `command_id` produced, if there was one.
fn replay(
    tx: &Transaction<'_>,
    correction: &ItemCorrection,
) -> Result<Option<CorrectionOutcome>, AbiFailure> {
    let Some(stored) =
        get_event(tx, &event_id_for(&correction.command_id)).map_err(|e| store_failure(&e))?
    else {
        return Ok(None);
    };
    let same_correction = stored.item_id == correction.item_id
        && matches!(
            &stored.payload,
            EventPayload::Correction(recorded)
                if recorded.kind == correction.change.kind()
                    && recorded.new_value == correction.change.new_value()
        );
    if !same_correction {
        return Ok(Some(CorrectionOutcome::CommandConflict));
    }
    let reminder_cancelled: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM reminder_commands WHERE command_id = ?)",
            [reminder_command_id_for(&correction.command_id)],
            |row| row.get(0),
        )
        .map_err(sqlite_failure)?;
    Ok(Some(CorrectionOutcome::Corrected {
        revision: stored.revision + 1,
        replayed: true,
        reminder_cancelled,
    }))
}

/// Cancel the item's live reminder because the item can no longer carry one. Returns whether a
/// live reminder was cancelled.
fn cancel_live_reminder(
    tx: &Transaction<'_>,
    correction: &ItemCorrection,
    revision_after_correction: i32,
) -> Result<bool, AbiFailure> {
    let reminder_failure = |error: ReminderStateError| match error {
        ReminderStateError::Database(error) => sqlite_failure(error),
        ReminderStateError::Store(error) => store_failure(&error),
        _ => AbiFailure::STORAGE_ERROR,
    };
    let Some(reminder) = get_reminder(tx, &correction.item_id).map_err(reminder_failure)? else {
        return Ok(false);
    };
    if reminder.request_state == RequestState::Cancelled {
        return Ok(false);
    }
    cancel_reminder(
        tx,
        &SystemClock,
        &ReminderCancellation {
            command_id: reminder_command_id_for(&correction.command_id),
            item_id: correction.item_id.clone(),
            expected_revision: revision_after_correction,
            expected_reminder_version: reminder.state_version,
        },
    )
    .map_err(reminder_failure)?;
    Ok(true)
}

/// Applies one correction in its own immediate transaction. See the module documentation for the
/// outcomes; a `Corrected` result is durable when this returns.
pub fn apply_item_correction(
    database: &mut Database,
    correction: &ItemCorrection,
) -> Result<CorrectionOutcome, AbiFailure> {
    let tx = database
        .immediate_transaction()
        .map_err(|e| store_failure(&e))?;
    if let Some(replayed) = replay(&tx, correction)? {
        return Ok(replayed);
    }
    let snapshot = match get_item_snapshot(&tx, &correction.item_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Err(AbiFailure::NOT_FOUND),
        Err(error) => return event_failure(error),
    };
    if snapshot.lifecycle_state == "deleted" {
        return Ok(CorrectionOutcome::ItemDeleted);
    }
    if snapshot.revision != correction.expected_revision {
        return Ok(CorrectionOutcome::StaleRevision {
            current_revision: snapshot.revision,
        });
    }

    let kind = correction.change.kind();
    let event = Event::new(
        event_id_for(&correction.command_id),
        correction.item_id.clone(),
        correction.expected_revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            old_value: replaced_value(&tx, &correction.item_id, &kind)?,
            kind,
            new_value: correction.change.new_value(),
        }),
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    )
    .map_err(|_| AbiFailure::INVALID_REQUEST)?;
    if let Err(error) =
        crate::store::events::save_event_in_tx(&tx, &event, correction.expected_revision)
    {
        return event_failure(error);
    }

    let revision = correction.expected_revision + 1;
    let leaves_actionable_type = matches!(
        &correction.change,
        CorrectionChange::Type(item_type) if !item_type.can_carry_obligation()
    );
    let reminder_cancelled = leaves_actionable_type
        && snapshot.lifecycle_state == "active"
        && cancel_live_reminder(&tx, correction, revision)?;
    tx.commit().map_err(sqlite_failure)?;
    Ok(CorrectionOutcome::Corrected {
        revision,
        replayed: false,
        reminder_cancelled,
    })
}

fn answer(item_id: &str, outcome: &CorrectionOutcome) -> Value {
    match outcome {
        CorrectionOutcome::Corrected {
            revision,
            replayed,
            reminder_cancelled,
        } => json!({
            "outcome": "corrected",
            "item_id": item_id,
            "revision": revision,
            "replayed": replayed,
            "reminder_cancelled": reminder_cancelled,
        }),
        CorrectionOutcome::StaleRevision { current_revision } => json!({
            "outcome": "stale_revision",
            "current_revision": current_revision,
        }),
        CorrectionOutcome::CommandConflict => json!({ "outcome": "command_conflict" }),
        CorrectionOutcome::ItemDeleted => json!({ "outcome": "item_deleted" }),
    }
}

fn correction_job(correction: ItemCorrection, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let outcome = apply_item_correction(&mut database, &correction)?;
        let mut body = answer(&correction.item_id, &outcome);
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one correction command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields, empty values and out-of-range
/// revisions are refused as `invalid_request` before anything is queued). The outcome event for
/// `operation_id` is the JSON documented in the module documentation, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_item_correction(
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
        let correction = command.into_correction()?;
        core.submit(operation_id, correction_job(correction, operation_id))
    })
}

#[cfg(test)]
mod tests;
