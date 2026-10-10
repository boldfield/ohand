//! Guarded transcript attachment (C05a).
//!
//! Native code transcribes a durable voice capture on the device and hands the outcome to
//! [`apply_transcription_outcome`]. The core, not the native handler, decides whether the outcome
//! may change anything. One IMMEDIATE transaction:
//!
//! * verifies the job is a running `transcription_attachment` job and that the caller holds its
//!   lease (the job's `attempt_count`), and that the job is authorized to run (on device only);
//! * refuses, and retires the job, when the outcome can no longer be authoritative: the item was
//!   deleted, the user already supplied text (a correction), a transcript was already attached,
//!   the recorded audio is gone, or the outcome was made from different audio than the capture's;
//! * for a transcript, writes it as the item's text correction through the event API (so the
//!   revision check, correction history, search index and interpretation source all see it),
//!   moves the transcription state to `transcribed`, completes the job and, only when the new
//!   job is authorized, enqueues the requested interpretation job at the new revision;
//! * for unsupported, failed or empty (abstained) recognition, writes no text: the audio stays the
//!   sole source, the transcription state says why, and the job ends with a content-free reason.
//!
//! A transcript is stored once. The correction event's ID derives from the job and its
//! `happened_at` is the successful-transcription time, so a retry (same job) after a crash or
//! lost acknowledgment finds that event and returns the committed result, and a different job
//! can never attach a second transcript because the item is already `transcribed`. Provenance
//! lives only in existing storage: the event (source job, revision, success time), the
//! completed job and the capture's immutable audio reference, which the outcome must match.
//! The recognizer's digest, name, language and confidence are validated but not persisted:
//! storing them needs a schema change that no task has been allowed to make yet. Saved audio (`save_state`), transcript
//! completion (`transcription_state`) and interpretation completion (`processing_state`) are
//! independent: attaching a transcript changes only the second, and queues the third.
//!
//! The capture row is never modified. The transcript text lives only in the correction history,
//! so deletion redacts it with the item's other readable content.

use crate::domain::status::TranscriptionState;
use crate::jobs::queue::{complete_job_in_tx, enqueue_job_in_tx, get_job_internal, Job, JobStatus};
use crate::privacy::routing::{
    authorize_job, AuthorizationDecision, DenialReason, JOB_TYPE_INTERPRET,
    JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
};
use crate::store::events::{
    get_event, save_event_in_tx, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use crate::store::schema::Database;
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;

pub use crate::ingress::import::CommitStatus;

/// Largest transcript accepted, in bytes.
pub const MAX_TRANSCRIPT_BYTES: usize = 256 * 1024;
const MAX_RECOGNIZER_BYTES: usize = 128;
const MAX_LANGUAGE_BYTES: usize = 35;
const AUDIO_SHA256_HEX_LENGTH: usize = 64;
/// Version of the job record layout that the queue supports.
const INTERPRETATION_JOB_SCHEMA_VERSION: i32 = 1;

/// Job reason recorded when recognition is unsupported for the language or model.
pub const UNSUPPORTED_REASON: &str = "transcription_unsupported";
/// Job reason recorded when the audio could not be recognized.
pub const FAILED_REASON: &str = "transcription_failed";
/// Job reason recorded when recognition produced no text.
pub const NO_SPEECH_REASON: &str = "no_speech_detected";

/// A transcript produced on the device from the capture's audio.
#[derive(Clone, Debug, PartialEq)]
pub struct RecognizedTranscript {
    pub text: String,
    /// Recognizer confidence in `0.0..=1.0`, when the recognizer reports one.
    pub confidence: Option<f64>,
    pub detected_language: Option<String>,
    /// Name and version of the on-device recognizer that produced the text.
    pub recognizer: String,
    /// Lowercase hex SHA-256 of the audio file the recognizer read.
    pub audio_sha256: String,
}

/// What the on-device recognizer concluded.
#[derive(Clone, Debug, PartialEq)]
pub enum RecognizerResult {
    Transcript(RecognizedTranscript),
    /// The language or model is not available on the device.
    Unsupported,
    /// The audio could not be recognized (for example unreadable).
    Failed,
}

/// Interpretation work to queue after a transcript is attached, if the core authorizes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpretationRequest {
    pub job_id: String,
    /// `None` queues the offline fast path only; `Some` pins the job to a stored profile.
    pub profile_version: Option<String>,
    pub request_version: String,
}

/// One native transcription outcome for a claimed job.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptionOutcome {
    pub job_id: String,
    /// The job's `attempt_count` as returned by the claim.
    pub lease_attempt: i32,
    /// The audio reference the recognizer read; it must be the capture's.
    pub audio_reference: String,
    pub result: RecognizerResult,
    pub interpretation: Option<InterpretationRequest>,
}

/// Why an outcome was refused after it was checked against current durable state. The job is
/// retired with the matching reason and nothing about the item changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardReason {
    ItemDeleted,
    AlreadyTranscribed,
    TextAlreadyPresent,
    NotAudioCapture,
    AudioUnavailable,
    AudioIdentityMismatch,
}

impl DiscardReason {
    /// Content-free reason stored on the retired job.
    pub fn job_reason(self) -> &'static str {
        match self {
            DiscardReason::ItemDeleted => "item_deleted",
            DiscardReason::AlreadyTranscribed => "already_transcribed",
            DiscardReason::TextAlreadyPresent => "text_already_present",
            DiscardReason::NotAudioCapture => "not_audio_capture",
            DiscardReason::AudioUnavailable => "audio_unavailable",
            DiscardReason::AudioIdentityMismatch => "audio_identity_mismatch",
        }
    }
}

/// Whether the requested interpretation job was queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpretationQueued {
    NotRequested,
    Queued {
        job_id: String,
    },
    /// The job was not authorized, so it was not created. The transcript is still attached.
    NotAuthorized(DenialReason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptionDisposition {
    /// The transcript is now the item's text. `revision` is the item revision that includes it.
    Attached {
        revision: i32,
        interpretation: InterpretationQueued,
    },
    /// An earlier delivery of the same job already attached the identical transcript.
    AlreadyAttached {
        revision: i32,
        interpretation: InterpretationQueued,
    },
    /// No text was attached. The item reports `state`, and the audio is retained.
    Pending {
        state: TranscriptionState,
    },
    Discarded(DiscardReason),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptionAcknowledgment {
    pub item_id: String,
    pub job_id: String,
    pub disposition: TranscriptionDisposition,
}

/// Why a request was refused before it could be checked against the item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptionRejection {
    MissingField(&'static str),
    TooLarge(&'static str),
    MalformedField(&'static str),
    InterpretationJobIdInUse,
}

#[derive(Debug)]
pub enum TranscriptionErrorKind {
    Validation(TranscriptionRejection),
    /// The job is not a transcription job.
    NotATranscriptionJob {
        job_type: String,
    },
    /// The job does not exist, or is not running under the presented lease (a duplicate or late
    /// delivery). Nothing was recorded.
    LeaseNotHeld {
        reason: &'static str,
    },
    /// Processing of this job is not authorized.
    NotAuthorized(DenialReason),
    /// The same job already attached a different result.
    ConflictingResult,
    Storage(anyhow::Error),
}

/// Failure together with whether the transaction is known to have committed.
#[derive(Debug)]
pub struct TranscriptionError {
    pub kind: TranscriptionErrorKind,
    pub commit_status: CommitStatus,
}

impl TranscriptionError {
    fn not_committed(kind: TranscriptionErrorKind) -> Self {
        TranscriptionError {
            kind,
            commit_status: CommitStatus::NotCommitted,
        }
    }

    fn storage(error: impl Into<anyhow::Error>, commit_status: CommitStatus) -> Self {
        TranscriptionError {
            kind: TranscriptionErrorKind::Storage(error.into()),
            commit_status,
        }
    }
}

impl From<anyhow::Error> for TranscriptionError {
    fn from(error: anyhow::Error) -> Self {
        TranscriptionError::storage(error, CommitStatus::NotCommitted)
    }
}

impl From<rusqlite::Error> for TranscriptionError {
    fn from(error: rusqlite::Error) -> Self {
        TranscriptionError::storage(error, CommitStatus::NotCommitted)
    }
}

impl fmt::Display for TranscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TranscriptionErrorKind::Validation(rejection) => {
                write!(f, "transcription outcome rejected: {rejection:?}")?
            }
            TranscriptionErrorKind::NotATranscriptionJob { job_type } => {
                write!(f, "job type {job_type} is not a transcription job")?
            }
            TranscriptionErrorKind::LeaseNotHeld { reason } => {
                write!(f, "job lease not held: {reason}")?
            }
            TranscriptionErrorKind::NotAuthorized(reason) => {
                write!(f, "transcription not authorized: {reason}")?
            }
            TranscriptionErrorKind::ConflictingResult => {
                write!(f, "the job already attached a different transcript")?
            }
            TranscriptionErrorKind::Storage(error) => write!(f, "storage failure: {error}")?,
        }
        write!(f, " (commit status: {:?})", self.commit_status)
    }
}

impl std::error::Error for TranscriptionError {}

/// Points at which a test can fail the operation to prove rollback and retry behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptionStage {
    EventWritten,
    StateUpdated,
    JobSettled,
    InterpretationQueued,
    Committed,
}

/// Apply one native transcription outcome to a durable voice capture in a single retry-safe
/// transaction. `transcribed_at` is the time recognition succeeded; a retry of an attached job
/// returns the original result and keeps the original time.
pub fn apply_transcription_outcome(
    database: &mut Database,
    outcome: &TranscriptionOutcome,
    transcribed_at: DateTime<Utc>,
) -> Result<TranscriptionAcknowledgment, TranscriptionError> {
    apply_with_fault_points(database, outcome, transcribed_at, &mut |_| Ok(()))
}

pub(crate) fn apply_with_fault_points(
    database: &mut Database,
    outcome: &TranscriptionOutcome,
    transcribed_at: DateTime<Utc>,
    fault_point: &mut dyn FnMut(TranscriptionStage) -> anyhow::Result<()>,
) -> Result<TranscriptionAcknowledgment, TranscriptionError> {
    validate_outcome(outcome).map_err(|rejection| {
        TranscriptionError::not_committed(TranscriptionErrorKind::Validation(rejection))
    })?;

    let transaction = database
        .immediate_transaction()
        .map_err(TranscriptionError::from)?;

    let job = load_transcription_job(&transaction, outcome)?;
    let item = load_item(&transaction, &job.item_id)?;
    let resolved = resolve(&outcome.result);

    if let Some(stored) = get_event(&transaction, &transcript_event_id(&job.job_id))? {
        let disposition = redelivery(&transaction, &item, &stored, &resolved, &job)?;
        return Ok(acknowledgment(&job, disposition));
    }
    if let Resolved::Pending(pending) = &resolved {
        if is_settled_pending(&job, outcome.lease_attempt, pending) {
            return Ok(acknowledgment(
                &job,
                TranscriptionDisposition::Pending {
                    state: item.transcription_state,
                },
            ));
        }
    }

    require_lease(&job, outcome.lease_attempt)?;
    match authorize_job(&transaction, &job.job_id)? {
        AuthorizationDecision::Authorized(_) => {}
        AuthorizationDecision::Denied(reason) => {
            return Err(TranscriptionError::not_committed(
                TranscriptionErrorKind::NotAuthorized(reason),
            ))
        }
    }

    if let Some(reason) = discard_reason(&transaction, &job, &item, outcome)? {
        retire_job(
            &transaction,
            &job,
            outcome.lease_attempt,
            reason.job_reason(),
        )?;
        commit(transaction, fault_point)?;
        return Ok(acknowledgment(
            &job,
            TranscriptionDisposition::Discarded(reason),
        ));
    }

    let disposition = match resolved {
        Resolved::Pending(pending) => {
            transaction.execute(
                "UPDATE items SET transcription_state = ? WHERE item_id = ?",
                rusqlite::params![pending.state.as_str(), job.item_id],
            )?;
            end_job_failed(&transaction, &job, outcome.lease_attempt, pending.reason)?;
            TranscriptionDisposition::Pending {
                state: pending.state,
            }
        }
        Resolved::Transcript(transcript) => {
            if let Some(request) = &outcome.interpretation {
                require_unused_job_id(&transaction, request)?;
            }
            let revision = attach_transcript(
                &transaction,
                &job,
                &item,
                transcript,
                transcribed_at,
                fault_point,
            )?;
            complete_job_in_tx(&transaction, &job.job_id, outcome.lease_attempt)?;
            fault_point(TranscriptionStage::JobSettled)?;
            let interpretation = match &outcome.interpretation {
                Some(request) => queue_interpretation(
                    &transaction,
                    &job.item_id,
                    revision,
                    request,
                    transcribed_at,
                )?,
                None => InterpretationQueued::NotRequested,
            };
            fault_point(TranscriptionStage::InterpretationQueued)?;
            TranscriptionDisposition::Attached {
                revision,
                interpretation,
            }
        }
    };

    commit(transaction, fault_point)?;
    Ok(acknowledgment(&job, disposition))
}

fn acknowledgment(job: &Job, disposition: TranscriptionDisposition) -> TranscriptionAcknowledgment {
    TranscriptionAcknowledgment {
        item_id: job.item_id.clone(),
        job_id: job.job_id.clone(),
        disposition,
    }
}

fn commit(
    transaction: Transaction<'_>,
    fault_point: &mut dyn FnMut(TranscriptionStage) -> anyhow::Result<()>,
) -> Result<(), TranscriptionError> {
    transaction
        .commit()
        .map_err(|error| TranscriptionError::storage(error, CommitStatus::Unknown))?;
    fault_point(TranscriptionStage::Committed)
        .map_err(|error| TranscriptionError::storage(error, CommitStatus::Committed))
}

/// Structural validation that needs no database.
fn validate_outcome(outcome: &TranscriptionOutcome) -> Result<(), TranscriptionRejection> {
    use TranscriptionRejection::{MalformedField, MissingField, TooLarge};

    if outcome.job_id.is_empty() {
        return Err(MissingField("job_id"));
    }
    if outcome.lease_attempt < 1 {
        return Err(MalformedField("lease_attempt"));
    }
    if outcome.audio_reference.is_empty() {
        return Err(MissingField("audio_reference"));
    }
    if outcome.audio_reference.contains('\0') {
        return Err(MalformedField("audio_reference"));
    }
    if let Some(request) = &outcome.interpretation {
        if request.job_id.is_empty() {
            return Err(MissingField("interpretation.job_id"));
        }
        if request.request_version.is_empty() {
            return Err(MissingField("interpretation.request_version"));
        }
        if request.profile_version.as_deref() == Some("") {
            return Err(MalformedField("interpretation.profile_version"));
        }
        if request.job_id == outcome.job_id {
            return Err(MalformedField("interpretation.job_id"));
        }
    }

    let RecognizerResult::Transcript(transcript) = &outcome.result else {
        return Ok(());
    };
    if transcript.text.len() > MAX_TRANSCRIPT_BYTES {
        return Err(TooLarge("text"));
    }
    if transcript.text.contains('\0') {
        return Err(MalformedField("text"));
    }
    if transcript.recognizer.trim().is_empty() {
        return Err(MissingField("recognizer"));
    }
    if transcript.recognizer.len() > MAX_RECOGNIZER_BYTES {
        return Err(TooLarge("recognizer"));
    }
    let well_formed_digest = transcript.audio_sha256.len() == AUDIO_SHA256_HEX_LENGTH
        && transcript
            .audio_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !well_formed_digest {
        return Err(MalformedField("audio_sha256"));
    }
    if let Some(language) = &transcript.detected_language {
        if language.is_empty() || language.len() > MAX_LANGUAGE_BYTES {
            return Err(MalformedField("detected_language"));
        }
    }
    if let Some(confidence) = transcript.confidence {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(MalformedField("confidence"));
        }
    }
    Ok(())
}

struct PendingOutcome {
    state: TranscriptionState,
    reason: &'static str,
}

enum Resolved<'a> {
    Transcript(&'a RecognizedTranscript),
    Pending(PendingOutcome),
}

/// A blank transcript is an abstention: recognition succeeded but produced no text to attach.
fn resolve(result: &RecognizerResult) -> Resolved<'_> {
    match result {
        RecognizerResult::Transcript(transcript) if !transcript.text.trim().is_empty() => {
            Resolved::Transcript(transcript)
        }
        RecognizerResult::Transcript(_) => Resolved::Pending(PendingOutcome {
            state: TranscriptionState::AudioPending,
            reason: NO_SPEECH_REASON,
        }),
        RecognizerResult::Unsupported => Resolved::Pending(PendingOutcome {
            state: TranscriptionState::TranscriptionUnsupported,
            reason: UNSUPPORTED_REASON,
        }),
        RecognizerResult::Failed => Resolved::Pending(PendingOutcome {
            state: TranscriptionState::TranscriptionFailed,
            reason: FAILED_REASON,
        }),
    }
}

fn load_transcription_job(
    transaction: &Transaction<'_>,
    outcome: &TranscriptionOutcome,
) -> Result<Job, TranscriptionError> {
    let job = get_job_internal(transaction, &outcome.job_id)?.ok_or_else(|| {
        TranscriptionError::not_committed(TranscriptionErrorKind::LeaseNotHeld {
            reason: "job not found",
        })
    })?;
    if job.job_type != JOB_TYPE_TRANSCRIPTION_ATTACHMENT {
        return Err(TranscriptionError::not_committed(
            TranscriptionErrorKind::NotATranscriptionJob {
                job_type: job.job_type,
            },
        ));
    }
    Ok(job)
}

fn require_lease(job: &Job, lease_attempt: i32) -> Result<(), TranscriptionError> {
    let reason = match job.status {
        JobStatus::Running if job.attempt_count == lease_attempt => return Ok(()),
        JobStatus::Running => "lease belongs to a later attempt",
        JobStatus::Queued => "job is not running",
        JobStatus::Completed => "job already completed",
        JobStatus::Failed => "job already failed",
        JobStatus::Cancelled => "job was cancelled",
    };
    Err(TranscriptionError::not_committed(
        TranscriptionErrorKind::LeaseNotHeld { reason },
    ))
}

struct ItemRow {
    revision: i32,
    lifecycle_state: String,
    transcription_state: TranscriptionState,
    capture_text: Option<String>,
    audio_reference: Option<String>,
}

fn load_item(transaction: &Transaction<'_>, item_id: &str) -> Result<ItemRow, TranscriptionError> {
    let row = transaction
        .query_row(
            "SELECT i.revision, i.lifecycle_state, i.transcription_state,
                    c.text, c.audio_reference
               FROM items i JOIN captures c ON c.capture_id = i.capture_id
              WHERE i.item_id = ?",
            [item_id],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("item {item_id} of the job does not exist"))?;
    let transcription_state = row
        .2
        .parse::<TranscriptionState>()
        .map_err(|error| anyhow!("stored transcription state is invalid: {}", error.0))?;
    Ok(ItemRow {
        revision: row.0,
        lifecycle_state: row.1,
        transcription_state,
        capture_text: row.3,
        audio_reference: row.4,
    })
}

fn transcript_event_id(job_id: &str) -> String {
    format!("transcript-attachment-{job_id}")
}

/// The job already attached its transcript in an earlier delivery.
fn redelivery(
    transaction: &Transaction<'_>,
    item: &ItemRow,
    stored: &Event,
    resolved: &Resolved<'_>,
    job: &Job,
) -> Result<TranscriptionDisposition, TranscriptionError> {
    if item.lifecycle_state == "deleted" {
        return Ok(TranscriptionDisposition::Discarded(
            DiscardReason::ItemDeleted,
        ));
    }
    let Resolved::Transcript(transcript) = resolved else {
        return Err(TranscriptionError::not_committed(
            TranscriptionErrorKind::ConflictingResult,
        ));
    };
    let attached_revision = stored.revision + 1;
    let same_text = matches!(
        &stored.payload,
        EventPayload::Correction(correction) if correction.new_value == transcript.text
    );
    if !same_text {
        return Err(TranscriptionError::not_committed(
            TranscriptionErrorKind::ConflictingResult,
        ));
    }
    let queued_interpretation: Option<String> = transaction
        .query_row(
            "SELECT job_id FROM jobs WHERE item_id = ? AND job_type = ? AND source_revision = ?
              ORDER BY created_at ASC, rowid ASC LIMIT 1",
            rusqlite::params![job.item_id, JOB_TYPE_INTERPRET, attached_revision],
            |row| row.get(0),
        )
        .optional()?;
    Ok(TranscriptionDisposition::AlreadyAttached {
        revision: attached_revision,
        interpretation: match queued_interpretation {
            Some(job_id) => InterpretationQueued::Queued { job_id },
            None => InterpretationQueued::NotRequested,
        },
    })
}

/// A pending outcome (unsupported, failed, no speech) that this job already ended with.
fn is_settled_pending(job: &Job, lease_attempt: i32, pending: &PendingOutcome) -> bool {
    job.status == JobStatus::Failed
        && job.attempt_count == lease_attempt
        && job.failure_reason.as_deref() == Some(pending.reason)
}

/// Whether the current durable state forbids applying any outcome for this job.
fn discard_reason(
    transaction: &Transaction<'_>,
    job: &Job,
    item: &ItemRow,
    outcome: &TranscriptionOutcome,
) -> Result<Option<DiscardReason>, TranscriptionError> {
    if item.lifecycle_state == "deleted" {
        return Ok(Some(DiscardReason::ItemDeleted));
    }
    match item.transcription_state {
        TranscriptionState::Transcribed => return Ok(Some(DiscardReason::AlreadyTranscribed)),
        TranscriptionState::NotApplicable => return Ok(Some(DiscardReason::NotAudioCapture)),
        TranscriptionState::AudioPending
        | TranscriptionState::Transcribing
        | TranscriptionState::TranscriptionUnsupported
        | TranscriptionState::TranscriptionFailed => {}
    }
    let user_text_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM corrections WHERE item_id = ? AND kind = 'text')",
        [&job.item_id],
        |row| row.get(0),
    )?;
    let capture_has_text = item
        .capture_text
        .as_deref()
        .is_some_and(|text| !text.is_empty());
    if user_text_exists || capture_has_text {
        return Ok(Some(DiscardReason::TextAlreadyPresent));
    }
    Ok(match item.audio_reference.as_deref() {
        None => Some(DiscardReason::AudioUnavailable),
        Some(stored) if stored != outcome.audio_reference => {
            Some(DiscardReason::AudioIdentityMismatch)
        }
        Some(_) => None,
    })
}

fn retire_job(
    transaction: &Transaction<'_>,
    job: &Job,
    lease_attempt: i32,
    reason: &str,
) -> Result<(), TranscriptionError> {
    settle_running_job(
        transaction,
        job,
        lease_attempt,
        JobStatus::Cancelled,
        reason,
    )
}

fn end_job_failed(
    transaction: &Transaction<'_>,
    job: &Job,
    lease_attempt: i32,
    reason: &str,
) -> Result<(), TranscriptionError> {
    settle_running_job(transaction, job, lease_attempt, JobStatus::Failed, reason)
}

fn settle_running_job(
    transaction: &Transaction<'_>,
    job: &Job,
    lease_attempt: i32,
    status: JobStatus,
    reason: &str,
) -> Result<(), TranscriptionError> {
    let affected = transaction.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL
          WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            status.as_str(),
            reason,
            job.job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!("job {} lease is no longer held", job.job_id).into());
    }
    Ok(())
}

fn require_unused_job_id(
    transaction: &Transaction<'_>,
    request: &InterpretationRequest,
) -> Result<(), TranscriptionError> {
    let in_use: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE job_id = ?)",
        [&request.job_id],
        |row| row.get(0),
    )?;
    if in_use {
        return Err(TranscriptionError::not_committed(
            TranscriptionErrorKind::Validation(TranscriptionRejection::InterpretationJobIdInUse),
        ));
    }
    Ok(())
}

/// Write the transcript as the item's text correction. Returns the item revision that includes
/// the transcript.
fn attach_transcript(
    transaction: &Transaction<'_>,
    job: &Job,
    item: &ItemRow,
    transcript: &RecognizedTranscript,
    transcribed_at: DateTime<Utc>,
    fault_point: &mut dyn FnMut(TranscriptionStage) -> anyhow::Result<()>,
) -> Result<i32, TranscriptionError> {
    let event = Event::new(
        transcript_event_id(&job.job_id),
        job.item_id.clone(),
        item.revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: None,
            new_value: transcript.text.clone(),
        }),
        transcribed_at.to_rfc3339(),
    )?;
    save_event_in_tx(transaction, &event, item.revision)
        .map_err(|error| TranscriptionError::from(anyhow::Error::new(error)))?;
    fault_point(TranscriptionStage::EventWritten)?;

    let attached_revision = item.revision + 1;
    transaction.execute(
        "UPDATE items SET transcription_state = ? WHERE item_id = ?",
        rusqlite::params![TranscriptionState::Transcribed.as_str(), job.item_id],
    )?;
    fault_point(TranscriptionStage::StateUpdated)?;
    Ok(attached_revision)
}

/// Queue the interpretation job at the post-transcript revision, and keep it only if the core
/// authorizes it. A denied job is rolled back so it never exists.
fn queue_interpretation(
    transaction: &Transaction<'_>,
    item_id: &str,
    revision: i32,
    request: &InterpretationRequest,
    now: DateTime<Utc>,
) -> Result<InterpretationQueued, TranscriptionError> {
    transaction.execute_batch("SAVEPOINT interpretation_queue")?;
    enqueue_job_in_tx(
        transaction,
        request.job_id.clone(),
        item_id.to_string(),
        JOB_TYPE_INTERPRET.to_string(),
        revision,
        request.profile_version.clone(),
        Some(request.request_version.clone()),
        INTERPRETATION_JOB_SCHEMA_VERSION,
        now,
    )?;
    match authorize_job(transaction, &request.job_id)? {
        AuthorizationDecision::Authorized(_) => {
            transaction.execute_batch("RELEASE interpretation_queue")?;
            Ok(InterpretationQueued::Queued {
                job_id: request.job_id.clone(),
            })
        }
        AuthorizationDecision::Denied(reason) => {
            transaction
                .execute_batch("ROLLBACK TO interpretation_queue; RELEASE interpretation_queue")?;
            Ok(InterpretationQueued::NotAuthorized(reason))
        }
    }
}

#[cfg(test)]
mod tests;
