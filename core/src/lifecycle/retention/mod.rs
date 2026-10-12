//! Audio retention policy (L03a).
//!
//! Decides, from durable state and an injected clock, what may happen to each item's raw audio.
//! This module only reads: it never deletes a file and never writes a row. Native code removes
//! only audio that [`authorize_audio_removal`] approves for the exact audio reference, at the
//! moment it removes it, so a stale sweep result cannot remove audio that became protected.
//!
//! Rules (architecture contract, "Audio retention"):
//!
//! * The retention clock starts at successful transcription (`transcript_attachments.
//!   transcribed_at`), never at capture. Audio expires `RetentionPolicy::period` later (seven days
//!   by default). A later user correction neither restarts nor shortens the clock.
//! * Audio with no usable transcription-success timestamp never expires automatically: pending,
//!   unsupported and failed transcription, audio kept next to user-written or captured text but
//!   without a transcript, and records that are missing, inconsistent or carry an unreadable
//!   timestamp. Each is a visible [`RetentionState::Unresolved`] with its reason.
//! * Audio that is the item's only source (no captured text, no user text correction, no usable
//!   transcript) is flagged `sole_source`. Storage pressure never changes eligibility: it only adds
//!   a [`StoragePressureNotice`] so the surface can say that nothing was or will be freed
//!   automatically. The user's explicit options are always [`RetentionOption::DeleteAudio`], plus
//!   [`RetentionOption::RetryTranscription`] whenever no text exists, no usable transcript is
//!   attached and no transcription job is queued or running. That covers failed, unsupported and
//!   pending states, including the no-speech outcome, which settles its job as failed and leaves
//!   the item `audio_pending`, and a `transcribed` item whose transcript record is missing or
//!   inconsistent.
//! * "Text exists" follows the item's current text: the latest text correction. Clearing a
//!   correction to empty makes audio with no transcript a sole source again (and retry is
//!   offered). Once a transcript is attached, the transcript stays recoverable, so clearing a
//!   correction does not keep the audio from expiring.
//! * Deleted items are not assessed: their audio removal is deletion work owned by
//!   `lifecycle::delete_intent`.

use crate::domain::status::TranscriptionState;
use crate::privacy::routing::JOB_TYPE_TRANSCRIPTION_ATTACHMENT;
use crate::store::schema::Clock;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension};

#[cfg(test)]
mod tests;

/// Days audio is kept after successful transcription unless configured otherwise.
pub const DEFAULT_RETENTION_DAYS: i64 = 7;
/// Longest configurable period, which keeps timestamp arithmetic far from overflow.
pub const MAX_RETENTION_DAYS: i64 = 3650;

/// The configurable period between successful transcription and audio expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    period: Duration,
}

impl RetentionPolicy {
    /// A policy with a period of at least one second and at most [`MAX_RETENTION_DAYS`].
    pub fn new(period: Duration) -> Result<Self> {
        if period < Duration::seconds(1) || period > Duration::days(MAX_RETENTION_DAYS) {
            return Err(anyhow!(
                "retention period must be between one second and {MAX_RETENTION_DAYS} days"
            ));
        }
        Ok(RetentionPolicy { period })
    }

    pub fn period(&self) -> Duration {
        self.period
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            period: Duration::days(DEFAULT_RETENTION_DAYS),
        }
    }
}

/// Free-space condition reported by the platform for the audio volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoragePressure {
    Normal,
    Low,
}

/// Why audio has no automatic expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// Transcription has not finished; the audio is the only source.
    TranscriptionPending,
    /// The language or model is unavailable on the device.
    TranscriptionUnsupported,
    /// Transcription failed (for example unreadable audio).
    TranscriptionFailed,
    /// Text exists (captured or user-written) but no transcript, so no clock was ever started.
    TextWithoutTranscript,
    /// The item claims a transcript that is absent, or a transcript exists the state ignores.
    TranscriptRecordInconsistent,
    /// The transcript was made from different audio than the capture's.
    TranscriptAudioMismatch,
    /// The recorded transcription time cannot be read, so the expiry cannot be computed.
    TranscriptionTimeUnreadable,
}

/// Where the audio stands against its retention period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionState {
    /// Transcribed; the audio is kept until `expires_at`.
    Retained { expires_at: DateTime<Utc> },
    /// Transcribed and past the retention period.
    Expired { expired_at: DateTime<Utc> },
    /// No automatic expiry applies; the audio stays until the user acts or text becomes
    /// recoverable.
    Unresolved { reason: UnresolvedReason },
}

/// Actions the user is offered explicitly. Nothing here happens automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionOption {
    RetryTranscription,
    DeleteAudio,
}

/// Why storage pressure did not free this audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoragePressureNotice {
    /// The audio is the only source; it is kept until the user deletes it or text exists.
    SoleSourceRetained,
    /// The audio is not yet eligible for removal.
    NotYetEligible,
}

/// Audio that native code may remove now (subject to [`authorize_audio_removal`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupIntent {
    None,
    RemoveAudio {
        audio_reference: String,
        expired_at: DateTime<Utc>,
    },
}

/// The retention status of one item's audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRetentionAssessment {
    pub item_id: String,
    pub audio_reference: String,
    pub state: RetentionState,
    /// True when no captured text, user text correction or usable transcript exists.
    pub sole_source: bool,
    pub options: Vec<RetentionOption>,
    pub cleanup: CleanupIntent,
    /// Present only under storage pressure when nothing is being freed for this audio.
    pub storage_pressure_notice: Option<StoragePressureNotice>,
}

/// Why removal is refused at the moment of removal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalDenial {
    ItemNotFound,
    /// The item has no audio, or its audio reference differs from the one presented.
    AudioReferenceMismatch,
    /// Deletion cleanup owns this audio.
    DeletionOwnsCleanup,
    NotYetExpired {
        expires_at: DateTime<Utc>,
    },
    Unresolved {
        reason: UnresolvedReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalDecision {
    Approved { expired_at: DateTime<Utc> },
    Denied(RemovalDenial),
}

struct AudioItemRow {
    item_id: String,
    lifecycle_state: String,
    transcription_state: String,
    audio_reference: String,
    capture_text_present: bool,
    user_text_present: bool,
    transcription_in_flight: bool,
    transcript_audio_reference: Option<String>,
    transcribed_at: Option<String>,
}

/// SQLite's one-argument `TRIM` strips only spaces; this also strips tabs and line breaks.
const WHITESPACE: &str = "' ' || char(9, 10, 11, 12, 13)";

/// Both text and in-flight flags read current state: user text is the item's latest text
/// correction (the same one the domain projection resolves), not any correction ever made.
fn audio_item_select() -> String {
    format!(
        "SELECT items.item_id, items.lifecycle_state,
        items.transcription_state, captures.audio_reference,
        COALESCE(TRIM(captures.text, {WHITESPACE}) <> '', 0),
        COALESCE((SELECT TRIM(corrections.new_value, {WHITESPACE}) <> '' FROM corrections
                  WHERE corrections.item_id = items.item_id AND corrections.kind = 'text'
                  ORDER BY corrections.revision DESC LIMIT 1), 0),
        EXISTS(SELECT 1 FROM jobs
               WHERE jobs.item_id = items.item_id AND jobs.job_type = '{JOB_TYPE_TRANSCRIPTION_ATTACHMENT}'
                 AND jobs.status IN ('queued', 'running')),
        transcript_attachments.audio_reference, transcript_attachments.transcribed_at
    FROM items
    JOIN captures ON captures.capture_id = items.capture_id
    LEFT JOIN transcript_attachments ON transcript_attachments.item_id = items.item_id
    WHERE captures.audio_reference IS NOT NULL"
    )
}

fn audio_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioItemRow> {
    Ok(AudioItemRow {
        item_id: row.get(0)?,
        lifecycle_state: row.get(1)?,
        transcription_state: row.get(2)?,
        audio_reference: row.get(3)?,
        capture_text_present: row.get(4)?,
        user_text_present: row.get(5)?,
        transcription_in_flight: row.get(6)?,
        transcript_audio_reference: row.get(7)?,
        transcribed_at: row.get(8)?,
    })
}

/// The retention status of one item's audio, or `None` when the item does not exist, has no
/// audio, or is deleted (deletion cleanup owns its audio).
pub fn assess_audio_retention(
    connection: &Connection,
    clock: &dyn Clock,
    policy: &RetentionPolicy,
    pressure: StoragePressure,
    item_id: &str,
) -> Result<Option<AudioRetentionAssessment>> {
    let now = clock.now();
    let row = connection
        .query_row(
            &format!("{} AND items.item_id = ?", audio_item_select()),
            [item_id],
            audio_item_from_row,
        )
        .optional()?;
    match row {
        Some(row) if row.lifecycle_state != "deleted" => {
            Ok(Some(assess_row(&row, policy, pressure, now)?))
        }
        _ => Ok(None),
    }
}

/// The retention status of every live item that holds audio, oldest capture first. Deleted
/// items are omitted. The result lists unresolved and retained audio too, so a surface can show
/// all of it; native cleanup acts only on entries whose `cleanup` is `RemoveAudio`.
pub fn sweep_audio_retention(
    connection: &Connection,
    clock: &dyn Clock,
    policy: &RetentionPolicy,
    pressure: StoragePressure,
) -> Result<Vec<AudioRetentionAssessment>> {
    let now = clock.now();
    let mut statement = connection.prepare(&format!(
        "{} AND items.lifecycle_state <> 'deleted'
         ORDER BY items.created_at, items.item_id",
        audio_item_select()
    ))?;
    let rows = statement
        .query_map([], audio_item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.iter()
        .map(|row| assess_row(row, policy, pressure, now))
        .collect()
}

/// Re-checks, at the moment native code is about to remove a file, that the audio is still
/// expired, still the item's audio, and not deleted-item audio. A removal planned earlier and
/// since invalidated (the item was deleted, the audio changed, the period was lengthened) is
/// denied.
pub fn authorize_audio_removal(
    connection: &Connection,
    clock: &dyn Clock,
    policy: &RetentionPolicy,
    item_id: &str,
    audio_reference: &str,
) -> Result<RemovalDecision> {
    let now = clock.now();
    let row = connection
        .query_row(
            &format!("{} AND items.item_id = ?", audio_item_select()),
            [item_id],
            audio_item_from_row,
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(RemovalDecision::Denied(RemovalDenial::ItemNotFound));
    };
    if row.lifecycle_state == "deleted" {
        return Ok(RemovalDecision::Denied(RemovalDenial::DeletionOwnsCleanup));
    }
    if row.audio_reference != audio_reference {
        return Ok(RemovalDecision::Denied(
            RemovalDenial::AudioReferenceMismatch,
        ));
    }
    let assessment = assess_row(&row, policy, StoragePressure::Normal, now)?;
    Ok(match assessment.state {
        RetentionState::Expired { expired_at } => RemovalDecision::Approved { expired_at },
        RetentionState::Retained { expires_at } => {
            RemovalDecision::Denied(RemovalDenial::NotYetExpired { expires_at })
        }
        RetentionState::Unresolved { reason } => {
            RemovalDecision::Denied(RemovalDenial::Unresolved { reason })
        }
    })
}

enum RetentionClock {
    Started(DateTime<Utc>),
    Unresolved(UnresolvedReason),
}

fn assess_row(
    row: &AudioItemRow,
    policy: &RetentionPolicy,
    pressure: StoragePressure,
    now: DateTime<Utc>,
) -> Result<AudioRetentionAssessment> {
    let transcription_state = row
        .transcription_state
        .parse::<TranscriptionState>()
        .map_err(|error| anyhow!("stored transcription state is invalid: {}", error.0))?;
    let transcript_recoverable = transcription_state == TranscriptionState::Transcribed
        && row.transcript_audio_reference.is_some();
    let text_present = row.capture_text_present || row.user_text_present || transcript_recoverable;

    let state = match retention_clock(row, transcription_state, text_present) {
        RetentionClock::Unresolved(reason) => RetentionState::Unresolved { reason },
        RetentionClock::Started(transcribed_at) => {
            match transcribed_at.checked_add_signed(policy.period()) {
                Some(expires_at) if expires_at > now => RetentionState::Retained { expires_at },
                Some(expires_at) => RetentionState::Expired {
                    expired_at: expires_at,
                },
                None => RetentionState::Unresolved {
                    reason: UnresolvedReason::TranscriptionTimeUnreadable,
                },
            }
        }
    };

    let mut options = Vec::new();
    if !text_present && !row.transcription_in_flight {
        options.push(RetentionOption::RetryTranscription);
    }
    options.push(RetentionOption::DeleteAudio);

    let cleanup = match state {
        RetentionState::Expired { expired_at } => CleanupIntent::RemoveAudio {
            audio_reference: row.audio_reference.clone(),
            expired_at,
        },
        _ => CleanupIntent::None,
    };

    let sole_source = !text_present;
    let storage_pressure_notice = match (pressure, &cleanup) {
        (StoragePressure::Low, CleanupIntent::None) if sole_source => {
            Some(StoragePressureNotice::SoleSourceRetained)
        }
        (StoragePressure::Low, CleanupIntent::None) => Some(StoragePressureNotice::NotYetEligible),
        _ => None,
    };

    Ok(AudioRetentionAssessment {
        item_id: row.item_id.clone(),
        audio_reference: row.audio_reference.clone(),
        state,
        sole_source,
        options,
        cleanup,
        storage_pressure_notice,
    })
}

fn retention_clock(
    row: &AudioItemRow,
    transcription_state: TranscriptionState,
    text_present: bool,
) -> RetentionClock {
    match (transcription_state, &row.transcript_audio_reference) {
        (TranscriptionState::Transcribed, Some(transcript_audio)) => {
            if transcript_audio != &row.audio_reference {
                return RetentionClock::Unresolved(UnresolvedReason::TranscriptAudioMismatch);
            }
            match row
                .transcribed_at
                .as_deref()
                .map(DateTime::parse_from_rfc3339)
            {
                Some(Ok(parsed)) => RetentionClock::Started(parsed.with_timezone(&Utc)),
                _ => RetentionClock::Unresolved(UnresolvedReason::TranscriptionTimeUnreadable),
            }
        }
        (TranscriptionState::Transcribed, None) | (_, Some(_)) => {
            RetentionClock::Unresolved(UnresolvedReason::TranscriptRecordInconsistent)
        }
        (_, None) if text_present => {
            RetentionClock::Unresolved(UnresolvedReason::TextWithoutTranscript)
        }
        (TranscriptionState::AudioPending | TranscriptionState::Transcribing, None) => {
            RetentionClock::Unresolved(UnresolvedReason::TranscriptionPending)
        }
        (TranscriptionState::TranscriptionUnsupported, None) => {
            RetentionClock::Unresolved(UnresolvedReason::TranscriptionUnsupported)
        }
        (TranscriptionState::TranscriptionFailed, None) => {
            RetentionClock::Unresolved(UnresolvedReason::TranscriptionFailed)
        }
        (TranscriptionState::NotApplicable, None) => {
            RetentionClock::Unresolved(UnresolvedReason::TranscriptRecordInconsistent)
        }
    }
}
