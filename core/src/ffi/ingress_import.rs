//! Foreground ingress import over the core handle (C02b).
//!
//! `ohand_core_start_import_foreground_ingress` queues the C02a transaction
//! (`ingress::import_foreground_ingress`) on the handle's worker thread. Its success event is
//! the Capture Ingestion Contract acknowledgment (capture ID, item ID, save timestamp) and is
//! delivered only after the import transaction committed. This is not the pre-import capture
//! write of `ohand_core_start_save_capture`.
//!
//! Every failure says what the native side may do with its staging record through a fixed code:
//!
//! * `ingress_not_committed` and the `ingress_*` validation codes: nothing was written. The
//!   record is kept; validation codes will not succeed until the record or configuration changes.
//! * `ingress_commit_unknown`: the commit outcome cannot be asserted. Keep the record and retry
//!   with the same capture ID; the import is idempotent.
//! * `ingress_conflicting_reuse`: the ID holds different source content. Nothing is overwritten
//!   and the record must not be deleted automatically.
//! * `ingress_item_deleted`: the user deleted the item; this is the only terminal outcome, after
//!   which the staging record may be discarded.
//!
//! Failure messages are fixed strings and never echo the request.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::ingress::{
    import_foreground_ingress, CommitStatus, ImportDisposition, IngressError, IngressErrorKind,
    IngressRejection,
};
use crate::providers::contracts::ErrorClass;
use crate::store::captures::Capture;
use crate::store::schema::Database;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Mutex;

/// The ingress record as it crosses the boundary: the fields of a capture, with the same names
/// as the capture exports use. `created_at` is the idempotency timestamp.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IngressRecord {
    capture_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    audio_reference: Option<String>,
    capture_instant: String,
    timezone_id: String,
    utc_offset_minutes: i32,
    locale: String,
    calendar: String,
    item_scope: String,
    route_id: String,
    entry_locked: bool,
    created_at: String,
    #[serde(default)]
    session_topic: Option<String>,
}

impl IngressRecord {
    /// No validation here: the importer owns it, so every refusal is a specific rejection.
    fn into_capture(self) -> Capture {
        Capture {
            capture_id: self.capture_id,
            text: self.text,
            audio_reference: self.audio_reference,
            capture_instant: self.capture_instant,
            timezone_id: self.timezone_id,
            utc_offset_minutes: self.utc_offset_minutes,
            locale: self.locale,
            calendar: self.calendar,
            item_scope: self.item_scope,
            route_id: self.route_id,
            entry_locked: self.entry_locked,
            created_at: self.created_at,
            session_topic: self.session_topic,
        }
    }
}

#[derive(Serialize)]
struct ImportAcknowledgment {
    operation_id: u64,
    capture_id: String,
    item_id: String,
    saved_at: String,
    disposition: &'static str,
}

const fn failure(class: ErrorClass, code: &'static str, message: &'static str) -> AbiFailure {
    AbiFailure {
        class,
        code,
        message: Cow::Borrowed(message),
    }
}

fn rejection_failure(rejection: &IngressRejection) -> AbiFailure {
    let (code, message) = match rejection {
        IngressRejection::MissingField(_) => {
            ("ingress_missing_field", "a required ingress field is empty")
        }
        IngressRejection::MissingContent => (
            "ingress_missing_content",
            "the ingress record has neither text nor an audio reference",
        ),
        IngressRejection::UnsupportedScope(_) => (
            "ingress_unsupported_scope",
            "the item scope is not supported",
        ),
        IngressRejection::MalformedTimeContext(_) => (
            "ingress_malformed_time_context",
            "the capture time context is malformed",
        ),
        IngressRejection::MalformedAudioReference => (
            "ingress_malformed_audio_reference",
            "the audio reference is malformed",
        ),
        IngressRejection::MalformedSessionTopic => (
            "ingress_malformed_session_topic",
            "the session topic is malformed",
        ),
        IngressRejection::UnknownRoute(_) => {
            ("ingress_unknown_route", "the route is not configured")
        }
        IngressRejection::RouteScopeMismatch { .. } => (
            "ingress_route_scope_mismatch",
            "the route does not serve the item scope",
        ),
    };
    failure(ErrorClass::Permanent, code, message)
}

fn import_failure(error: &IngressError) -> AbiFailure {
    match &error.kind {
        IngressErrorKind::Validation(rejection) => rejection_failure(rejection),
        IngressErrorKind::ConflictingReuse { .. } => failure(
            ErrorClass::Permanent,
            "ingress_conflicting_reuse",
            "the capture ID is already used by a capture with different content",
        ),
        IngressErrorKind::ItemDeleted { .. } => failure(
            ErrorClass::Permanent,
            "ingress_item_deleted",
            "the item for this capture was deleted and is not re-imported",
        ),
        IngressErrorKind::Storage(cause) => match error.commit_status {
            CommitStatus::NotCommitted => {
                let class = AbiFailure::from_store(cause, AbiFailure::STORAGE_ERROR).class;
                failure(
                    class,
                    "ingress_not_committed",
                    "the import did not commit; nothing was written",
                )
            }
            CommitStatus::Unknown | CommitStatus::Committed => failure(
                ErrorClass::Transient,
                "ingress_commit_unknown",
                "the import outcome is unknown; retry with the same capture ID",
            ),
        },
    }
}

fn import_job(capture: Capture, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let acknowledgment = import_foreground_ingress(&mut database, &capture)
            .map_err(|error| import_failure(&error))?;
        serde_json::to_vec(&ImportAcknowledgment {
            operation_id,
            capture_id: acknowledgment.capture_id,
            item_id: acknowledgment.item_id,
            saved_at: acknowledgment.saved_at,
            disposition: match acknowledgment.disposition {
                ImportDisposition::Imported => "imported",
                ImportDisposition::AlreadyImported => "already_imported",
            },
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues the foreground import of the ingress record described by the `request_len` JSON bytes
/// at `request` (capture fields; unknown fields are rejected). The outcome event for
/// `operation_id` is success only after the import transaction committed, with JSON
/// `{"operation_id","capture_id","item_id","saved_at","disposition"}` where `disposition` is
/// `imported` or `already_imported`; otherwise it is a normalized failure whose code says
/// whether the commit happened (see the module documentation).
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_import_foreground_ingress(
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
        let record: IngressRecord =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(
            operation_id,
            import_job(record.into_capture(), operation_id),
        )
    })
}

#[cfg(test)]
mod tests;
