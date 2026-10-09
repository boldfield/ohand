use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};

use crate::jobs::queue::JobStatus;
use crate::privacy::routing::JOB_TYPE_SHADOW_REVIEW;

pub(super) const UNREVIEWED_PREFIX: &str = "shadow_unreviewed:";
pub(super) const ERROR_PREFIX: &str = "shadow_error:";
/// Marks, while a case is running, the lease attempt whose provider request has been authorized:
/// `shadow_dispatched:<attempt>`, followed by `|<previous failure>` when an earlier attempt
/// failed. Every outcome write replaces it.
pub(super) const DISPATCHED_PREFIX: &str = "shadow_dispatched:";
const DISPATCHED_SEPARATOR: char = '|';

/// The lease attempt a dispatch marker names, if `failure_reason` is one.
pub(super) fn dispatched_attempt(failure_reason: Option<&str>) -> Option<i32> {
    let marker = failure_reason?.strip_prefix(DISPATCHED_PREFIX)?;
    let attempt = marker
        .split_once(DISPATCHED_SEPARATOR)
        .map_or(marker, |(attempt, _)| attempt);
    attempt.parse().ok()
}

/// A dispatch marker for `lease_attempt` that keeps the previous failure code readable.
pub(super) fn dispatch_marker(lease_attempt: i32, failure_reason: Option<&str>) -> String {
    match failure_reason.map(previous_failure) {
        Some(Some(previous)) => {
            format!("{DISPATCHED_PREFIX}{lease_attempt}{DISPATCHED_SEPARATOR}{previous}")
        }
        _ => format!("{DISPATCHED_PREFIX}{lease_attempt}"),
    }
}

/// The failure code of an earlier attempt, with any dispatch marker removed.
fn previous_failure(failure_reason: &str) -> Option<&str> {
    match failure_reason.strip_prefix(DISPATCHED_PREFIX) {
        Some(marker) => marker
            .split_once(DISPATCHED_SEPARATOR)
            .map(|(_, previous)| previous),
        None => Some(failure_reason),
    }
}

/// Why a sampled case ended without a review verdict. Every variant means "unreviewed": the
/// reviewer's silence is never read as agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreviewedReason {
    Timeout,
    Cancelled,
    /// A dispatch-time check (policy, route grant, profile, attempts) refused the call.
    DispatchDenied,
    /// The job was retired before dispatch: profile revoked, item deleted or revised, or cancelled.
    Retired,
}

impl UnreviewedReason {
    pub(super) fn code(self) -> &'static str {
        match self {
            UnreviewedReason::Timeout => "timeout",
            UnreviewedReason::Cancelled => "cancelled",
            UnreviewedReason::DispatchDenied => "dispatch_denied",
            UnreviewedReason::Retired => "retired",
        }
    }

    fn from_code(code: &str) -> Option<UnreviewedReason> {
        [
            UnreviewedReason::Timeout,
            UnreviewedReason::Cancelled,
            UnreviewedReason::DispatchDenied,
            UnreviewedReason::Retired,
        ]
        .into_iter()
        .find(|reason| reason.code() == code)
    }
}

/// Diagnostic state of one sampled case. A review verdict, when a later task records one, is
/// diagnostic only; nothing in this module reads an outcome to change item, reminder or
/// permission state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowOutcome {
    /// Reserved or being retried; no terminal result yet.
    Pending,
    /// The review attempt finished.
    Reviewed,
    Unreviewed(UnreviewedReason),
    /// Terminal provider or job error, as a fixed normalized code (never provider text).
    Error(String),
}

/// One sampled case with the provenance needed to attribute and audit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowRecord {
    pub job_id: String,
    pub item_id: String,
    pub source_revision: i32,
    /// Identifies the interpretation request whose result is being reviewed.
    pub request_version: String,
    /// The separately approved review profile version the case is pinned to.
    pub review_profile_version: String,
    /// The capture's stored route, whose `review` grant authorizes the destination.
    pub route_id: String,
    pub created_at: DateTime<Utc>,
    pub attempts_used: i32,
    pub outcome: ShadowOutcome,
    /// Fixed code of the most recent failure, kept while the case is pending a retry.
    pub last_failure: Option<String>,
}

pub(super) fn outcome_for(status: &JobStatus, failure_reason: Option<&str>) -> ShadowOutcome {
    match status {
        JobStatus::Queued | JobStatus::Running => ShadowOutcome::Pending,
        JobStatus::Completed => ShadowOutcome::Reviewed,
        JobStatus::Cancelled => ShadowOutcome::Unreviewed(UnreviewedReason::Retired),
        JobStatus::Failed => match failure_reason {
            Some(reason) if reason.starts_with(UNREVIEWED_PREFIX) => {
                UnreviewedReason::from_code(&reason[UNREVIEWED_PREFIX.len()..])
                    .map(ShadowOutcome::Unreviewed)
                    .unwrap_or_else(|| ShadowOutcome::Error(reason.to_string()))
            }
            Some(reason) if reason.starts_with(ERROR_PREFIX) => {
                ShadowOutcome::Error(reason[ERROR_PREFIX.len()..].to_string())
            }
            Some(reason) => ShadowOutcome::Error(reason.to_string()),
            None => ShadowOutcome::Error("unknown".to_string()),
        },
    }
}

const RECORD_SELECT: &str = "SELECT jobs.job_id, jobs.item_id, jobs.source_revision, \
    jobs.request_version, jobs.profile_version, captures.route_id, jobs.created_at, \
    jobs.attempt_count, jobs.status, jobs.failure_reason \
    FROM jobs \
    JOIN items ON items.item_id = jobs.item_id \
    JOIN captures ON captures.capture_id = items.capture_id \
    WHERE jobs.job_type = ?";

fn record_from_row(row: &rusqlite::Row<'_>) -> Result<ShadowRecord> {
    let job_id: String = row.get(0)?;
    let created_at_text: String = row.get(6)?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_text)
        .map_err(|error| anyhow!("job {job_id} has malformed created_at: {error}"))?
        .with_timezone(&Utc);
    let status_text: String = row.get(8)?;
    let status = status_text.parse::<JobStatus>()?;
    let failure_reason: Option<String> = row.get(9)?;
    let outcome = outcome_for(&status, failure_reason.as_deref());
    let last_failure = match (&outcome, &failure_reason) {
        (ShadowOutcome::Pending, Some(reason)) => previous_failure(reason)
            .map(|previous| previous.strip_prefix(ERROR_PREFIX).unwrap_or(previous))
            .map(str::to_string),
        _ => None,
    };
    Ok(ShadowRecord {
        item_id: row.get(1)?,
        source_revision: row.get(2)?,
        request_version: row
            .get::<_, Option<String>>(3)?
            .ok_or_else(|| anyhow!("shadow job {job_id} has no request version"))?,
        review_profile_version: row
            .get::<_, Option<String>>(4)?
            .ok_or_else(|| anyhow!("shadow job {job_id} has no pinned review profile"))?,
        route_id: row.get(5)?,
        created_at,
        attempts_used: row.get(7)?,
        outcome,
        last_failure,
        job_id,
    })
}

/// Read one shadow record by its job id; `None` for an unknown id or a non-shadow job.
pub fn load_shadow_record(conn: &Connection, job_id: &str) -> Result<Option<ShadowRecord>> {
    let sql = format!("{RECORD_SELECT} AND jobs.job_id = ?");
    let mut statement = conn.prepare(&sql).context("preparing shadow record read")?;
    let mut rows = statement.query(rusqlite::params![JOB_TYPE_SHADOW_REVIEW, job_id])?;
    rows.next()?.map(record_from_row).transpose()
}

/// Every shadow record for an item, oldest first.
pub fn list_shadow_records(conn: &Connection, item_id: &str) -> Result<Vec<ShadowRecord>> {
    let sql = format!("{RECORD_SELECT} AND jobs.item_id = ? ORDER BY jobs.created_at, jobs.job_id");
    let mut statement = conn.prepare(&sql).context("preparing shadow record list")?;
    let mut rows = statement.query(rusqlite::params![JOB_TYPE_SHADOW_REVIEW, item_id])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(record_from_row(row)?);
    }
    Ok(records)
}

pub(super) fn find_logical_record(
    conn: &Connection,
    item_id: &str,
    source_revision: i32,
    request_version: &str,
    review_profile_version: &str,
) -> Result<Option<ShadowRecord>> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT job_id FROM jobs WHERE job_type = ? AND item_id = ? AND source_revision = ? \
             AND request_version = ? AND profile_version = ?",
            rusqlite::params![
                JOB_TYPE_SHADOW_REVIEW,
                item_id,
                source_revision,
                request_version,
                review_profile_version
            ],
            |row| row.get(0),
        )
        .optional()
        .context("looking up existing shadow job")?;
    match existing {
        Some(job_id) => load_shadow_record(conn, &job_id),
        None => Ok(None),
    }
}
