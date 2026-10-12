//! Local metrics over the core handle (T01).
//!
//! `ohand_core_start_metrics` queues one metrics operation on the handle's worker thread. The
//! request is one JSON object with an `op` field and no other unknown fields:
//!
//! * `{"op":"record_timing","series","dimension","duration_ms"}`: one latency sample. `series` is
//!   `trigger_to_ready` with `dimension` `cold`, `warm` or `locked_handoff`, or
//!   `end_of_input_to_save` with `voice` or `text`.
//! * `{"op":"record_failure","series","dimension"}`: one observed failure. `series` is
//!   `processing_failure` or `scheduling_failure`; `dimension` is one of the closed reason labels
//!   in [`crate::metrics`].
//! * `{"op":"set_enabled","enabled"}` switches recording on or off; `{"op":"clear"}` removes the
//!   stored samples and counters.
//! * `{"op":"export","read_authentication"}` returns the aggregate report. `read_authentication`
//!   is `authenticated` or `capture_only`, the native session read scope; `capture_only` is
//!   refused as `metrics_export_not_authorized` before anything is read.
//!
//! Anything else (an unknown op, series, dimension or field, a duration over the maximum) is
//! refused as `invalid_request` and nothing is stored. The outcome event is `{"operation_id",
//! "outcome"}` with `outcome` `recorded`, `disabled` (recording is off, nothing stored) or `ok`; an
//! export adds `"report"`. Failures are normalized and never echo request fields. The core sends
//! metrics nowhere: the report reaches the host only as this event.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::export::ReadAuthentication;
use crate::metrics::{
    clear_metrics, export_metrics, record_failure, record_timing, set_metrics_enabled,
    FailureSeries, MetricsError, RecordOutcome, TimingSeries,
};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Database;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::Mutex;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    RecordTiming {
        series: String,
        dimension: String,
        duration_ms: u64,
    },
    RecordFailure {
        series: String,
        dimension: String,
    },
    SetEnabled {
        enabled: bool,
    },
    Clear,
    Export {
        read_authentication: String,
    },
}

enum Validated {
    RecordTiming(TimingSeries, u64),
    RecordFailure(FailureSeries),
    SetEnabled(bool),
    Clear,
    Export(ReadAuthentication),
}

const fn unauthorized() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Unauthorized,
        code: "metrics_export_not_authorized",
        message: Cow::Borrowed("metrics export requires an authenticated read session"),
    }
}

impl Command {
    fn validate(self) -> Result<Validated, AbiFailure> {
        match self {
            Command::RecordTiming {
                series,
                dimension,
                duration_ms,
            } => {
                let series =
                    TimingSeries::parse(&series, &dimension).ok_or(AbiFailure::INVALID_REQUEST)?;
                Ok(Validated::RecordTiming(series, duration_ms))
            }
            Command::RecordFailure { series, dimension } => {
                let series =
                    FailureSeries::parse(&series, &dimension).ok_or(AbiFailure::INVALID_REQUEST)?;
                Ok(Validated::RecordFailure(series))
            }
            Command::SetEnabled { enabled } => Ok(Validated::SetEnabled(enabled)),
            Command::Clear => Ok(Validated::Clear),
            Command::Export {
                read_authentication,
            } => match read_authentication.as_str() {
                "authenticated" => Ok(Validated::Export(ReadAuthentication::Authenticated)),
                "capture_only" => Ok(Validated::Export(ReadAuthentication::CaptureOnly)),
                _ => Err(AbiFailure::INVALID_REQUEST),
            },
        }
    }
}

fn metrics_failure(error: MetricsError) -> AbiFailure {
    match error {
        MetricsError::DurationOutOfRange => AbiFailure::INVALID_REQUEST,
        MetricsError::NotAuthenticated => unauthorized(),
        MetricsError::Storage(error) => {
            AbiFailure::from_store(&anyhow::Error::new(error), AbiFailure::STORAGE_ERROR)
        }
    }
}

fn outcome_label(outcome: RecordOutcome) -> &'static str {
    match outcome {
        RecordOutcome::Recorded => "recorded",
        RecordOutcome::Disabled => "disabled",
    }
}

fn run(database: &Database, command: &Validated) -> Result<Value, AbiFailure> {
    let now = Utc::now();
    match command {
        Validated::RecordTiming(series, duration_ms) => {
            let outcome =
                record_timing(database, *series, *duration_ms).map_err(metrics_failure)?;
            Ok(json!({ "outcome": outcome_label(outcome) }))
        }
        Validated::RecordFailure(series) => {
            let outcome = record_failure(database, *series).map_err(metrics_failure)?;
            Ok(json!({ "outcome": outcome_label(outcome) }))
        }
        Validated::SetEnabled(enabled) => {
            set_metrics_enabled(database, *enabled, now).map_err(metrics_failure)?;
            Ok(json!({ "outcome": "ok" }))
        }
        Validated::Clear => {
            clear_metrics(database).map_err(metrics_failure)?;
            Ok(json!({ "outcome": "ok" }))
        }
        Validated::Export(authentication) => {
            let report = export_metrics(database, *authentication, now).map_err(metrics_failure)?;
            Ok(json!({ "outcome": "report", "report": report }))
        }
    }
}

fn metrics_job(command: Validated, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let mut body = run(&database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one metrics operation described by the `request_len` JSON bytes at `request` (see the
/// module documentation). A malformed or unknown request is refused as `invalid_request` before
/// anything is queued. A `capture_only` export is queued but its event is
/// `metrics_export_not_authorized`. The outcome event for `operation_id` is the documented JSON or
/// a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_metrics(
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
        core.submit(operation_id, metrics_job(command.validate()?, operation_id))
    })
}

#[cfg(test)]
mod tests;
