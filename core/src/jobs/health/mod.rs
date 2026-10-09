//! Bounded, content-free processing health (J03).
//!
//! [`read_processing_health`] answers "is background processing alive, and if not, why?" from the
//! durable job queue alone. It never starts, retries, cancels or edits a job, so reading health
//! cannot change what the queue does, and a user never has to operate the queue: capture saves
//! without consulting it.
//!
//! * **Stalls are independent of model success.** A stall is work that is due (or a lease that
//!   lapsed) and has not been picked up for longer than [`HealthConfig::stall_after`] (or
//!   [`HealthConfig::lease_grace`]). It is judged from queue timestamps only, so it is reported
//!   whether or not any provider ever answered, and a model that keeps failing is *retrying*,
//!   not stalled.
//! * **Waiting is not failing.** Backoff after a transient failure, a network the host reports
//!   unavailable, a missing native capability and an unavailable destination are distinct
//!   [`RecoverableErrorKind`]s with a [`RecoveryPath`] each, and none of them is a stall until the
//!   work is also overdue.
//! * **Content-free by construction.** The snapshot holds counts, ages, timestamps and
//!   machine-readable reason labels only: no item, job or capture identifiers and no text. A
//!   stored reason is reported only if it is one of the known reason constants the core and its
//!   native bridge record ([`KNOWN_REASONS`]); anything else, however label-shaped, is reported
//!   as [`UNRECOGNIZED_REASON`]. Each kind reports at most [`MAX_REASONS_PER_KIND`] distinct
//!   reasons.
//! * **No notification stream.** Health is a pull-only fact. [`HealthSummary`] says what a status
//!   surface may show; nothing here, or in the native service that carries it, posts a
//!   notification.
//!
//! `last_interpretation_success_at` is the newest recorded interpretation result (applied,
//! superseded or abstained). The queue records no completion time, so job types that leave no
//! timestamped result row, such as transcription, are not reflected in it.

use crate::jobs::queue::{JobStatus, PROFILE_MISSING_REASON, PROFILE_REVOKED_REASON};
use crate::jobs::runner::{
    CAPABILITY_UNAVAILABLE_REASON, INTERRUPTED_REASON, RETRIES_EXHAUSTED_REASON,
};
use crate::jobs::runner::{INTERNAL_ERROR_REASON, NOT_SETTLED_REASON};
use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::BTreeMap;

/// Reason label the host records on provider work it put back because the device was offline.
pub const OFFLINE_DEFERRED_REASON: &str = "offline_deferred";
/// Label reported in place of a stored reason that is not a known reason constant.
pub const UNRECOGNIZED_REASON: &str = "unrecognized";
/// Label that absorbs reasons beyond [`MAX_REASONS_PER_KIND`] for a kind.
pub const OVERFLOW_REASON: &str = "other";
/// Most distinct reason labels reported per error kind; further reasons fold into
/// [`OVERFLOW_REASON`], so a snapshot holds at most `(MAX_REASONS_PER_KIND + 1)` groups per kind.
pub const MAX_REASONS_PER_KIND: usize = 8;

/// Failure reasons that mean the job waits for the user to fix configuration or authorization
/// rather than for the provider to recover.
const CONFIGURATION_REASONS: &[&str] = &["unauthorized", "unsupported", "unsupported_job_version"];

/// Every reason the core or its native bridge records on a job. A stored reason is reported only
/// if it is listed here: a reason's syntax says nothing about where it came from, so unlisted
/// text is never echoed.
pub const KNOWN_REASONS: &[&str] = &[
    "timeout",
    "cancelled",
    "unavailable",
    "rate_limited",
    "unauthorized",
    "unsupported",
    "unsupported_job_version",
    "invalid_output",
    "output_too_large",
    "input_too_large",
    "capability_unavailable",
    "profile_mismatch",
    "rejected",
    "source_unavailable",
    "permanent",
    "native_failure",
    OFFLINE_DEFERRED_REASON,
    RETRIES_EXHAUSTED_REASON,
    INTERRUPTED_REASON,
    CAPABILITY_UNAVAILABLE_REASON,
    INTERNAL_ERROR_REASON,
    NOT_SETTLED_REASON,
    PROFILE_REVOKED_REASON,
    PROFILE_MISSING_REASON,
];

/// Thresholds for judging a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthConfig {
    /// How long due work may wait unclaimed before it counts as a stall.
    pub stall_after: Duration,
    /// How long after its lease lapsed a running job may stay unrecovered before it counts as a stall.
    pub lease_grace: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            stall_after: Duration::minutes(15),
            lease_grace: Duration::minutes(2),
        }
    }
}

/// What a status surface may say about processing as a whole, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthSummary {
    /// Work is overdue or a lease lapsed and nothing picked it up.
    Stalled,
    /// A failure that retrying cannot clear, or a destination that needs the user's action.
    NeedsAttention,
    /// Everything pending is deliberately waiting: backoff, offline, or a missing capability.
    Waiting,
    /// Work is queued or running within its normal window.
    Working,
    /// Nothing is pending and nothing has failed.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StallReason {
    /// Queued work is due and no drain claimed it.
    Overdue,
    /// A running job's lease lapsed and no drain recovered it.
    LeaseNotRecovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverableErrorKind {
    /// A transient failure; the job waits for its next attempt within its retry budget.
    RetryScheduled,
    /// Provider work was put back because the host reported no network.
    WaitingForNetwork,
    /// No native capability is registered for the job's type yet.
    CapabilityUnavailable,
    /// A running job's lease lapsed; the next drain recovers it.
    LeaseExpired,
    /// The job's pinned provider profile was revoked or no longer exists.
    DestinationUnavailable,
    /// The job waits for credentials or configuration the user must fix (rejected or missing
    /// authorization, unsupported configuration).
    ConfigurationNeeded,
    /// The job ended after spending its retry budget.
    RetriesExhausted,
    /// The job ended with a failure retrying cannot fix.
    PermanentFailure,
}

impl RecoverableErrorKind {
    pub fn recovery(self) -> RecoveryPath {
        match self {
            RecoverableErrorKind::RetryScheduled
            | RecoverableErrorKind::WaitingForNetwork
            | RecoverableErrorKind::CapabilityUnavailable
            | RecoverableErrorKind::LeaseExpired => RecoveryPath::Automatic,
            RecoverableErrorKind::DestinationUnavailable
            | RecoverableErrorKind::ConfigurationNeeded => RecoveryPath::UserAction,
            RecoverableErrorKind::RetriesExhausted | RecoverableErrorKind::PermanentFailure => {
                RecoveryPath::Terminal
            }
        }
    }
}

/// How a reported error clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPath {
    /// The queue resumes the work by itself.
    Automatic,
    /// The user must choose or re-authorize a destination.
    UserAction,
    /// The job has ended; the capture and its source are untouched.
    Terminal,
}

/// Jobs sharing a kind and reason label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoverableError {
    pub kind: RecoverableErrorKind,
    pub reason: String,
    pub recovery: RecoveryPath,
    pub job_count: u32,
    pub oldest_age_seconds: i64,
    /// Earliest scheduled attempt among the group's waiting jobs, when it has one.
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// The content-free account of processing health at `generated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessingHealth {
    pub generated_at: DateTime<Utc>,
    pub summary: HealthSummary,
    pub stalled: bool,
    pub stall_reasons: Vec<StallReason>,
    /// Queued and running jobs.
    pub pending_jobs: u32,
    pub running_jobs: u32,
    /// Queued jobs that are due now (not waiting on a backoff).
    pub overdue_jobs: u32,
    /// Age of the oldest queued or running job, from when it was enqueued.
    pub oldest_pending_age_seconds: Option<i64>,
    /// How long the longest-waiting due job has been waiting since it became due.
    pub oldest_overdue_seconds: Option<i64>,
    pub last_interpretation_success_at: Option<DateTime<Utc>>,
    pub errors: Vec<RecoverableError>,
}

#[derive(Default)]
struct GroupAccumulator {
    job_count: u32,
    oldest_created_at: Option<DateTime<Utc>>,
    next_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct ErrorGroups {
    groups: BTreeMap<(RecoverableErrorKind, String), GroupAccumulator>,
}

impl ErrorGroups {
    fn add(
        &mut self,
        kind: RecoverableErrorKind,
        reason: String,
        job_count: u32,
        created_at: DateTime<Utc>,
        next_attempt_at: Option<DateTime<Utc>>,
    ) {
        let mut key = (kind, reason);
        if !self.groups.contains_key(&key) {
            let reasons_of_kind = self
                .groups
                .keys()
                .filter(|(existing_kind, reason)| {
                    *existing_kind == kind && reason != OVERFLOW_REASON
                })
                .count();
            if reasons_of_kind >= MAX_REASONS_PER_KIND {
                key = (kind, OVERFLOW_REASON.to_string());
            }
        }
        let group = self.groups.entry(key).or_default();
        group.job_count = group.job_count.saturating_add(job_count);
        group.oldest_created_at = Some(
            group
                .oldest_created_at
                .map_or(created_at, |oldest| oldest.min(created_at)),
        );
        group.next_attempt_at = match (group.next_attempt_at, next_attempt_at) {
            (Some(existing), Some(candidate)) => Some(existing.min(candidate)),
            (existing, candidate) => existing.or(candidate),
        };
    }

    fn into_errors(self, now: DateTime<Utc>) -> Vec<RecoverableError> {
        self.groups
            .into_iter()
            .map(|((kind, reason), group)| RecoverableError {
                kind,
                reason,
                recovery: kind.recovery(),
                job_count: group.job_count,
                oldest_age_seconds: group
                    .oldest_created_at
                    .map_or(0, |created_at| age_seconds(now, created_at)),
                next_attempt_at: group.next_attempt_at,
            })
            .collect()
    }
}

/// Reports a stored reason only when it is a known reason constant, so a reason that somehow
/// carried captured text, even a single lowercase word, can never leave the core through health.
fn safe_reason_label(reason: &str) -> String {
    if KNOWN_REASONS.contains(&reason) {
        reason.to_string()
    } else {
        UNRECOGNIZED_REASON.to_string()
    }
}

fn is_configuration_reason(reason: &str) -> bool {
    CONFIGURATION_REASONS.contains(&reason)
}

fn age_seconds(now: DateTime<Utc>, since: DateTime<Utc>) -> i64 {
    (now - since).num_seconds().max(0)
}

fn parse_timestamp(text: &str, column: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| anyhow!("malformed {column} timestamp {text:?}: {error}"))
}

fn parse_optional_timestamp(text: Option<String>, column: &str) -> Result<Option<DateTime<Utc>>> {
    text.map(|text| parse_timestamp(&text, column)).transpose()
}

/// Reads processing health at `now`. Read-only: it never changes a job.
pub fn read_processing_health(
    db: &Database,
    config: &HealthConfig,
    now: DateTime<Utc>,
) -> Result<ProcessingHealth> {
    let conn = db.conn();
    let mut groups = ErrorGroups::default();
    let mut stall_reasons = Vec::new();
    let mut pending_jobs = 0u32;
    let mut running_jobs = 0u32;
    let mut overdue_jobs = 0u32;
    let mut waiting_jobs = 0u32;
    let mut oldest_pending: Option<DateTime<Utc>> = None;
    let mut oldest_overdue: Option<DateTime<Utc>> = None;
    let mut overdue_past_threshold = false;
    let mut lease_past_grace = false;

    {
        let mut statement = conn.prepare(
            "SELECT j.status, j.failure_reason, j.next_attempt_at, j.lease_expires_at, j.created_at,
                    CASE WHEN j.profile_version IS NULL THEN 0
                         WHEN p.profile_version IS NULL THEN 1
                         WHEN p.revoked_at IS NOT NULL THEN 1
                         ELSE 0 END
               FROM jobs j
               LEFT JOIN provider_profiles p ON p.profile_version = j.profile_version
              WHERE j.status IN (?, ?)",
        )?;
        let mut rows = statement.query(rusqlite::params![
            JobStatus::Queued.as_str(),
            JobStatus::Running.as_str()
        ])?;
        while let Some(row) = rows.next()? {
            let status: String = row.get(0)?;
            let failure_reason: Option<String> = row.get(1)?;
            let next_attempt_at = parse_optional_timestamp(row.get(2)?, "next_attempt_at")?;
            let lease_expires_at = parse_optional_timestamp(row.get(3)?, "lease_expires_at")?;
            let created_at = parse_timestamp(&row.get::<_, String>(4)?, "created_at")?;
            let destination_unavailable: bool = row.get::<_, i64>(5)? != 0;

            pending_jobs = pending_jobs.saturating_add(1);
            oldest_pending =
                Some(oldest_pending.map_or(created_at, |oldest| oldest.min(created_at)));

            if status == JobStatus::Running.as_str() {
                running_jobs = running_jobs.saturating_add(1);
                if let Some(lease_expires_at) = lease_expires_at.filter(|expiry| *expiry < now) {
                    groups.add(
                        RecoverableErrorKind::LeaseExpired,
                        "lease_expired".to_string(),
                        1,
                        created_at,
                        None,
                    );
                    if now - lease_expires_at > config.lease_grace {
                        lease_past_grace = true;
                    }
                }
                continue;
            }

            let due_at = next_attempt_at.unwrap_or(created_at);
            let is_due = due_at <= now;
            if is_due {
                overdue_jobs = overdue_jobs.saturating_add(1);
                oldest_overdue = Some(oldest_overdue.map_or(due_at, |oldest| oldest.min(due_at)));
                if now - due_at > config.stall_after {
                    overdue_past_threshold = true;
                }
            }

            let reason = failure_reason.as_deref();
            let classified = if destination_unavailable {
                Some((
                    RecoverableErrorKind::DestinationUnavailable,
                    "destination_unavailable".to_string(),
                ))
            } else {
                match reason {
                    None => None,
                    Some(INTERRUPTED_REASON) => None,
                    Some(CAPABILITY_UNAVAILABLE_REASON) => Some((
                        RecoverableErrorKind::CapabilityUnavailable,
                        CAPABILITY_UNAVAILABLE_REASON.to_string(),
                    )),
                    Some(OFFLINE_DEFERRED_REASON) => Some((
                        RecoverableErrorKind::WaitingForNetwork,
                        OFFLINE_DEFERRED_REASON.to_string(),
                    )),
                    Some(configuration) if is_configuration_reason(configuration) => Some((
                        RecoverableErrorKind::ConfigurationNeeded,
                        configuration.to_string(),
                    )),
                    Some(other) => Some((
                        RecoverableErrorKind::RetryScheduled,
                        safe_reason_label(other),
                    )),
                }
            };
            if !is_due {
                waiting_jobs = waiting_jobs.saturating_add(1);
            }
            if let Some((kind, label)) = classified {
                groups.add(kind, label, 1, created_at, next_attempt_at);
            }
        }
    }

    {
        let mut statement = conn.prepare(
            "SELECT j.status, j.failure_reason, COUNT(*), MIN(j.created_at)
               FROM jobs j
               JOIN items i ON i.item_id = j.item_id
              WHERE i.lifecycle_state != 'deleted'
                AND ((j.status = ?1)
                     OR (j.status = ?2 AND j.failure_reason IN (?3, ?4)))
                AND NOT EXISTS (
                      SELECT 1 FROM jobs newer
                       WHERE newer.item_id = j.item_id
                         AND newer.job_type = j.job_type
                         AND newer.job_id != j.job_id
                         AND (newer.created_at > j.created_at
                              OR (newer.created_at = j.created_at AND newer.rowid > j.rowid)))
              GROUP BY j.status, j.failure_reason",
        )?;
        let mut rows = statement.query(rusqlite::params![
            JobStatus::Failed.as_str(),
            JobStatus::Cancelled.as_str(),
            PROFILE_REVOKED_REASON,
            PROFILE_MISSING_REASON,
        ])?;
        while let Some(row) = rows.next()? {
            let status: String = row.get(0)?;
            let failure_reason: Option<String> = row.get(1)?;
            let count: i64 = row.get(2)?;
            let oldest_created_at = parse_timestamp(&row.get::<_, String>(3)?, "created_at")?;
            let job_count = u32::try_from(count).unwrap_or(u32::MAX);
            if status == JobStatus::Cancelled.as_str() {
                groups.add(
                    RecoverableErrorKind::DestinationUnavailable,
                    "destination_unavailable".to_string(),
                    job_count,
                    oldest_created_at,
                    None,
                );
                continue;
            }
            let (kind, label) = match failure_reason.as_deref() {
                Some(RETRIES_EXHAUSTED_REASON) => (
                    RecoverableErrorKind::RetriesExhausted,
                    RETRIES_EXHAUSTED_REASON.to_string(),
                ),
                Some(configuration) if is_configuration_reason(configuration) => (
                    RecoverableErrorKind::ConfigurationNeeded,
                    configuration.to_string(),
                ),
                Some(other) => (
                    RecoverableErrorKind::PermanentFailure,
                    safe_reason_label(other),
                ),
                None => (
                    RecoverableErrorKind::PermanentFailure,
                    UNRECOGNIZED_REASON.to_string(),
                ),
            };
            groups.add(kind, label, job_count, oldest_created_at, None);
        }
    }

    let last_interpretation_success_at = parse_optional_timestamp(
        conn.query_row(
            "SELECT MAX(created_at) FROM proposals
              WHERE applied_state IN ('applied', 'superseded', 'abstained')",
            [],
            |row| row.get::<_, Option<String>>(0),
        )?,
        "proposal created_at",
    )?;

    if overdue_past_threshold {
        stall_reasons.push(StallReason::Overdue);
    }
    if lease_past_grace {
        stall_reasons.push(StallReason::LeaseNotRecovered);
    }
    let stalled = !stall_reasons.is_empty();

    let errors = groups.into_errors(now);
    let needs_attention = errors
        .iter()
        .any(|error| error.recovery != RecoveryPath::Automatic);
    let summary = if stalled {
        HealthSummary::Stalled
    } else if needs_attention {
        HealthSummary::NeedsAttention
    } else if pending_jobs == 0 {
        HealthSummary::Idle
    } else if waiting_jobs == pending_jobs {
        HealthSummary::Waiting
    } else {
        HealthSummary::Working
    };

    Ok(ProcessingHealth {
        generated_at: now,
        summary,
        stalled,
        stall_reasons,
        pending_jobs,
        running_jobs,
        overdue_jobs,
        oldest_pending_age_seconds: oldest_pending.map(|created_at| age_seconds(now, created_at)),
        oldest_overdue_seconds: oldest_overdue.map(|due_at| age_seconds(now, due_at)),
        last_interpretation_success_at,
        errors,
    })
}

#[cfg(test)]
mod tests;
