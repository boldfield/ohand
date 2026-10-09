//! Normalized failures at the native boundary.
//!
//! Classes are the existing typed `providers::contracts::failure::ErrorClass`; this module
//! adds no second taxonomy. A failure is chosen from the typed error that occurred (a
//! `ProviderFailure`, a `rusqlite` error code) or from the boundary check that rejected the
//! call, never by inspecting message text. Codes and messages are fixed strings that cannot
//! echo caller content, paths or secrets.

use crate::providers::contracts::{ErrorClass, FailureKind, ProviderFailure};
use serde::Serialize;
use std::borrow::Cow;

pub const OHAND_CORE_STATUS_OK: u32 = 0;
pub const OHAND_CORE_STATUS_TRANSIENT: u32 = 1;
pub const OHAND_CORE_STATUS_PERMANENT: u32 = 2;
pub const OHAND_CORE_STATUS_UNAUTHORIZED: u32 = 3;
pub const OHAND_CORE_STATUS_CANCELLED: u32 = 4;
pub const OHAND_CORE_STATUS_UNSUPPORTED: u32 = 5;

pub fn status_for(class: ErrorClass) -> u32 {
    match class {
        ErrorClass::Transient => OHAND_CORE_STATUS_TRANSIENT,
        ErrorClass::Permanent => OHAND_CORE_STATUS_PERMANENT,
        ErrorClass::Unauthorized => OHAND_CORE_STATUS_UNAUTHORIZED,
        ErrorClass::Cancelled => OHAND_CORE_STATUS_CANCELLED,
        ErrorClass::Unsupported => OHAND_CORE_STATUS_UNSUPPORTED,
    }
}

/// A failure as it crosses the ABI: `{"class","code","message"}` JSON plus the class status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AbiFailure {
    pub class: ErrorClass,
    pub code: &'static str,
    pub message: Cow<'static, str>,
}

impl AbiFailure {
    const fn fixed(class: ErrorClass, code: &'static str, message: &'static str) -> Self {
        AbiFailure {
            class,
            code,
            message: Cow::Borrowed(message),
        }
    }

    pub fn status(&self) -> u32 {
        status_for(self.class)
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| {
            br#"{"class":"permanent","code":"internal","message":"unexpected internal failure"}"#
                .to_vec()
        })
    }

    pub const NULL_ARGUMENT: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "null_argument",
        "a required argument was null",
    );
    pub const REQUEST_TOO_LARGE: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "request_too_large",
        "request exceeds the boundary size limit",
    );
    pub const INVALID_UTF8: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "invalid_utf8",
        "request bytes are not valid UTF-8",
    );
    pub const INVALID_PATH: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "invalid_path",
        "the store path is empty",
    );
    pub const INVALID_HANDLE: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "invalid_handle",
        "the handle does not refer to an open core",
    );
    pub const REENTRANT_CALL: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "reentrant_call",
        "the call is not allowed from inside an event callback",
    );
    pub const STORE_UNAVAILABLE: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "store_unavailable",
        "the core store could not be opened",
    );
    pub const STORAGE_ERROR: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "storage_error",
        "the core store failed",
    );
    pub const STORE_BUSY: AbiFailure = AbiFailure::fixed(
        ErrorClass::Transient,
        "store_busy",
        "the core store is busy; retry later",
    );
    pub const QUEUE_FULL: AbiFailure = AbiFailure::fixed(
        ErrorClass::Transient,
        "busy",
        "too many operations are waiting; retry later",
    );
    pub const CANCELLED: AbiFailure = AbiFailure::fixed(
        ErrorClass::Cancelled,
        "cancelled",
        "the core was cancelled; no further work is accepted or delivered",
    );
    pub const INTERNAL: AbiFailure = AbiFailure::fixed(
        ErrorClass::Permanent,
        "internal",
        "unexpected internal failure",
    );

    /// Converts a typed provider failure. Only `kind` is trusted: the class and the fixed
    /// message are re-derived from it, because the other fields are public and deserializable
    /// and could be inconsistent or carry arbitrary content.
    pub fn from_provider(failure: &ProviderFailure) -> AbiFailure {
        let normalized = ProviderFailure::new(failure.kind);
        AbiFailure {
            class: normalized.class,
            code: provider_code(normalized.kind),
            message: Cow::Owned(normalized.message),
        }
    }

    /// Converts a store error by its typed SQLite result code. Errors that carry no typed
    /// SQLite code (for example schema validation failures) become `fallback`.
    pub fn from_store(error: &anyhow::Error, fallback: AbiFailure) -> AbiFailure {
        use rusqlite::ErrorCode;
        for cause in error.chain() {
            if let Some(rusqlite::Error::SqliteFailure(sqlite_error, _)) =
                cause.downcast_ref::<rusqlite::Error>()
            {
                return match sqlite_error.code {
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => AbiFailure::STORE_BUSY,
                    ErrorCode::CannotOpen => AbiFailure::STORE_UNAVAILABLE,
                    _ => fallback,
                };
            }
        }
        fallback
    }
}

fn provider_code(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Timeout => "timeout",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Unavailable => "unavailable",
        FailureKind::RateLimited => "rate_limited",
        FailureKind::Unauthorized => "unauthorized",
        FailureKind::InvalidOutput => "invalid_output",
        FailureKind::OutputTooLarge => "output_too_large",
        FailureKind::InputTooLarge => "input_too_large",
        FailureKind::CapabilityUnavailable => "capability_unavailable",
        FailureKind::ProfileMismatch => "profile_mismatch",
        FailureKind::Rejected => "rejected",
    }
}
