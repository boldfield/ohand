//! Configuring audio retention and acting on unresolved sole audio over the core handle (A08).
//!
//! Two exports share this module. The read carries the caller's read scopes and is meant to be
//! issued only through the native read gate; the control is a user command that native issues only
//! for an authenticated session. Neither touches a file: removing audio stays the native effect
//! inside the `begin_user_removal` fence of `audio_retention`.
//!
//! # Read: `ohand_core_start_retention_read`
//!
//! One tagged JSON object with a required, non-empty `read_scopes` (`personal` and/or `work`).
//!
//! * `{"command": "retention_status", "read_scopes"}` answers `{"operation_id", "policy",
//!   "unresolved"}`. `policy` is `{"period_seconds", "is_default"}`: the stored retention period
//!   (seven days and `is_default` true until the user stores one). The clock starts at successful
//!   transcription. `unresolved` lists the live items in the scopes whose audio is the item's only
//!   source, oldest capture first, each `{"item_id", "captured_at", "reason", "options"}` where
//!   `reason` is the unresolved reason `audio_retention` reports and `options` is a subset of
//!   `retry_transcription` and `delete_audio`. Such audio never expires, whatever the period is;
//!   the way out is to retry transcription or to delete the item. Storage pressure is measured by
//!   native code, not here.
//! * `{"command": "get_audio", "item_id", "read_scopes"}` answers `{"operation_id", "item_id",
//!   "audio_reference", "sole_source"}`. `audio_reference` is `null` when the item holds no audio.
//!   An item that is unknown, deleted or outside the scopes fails as `not_found`, indistinguishably.
//!
//! # Control: `ohand_core_start_retention_control`
//!
//! * `{"command": "set_period", "period_seconds"}` stores the retention period, from one second to
//!   3650 days; anything else is `invalid_request` and nothing is stored. It answers
//!   `{"operation_id", "outcome": "stored", "period_seconds"}`. Changing the period only moves
//!   when transcribed audio expires. It deletes nothing, and audio that is an item's only source
//!   never expires.
//! * `{"command": "retry_transcription", "item_id"}` re-queues on-device transcription through
//!   `requeue_transcription`. Outcomes: `requeued` (`job_id`); `item_deleted`; `audio_removed`;
//!   `not_retryable` (`transcription_state`); `job_in_flight`. A refusal wrote nothing. An unknown
//!   item fails as `not_found`.
//!
//! Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::domain::items::{load_item_state, LifecycleState};
use crate::ingress::transcription::queue::{requeue_transcription, RequeueError, RequeueRefusal};
use crate::lifecycle::retention::{
    assess_audio_retention, stored_retention_policy, unresolved_sole_audio, RetentionOption,
    RetentionPolicy, StoragePressure, UnresolvedReason,
};
use crate::store::events::ItemScope;
use crate::store::schema::{Clock, Database, SystemClock};
use chrono::{Duration, SecondsFormat};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

const MAX_IDENTIFIER_BYTES: usize = 512;

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

fn acceptable_identifier(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES
}

fn reason_name(reason: UnresolvedReason) -> &'static str {
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

fn option_name(option: RetentionOption) -> &'static str {
    match option {
        RetentionOption::RetryTranscription => "retry_transcription",
        RetentionOption::DeleteAudio => "delete_audio",
    }
}

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

/// What a read asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetentionReadKind {
    Status,
    Audio { item_id: String },
}

/// One read of the retention setting or an item's audio, limited to the scopes the caller may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionRead {
    pub kind: RetentionReadKind,
    pub read_scopes: Vec<ItemScope>,
}

fn period_seconds(policy: &RetentionPolicy) -> i64 {
    policy.period().num_seconds()
}

fn policy_json(connection: &Connection) -> Result<Value, AbiFailure> {
    let policy = stored_retention_policy(connection).map_err(|error| store_failure(&error))?;
    let stored: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM retention_settings WHERE singleton = 1)",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_failure)?;
    Ok(json!({ "period_seconds": period_seconds(&policy), "is_default": !stored }))
}

/// Reads the retention policy with the unresolved sole-audio cases, or one item's audio
/// reference. An unknown, deleted or out-of-scope item is `NOT_FOUND`. Writes nothing.
pub fn read_retention(database: &mut Database, read: &RetentionRead) -> Result<Value, AbiFailure> {
    let transaction = database
        .transaction()
        .map_err(|error| store_failure(&error))?;
    let connection = &transaction;
    match &read.kind {
        RetentionReadKind::Status => {
            let cases = unresolved_sole_audio(connection).map_err(|error| store_failure(&error))?;
            let mut unresolved = Vec::new();
            for case in cases {
                let in_scope = load_item_state(&transaction, &case.item_id)
                    .map_err(|error| store_failure(&error))?
                    .is_some_and(|item| read.read_scopes.contains(&item.scope));
                if !in_scope {
                    continue;
                }
                unresolved.push(json!({
                    "item_id": case.item_id,
                    "captured_at": case.captured_at,
                    "reason": reason_name(case.reason),
                    "options": case.options.iter().map(|option| option_name(*option)).collect::<Vec<_>>(),
                }));
            }
            Ok(json!({ "policy": policy_json(connection)?, "unresolved": unresolved }))
        }
        RetentionReadKind::Audio { item_id } => {
            let item = load_item_state(&transaction, item_id)
                .map_err(|error| store_failure(&error))?
                .filter(|item| {
                    item.lifecycle_state != LifecycleState::Deleted
                        && read.read_scopes.contains(&item.scope)
                })
                .ok_or(AbiFailure::NOT_FOUND)?;
            let policy =
                stored_retention_policy(connection).map_err(|error| store_failure(&error))?;
            let assessment = assess_audio_retention(
                connection,
                &SystemClock,
                &policy,
                StoragePressure::Normal,
                &item.item_id,
            )
            .map_err(|error| store_failure(&error))?;
            Ok(json!({
                "item_id": item.item_id,
                "audio_reference": assessment.as_ref().map(|found| found.audio_reference.clone()),
                "sole_source": assessment.as_ref().is_some_and(|found| found.sole_source),
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ReadCommand {
    RetentionStatus {
        read_scopes: Vec<String>,
    },
    GetAudio {
        item_id: String,
        read_scopes: Vec<String>,
    },
}

impl ReadCommand {
    fn into_read(self) -> Result<RetentionRead, AbiFailure> {
        let (kind, scope_names) = match self {
            ReadCommand::RetentionStatus { read_scopes } => {
                (RetentionReadKind::Status, read_scopes)
            }
            ReadCommand::GetAudio {
                item_id,
                read_scopes,
            } => {
                if !acceptable_identifier(&item_id) {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                (RetentionReadKind::Audio { item_id }, read_scopes)
            }
        };
        let mut read_scopes: Vec<ItemScope> = Vec::new();
        for name in &scope_names {
            let scope = ItemScope::from_str(name).map_err(|_| AbiFailure::INVALID_REQUEST)?;
            if !read_scopes.contains(&scope) {
                read_scopes.push(scope);
            }
        }
        if read_scopes.is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(RetentionRead { kind, read_scopes })
    }
}

// ---------------------------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------------------------

/// Stores the retention period. Replaces any earlier value; repeating a value changes nothing.
pub fn store_retention_period(
    database: &Database,
    clock: &dyn Clock,
    policy: &RetentionPolicy,
) -> Result<(), AbiFailure> {
    database
        .conn()
        .execute(
            "INSERT INTO retention_settings (singleton, period_seconds, updated_at)
             VALUES (1, ?1, ?2)
             ON CONFLICT(singleton) DO UPDATE SET period_seconds = ?1, updated_at = ?2",
            rusqlite::params![
                period_seconds(policy),
                clock.now().to_rfc3339_opts(SecondsFormat::Secs, true)
            ],
        )
        .map_err(sqlite_failure)?;
    Ok(())
}

#[derive(Clone, Debug)]
enum ControlRequest {
    SetPeriod(RetentionPolicy),
    RetryTranscription { item_id: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ControlCommand {
    SetPeriod { period_seconds: i64 },
    RetryTranscription { item_id: String },
}

impl ControlCommand {
    /// Refuse malformed commands and out-of-bounds periods before anything is queued.
    fn into_request(self) -> Result<ControlRequest, AbiFailure> {
        match self {
            ControlCommand::SetPeriod { period_seconds } => {
                let period =
                    Duration::try_seconds(period_seconds).ok_or(AbiFailure::INVALID_REQUEST)?;
                RetentionPolicy::new(period)
                    .map(ControlRequest::SetPeriod)
                    .map_err(|_| AbiFailure::INVALID_REQUEST)
            }
            ControlCommand::RetryTranscription { item_id } => {
                if !acceptable_identifier(&item_id) {
                    return Err(AbiFailure::INVALID_REQUEST);
                }
                Ok(ControlRequest::RetryTranscription { item_id })
            }
        }
    }
}

fn refusal_name(refusal: &RequeueRefusal) -> Value {
    match refusal {
        RequeueRefusal::ItemDeleted => json!({ "outcome": "item_deleted" }),
        RequeueRefusal::AudioRemoved => json!({ "outcome": "audio_removed" }),
        RequeueRefusal::NotRetryable(state) => {
            json!({ "outcome": "not_retryable", "transcription_state": state.as_str() })
        }
        RequeueRefusal::JobInFlight => json!({ "outcome": "job_in_flight" }),
    }
}

/// Applies one control. `set_period` writes one row; `retry_transcription` is the core's own
/// transactional re-queue.
fn apply_control(
    database: &mut Database,
    clock: &dyn Clock,
    request: &ControlRequest,
) -> Result<Value, AbiFailure> {
    match request {
        ControlRequest::SetPeriod(policy) => {
            store_retention_period(database, clock, policy)?;
            Ok(json!({ "outcome": "stored", "period_seconds": period_seconds(policy) }))
        }
        ControlRequest::RetryTranscription { item_id } => {
            match requeue_transcription(database, item_id, clock.now()) {
                Ok(job) => Ok(json!({ "outcome": "requeued", "job_id": job.job_id })),
                Err(RequeueError::ItemNotFound) => Err(AbiFailure::NOT_FOUND),
                Err(RequeueError::Refused(refusal)) => Ok(refusal_name(&refusal)),
                Err(RequeueError::Storage(error)) => Err(store_failure(&error)),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------------------------

fn with_operation_id(mut body: Value, operation_id: u64) -> Result<Vec<u8>, AbiFailure> {
    if let Value::Object(fields) = &mut body {
        fields.insert("operation_id".to_string(), json!(operation_id));
    }
    serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
}

fn read_job(read: RetentionRead, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        with_operation_id(read_retention(&mut database, &read)?, operation_id)
    })
}

fn control_job(request: ControlRequest, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        with_operation_id(
            apply_control(&mut database, &SystemClock, &request)?,
            operation_id,
        )
    })
}

fn request_bytes<'a>(request: *const u8, request_len: usize) -> Result<&'a [u8], AbiFailure> {
    if request_len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
        return Err(AbiFailure::REQUEST_TOO_LARGE);
    }
    if request_len == 0 {
        return Err(AbiFailure::INVALID_REQUEST);
    }
    if request.is_null() {
        return Err(AbiFailure::NULL_ARGUMENT);
    }
    // SAFETY: the caller of each export guarantees `request` points at `request_len` readable
    // bytes for the duration of the call, and the bytes are parsed before the call returns.
    Ok(unsafe { std::slice::from_raw_parts(request, request_len) })
}

/// Queues one read of the retention setting and unresolved sole audio, or of one item's audio
/// reference, described by the `request_len` JSON bytes at `request` (see the module
/// documentation). A malformed request, one with no read scope, or with unknown fields is refused
/// as `invalid_request` before anything is queued. Native must issue it only through the read
/// gate, passing only scopes the authenticated session may read. The outcome event for
/// `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_retention_read(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = request_bytes(request, request_len)?;
        let command: ReadCommand =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(operation_id, read_job(command.into_read()?, operation_id))
    })
}

/// Queues one retention control (`set_period` or `retry_transcription`) described by the
/// `request_len` JSON bytes at `request` (see the module documentation). Unknown commands and
/// fields, empty identifiers and out-of-bounds periods are refused as `invalid_request` before
/// anything is queued. Native must issue it only for an authenticated session. The outcome event
/// for `operation_id` is the documented JSON or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_retention_control(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let bytes = request_bytes(request, request_len)?;
        let command: ControlCommand =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(
            operation_id,
            control_job(command.into_request()?, operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
