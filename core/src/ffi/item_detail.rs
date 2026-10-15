//! One item's detail over the core handle (A01).
//!
//! `ohand_core_start_item_detail` queues one read on the handle's worker thread. The request is
//! one JSON object with no unknown fields:
//!
//! * `item_id`: the opaque item identifier, non-empty and at most
//!   [`OHAND_CORE_MAX_ITEM_DETAIL_IDENTIFIER_BYTES`] bytes. Nothing is parsed out of it.
//! * `read_scopes`: the scopes the caller is authorized to read, a non-empty list of `personal`
//!   and/or `work`. There is no default: an omitted or empty list is `invalid_request`. Native
//!   authenticates the session and passes only the scopes that session may read.
//!
//! The outcome event is the item as `load_item_state` resolves it, read in one transaction:
//! `{"operation_id", "item_id", "capture_id", "revision", "item_type", "scope", "session_topic",
//! "lifecycle_state", "captured_at", "text", "text_basis", "original_text", "corrected_text",
//! "audio_reference", "transcript", "history", "history_truncated"}`.
//!
//! * `text` is the current text to show (`null` for an audio capture with no transcript yet) and
//!   `text_basis` says where it comes from: `original` (typed at capture), `transcribed`
//!   (recognized on the device from the capture's audio) or `corrected` (the user's latest text
//!   correction). `original_text` is the capture's own text, `null` for an audio capture.
//!   `corrected_text` is the user's correction and is set only when `text_basis` is `corrected`.
//! * `item_type` and `session_topic` are `null` when unset. `lifecycle_state` is `active`,
//!   `completed` or `cancelled`. `captured_at` is the capture instant as stored (RFC 3339).
//! * `audio_reference` is the capture's original audio reference or `null`. `transcript` is the
//!   provenance of the attached transcript or `null`: `{"transcript_record_id", "source_job_id",
//!   "text", "recognizer", "detected_language", "confidence", "transcribed_at",
//!   "attached_revision", "audio_sha256"}`. `audio_sha256` binds the transcript to the exact
//!   original audio the recognizer read and `source_job_id` names the transcription job.
//! * `history` lists the item's events newest first, at most
//!   [`OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT`] of them, and `history_truncated` says whether older
//!   ones were left out. Each entry is `{"event_id", "revision", "event_type", "happened_at",
//!   "correction", "suggestion_control"}`: `revision` is the item revision the event was applied
//!   to, `correction` is `{"kind", "old_value", "new_value"}` for a correction event, and
//!   `suggestion_control` is `not_now` or `stop_suggesting` for that event type; both are `null`
//!   otherwise.
//!
//! An unknown item, a deleted item and an item outside `read_scopes` all fail as the same fixed
//! `not_found`, so a caller cannot tell them apart. Failures are normalized and never echo
//! identifiers or stored content. The detail stays on the device: this module sends nothing
//! anywhere.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::domain::items::{load_item_state, LifecycleState, TextState};
use crate::ingress::transcription::transcript_provenance;
use crate::store::events::{get_events_for_item, Event, EventPayload, ItemScope};
use crate::store::schema::Database;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

/// Longest item identifier accepted, in bytes.
pub const OHAND_CORE_MAX_ITEM_DETAIL_IDENTIFIER_BYTES: usize = 512;
/// Most events one detail carries; older events are left out and `history_truncated` is true.
pub const OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    item_id: String,
    #[serde(default)]
    read_scopes: Vec<String>,
}

struct Validated {
    item_id: String,
    read_scopes: Vec<ItemScope>,
}

impl Command {
    fn validate(self) -> Result<Validated, AbiFailure> {
        if self.item_id.is_empty()
            || self.item_id.len() > OHAND_CORE_MAX_ITEM_DETAIL_IDENTIFIER_BYTES
        {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let mut read_scopes: Vec<ItemScope> = Vec::new();
        for scope in &self.read_scopes {
            let scope = ItemScope::from_str(scope).map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if !read_scopes.contains(&scope) {
                read_scopes.push(scope);
            }
        }
        if read_scopes.is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(Validated {
            item_id: self.item_id,
            read_scopes,
        })
    }
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

fn event_json(event: &Event) -> Value {
    let (correction, suggestion_control) = match &event.payload {
        EventPayload::Correction(correction) => (
            json!({
                "kind": correction.kind.as_str(),
                "old_value": correction.old_value,
                "new_value": correction.new_value,
            }),
            Value::Null,
        ),
        EventPayload::SuggestionControl(control) => (Value::Null, json!(control.kind.as_str())),
        _ => (Value::Null, Value::Null),
    };
    json!({
        "event_id": event.event_id,
        "revision": event.revision,
        "event_type": event.event_type.as_str(),
        "happened_at": event.happened_at,
        "correction": correction,
        "suggestion_control": suggestion_control,
    })
}

/// The detail of one item the caller may read, or `not_found`.
fn read_item_detail(database: &mut Database, request: &Validated) -> Result<Value, AbiFailure> {
    let transaction = database.transaction().map_err(|e| store_failure(&e))?;
    let state = load_item_state(&transaction, &request.item_id)
        .map_err(|e| store_failure(&e))?
        .filter(|state| {
            state.lifecycle_state != LifecycleState::Deleted
                && request.read_scopes.contains(&state.scope)
        })
        .ok_or(AbiFailure::NOT_FOUND)?;

    let (original_text, audio_reference, captured_at): (Option<String>, Option<String>, String) =
        transaction
            .query_row(
                "SELECT text, audio_reference, capture_instant FROM captures WHERE capture_id = ?",
                [&state.capture_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_failure)?
            .ok_or(AbiFailure::STORAGE_ERROR)?;

    let (text_basis, corrected_text) = match &state.current_text {
        TextState::Original { .. } => ("original", None),
        TextState::Transcribed { .. } => ("transcribed", None),
        TextState::Corrected { text, .. } => ("corrected", Some(text.clone())),
    };

    let transcript = transcript_provenance(&transaction, &request.item_id)
        .map_err(|e| store_failure(&e))?
        .map(|provenance| {
            json!({
                "transcript_record_id": provenance.transcript_record_id,
                "source_job_id": provenance.source_job_id,
                "text": provenance.text,
                "recognizer": provenance.recognizer,
                "detected_language": provenance.detected_language,
                "confidence": provenance.confidence,
                "transcribed_at": provenance.transcribed_at,
                "attached_revision": provenance.attached_revision,
                "audio_sha256": provenance.audio_sha256,
            })
        });

    let events =
        get_events_for_item(&transaction, &request.item_id).map_err(|e| store_failure(&e))?;
    let history_truncated = events.len() > OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT;
    let history: Vec<Value> = events
        .iter()
        .rev()
        .take(OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT)
        .map(event_json)
        .collect();

    Ok(json!({
        "item_id": state.item_id,
        "capture_id": state.capture_id,
        "revision": state.revision,
        "item_type": state.item_type.map(|item_type| item_type.as_str()),
        "scope": state.scope.as_str(),
        "session_topic": state.session_topic,
        "lifecycle_state": state.lifecycle_state.as_str(),
        "captured_at": captured_at,
        "text": state.current_text.text(),
        "text_basis": text_basis,
        "original_text": original_text,
        "corrected_text": corrected_text,
        "audio_reference": audio_reference,
        "transcript": transcript,
        "history": history,
        "history_truncated": history_truncated,
    }))
}

fn item_detail_job(request: Validated, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = read_item_detail(&mut database, &request)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues the read of one item described by the `request_len` JSON bytes at `request` (see the
/// module documentation). A malformed request, including one that names no read scope, is
/// refused as `invalid_request` before anything is queued. Native must have authenticated the
/// session before it calls this and passes only scopes that session may read. The outcome event
/// for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_item_detail(
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
        core.submit(
            operation_id,
            item_detail_job(command.validate()?, operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
