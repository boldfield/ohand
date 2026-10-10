//! Native transcription of queued voice captures over the core handle (C05b).
//!
//! The job runner (J02b) hands a `transcription_attachment` job to the native host it holds. The
//! host reads what it needs to recognize the audio and hands the outcome back; the guarded
//! transcript attachment (C05a, `ingress::apply_transcription_outcome`) alone decides what changes.
//!
//! * `ohand_core_start_read_transcription_job` reads the running job under its lease: the audio
//!   reference and capture locale to recognize, and the item's lifecycle and transcription state
//!   so a host can skip recognition of audio whose item is gone. It writes nothing.
//! * `ohand_core_start_apply_transcription_outcome` applies a recognizer outcome in one core
//!   transaction. The outcome is a transcript, `unsupported` (language or on-device model not
//!   available) or `failed`. The transcript is bounded; the failure reasons are fixed labels.
//!   Whatever the core accepts or refuses is reported with a content-free disposition; the host
//!   then answers the runner with `settled`.
//!
//! Neither export can start recognition, reach a provider or change anything but through the
//! guarded operation: a result presented under a lapsed or foreign lease, for a deleted or
//! already-texted item, or made from different audio, is refused or retired by the core and never
//! replaces authoritative content. Failure messages are fixed strings and never echo the request.
//!
//! Failure codes of the apply export:
//!
//! * `transcription_rejected`: the outcome is malformed (empty or oversized field, bad digest or
//!   confidence); nothing was written.
//! * `transcription_wrong_job_type`: the job is not a transcription job; nothing was written.
//! * `transcription_lease_not_held`: the job is unknown or no longer running under this lease (a
//!   stale or duplicate delivery); nothing was written.
//! * `transcription_not_authorized`: processing of the job is not authorized; nothing was written.
//! * `transcription_conflicting_result`: the job already attached a different transcript.
//! * `transcription_not_committed`: a storage fault; nothing was written, retry is safe.
//! * `transcription_commit_unknown`: the commit outcome cannot be asserted; retry with the same
//!   outcome, which converges.

use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::ingress::transcription::MAX_TRANSCRIPT_BYTES;
use crate::ingress::{
    apply_transcription_outcome, CommitStatus, DiscardReason, InterpretationQueued,
    InterpretationRequest, RecognizedTranscript, RecognizerResult, TranscriptionDisposition,
    TranscriptionError, TranscriptionErrorKind, TranscriptionOutcome,
};
use crate::jobs::queue::{get_job, JobStatus};
use crate::privacy::routing::{DenialReason, JOB_TYPE_TRANSCRIPTION_ATTACHMENT};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Mutex;

/// Largest transcription request accepted, in bytes: a transcript at its limit with every
/// character escaped in JSON still fits.
pub const OHAND_CORE_MAX_TRANSCRIPTION_REQUEST_BYTES: usize = 2_097_152;

const fn failure(class: ErrorClass, code: &'static str, message: &'static str) -> AbiFailure {
    AbiFailure {
        class,
        code,
        message: Cow::Borrowed(message),
    }
}

const REJECTED: AbiFailure = failure(
    ErrorClass::Permanent,
    "transcription_rejected",
    "the transcription outcome is malformed",
);
const LEASE_NOT_HELD: AbiFailure = failure(
    ErrorClass::Permanent,
    "transcription_lease_not_held",
    "the job is not running under the presented lease",
);

fn storage_failure(error: impl Into<anyhow::Error>) -> AbiFailure {
    AbiFailure::from_store(&error.into(), AbiFailure::STORAGE_ERROR)
}

fn to_json(value: &impl Serialize) -> Result<Vec<u8>, AbiFailure> {
    serde_json::to_vec(value).map_err(|_| AbiFailure::INTERNAL)
}

/// Copies the caller's bytes. The length is checked before the pointer is read.
///
/// # Safety
/// `bytes` must point at `len` readable bytes (it may be null only when `len` is 0).
unsafe fn copy_request(bytes: *const u8, len: usize) -> Result<Vec<u8>, AbiFailure> {
    if len > OHAND_CORE_MAX_TRANSCRIPTION_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if bytes.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    Ok(std::slice::from_raw_parts(bytes, len).to_vec())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    job_id: String,
    lease_attempt: i32,
}

#[derive(Serialize)]
struct SourceReadout {
    operation_id: u64,
    job_id: String,
    item_id: String,
    capture_id: String,
    lease_attempt: i32,
    /// The audio file reference stored with the capture; `null` when the capture has no audio.
    audio_reference: Option<String>,
    /// The locale the capture was made in, such as `en_US`.
    locale: String,
    lifecycle_state: String,
    transcription_state: String,
}

fn read_job(request: ReadRequest, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let job = get_job(&database, &request.job_id)
            .map_err(storage_failure)?
            .ok_or(LEASE_NOT_HELD)?;
        if job.job_type != JOB_TYPE_TRANSCRIPTION_ATTACHMENT {
            return Err(failure(
                ErrorClass::Permanent,
                "transcription_wrong_job_type",
                "the job is not a transcription job",
            ));
        }
        if job.status != JobStatus::Running || job.attempt_count != request.lease_attempt {
            return Err(LEASE_NOT_HELD);
        }
        let row = database
            .conn()
            .query_row(
                "SELECT i.item_id, c.capture_id, c.audio_reference, c.locale, i.lifecycle_state,
                        i.transcription_state
                   FROM items i JOIN captures c ON c.capture_id = i.capture_id
                  WHERE i.item_id = ?",
                [&job.item_id],
                |row| {
                    Ok(SourceReadout {
                        operation_id,
                        job_id: job.job_id.clone(),
                        item_id: row.get(0)?,
                        capture_id: row.get(1)?,
                        lease_attempt: job.attempt_count,
                        audio_reference: row.get(2)?,
                        locale: row.get(3)?,
                        lifecycle_state: row.get(4)?,
                        transcription_state: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(storage_failure)?
            .ok_or(AbiFailure::NOT_FOUND)?;
        to_json(&row)
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResultKind {
    Transcript,
    Unsupported,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptWire {
    text: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    detected_language: Option<String>,
    recognizer: String,
    audio_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterpretationWire {
    job_id: String,
    #[serde(default)]
    profile_version: Option<String>,
    request_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    job_id: String,
    lease_attempt: i32,
    audio_reference: String,
    result: ResultKind,
    #[serde(default)]
    transcript: Option<TranscriptWire>,
    #[serde(default)]
    interpretation: Option<InterpretationWire>,
}

impl ApplyRequest {
    /// The result and its transcript must agree; only structural consistency is checked here, the
    /// guarded operation validates every field.
    fn into_outcome(self) -> Result<TranscriptionOutcome, AbiFailure> {
        let result = match (self.result, self.transcript) {
            (ResultKind::Transcript, Some(transcript)) => {
                if transcript.text.len() > MAX_TRANSCRIPT_BYTES {
                    return Err(REJECTED);
                }
                RecognizerResult::Transcript(RecognizedTranscript {
                    text: transcript.text,
                    confidence: transcript.confidence,
                    detected_language: transcript.detected_language,
                    recognizer: transcript.recognizer,
                    audio_sha256: transcript.audio_sha256,
                })
            }
            (ResultKind::Unsupported, None) => RecognizerResult::Unsupported,
            (ResultKind::Failed, None) => RecognizerResult::Failed,
            _ => return Err(AbiFailure::INVALID_REQUEST),
        };
        Ok(TranscriptionOutcome {
            job_id: self.job_id,
            lease_attempt: self.lease_attempt,
            audio_reference: self.audio_reference,
            result,
            interpretation: self.interpretation.map(|request| InterpretationRequest {
                job_id: request.job_id,
                profile_version: request.profile_version,
                request_version: request.request_version,
            }),
        })
    }
}

#[derive(Serialize)]
struct InterpretationReadout {
    status: &'static str,
    job_id: Option<String>,
    denial: Option<&'static str>,
}

#[derive(Serialize)]
struct ApplyReadout {
    operation_id: u64,
    item_id: String,
    job_id: String,
    /// `attached`, `already_attached`, `pending` or `discarded`.
    disposition: &'static str,
    /// The item revision that includes the transcript, for the two attached dispositions.
    revision: Option<i32>,
    /// The transcription state the item reports after a `pending` outcome.
    transcription_state: Option<&'static str>,
    /// The content-free reason a `discarded` outcome was retired with.
    discard_reason: Option<&'static str>,
    interpretation: Option<InterpretationReadout>,
}

fn denial_code(reason: DenialReason) -> &'static str {
    match reason {
        DenialReason::JobNotFound => "job_not_found",
        DenialReason::JobRetired => "job_retired",
        DenialReason::UnknownJobType => "unknown_job_type",
        DenialReason::LocalOnlyJobHasProfile => "local_only_job_has_profile",
        DenialReason::ProfileUnavailable => "profile_unavailable",
        DenialReason::ProfileRevoked => "profile_revoked",
        DenialReason::UnknownProviderType => "unknown_provider_type",
        DenialReason::MalformedProfile => "malformed_profile",
        DenialReason::RouteNotConfigured => "route_not_configured",
        DenialReason::MalformedPolicy => "malformed_policy",
        DenialReason::DestinationNotInRoute => "destination_not_in_route",
        DenialReason::CapabilityNotAuthorized => "capability_not_authorized",
        DenialReason::DestinationNotAuthorizedForCapability => {
            "destination_not_authorized_for_capability"
        }
    }
}

fn interpretation_readout(queued: &InterpretationQueued) -> InterpretationReadout {
    match queued {
        InterpretationQueued::NotRequested => InterpretationReadout {
            status: "not_requested",
            job_id: None,
            denial: None,
        },
        InterpretationQueued::Queued { job_id } => InterpretationReadout {
            status: "queued",
            job_id: Some(job_id.clone()),
            denial: None,
        },
        InterpretationQueued::NotAuthorized(reason) => InterpretationReadout {
            status: "not_authorized",
            job_id: None,
            denial: Some(denial_code(*reason)),
        },
    }
}

fn discard_code(reason: DiscardReason) -> &'static str {
    reason.job_reason()
}

fn apply_failure(error: &TranscriptionError) -> AbiFailure {
    match &error.kind {
        TranscriptionErrorKind::Validation(_) => REJECTED,
        TranscriptionErrorKind::NotATranscriptionJob { .. } => failure(
            ErrorClass::Permanent,
            "transcription_wrong_job_type",
            "the job is not a transcription job",
        ),
        TranscriptionErrorKind::LeaseNotHeld { .. } => LEASE_NOT_HELD,
        TranscriptionErrorKind::NotAuthorized(_) => failure(
            ErrorClass::Unauthorized,
            "transcription_not_authorized",
            "processing of the transcription job is not authorized",
        ),
        TranscriptionErrorKind::ConflictingResult => failure(
            ErrorClass::Permanent,
            "transcription_conflicting_result",
            "the job already attached a different transcript",
        ),
        TranscriptionErrorKind::Storage(cause) => match error.commit_status {
            CommitStatus::NotCommitted => failure(
                AbiFailure::from_store(cause, AbiFailure::STORAGE_ERROR).class,
                "transcription_not_committed",
                "the outcome did not commit; nothing was written",
            ),
            CommitStatus::Unknown | CommitStatus::Committed => failure(
                ErrorClass::Transient,
                "transcription_commit_unknown",
                "the outcome is unknown; retry with the same outcome",
            ),
        },
    }
}

fn apply_job(outcome: TranscriptionOutcome, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let acknowledgment = apply_transcription_outcome(&mut database, &outcome, Utc::now())
            .map_err(|error| apply_failure(&error))?;
        let mut readout = ApplyReadout {
            operation_id,
            item_id: acknowledgment.item_id,
            job_id: acknowledgment.job_id,
            disposition: "",
            revision: None,
            transcription_state: None,
            discard_reason: None,
            interpretation: None,
        };
        match &acknowledgment.disposition {
            TranscriptionDisposition::Attached {
                revision,
                interpretation,
            } => {
                readout.disposition = "attached";
                readout.revision = Some(*revision);
                readout.interpretation = Some(interpretation_readout(interpretation));
            }
            TranscriptionDisposition::AlreadyAttached {
                revision,
                interpretation,
            } => {
                readout.disposition = "already_attached";
                readout.revision = Some(*revision);
                readout.interpretation = Some(interpretation_readout(interpretation));
            }
            TranscriptionDisposition::Pending { state } => {
                readout.disposition = "pending";
                readout.transcription_state = Some(state.as_str());
            }
            TranscriptionDisposition::Discarded(reason) => {
                readout.disposition = "discarded";
                readout.discard_reason = Some(discard_code(*reason));
            }
        }
        to_json(&readout)
    })
}

/// Queues a read of the running transcription job described by the `request_len` JSON bytes at
/// `request` (`{"job_id","lease_attempt"}`; unknown fields are rejected). The outcome event for
/// `operation_id` holds `{"operation_id","job_id","item_id","capture_id","lease_attempt",
/// "audio_reference","locale","lifecycle_state","transcription_state"}`. It is the failure
/// `transcription_lease_not_held` when the job is unknown or is not running under that lease,
/// and `transcription_wrong_job_type` for any other kind of job. Nothing is written.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_read_transcription_job(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = copy_request(request, request_len)?;
        let request: ReadRequest =
            serde_json::from_slice(&bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(operation_id, read_job(request, operation_id))
    })
}

/// Queues the guarded attachment of the recognizer outcome described by the `request_len` JSON
/// bytes at `request`: `{"job_id","lease_attempt","audio_reference","result","transcript",
/// "interpretation"}` where `result` is `transcript`, `unsupported` or `failed`, `transcript`
/// (`text`, `confidence`, `detected_language`, `recognizer`, `audio_sha256`) is present exactly
/// for `transcript`, and `interpretation` (`job_id`, `profile_version`, `request_version`) is
/// optional. Unknown fields are rejected. The outcome event for `operation_id` is success only
/// after the transaction committed (or an earlier identical delivery had), with JSON
/// `{"operation_id","item_id","job_id","disposition","revision","transcription_state",
/// "discard_reason","interpretation"}`, or a normalized failure (see the module documentation).
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_apply_transcription_outcome(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = copy_request(request, request_len)?;
        let request: ApplyRequest =
            serde_json::from_slice(&bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let outcome = request.into_outcome()?;
        core.submit(operation_id, apply_job(outcome, operation_id))
    })
}

#[cfg(test)]
mod tests;
