//! Content-free local latency and reliability metrics (T01).
//!
//! The trial needs a few numbers about this device: how long capture takes to become ready, how
//! long a finished input takes to be durably saved, and how often processing or reminder
//! scheduling fails. Nothing here can carry what the user captured.
//!
//! * **Closed vocabulary.** A sample names a [`TimingSeries`] or a [`FailureSeries`], each a
//!   closed enum with machine labels. There is no free-text field, no item, capture, job or
//!   reminder identifier, no prompt, address or credential, so no caller can put content into a
//!   metric by mistake.
//! * **Two separate clocks.** [`TimingSeries::TriggerToReady`] (user action until the capture
//!   surface can take input) and [`TimingSeries::EndOfInputToSave`] (user finishes input until
//!   the durable acknowledgment) are different series and are never combined.
//! * **Failures are not attention.** A [`FailureSeries`] counts what the system observed going
//!   wrong. It says nothing about whether the user saw it, read it or acted on it, and the export
//!   has no field that could claim so.
//! * **No event times.** Samples are stored without a clock reading, so the store cannot reveal
//!   when the user captured something.
//! * **Bounded.** Each timing series keeps its newest [`MAX_SAMPLES_PER_SERIES`] samples; failure
//!   series are counters over a closed set. The series set is finite, so the whole store is too.
//! * **Local only, explicit export.** The module has no network code and no destination. The only
//!   way data leaves is [`export_metrics`], which the host calls for an explicit action and which
//!   refuses unless the native read session is authenticated. Recording can be switched off with
//!   [`set_metrics_enabled`] and the stored data removed with [`clear_metrics`]; neither touches
//!   captures, items or reminders.

use crate::export::ReadAuthentication;
use crate::store::schema::Database;
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

/// Timing samples kept per series; recording another drops the oldest.
pub const MAX_SAMPLES_PER_SERIES: usize = 500;
/// Longest duration accepted, in milliseconds (one hour). Longer values are a clock or caller
/// fault, not a latency, and are refused rather than clamped.
pub const MAX_DURATION_MS: u64 = 3_600_000;
/// Version label of the exported report layout.
pub const REPORT_SCHEMA: &str = "ohand.metrics.v1";

/// How the capture surface was reached for a trigger-to-ready measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind {
    Cold,
    Warm,
    LockedHandoff,
}

impl LaunchKind {
    pub const ALL: [LaunchKind; 3] = [
        LaunchKind::Cold,
        LaunchKind::Warm,
        LaunchKind::LockedHandoff,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            LaunchKind::Cold => "cold",
            LaunchKind::Warm => "warm",
            LaunchKind::LockedHandoff => "locked_handoff",
        }
    }
}

/// How a capture was entered, for an end-of-input-to-save measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    Voice,
    Text,
}

impl CaptureKind {
    pub const ALL: [CaptureKind; 2] = [CaptureKind::Voice, CaptureKind::Text];

    pub const fn label(self) -> &'static str {
        match self {
            CaptureKind::Voice => "voice",
            CaptureKind::Text => "text",
        }
    }
}

/// A latency series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimingSeries {
    /// From the user's trigger (system control, app open, notification tap) until the capture
    /// surface is ready for input. Excludes saving.
    TriggerToReady(LaunchKind),
    /// From the user finishing input (releasing the microphone, tapping save) until the device
    /// shows the durable acknowledgment. Excludes network and processing.
    EndOfInputToSave(CaptureKind),
}

impl TimingSeries {
    pub const fn series_label(self) -> &'static str {
        match self {
            TimingSeries::TriggerToReady(_) => "trigger_to_ready",
            TimingSeries::EndOfInputToSave(_) => "end_of_input_to_save",
        }
    }

    pub const fn dimension_label(self) -> &'static str {
        match self {
            TimingSeries::TriggerToReady(launch) => launch.label(),
            TimingSeries::EndOfInputToSave(capture) => capture.label(),
        }
    }

    pub fn parse(series: &str, dimension: &str) -> Option<TimingSeries> {
        Self::all().into_iter().find(|candidate| {
            candidate.series_label() == series && candidate.dimension_label() == dimension
        })
    }

    /// Every series, in report order.
    pub fn all() -> Vec<TimingSeries> {
        LaunchKind::ALL
            .into_iter()
            .map(TimingSeries::TriggerToReady)
            .chain(
                CaptureKind::ALL
                    .into_iter()
                    .map(TimingSeries::EndOfInputToSave),
            )
            .collect()
    }
}

/// Why background processing failed, as a machine label only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessingFailureReason {
    Timeout,
    Unavailable,
    RateLimited,
    Unauthorized,
    InvalidOutput,
    CapabilityUnavailable,
    UnsupportedLanguage,
    Interrupted,
    RetriesExhausted,
    Other,
}

impl ProcessingFailureReason {
    pub const ALL: [ProcessingFailureReason; 10] = [
        ProcessingFailureReason::Timeout,
        ProcessingFailureReason::Unavailable,
        ProcessingFailureReason::RateLimited,
        ProcessingFailureReason::Unauthorized,
        ProcessingFailureReason::InvalidOutput,
        ProcessingFailureReason::CapabilityUnavailable,
        ProcessingFailureReason::UnsupportedLanguage,
        ProcessingFailureReason::Interrupted,
        ProcessingFailureReason::RetriesExhausted,
        ProcessingFailureReason::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ProcessingFailureReason::Timeout => "timeout",
            ProcessingFailureReason::Unavailable => "unavailable",
            ProcessingFailureReason::RateLimited => "rate_limited",
            ProcessingFailureReason::Unauthorized => "unauthorized",
            ProcessingFailureReason::InvalidOutput => "invalid_output",
            ProcessingFailureReason::CapabilityUnavailable => "capability_unavailable",
            ProcessingFailureReason::UnsupportedLanguage => "unsupported_language",
            ProcessingFailureReason::Interrupted => "interrupted",
            ProcessingFailureReason::RetriesExhausted => "retries_exhausted",
            ProcessingFailureReason::Other => "other",
        }
    }
}

/// Why a reminder could not be scheduled, as a machine label only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchedulingFailureReason {
    PermissionDenied,
    CapacityExceeded,
    Unschedulable,
    NativeFailure,
    Other,
}

impl SchedulingFailureReason {
    pub const ALL: [SchedulingFailureReason; 5] = [
        SchedulingFailureReason::PermissionDenied,
        SchedulingFailureReason::CapacityExceeded,
        SchedulingFailureReason::Unschedulable,
        SchedulingFailureReason::NativeFailure,
        SchedulingFailureReason::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            SchedulingFailureReason::PermissionDenied => "permission_denied",
            SchedulingFailureReason::CapacityExceeded => "capacity_exceeded",
            SchedulingFailureReason::Unschedulable => "unschedulable",
            SchedulingFailureReason::NativeFailure => "native_failure",
            SchedulingFailureReason::Other => "other",
        }
    }
}

/// A failure counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureSeries {
    Processing(ProcessingFailureReason),
    Scheduling(SchedulingFailureReason),
}

impl FailureSeries {
    pub const fn series_label(self) -> &'static str {
        match self {
            FailureSeries::Processing(_) => "processing_failure",
            FailureSeries::Scheduling(_) => "scheduling_failure",
        }
    }

    pub const fn dimension_label(self) -> &'static str {
        match self {
            FailureSeries::Processing(reason) => reason.label(),
            FailureSeries::Scheduling(reason) => reason.label(),
        }
    }

    pub fn parse(series: &str, dimension: &str) -> Option<FailureSeries> {
        Self::all().into_iter().find(|candidate| {
            candidate.series_label() == series && candidate.dimension_label() == dimension
        })
    }

    /// Every series, in report order.
    pub fn all() -> Vec<FailureSeries> {
        ProcessingFailureReason::ALL
            .into_iter()
            .map(FailureSeries::Processing)
            .chain(
                SchedulingFailureReason::ALL
                    .into_iter()
                    .map(FailureSeries::Scheduling),
            )
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("the duration is outside the accepted range")]
    DurationOutOfRange,
    #[error("metrics export requires an authenticated read session")]
    NotAuthenticated,
    #[error("metrics storage failed: {0}")]
    Storage(#[from] rusqlite::Error),
}

/// What a record call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordOutcome {
    Recorded,
    /// Recording is switched off; nothing was stored.
    Disabled,
}

fn timestamp(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Whether recording is on. It is on until [`set_metrics_enabled`] says otherwise.
pub fn metrics_enabled(database: &Database) -> Result<bool, MetricsError> {
    let enabled: Option<i64> = database
        .conn()
        .query_row(
            "SELECT enabled FROM metric_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(enabled.unwrap_or(1) == 1)
}

/// Switches recording on or off. Stored samples are kept; use [`clear_metrics`] to remove them.
pub fn set_metrics_enabled(
    database: &Database,
    enabled: bool,
    now: DateTime<Utc>,
) -> Result<(), MetricsError> {
    database.conn().execute(
        "INSERT INTO metric_settings (id, enabled, updated_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
        params![i64::from(enabled), timestamp(now)],
    )?;
    Ok(())
}

/// Records one latency sample and drops the series' oldest samples beyond
/// [`MAX_SAMPLES_PER_SERIES`].
pub fn record_timing(
    database: &Database,
    series: TimingSeries,
    duration_ms: u64,
) -> Result<RecordOutcome, MetricsError> {
    if duration_ms > MAX_DURATION_MS {
        return Err(MetricsError::DurationOutOfRange);
    }
    if !metrics_enabled(database)? {
        return Ok(RecordOutcome::Disabled);
    }
    let transaction = database.conn().unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO metric_timing_samples (series, dimension, duration_ms) VALUES (?1, ?2, ?3)",
        params![
            series.series_label(),
            series.dimension_label(),
            duration_ms as i64
        ],
    )?;
    transaction.execute(
        "DELETE FROM metric_timing_samples
         WHERE series = ?1 AND dimension = ?2 AND sample_id <= COALESCE(
             (SELECT sample_id FROM metric_timing_samples WHERE series = ?1 AND dimension = ?2
              ORDER BY sample_id DESC LIMIT 1 OFFSET ?3), 0)",
        params![
            series.series_label(),
            series.dimension_label(),
            MAX_SAMPLES_PER_SERIES as i64
        ],
    )?;
    transaction.commit()?;
    Ok(RecordOutcome::Recorded)
}

/// Counts one observed failure. The counter saturates rather than overflowing.
pub fn record_failure(
    database: &Database,
    series: FailureSeries,
) -> Result<RecordOutcome, MetricsError> {
    if !metrics_enabled(database)? {
        return Ok(RecordOutcome::Disabled);
    }
    database.conn().execute(
        "INSERT INTO metric_failure_counts (series, dimension, occurrences) VALUES (?1, ?2, 1)
         ON CONFLICT(series, dimension) DO UPDATE SET
             occurrences = MIN(occurrences + 1, 9223372036854775807)",
        params![series.series_label(), series.dimension_label()],
    )?;
    Ok(RecordOutcome::Recorded)
}

/// Removes every stored sample and counter. The enabled setting is unchanged.
pub fn clear_metrics(database: &Database) -> Result<(), MetricsError> {
    database
        .conn()
        .execute("DELETE FROM metric_timing_samples", [])?;
    database
        .conn()
        .execute("DELETE FROM metric_failure_counts", [])?;
    Ok(())
}

/// Distribution of the retained samples of one timing series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimingSummary {
    pub series: String,
    pub dimension: String,
    /// Samples the figures below are computed from: the newest ones, at most
    /// [`MAX_SAMPLES_PER_SERIES`].
    pub retained_samples: u32,
    pub min_ms: u64,
    pub median_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

/// How often the system observed one kind of failure. This is not a count of failures anyone saw
/// or handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureSummary {
    pub series: String,
    pub dimension: String,
    pub occurrences: u64,
}

/// The exported report: aggregates only, no timestamps of individual events and no identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsReport {
    pub schema: &'static str,
    pub generated_at: DateTime<Utc>,
    pub recording_enabled: bool,
    pub sample_limit_per_series: u32,
    pub timings: Vec<TimingSummary>,
    pub failures: Vec<FailureSummary>,
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Builds the report for an explicit, authenticated export. Refused unless `authentication` is
/// [`ReadAuthentication::Authenticated`]; nothing is read before that check. Series with no data
/// are omitted. Exporting changes nothing stored.
pub fn export_metrics(
    database: &Database,
    authentication: ReadAuthentication,
    now: DateTime<Utc>,
) -> Result<MetricsReport, MetricsError> {
    if authentication != ReadAuthentication::Authenticated {
        return Err(MetricsError::NotAuthenticated);
    }
    let mut timings = Vec::new();
    for series in TimingSeries::all() {
        let mut statement = database.conn().prepare(
            "SELECT duration_ms FROM metric_timing_samples WHERE series = ?1 AND dimension = ?2
             ORDER BY duration_ms",
        )?;
        let sorted = statement
            .query_map(
                params![series.series_label(), series.dimension_label()],
                |row| row.get::<_, i64>(0),
            )?
            .map(|value| value.map(|milliseconds| milliseconds.max(0) as u64))
            .collect::<Result<Vec<u64>, _>>()?;
        if sorted.is_empty() {
            continue;
        }
        timings.push(TimingSummary {
            series: series.series_label().to_string(),
            dimension: series.dimension_label().to_string(),
            retained_samples: sorted.len() as u32,
            min_ms: sorted[0],
            median_ms: nearest_rank(&sorted, 50),
            p95_ms: nearest_rank(&sorted, 95),
            max_ms: sorted[sorted.len() - 1],
        });
    }
    let mut failures = Vec::new();
    for series in FailureSeries::all() {
        let occurrences: Option<i64> = database
            .conn()
            .query_row(
                "SELECT occurrences FROM metric_failure_counts WHERE series = ?1 AND dimension = ?2",
                params![series.series_label(), series.dimension_label()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(occurrences) = occurrences.filter(|count| *count > 0) {
            failures.push(FailureSummary {
                series: series.series_label().to_string(),
                dimension: series.dimension_label().to_string(),
                occurrences: occurrences as u64,
            });
        }
    }
    Ok(MetricsReport {
        schema: REPORT_SCHEMA,
        generated_at: now,
        recording_enabled: metrics_enabled(database)?,
        sample_limit_per_series: MAX_SAMPLES_PER_SERIES as u32,
        timings,
        failures,
    })
}

#[cfg(test)]
mod tests;
