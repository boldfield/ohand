//! Replacing a voice note's machine transcript from its retained audio (F71a).
//!
//! Before F69 the app kept only the last segment the on-device recognizer delivered, so a long or
//! paused voice note was stored with a transcript that lacks its beginning. A transcript is
//! otherwise stored once; this module is the one explicit path that replaces one.
//!
//! * `transcript_attachments.transcript_generation` records which recognizer behaviour produced a
//!   transcript. A candidate is a live, `transcribed` voice note whose transcript has a generation
//!   below [`CURRENT_TRANSCRIPT_GENERATION`], whose text is still that transcript (no user text
//!   correction, not even one that cleared the text), whose audio is still recorded and which has
//!   no transcription job queued or running.
//! * [`request_retranscription`] queues one ordinary on-device `transcription_attachment` job per
//!   candidate at the item's current revision, and records each in
//!   `transcript_replacement_requests` keyed by job id. It never changes the item: its
//!   `transcription_state` stays `transcribed` and its text stays readable and searchable.
//! * `apply_transcription_outcome` finds the request row of the job and settles the outcome here,
//!   in one transaction, under the lease, redelivery and conflict rules of a first attachment:
//!   an identical transcript is `unchanged` (no event, no revision), a different one is `replaced`
//!   (a `transcript` event at a new revision, the superseded transcript kept in
//!   `superseded_transcripts`) and unsupported, failed or empty recognition is `kept` (nothing is
//!   written to the item and a failed replacement never moves it to `transcription_failed`). An
//!   outcome that can no longer be authoritative (the item was deleted, a user correction landed,
//!   the audio is gone or is not the audio the transcript came from, another replacement landed)
//!   is discarded and the job ends with a content-free reason.
//! * A replacement job the queue cancels outside this settlement (a revision moved on, the item was
//!   deleted, a lease expired on a stale revision) ends its request as `discarded` with the
//!   cancellation reason, in the cancelling transaction, so no request row stays pending forever.
//! * The core does not know whether an audio file still exists (removal is a native file effect),
//!   so a candidate is any item whose capture still has the audio reference; native code decides
//!   which candidates have a recording, and a job whose file is missing settles as `kept`.
//! * The retention clock does not restart: `transcribed_at` stays the first successful
//!   transcription and the replacement's time is `replaced_at`.
//!
//! A replacement's new revision behaves as a user text correction's does for earlier
//! interpretation results: proposals keep the revision they were made at, nothing cancels the
//! item's reminder (the `reminders` table holds one row per item, and a later derived request
//! leaves a committed reminder as it is), and suggestions are not touched. The differences are
//! that `text_corrected` stays false, the text basis stays `Transcript`, and, as for a first
//! attachment, an interpretation job is queued at the new revision.
//!
//! Item deletion and reset remove the requests and the superseded transcripts with the current
//! transcript. The capture row is never modified.

use super::{
    acknowledgment, commit, complete_job_in_tx, end_job_failed, interpretation_request_for, queue,
    queue_interpretation, recorded_interpretation, require_lease, require_unused_job_id,
    retire_job, unused_transcript_record_id, DiscardReason, ItemRow, Resolved,
    TranscriptionAcknowledgment, TranscriptionDisposition, TranscriptionError,
    TranscriptionErrorKind, TranscriptionOutcome, TranscriptionStage, FAILED_REASON,
    NO_SPEECH_REASON, UNSUPPORTED_REASON,
};
use crate::jobs::queue::Job;
use crate::privacy::routing::{
    authorize_job, AuthorizationDecision, JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
};
use crate::store::events::{save_transcript_event_in_tx, Event, EventPayload, EventType};
use crate::store::schema::Database;
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

/// The transcript generation written by a recognizer that keeps every recognized segment (F69).
/// Rows written before it carry generation 1.
pub const CURRENT_TRANSCRIPT_GENERATION: i32 = 2;
/// Most replacement jobs one [`request_retranscription`] call queues.
pub const MAX_RETRANSCRIPTION_BATCH: usize = 100;

const OUTCOME_REPLACED: &str = "replaced";
const OUTCOME_UNCHANGED: &str = "unchanged";
const OUTCOME_KEPT: &str = "kept";
const OUTCOME_DISCARDED: &str = "discarded";

/// Which items a retranscription request names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetranscriptionSelection<'a> {
    /// Every candidate, up to the limit.
    AllCandidates,
    /// These items, up to the limit; the rest are reported as deferred.
    Items(&'a [String]),
}

/// Why a named item was not queued. Content-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetranscriptionSkip {
    ItemNotFound,
    ItemDeleted,
    /// The item has no attached transcript to replace.
    NotTranscribed,
    /// A user text correction is in force, so the text is not the machine transcript.
    TextCorrected,
    /// The capture's audio reference is gone, or is not the audio the transcript came from.
    AudioUnavailable,
    /// The transcript was already produced by the current recognizer generation.
    GenerationCurrent,
    /// A transcription job for the item is queued or running.
    JobInFlight,
}

impl RetranscriptionSkip {
    pub fn as_str(self) -> &'static str {
        match self {
            RetranscriptionSkip::ItemNotFound => "item_not_found",
            RetranscriptionSkip::ItemDeleted => "item_deleted",
            RetranscriptionSkip::NotTranscribed => "not_transcribed",
            RetranscriptionSkip::TextCorrected => "text_corrected",
            RetranscriptionSkip::AudioUnavailable => "audio_unavailable",
            RetranscriptionSkip::GenerationCurrent => "generation_current",
            RetranscriptionSkip::JobInFlight => "job_in_flight",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedReplacement {
    pub item_id: String,
    pub job_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetranscriptionRequestResult {
    pub queued: Vec<QueuedReplacement>,
    pub skipped: Vec<(String, RetranscriptionSkip)>,
    /// Items beyond the batch limit that this call did not look at; ask again for them.
    pub deferred: usize,
}

/// The SQL that selects candidates; `?1` is the current generation and `?2` the job type.
const CANDIDATE_WHERE: &str = "i.lifecycle_state <> 'deleted'
    AND i.transcription_state = 'transcribed'
    AND t.transcript_generation < ?1
    AND c.audio_reference IS NOT NULL
    AND t.audio_reference = c.audio_reference
    AND NOT EXISTS (SELECT 1 FROM corrections k WHERE k.item_id = i.item_id AND k.kind = 'text')
    AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.item_id = i.item_id AND j.job_type = ?2
                      AND j.status IN ('queued', 'running'))";

const CANDIDATE_FROM: &str = "FROM items i
    JOIN captures c ON c.capture_id = i.capture_id
    JOIN transcript_attachments t ON t.item_id = i.item_id";

/// How many items a replacement could be requested for now.
pub fn count_retranscription_candidates(connection: &Connection) -> anyhow::Result<usize> {
    let count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) {CANDIDATE_FROM} WHERE {CANDIDATE_WHERE}"),
        rusqlite::params![
            CURRENT_TRANSCRIPT_GENERATION,
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT
        ],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// The ids of up to `limit` candidates, in item id order.
pub fn retranscription_candidates(
    connection: &Connection,
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let mut statement = connection.prepare(&format!(
        "SELECT i.item_id {CANDIDATE_FROM} WHERE {CANDIDATE_WHERE} ORDER BY i.item_id LIMIT ?3"
    ))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                CURRENT_TRANSCRIPT_GENERATION,
                JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
                i64::try_from(limit)?
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// What decides whether an item is a candidate, read inside the request's transaction.
struct CandidateFacts {
    revision: i32,
    lifecycle_state: String,
    transcription_state: String,
    capture_audio_reference: Option<String>,
    transcript_record_id: Option<String>,
    transcript_audio_reference: Option<String>,
    transcript_generation: Option<i32>,
    text_corrected: bool,
    job_in_flight: bool,
}

fn candidate_facts(
    transaction: &Transaction<'_>,
    item_id: &str,
) -> anyhow::Result<Option<CandidateFacts>> {
    Ok(transaction
        .query_row(
            "SELECT i.revision, i.lifecycle_state, i.transcription_state, c.audio_reference,
                    t.transcript_record_id, t.audio_reference, t.transcript_generation,
                    EXISTS (SELECT 1 FROM corrections k
                             WHERE k.item_id = i.item_id AND k.kind = 'text'),
                    EXISTS (SELECT 1 FROM jobs j
                             WHERE j.item_id = i.item_id AND j.job_type = ?2
                               AND j.status IN ('queued', 'running'))
               FROM items i
               JOIN captures c ON c.capture_id = i.capture_id
               LEFT JOIN transcript_attachments t ON t.item_id = i.item_id
              WHERE i.item_id = ?1",
            rusqlite::params![item_id, JOB_TYPE_TRANSCRIPTION_ATTACHMENT],
            |row| {
                Ok(CandidateFacts {
                    revision: row.get(0)?,
                    lifecycle_state: row.get(1)?,
                    transcription_state: row.get(2)?,
                    capture_audio_reference: row.get(3)?,
                    transcript_record_id: row.get(4)?,
                    transcript_audio_reference: row.get(5)?,
                    transcript_generation: row.get(6)?,
                    text_corrected: row.get(7)?,
                    job_in_flight: row.get(8)?,
                })
            },
        )
        .optional()?)
}

/// The transcript record a replacement would replace, or why the item is not a candidate.
fn classify(facts: Option<&CandidateFacts>) -> Result<(i32, String), RetranscriptionSkip> {
    use RetranscriptionSkip::*;
    let facts = facts.ok_or(ItemNotFound)?;
    if facts.lifecycle_state == "deleted" {
        return Err(ItemDeleted);
    }
    let (Some(record_id), Some(generation)) =
        (&facts.transcript_record_id, facts.transcript_generation)
    else {
        return Err(NotTranscribed);
    };
    if facts.transcription_state != "transcribed" {
        return Err(NotTranscribed);
    }
    if facts.text_corrected {
        return Err(TextCorrected);
    }
    match (
        &facts.capture_audio_reference,
        &facts.transcript_audio_reference,
    ) {
        (Some(captured), Some(transcribed)) if captured == transcribed => {}
        _ => return Err(AudioUnavailable),
    }
    if generation >= CURRENT_TRANSCRIPT_GENERATION {
        return Err(GenerationCurrent);
    }
    if facts.job_in_flight {
        return Err(JobInFlight);
    }
    Ok((facts.revision, record_id.clone()))
}

/// Queue a replacement transcription job for each selected candidate, in one transaction, at
/// most `limit` (and never more than [`MAX_RETRANSCRIPTION_BATCH`]) per call. An item that is not
/// a candidate is reported with its reason; an item with a transcription job queued or running is
/// skipped, so repeating a request while jobs are in flight queues nothing more. The items
/// themselves are not changed.
pub fn request_retranscription(
    database: &mut Database,
    selection: RetranscriptionSelection<'_>,
    limit: usize,
    now: DateTime<Utc>,
) -> anyhow::Result<RetranscriptionRequestResult> {
    let limit = limit.min(MAX_RETRANSCRIPTION_BATCH);
    let transaction = database.immediate_transaction()?;
    let mut result = RetranscriptionRequestResult::default();
    let item_ids: Vec<String> = match selection {
        RetranscriptionSelection::AllCandidates => {
            let mut candidates = retranscription_candidates(&transaction, limit.saturating_add(1))?;
            if candidates.len() > limit {
                candidates.truncate(limit);
                result.deferred = count_retranscription_candidates(&transaction)? - limit;
            }
            candidates
        }
        RetranscriptionSelection::Items(item_ids) => {
            result.deferred = item_ids.len().saturating_sub(limit);
            item_ids.iter().take(limit).cloned().collect()
        }
    };
    for item_id in item_ids {
        let facts = candidate_facts(&transaction, &item_id)?;
        match classify(facts.as_ref()) {
            Err(skip) => result.skipped.push((item_id, skip)),
            Ok((revision, replaces_record_id)) => {
                let job = queue::enqueue_for_new_item(&transaction, &item_id, revision, now)?;
                transaction.execute(
                    "INSERT INTO transcript_replacement_requests
                        (job_id, item_id, requested_at, replaces_record_id)
                     VALUES (?, ?, ?, ?)",
                    rusqlite::params![job.job_id, item_id, now.to_rfc3339(), replaces_record_id],
                )?;
                result.queued.push(QueuedReplacement {
                    item_id,
                    job_id: job.job_id,
                });
            }
        }
    }
    transaction.commit()?;
    Ok(result)
}

/// A transcript a replacement displaced, kept as history.
#[derive(Clone, Debug, PartialEq)]
pub struct SupersededTranscript {
    pub item_id: String,
    pub source_job_id: String,
    pub transcript_record_id: String,
    pub text: String,
    /// The item revision that included this transcript.
    pub attached_revision: i32,
    pub audio_reference: String,
    pub audio_sha256: String,
    pub recognizer: String,
    pub detected_language: Option<String>,
    pub confidence: Option<f64>,
    pub transcribed_at: String,
    pub transcript_generation: i32,
    /// When the replacement that displaced it was settled (RFC 3339).
    pub superseded_at: String,
    /// The record id of the transcript that replaced it.
    pub superseded_by_record_id: String,
}

const SUPERSEDED_COLUMNS: &str = "item_id, source_job_id, transcript_record_id, text,
    attached_revision, audio_reference, audio_sha256, recognizer, detected_language, confidence,
    transcribed_at, transcript_generation, superseded_at, superseded_by_record_id";

fn superseded_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SupersededTranscript> {
    Ok(SupersededTranscript {
        item_id: row.get(0)?,
        source_job_id: row.get(1)?,
        transcript_record_id: row.get(2)?,
        text: row.get(3)?,
        attached_revision: row.get(4)?,
        audio_reference: row.get(5)?,
        audio_sha256: row.get(6)?,
        recognizer: row.get(7)?,
        detected_language: row.get(8)?,
        confidence: row.get(9)?,
        transcribed_at: row.get(10)?,
        transcript_generation: row.get(11)?,
        superseded_at: row.get(12)?,
        superseded_by_record_id: row.get(13)?,
    })
}

/// The transcripts replacements displaced for an item, oldest first. Empty after deletion, which
/// removes them with the current transcript.
pub fn superseded_transcripts(
    connection: &Connection,
    item_id: &str,
) -> anyhow::Result<Vec<SupersededTranscript>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {SUPERSEDED_COLUMNS} FROM superseded_transcripts
          WHERE item_id = ? ORDER BY attached_revision, transcript_record_id"
    ))?;
    let rows = statement
        .query_map([item_id], superseded_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One request row, read inside the settling transaction.
pub(super) struct ReplacementRequest {
    replaces_record_id: String,
    outcome: Option<String>,
    outcome_reason: Option<String>,
    outcome_digest: Option<String>,
}

pub(super) fn load_replacement_request(
    transaction: &Transaction<'_>,
    job_id: &str,
) -> Result<Option<ReplacementRequest>, TranscriptionError> {
    Ok(transaction
        .query_row(
            "SELECT replaces_record_id, outcome, outcome_reason, outcome_digest
               FROM transcript_replacement_requests WHERE job_id = ?",
            [job_id],
            |row| {
                Ok(ReplacementRequest {
                    replaces_record_id: row.get(0)?,
                    outcome: row.get(1)?,
                    outcome_reason: row.get(2)?,
                    outcome_digest: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// The current attachment as a replacement reads it.
struct CurrentTranscript {
    record_id: String,
    text: String,
    audio_reference: String,
    audio_sha256: String,
}

fn load_current_transcript(
    transaction: &Transaction<'_>,
    item_id: &str,
) -> Result<Option<CurrentTranscript>, TranscriptionError> {
    Ok(transaction
        .query_row(
            "SELECT transcript_record_id, text, audio_reference, audio_sha256
               FROM transcript_attachments WHERE item_id = ?",
            [item_id],
            |row| {
                Ok(CurrentTranscript {
                    record_id: row.get(0)?,
                    text: row.get(1)?,
                    audio_reference: row.get(2)?,
                    audio_sha256: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// A digest of the delivered outcome, so a redelivery of the same outcome is recognised and a
/// different outcome for the same job is a conflict. Holds no readable text.
fn outcome_digest(outcome: &TranscriptionOutcome, resolved: &Resolved<'_>) -> String {
    let confidence_bits = |transcript: &super::RecognizedTranscript| {
        transcript
            .confidence
            .map_or_else(|| "none".to_string(), |value| value.to_bits().to_string())
    };
    let parts: Vec<String> = match resolved {
        Resolved::Pending(pending) => vec![
            "pending".to_string(),
            pending.reason.to_string(),
            outcome.audio_reference.clone(),
        ],
        Resolved::Transcript(transcript) => vec![
            "transcript".to_string(),
            outcome.audio_reference.clone(),
            transcript.text.clone(),
            transcript.audio_sha256.clone(),
            transcript.recognizer.clone(),
            transcript.detected_language.clone().unwrap_or_default(),
            confidence_bits(transcript),
        ],
    };
    let mut hasher = Sha256::new();
    hasher.update(b"ohand.transcript-replacement-outcome.v1");
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Settle the outcome of a replacement job in the caller's transaction. Consumes it.
#[allow(clippy::too_many_arguments)]
pub(super) fn settle(
    transaction: Transaction<'_>,
    job: &Job,
    item: &ItemRow,
    request: &ReplacementRequest,
    outcome: &TranscriptionOutcome,
    resolved: &Resolved<'_>,
    settled_at: DateTime<Utc>,
    fault_point: &mut dyn FnMut(TranscriptionStage) -> anyhow::Result<()>,
) -> Result<TranscriptionAcknowledgment, TranscriptionError> {
    let digest = outcome_digest(outcome, resolved);
    if let Some(settled) = &request.outcome {
        let disposition = redelivery(&transaction, job, request, settled, &digest)?;
        return Ok(acknowledgment(job, disposition));
    }

    require_lease(job, outcome.lease_attempt)?;
    match authorize_job(&transaction, &job.job_id)? {
        AuthorizationDecision::Authorized(_) => {}
        AuthorizationDecision::Denied(reason) => {
            return Err(TranscriptionError::not_committed(
                TranscriptionErrorKind::NotAuthorized(reason),
            ))
        }
    }

    let current = load_current_transcript(&transaction, &job.item_id)?;
    if let Some(reason) =
        discard_reason(&transaction, job, item, request, current.as_ref(), outcome)?
    {
        retire_job(
            &transaction,
            job,
            outcome.lease_attempt,
            reason.job_reason(),
        )?;
        record_outcome(
            &transaction,
            job,
            OUTCOME_DISCARDED,
            Some(reason.job_reason()),
            &digest,
            settled_at,
        )?;
        commit(transaction, fault_point)?;
        return Ok(acknowledgment(
            job,
            TranscriptionDisposition::Discarded(reason),
        ));
    }
    let current = current.expect("a discard reason covers a missing transcript");

    let disposition = match resolved {
        Resolved::Pending(pending) => {
            // The existing transcript stays; the item's transcription state is not touched.
            end_job_failed(&transaction, job, outcome.lease_attempt, pending.reason)?;
            record_outcome(
                &transaction,
                job,
                OUTCOME_KEPT,
                Some(pending.reason),
                &digest,
                settled_at,
            )?;
            TranscriptionDisposition::Kept {
                reason: pending.reason,
            }
        }
        Resolved::Transcript(transcript) if transcript.text.trim() == current.text.trim() => {
            transaction.execute(
                "UPDATE transcript_attachments SET transcript_generation = ?, recognizer = ?
                  WHERE item_id = ?",
                rusqlite::params![
                    transcript.transcript_generation,
                    transcript.recognizer,
                    job.item_id
                ],
            )?;
            complete_job_in_tx(&transaction, &job.job_id, outcome.lease_attempt)?;
            record_outcome(
                &transaction,
                job,
                OUTCOME_UNCHANGED,
                None,
                &digest,
                settled_at,
            )?;
            TranscriptionDisposition::Unchanged
        }
        Resolved::Transcript(transcript) => {
            let interpretation_request =
                interpretation_request_for(&transaction, job, item, outcome)?;
            require_unused_job_id(&transaction, &interpretation_request)?;
            let revision = replace_transcript(
                &transaction,
                job,
                item,
                &current,
                transcript,
                settled_at,
                fault_point,
            )?;
            complete_job_in_tx(&transaction, &job.job_id, outcome.lease_attempt)?;
            fault_point(TranscriptionStage::JobSettled)?;
            let interpretation = queue_interpretation(
                &transaction,
                &job.item_id,
                revision,
                &interpretation_request,
                settled_at,
            )?;
            fault_point(TranscriptionStage::InterpretationQueued)?;
            record_outcome(
                &transaction,
                job,
                OUTCOME_REPLACED,
                None,
                &digest,
                settled_at,
            )?;
            TranscriptionDisposition::Replaced {
                revision,
                interpretation,
            }
        }
    };
    commit(transaction, fault_point)?;
    Ok(acknowledgment(job, disposition))
}

fn record_outcome(
    transaction: &Transaction<'_>,
    job: &Job,
    outcome: &str,
    reason: Option<&str>,
    digest: &str,
    settled_at: DateTime<Utc>,
) -> Result<(), TranscriptionError> {
    let affected = transaction.execute(
        "UPDATE transcript_replacement_requests
            SET outcome = ?, outcome_reason = ?, outcome_digest = ?, settled_at = ?
          WHERE job_id = ? AND outcome IS NULL",
        rusqlite::params![outcome, reason, digest, settled_at.to_rfc3339(), job.job_id],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "replacement request of job {} is already settled",
            job.job_id
        )
        .into());
    }
    Ok(())
}

/// A job that settled earlier: the same outcome returns the original result, a different one is
/// a conflict. Nothing is written.
fn redelivery(
    transaction: &Transaction<'_>,
    job: &Job,
    request: &ReplacementRequest,
    settled: &str,
    digest: &str,
) -> Result<TranscriptionDisposition, TranscriptionError> {
    if request.outcome_digest.as_deref() != Some(digest) {
        return Err(TranscriptionError::not_committed(
            TranscriptionErrorKind::ConflictingResult,
        ));
    }
    let reason = request.outcome_reason.as_deref();
    match (settled, reason) {
        (OUTCOME_UNCHANGED, _) => Ok(TranscriptionDisposition::Unchanged),
        (OUTCOME_KEPT, Some(reason)) => Ok(TranscriptionDisposition::Kept {
            reason: kept_reason(reason)?,
        }),
        (OUTCOME_DISCARDED, Some(reason)) => Ok(TranscriptionDisposition::Discarded(
            DiscardReason::from_job_reason(reason)
                .ok_or_else(|| anyhow!("stored discard reason is not recognised"))?,
        )),
        (OUTCOME_REPLACED, _) => {
            // The record may since have been displaced by a later replacement.
            let attached_revision: i32 = transaction
                .query_row(
                    "SELECT attached_revision FROM transcript_attachments WHERE source_job_id = ?1
                     UNION ALL
                     SELECT attached_revision FROM superseded_transcripts WHERE source_job_id = ?1",
                    [&job.job_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow!("replacement of job {} has no transcript record", job.job_id)
                })?;
            Ok(TranscriptionDisposition::AlreadyReplaced {
                revision: attached_revision,
                interpretation: recorded_interpretation(
                    transaction,
                    &job.item_id,
                    attached_revision,
                )?,
            })
        }
        _ => Err(anyhow!("stored replacement outcome is not recognised").into()),
    }
}

fn kept_reason(stored: &str) -> Result<&'static str, TranscriptionError> {
    [UNSUPPORTED_REASON, FAILED_REASON, NO_SPEECH_REASON]
        .into_iter()
        .find(|reason| *reason == stored)
        .ok_or_else(|| anyhow!("stored kept reason is not recognised").into())
}

/// Whether the current durable state forbids applying any outcome of this replacement.
fn discard_reason(
    transaction: &Transaction<'_>,
    job: &Job,
    item: &ItemRow,
    request: &ReplacementRequest,
    current: Option<&CurrentTranscript>,
    outcome: &TranscriptionOutcome,
) -> Result<Option<DiscardReason>, TranscriptionError> {
    if item.lifecycle_state == "deleted" {
        return Ok(Some(DiscardReason::ItemDeleted));
    }
    let Some(current) = current else {
        return Ok(Some(DiscardReason::NoTranscriptToReplace));
    };
    if item.transcription_state != crate::domain::status::TranscriptionState::Transcribed {
        return Ok(Some(DiscardReason::NoTranscriptToReplace));
    }
    // A user text correction, even one that cleared the text, is the item's text and wins.
    let text_corrected: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM corrections WHERE item_id = ? AND kind = 'text')",
        [&job.item_id],
        |row| row.get(0),
    )?;
    if text_corrected {
        return Ok(Some(DiscardReason::TextCorrected));
    }
    if current.record_id != request.replaces_record_id {
        return Ok(Some(DiscardReason::TranscriptChanged));
    }
    let Some(captured) = item.audio_reference.as_deref() else {
        return Ok(Some(DiscardReason::AudioUnavailable));
    };
    let wrong_audio = captured != outcome.audio_reference
        || captured != current.audio_reference
        || match &outcome.result {
            super::RecognizerResult::Transcript(transcript) => {
                transcript.audio_sha256 != current.audio_sha256
            }
            _ => false,
        };
    Ok(wrong_audio.then_some(DiscardReason::AudioIdentityMismatch))
}

/// Move the current transcript to the superseded history, make the new one current and write the
/// `transcript` event. Returns the item revision that includes the new transcript.
fn replace_transcript(
    transaction: &Transaction<'_>,
    job: &Job,
    item: &ItemRow,
    current: &CurrentTranscript,
    transcript: &super::RecognizedTranscript,
    replaced_at: DateTime<Utc>,
    fault_point: &mut dyn FnMut(TranscriptionStage) -> anyhow::Result<()>,
) -> Result<i32, TranscriptionError> {
    let new_record_id = unused_transcript_record_id(transaction, &job.item_id, &job.job_id)?;
    let new_revision = item.revision + 1;
    let happened_at = replaced_at.to_rfc3339();
    transaction.execute(
        "INSERT INTO superseded_transcripts
            (transcript_record_id, item_id, source_job_id, text, attached_revision,
             audio_reference, audio_sha256, recognizer, detected_language, confidence,
             transcribed_at, transcript_generation, superseded_at, superseded_by_record_id)
         SELECT transcript_record_id, item_id, source_job_id, text, attached_revision,
                audio_reference, audio_sha256, recognizer, detected_language, confidence,
                transcribed_at, transcript_generation, ?, ?
           FROM transcript_attachments WHERE item_id = ?",
        rusqlite::params![happened_at, new_record_id, job.item_id],
    )?;
    // The row comes first, as for a first attachment: the event write refuses a transcript event
    // without its record, and the search index reads the text from the row. `transcribed_at`
    // keeps the first successful transcription so the retention clock does not restart.
    let affected = transaction.execute(
        "UPDATE transcript_attachments
            SET source_job_id = ?, transcript_record_id = ?, text = ?, attached_revision = ?,
                recognizer = ?, detected_language = ?, confidence = ?, transcript_generation = ?,
                replaced_at = ?
          WHERE item_id = ? AND transcript_record_id = ?",
        rusqlite::params![
            job.job_id,
            new_record_id,
            transcript.text,
            new_revision,
            transcript.recognizer,
            transcript.detected_language,
            transcript.confidence,
            transcript.transcript_generation,
            happened_at,
            job.item_id,
            current.record_id,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!("transcript of item {} changed while replacing", job.item_id).into());
    }
    let event = Event::new(
        new_record_id,
        job.item_id.clone(),
        item.revision,
        EventType::Transcript,
        EventPayload::Transcript,
        happened_at,
    )?;
    save_transcript_event_in_tx(transaction, &event, item.revision)
        .map_err(|error| TranscriptionError::from(anyhow::Error::new(error)))?;
    fault_point(TranscriptionStage::EventWritten)?;
    Ok(new_revision)
}
