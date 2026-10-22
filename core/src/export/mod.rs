// Versioned export and serialization (L04a).
//
//! Consistent, scoped export of authoritative item state in a portable text format.
//!
//! An export is one read snapshot of the user-requested scopes, taken only after the caller
//! states that the read session is authenticated. It holds source, corrections, metadata and
//! history. It never holds credentials, endpoint configuration, route or provider settings,
//! model proposals, jobs or search indexes, and it never holds deleted items or their
//! tombstones. Reminders appear as history (their recorded states and operations), not as
//! scheduled events that could be re-imported. Export is not a restore feature.
//!
//! # Format (`ohand.core-export`, version 1)
//!
//! UTF-8 JSON Lines: every record is one compact JSON object on its own line, tagged by a
//! `record` field.
//!
//! 1. exactly one `header` record: format name and version, export instant, store schema
//!    version, the exported scopes and the audio mode (`excluded` or `references`);
//! 2. zero or more `item` records, ordered by creation time then item id;
//! 3. exactly one `footer` record holding the item count and the lowercase hex SHA-256 of every
//!    byte before the footer line (header and item lines, newlines included).
//!
//! Output without a footer, or whose footer count or digest disagrees with its content, is
//! incomplete or damaged and must be refused by a reader.
//!
//! Audio is explicit per item through `audio.status`: `none` (the item has no audio), `omitted`
//! (it has audio and the request did not ask for it) or `referenced` with the native audio
//! reference. Core never reads audio files; whether a referenced file still exists is a native
//! concern.
//!
//! # Snapshot consistency
//!
//! All reads happen inside one read transaction, so concurrent writers cannot make one export
//! show one item, correction or reminder at two different moments. Bytes are serialized after
//! the snapshot is read and the read transaction is released.

use crate::domain::items::{load_item_state, LifecycleState, TextState};
use crate::store::captures::get_capture;
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use thiserror::Error;

#[cfg(test)]
mod reader;
#[cfg(test)]
mod tests;

pub const FORMAT_NAME: &str = "ohand.core-export";
pub const FORMAT_VERSION: u32 = 1;

/// The runtime read-authorization state supplied by the native authentication service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAuthentication {
    /// Locked, cancelled, failed or relocked: no stored content may be returned.
    CaptureOnly,
    Authenticated,
}

/// Whether audio references are part of the export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioInclusion {
    /// Items with audio are marked `omitted`; no audio reference or digest is written.
    Excluded,
    /// Items with audio carry their native audio reference and transcript audio digest.
    References,
}

impl AudioInclusion {
    fn as_str(self) -> &'static str {
        match self {
            AudioInclusion::Excluded => "excluded",
            AudioInclusion::References => "references",
        }
    }
}

/// What to export. Scopes are explicit: there is no default scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRequest {
    pub authentication: ReadAuthentication,
    pub scopes: Vec<ItemScope>,
    pub audio: AudioInclusion,
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("export requires an authenticated read session")]
    NotAuthenticated,
    #[error("export requires at least one explicitly selected scope")]
    NoScopeSelected,
    #[error("export could not read the store: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("export found inconsistent stored state: {0}")]
    Storage(#[from] anyhow::Error),
    #[error("export output could not be written: {0}")]
    Write(#[source] std::io::Error),
    #[error("export was cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub format: String,
    pub format_version: u32,
    pub exported_at: String,
    pub store_schema_version: u32,
    pub scopes: Vec<String>,
    pub audio: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFooter {
    pub item_count: usize,
    pub sha256: String,
}

/// The effective text and where it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedText {
    /// `original`, `transcribed` or `corrected`.
    pub basis: String,
    pub text: Option<String>,
    /// When the transcript or correction was recorded; absent for original text.
    pub at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedProvenance {
    pub type_corrected: bool,
    pub scope_corrected: bool,
    pub session_topic_corrected: bool,
    pub text_corrected: bool,
}

/// The raw capture as saved. The audio reference is deliberately not a field here; see
/// [`ExportedAudio`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedCapture {
    pub capture_id: String,
    pub text: Option<String>,
    pub capture_instant: String,
    pub timezone_id: String,
    pub utc_offset_minutes: i32,
    pub locale: String,
    pub calendar: String,
    pub item_scope: String,
    pub route_id: String,
    pub session_topic: Option<String>,
    pub entry_locked: bool,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedTranscript {
    pub transcript_record_id: String,
    pub text: String,
    pub attached_revision: i32,
    pub recognizer: String,
    pub detected_language: Option<String>,
    pub confidence: Option<f64>,
    pub transcribed_at: String,
    /// Present only when audio references were requested.
    pub audio_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedCorrection {
    pub correction_id: String,
    pub revision: i32,
    pub kind: String,
    pub old_value: Option<String>,
    pub new_value: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedEvent {
    pub event_id: String,
    pub revision: i32,
    pub event_type: String,
    pub happened_at: String,
    pub correction_kind: Option<String>,
    pub correction_old_value: Option<String>,
    pub correction_new_value: Option<String>,
    pub suggestion_control_kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedReminderOperation {
    pub operation_type: String,
    pub operation_state: String,
    pub scheduled_for: Option<String>,
    pub created_at: String,
}

/// Reminder history: recorded states, never a schedulable event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedReminder {
    pub reminder_id: String,
    pub request_state: String,
    pub unschedulable_reason: Option<String>,
    pub schedule_state: String,
    pub delivery_state: String,
    pub acknowledgment_state: String,
    pub resolved_instant: Option<String>,
    pub timezone_id: Option<String>,
    pub ambiguity_reason: Option<String>,
    pub unsupported_reason: Option<String>,
    pub source_phrase: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub operations: Vec<ExportedReminderOperation>,
}

/// Explicit audio disposition for one item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExportedAudio {
    None,
    Omitted,
    Referenced { reference: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedItem {
    pub item_id: String,
    pub capture_id: String,
    pub revision: i32,
    pub item_type: Option<String>,
    pub scope: String,
    pub session_topic: Option<String>,
    pub lifecycle_state: String,
    pub save_state: String,
    pub transcription_state: String,
    pub created_at: String,
    pub updated_at: String,
    pub text: ExportedText,
    pub provenance: ExportedProvenance,
    pub capture: ExportedCapture,
    pub transcript: Option<ExportedTranscript>,
    pub audio: ExportedAudio,
    pub corrections: Vec<ExportedCorrection>,
    pub history: Vec<ExportedEvent>,
    pub reminder: Option<ExportedReminder>,
}

/// Everything one export contains, read from a single snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct CoreSnapshot {
    pub header: SnapshotHeader,
    pub items: Vec<ExportedItem>,
}

/// What a completed write produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSummary {
    pub item_count: usize,
    pub sha256: String,
}

#[derive(Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum RecordRef<'a> {
    Header(&'a SnapshotHeader),
    Item(&'a ExportedItem),
    Footer(&'a SnapshotFooter),
}

/// Read the requested scopes and write them to `output` in the portable format.
pub fn export_core_snapshot(
    db: &mut Database,
    request: &ExportRequest,
    exported_at: DateTime<Utc>,
    output: &mut dyn Write,
) -> Result<ExportSummary, ExportError> {
    let snapshot = read_core_snapshot(db, request, exported_at)?;
    write_snapshot(&snapshot, output)
}

/// Read one consistent snapshot of the requested scopes. Nothing is read, counted or
/// revealed unless the read session is authenticated and a scope is selected.
pub fn read_core_snapshot(
    db: &mut Database,
    request: &ExportRequest,
    exported_at: DateTime<Utc>,
) -> Result<CoreSnapshot, ExportError> {
    read_core_snapshot_observed(db, request, exported_at, &mut |_| {})
}

/// As [`read_core_snapshot`], calling `after_items_read` inside the read transaction with the
/// number of items read so far (0 once the item list is fixed), so tests can interleave writers.
pub(crate) fn read_core_snapshot_observed(
    db: &mut Database,
    request: &ExportRequest,
    exported_at: DateTime<Utc>,
    after_items_read: &mut dyn FnMut(usize),
) -> Result<CoreSnapshot, ExportError> {
    if request.authentication != ReadAuthentication::Authenticated {
        return Err(ExportError::NotAuthenticated);
    }
    let scopes = canonical_scopes(&request.scopes);
    if scopes.is_empty() {
        return Err(ExportError::NoScopeSelected);
    }

    let tx = db.transaction()?;
    let store_schema_version: u32 =
        tx.query_row("SELECT MAX(version) FROM _schema_metadata", [], |row| {
            row.get(0)
        })?;
    let candidate_ids = candidate_item_ids(&tx)?;
    after_items_read(0);

    let mut items = Vec::new();
    for (index, item_id) in candidate_ids.iter().enumerate() {
        if let Some(item) = read_item(&tx, item_id, &scopes, request.audio)? {
            items.push(item);
        }
        after_items_read(index + 1);
    }
    drop(tx);

    Ok(CoreSnapshot {
        header: SnapshotHeader {
            format: FORMAT_NAME.to_string(),
            format_version: FORMAT_VERSION,
            exported_at: exported_at.to_rfc3339(),
            store_schema_version,
            scopes: scopes
                .iter()
                .map(|scope| scope.as_str().to_string())
                .collect(),
            audio: request.audio.as_str().to_string(),
        },
        items,
    })
}

/// Write a snapshot as header, item lines and a footer that lets a reader detect truncation.
pub fn write_snapshot(
    snapshot: &CoreSnapshot,
    output: &mut dyn Write,
) -> Result<ExportSummary, ExportError> {
    write_snapshot_until(snapshot, output, &|| false)
}

/// Like `write_snapshot`, but asks `is_cancelled` before each line and stops with
/// `ExportError::Cancelled` as soon as it returns true.
pub fn write_snapshot_until(
    snapshot: &CoreSnapshot,
    output: &mut dyn Write,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ExportSummary, ExportError> {
    let mut digest = Sha256::new();
    if is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    write_line(
        output,
        Some(&mut digest),
        &RecordRef::Header(&snapshot.header),
    )?;
    for item in &snapshot.items {
        if is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        write_line(output, Some(&mut digest), &RecordRef::Item(item))?;
    }
    if is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let footer = SnapshotFooter {
        item_count: snapshot.items.len(),
        sha256: hex_lower(&digest.finalize()),
    };
    write_line(output, None, &RecordRef::Footer(&footer))?;
    output.flush().map_err(ExportError::Write)?;

    Ok(ExportSummary {
        item_count: footer.item_count,
        sha256: footer.sha256,
    })
}

fn write_line(
    output: &mut dyn Write,
    digest: Option<&mut Sha256>,
    record: &RecordRef<'_>,
) -> Result<(), ExportError> {
    let mut line = serde_json::to_vec(record).map_err(|error| anyhow!(error))?;
    line.push(b'\n');
    if let Some(digest) = digest {
        digest.update(&line);
    }
    output.write_all(&line).map_err(ExportError::Write)
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Requested scopes without duplicates, in a fixed order.
fn canonical_scopes(requested: &[ItemScope]) -> Vec<ItemScope> {
    [ItemScope::Personal, ItemScope::Work]
        .into_iter()
        .filter(|scope| requested.contains(scope))
        .collect()
}

/// Deleted items are never candidates: their tombstones, counts and ids stay out of exports.
fn candidate_item_ids(tx: &Transaction<'_>) -> Result<Vec<String>, ExportError> {
    let mut statement = tx.prepare(
        "SELECT item_id FROM items WHERE lifecycle_state != 'deleted' ORDER BY created_at, item_id",
    )?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

struct ItemRow {
    save_state: String,
    transcription_state: String,
    created_at: String,
    updated_at: String,
}

fn read_item(
    tx: &Transaction<'_>,
    item_id: &str,
    scopes: &[ItemScope],
    audio: AudioInclusion,
) -> Result<Option<ExportedItem>, ExportError> {
    let state = load_item_state(tx, item_id)?
        .ok_or_else(|| anyhow!("item {item_id} vanished inside the export snapshot"))?;
    if state.lifecycle_state == LifecycleState::Deleted || !scopes.contains(&state.scope) {
        return Ok(None);
    }

    let row: ItemRow = tx.query_row(
        "SELECT save_state, transcription_state, created_at, updated_at FROM items WHERE item_id = ?",
        [item_id],
        |row| {
            Ok(ItemRow {
                save_state: row.get(0)?,
                transcription_state: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        },
    )?;
    let capture = get_capture(tx, &state.capture_id)?
        .ok_or_else(|| anyhow!("item {item_id} has no capture {}", state.capture_id))?;

    let audio_reference = match (&capture.audio_reference, audio) {
        (None, _) => ExportedAudio::None,
        (Some(_), AudioInclusion::Excluded) => ExportedAudio::Omitted,
        (Some(reference), AudioInclusion::References) => ExportedAudio::Referenced {
            reference: reference.clone(),
        },
    };

    let text = match &state.current_text {
        TextState::Original { text } => ExportedText {
            basis: "original".to_string(),
            text: text.clone(),
            at: None,
        },
        TextState::Transcribed {
            text,
            transcribed_at,
        } => ExportedText {
            basis: "transcribed".to_string(),
            text: Some(text.clone()),
            at: Some(transcribed_at.clone()),
        },
        TextState::Corrected { text, corrected_at } => ExportedText {
            basis: "corrected".to_string(),
            text: Some(text.clone()),
            at: Some(corrected_at.clone()),
        },
    };

    Ok(Some(ExportedItem {
        item_id: state.item_id.clone(),
        capture_id: state.capture_id.clone(),
        revision: state.revision,
        item_type: state.item_type.map(|kind| kind.as_str().to_string()),
        scope: state.scope.as_str().to_string(),
        session_topic: state.session_topic.clone(),
        lifecycle_state: state.lifecycle_state.as_str().to_string(),
        save_state: row.save_state,
        transcription_state: row.transcription_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
        text,
        provenance: ExportedProvenance {
            type_corrected: state.provenance.type_corrected,
            scope_corrected: state.provenance.scope_corrected,
            session_topic_corrected: state.provenance.session_topic_corrected,
            text_corrected: state.provenance.text_corrected,
        },
        capture: ExportedCapture {
            capture_id: capture.capture_id,
            text: capture.text,
            capture_instant: capture.capture_instant,
            timezone_id: capture.timezone_id,
            utc_offset_minutes: capture.utc_offset_minutes,
            locale: capture.locale,
            calendar: capture.calendar,
            item_scope: capture.item_scope,
            route_id: capture.route_id,
            session_topic: capture.session_topic,
            entry_locked: capture.entry_locked,
            created_at: capture.created_at,
        },
        transcript: read_transcript(tx, item_id, audio)?,
        audio: audio_reference,
        corrections: read_corrections(tx, item_id)?,
        history: read_events(tx, item_id)?,
        reminder: read_reminder(tx, item_id)?,
    }))
}

fn read_transcript(
    tx: &Transaction<'_>,
    item_id: &str,
    audio: AudioInclusion,
) -> Result<Option<ExportedTranscript>, ExportError> {
    let transcript = tx
        .query_row(
            "SELECT transcript_record_id, text, attached_revision, recognizer,
                    detected_language, confidence, transcribed_at, audio_sha256
             FROM transcript_attachments WHERE item_id = ?",
            [item_id],
            |row| {
                Ok(ExportedTranscript {
                    transcript_record_id: row.get(0)?,
                    text: row.get(1)?,
                    attached_revision: row.get(2)?,
                    recognizer: row.get(3)?,
                    detected_language: row.get(4)?,
                    confidence: row.get(5)?,
                    transcribed_at: row.get(6)?,
                    audio_sha256: Some(row.get(7)?),
                })
            },
        )
        .optional()?;
    Ok(transcript.map(|mut transcript| {
        if audio == AudioInclusion::Excluded {
            transcript.audio_sha256 = None;
        }
        transcript
    }))
}

fn read_corrections(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Vec<ExportedCorrection>, ExportError> {
    let mut statement = tx.prepare(
        "SELECT correction_id, revision, kind, old_value, new_value, created_at
         FROM corrections WHERE item_id = ? ORDER BY revision, kind",
    )?;
    let corrections = statement
        .query_map([item_id], |row| {
            Ok(ExportedCorrection {
                correction_id: row.get(0)?,
                revision: row.get(1)?,
                kind: row.get(2)?,
                old_value: row.get(3)?,
                new_value: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(corrections)
}

fn read_events(tx: &Transaction<'_>, item_id: &str) -> Result<Vec<ExportedEvent>, ExportError> {
    let mut statement = tx.prepare(
        "SELECT event_id, revision, event_type, happened_at, correction_kind,
                correction_old_value, correction_new_value, suggestion_control_kind
         FROM events WHERE item_id = ? ORDER BY revision, rowid",
    )?;
    let events = statement
        .query_map([item_id], |row| {
            Ok(ExportedEvent {
                event_id: row.get(0)?,
                revision: row.get(1)?,
                event_type: row.get(2)?,
                happened_at: row.get(3)?,
                correction_kind: row.get(4)?,
                correction_old_value: row.get(5)?,
                correction_new_value: row.get(6)?,
                suggestion_control_kind: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

fn read_reminder(
    tx: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<ExportedReminder>, ExportError> {
    let reminder = tx
        .query_row(
            "SELECT reminder_id, request_state, unschedulable_reason, schedule_state,
                    delivery_state, acknowledgment_state, resolved_instant, timezone_id,
                    ambiguity_reason, unsupported_reason, source_phrase, created_at, updated_at
             FROM reminders WHERE item_id = ?",
            [item_id],
            |row| {
                Ok(ExportedReminder {
                    reminder_id: row.get(0)?,
                    request_state: row.get(1)?,
                    unschedulable_reason: row.get(2)?,
                    schedule_state: row.get(3)?,
                    delivery_state: row.get(4)?,
                    acknowledgment_state: row.get(5)?,
                    resolved_instant: row.get(6)?,
                    timezone_id: row.get(7)?,
                    ambiguity_reason: row.get(8)?,
                    unsupported_reason: row.get(9)?,
                    source_phrase: row.get(10)?,
                    created_at: row.get(11)?,
                    updated_at: row.get(12)?,
                    operations: Vec::new(),
                })
            },
        )
        .optional()?;
    let Some(mut reminder) = reminder else {
        return Ok(None);
    };
    let mut statement = tx.prepare(
        "SELECT operation_type, operation_state, scheduled_for, created_at
         FROM reminder_operations WHERE reminder_id = ? ORDER BY created_at, rowid",
    )?;
    reminder.operations = statement
        .query_map([reminder.reminder_id.as_str()], |row| {
            Ok(ExportedReminderOperation {
                operation_type: row.get(0)?,
                operation_state: row.get(1)?,
                scheduled_for: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(reminder))
}
