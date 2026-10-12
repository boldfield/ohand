//! Audio retention policy over the core handle (L03b).
//!
//! `ohand_core_start_audio_retention` exposes the read-only L03a retention policy
//! (`lifecycle::retention`) to the native cleanup service as one tagged JSON command per call. The
//! clock is the system clock; nothing here writes a row or removes a file. Each command runs on
//! the handle's worker thread and its outcome event for `operation_id` is `{"operation_id", ...}`:
//!
//! * `sweep` (`pressure`: `normal` or `low`, optional `period_seconds`) -> `assessments`: one entry
//!   per live item that holds audio, with `state`, `sole_source`, the explicit `options`, the
//!   `cleanup` intent and any `storage_pressure_notice`.
//! * `authorize_removal` (`item_id`, `audio_reference`, optional `period_seconds`) -> `decision`:
//!   `approved` (with `expired_at`) or `denied` (with `reason`). A read-only question; it does not
//!   permit a removal, which only a `begin_removal` lease does.
//!
//! * `begin_removal` (`item_id`, `audio_reference`, optional `period_seconds`) -> the same
//!   `decision` as `authorize_removal`, decided inside a store-wide fence. When it is `approved`
//!   the answer also carries `lease_id` and `lease_millis`: the store's write lock is held, so no
//!   other writer on any handle, connection or process can commit until the native caller has
//!   removed the one approved file and ended the lease with `ohand_core_release_audio_removal`
//!   (which is not queued behind the worker, because the worker may be waiting on that lock), or
//!   `lease_millis` pass without the lease being pinned. Native code pins the lease with
//!   `ohand_core_pin_audio_removal` immediately before the file effect; a pinned lease cannot
//!   expire, so the fence cannot lapse during the effect. A denial holds nothing. The file effect is native: the core never opens,
//!   reads or removes an audio file. See [`lease`] for the guarantees and the expiry rule.
//!
//! `period_seconds` is the configured retention period; absent or 0 selects the default of seven
//! days, and a value outside the policy's bounds is refused as `invalid_request`. Failures are
//! normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::lifecycle::retention::{
    authorize_audio_removal, sweep_audio_retention, AudioRetentionAssessment, CleanupIntent,
    RemovalDecision, RemovalDenial, RetentionOption, RetentionPolicy, RetentionState,
    StoragePressure, StoragePressureNotice, UnresolvedReason,
};
use crate::store::schema::{Database, SystemClock};
use chrono::Duration;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;

/// Longest identifier or audio reference accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

mod lease;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Pressure {
    Normal,
    Low,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Sweep {
        pressure: Pressure,
        #[serde(default)]
        period_seconds: u64,
    },
    AuthorizeRemoval {
        item_id: String,
        audio_reference: String,
        #[serde(default)]
        period_seconds: u64,
    },
    BeginRemoval {
        item_id: String,
        audio_reference: String,
        #[serde(default)]
        period_seconds: u64,
    },
}

impl Command {
    fn policy(&self) -> Result<RetentionPolicy, AbiFailure> {
        let period_seconds = match self {
            Command::Sweep { period_seconds, .. }
            | Command::AuthorizeRemoval { period_seconds, .. }
            | Command::BeginRemoval { period_seconds, .. } => *period_seconds,
        };
        if period_seconds == 0 {
            return Ok(RetentionPolicy::default());
        }
        let seconds = i64::try_from(period_seconds).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        let period = Duration::try_seconds(seconds).ok_or(AbiFailure::INVALID_REQUEST)?;
        RetentionPolicy::new(period).map_err(|_| AbiFailure::INVALID_REQUEST)
    }

    fn validate(&self) -> Result<(), AbiFailure> {
        match self {
            Command::Sweep { .. } => {}
            Command::AuthorizeRemoval {
                item_id,
                audio_reference,
                ..
            }
            | Command::BeginRemoval {
                item_id,
                audio_reference,
                ..
            } => validate_identifiers(item_id, audio_reference)?,
        }
        self.policy().map(|_| ())
    }
}

fn validate_identifiers(item_id: &str, audio_reference: &str) -> Result<(), AbiFailure> {
    for value in [item_id, audio_reference] {
        if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
            return Err(AbiFailure::INVALID_REQUEST);
        }
    }
    Ok(())
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn unresolved_reason(reason: UnresolvedReason) -> &'static str {
    match reason {
        UnresolvedReason::TranscriptionPending => "transcription_pending",
        UnresolvedReason::TranscriptionUnsupported => "transcription_unsupported",
        UnresolvedReason::TranscriptionFailed => "transcription_failed",
        UnresolvedReason::TextWithoutTranscript => "text_without_transcript",
        UnresolvedReason::TranscriptRecordInconsistent => "transcript_record_inconsistent",
        UnresolvedReason::TranscriptEmpty => "transcript_empty",
        UnresolvedReason::TranscriptAudioMismatch => "transcript_audio_mismatch",
        UnresolvedReason::TranscriptionTimeUnreadable => "transcription_time_unreadable",
    }
}

fn state_json(state: &RetentionState) -> Value {
    match state {
        RetentionState::Retained { expires_at } => {
            json!({ "kind": "retained", "expires_at": expires_at.to_rfc3339() })
        }
        RetentionState::Expired { expired_at } => {
            json!({ "kind": "expired", "expired_at": expired_at.to_rfc3339() })
        }
        RetentionState::Unresolved { reason } => {
            json!({ "kind": "unresolved", "reason": unresolved_reason(*reason) })
        }
    }
}

fn option_name(option: RetentionOption) -> &'static str {
    match option {
        RetentionOption::RetryTranscription => "retry_transcription",
        RetentionOption::DeleteAudio => "delete_audio",
    }
}

fn notice_name(notice: StoragePressureNotice) -> &'static str {
    match notice {
        StoragePressureNotice::SoleSourceRetained => "sole_source_retained",
        StoragePressureNotice::NotYetEligible => "not_yet_eligible",
    }
}

fn assessment_json(assessment: &AudioRetentionAssessment) -> Value {
    let cleanup = match &assessment.cleanup {
        CleanupIntent::None => json!({ "kind": "none" }),
        CleanupIntent::RemoveAudio {
            audio_reference,
            expired_at,
        } => json!({
            "kind": "remove_audio",
            "audio_reference": audio_reference,
            "expired_at": expired_at.to_rfc3339(),
        }),
    };
    json!({
        "item_id": assessment.item_id,
        "audio_reference": assessment.audio_reference,
        "state": state_json(&assessment.state),
        "sole_source": assessment.sole_source,
        "options": assessment.options.iter().map(|option| option_name(*option)).collect::<Vec<_>>(),
        "cleanup": cleanup,
        "storage_pressure_notice": assessment.storage_pressure_notice.map(notice_name),
    })
}

fn decision_json(decision: &RemovalDecision) -> Value {
    match decision {
        RemovalDecision::Approved { expired_at } => {
            json!({ "decision": "approved", "expired_at": expired_at.to_rfc3339() })
        }
        RemovalDecision::Denied(denial) => match denial {
            RemovalDenial::ItemNotFound => {
                json!({ "decision": "denied", "reason": "item_not_found" })
            }
            RemovalDenial::AudioReferenceMismatch => {
                json!({ "decision": "denied", "reason": "audio_reference_mismatch" })
            }
            RemovalDenial::DeletionOwnsCleanup => {
                json!({ "decision": "denied", "reason": "deletion_owns_cleanup" })
            }
            RemovalDenial::NotYetExpired { expires_at } => json!({
                "decision": "denied",
                "reason": "not_yet_expired",
                "expires_at": expires_at.to_rfc3339(),
            }),
            RemovalDenial::Unresolved { reason } => json!({
                "decision": "denied",
                "reason": "unresolved",
                "unresolved_reason": unresolved_reason(*reason),
            }),
        },
    }
}

fn run(database: &Database, handle: u64, command: &Command) -> Result<Value, AbiFailure> {
    let policy = command.policy()?;
    match command {
        Command::Sweep { pressure, .. } => {
            let pressure = match pressure {
                Pressure::Normal => StoragePressure::Normal,
                Pressure::Low => StoragePressure::Low,
            };
            let assessments =
                sweep_audio_retention(database.conn(), &SystemClock, &policy, pressure)
                    .map_err(|error| store_failure(&error))?;
            Ok(json!({
                "assessments": assessments.iter().map(assessment_json).collect::<Vec<_>>(),
            }))
        }
        Command::AuthorizeRemoval {
            item_id,
            audio_reference,
            ..
        } => {
            let decision = authorize_audio_removal(
                database.conn(),
                &SystemClock,
                &policy,
                item_id,
                audio_reference,
            )
            .map_err(|error| store_failure(&error))?;
            Ok(decision_json(&decision))
        }
        Command::BeginRemoval {
            item_id,
            audio_reference,
            ..
        } => {
            let store_path = database
                .conn()
                .path()
                .map(Path::new)
                .ok_or(AbiFailure::STORE_UNAVAILABLE)?;
            let begun = lease::begin(
                store_path,
                handle,
                &SystemClock,
                &policy,
                item_id,
                audio_reference,
                lease::LEASE_DURATION,
            )?;
            let mut body = decision_json(&begun.decision);
            if let Some(lease_id) = begun.lease_id {
                body["lease_id"] = json!(lease_id);
                body["lease_millis"] =
                    json!(u64::try_from(lease::LEASE_DURATION.as_millis()).unwrap_or(u64::MAX));
            }
            Ok(body)
        }
    }
}

fn retention_job(handle: u64, command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let mut body = run(&database, handle, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one audio-retention command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands, fields and out-of-range periods are refused as
/// `invalid_request` before anything is queued). The outcome event for `operation_id` is the JSON
/// documented in the module documentation, or a normalized failure. Nothing here writes a row or
/// removes a file.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_audio_retention(
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
        core.submit(operation_id, retention_job(handle, command, operation_id))
    })
}

/// Pins the removal lease `lease_id` that `begin_removal` granted to `handle`, immediately before
/// native code carries out the file effect. Succeeds only while the store's write lock is still
/// held; from then on the lease cannot expire, and only `ohand_core_release_audio_removal` ends
/// it, so the fence cannot lapse while the effect runs. An expired or unknown lease fails as
/// transient `lease_expired` and the effect must not run (the caller still releases). Not queued,
/// for the same reason as the release.
#[no_mangle]
pub extern "C" fn ohand_core_pin_audio_removal(
    handle: OhandCoreHandle,
    lease_id: u64,
) -> OhandCoreResult {
    guarded(|| {
        instance::lookup(handle)?;
        lease::pin(handle, lease_id)
    })
}

/// Ends the removal lease `lease_id` that `begin_removal` granted to `handle`, once native code has
/// carried out (or given up) the file effect. Succeeds only if the store's write lock was held
/// without a gap until now. An expired or unknown lease fails as transient `lease_expired`: the
/// fence may have lapsed, so the removal must not be reported as done. Not queued: it runs on the
/// caller's thread because the worker may be blocked behind the lease's own lock. A lease is
/// released even if its handle has since been closed, because a pinned lease ends only here.
#[no_mangle]
pub extern "C" fn ohand_core_release_audio_removal(
    handle: OhandCoreHandle,
    lease_id: u64,
) -> OhandCoreResult {
    guarded(|| match lease::release(handle, lease_id) {
        Ok(()) => Ok(()),
        Err(failure) => {
            instance::lookup(handle)?;
            Err(failure)
        }
    })
}

#[cfg(test)]
mod tests;
