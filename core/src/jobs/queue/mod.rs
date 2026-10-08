// Durable job queue with lease management.
// Implementation owned by J01.

use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;
use std::str::FromStr;

const SUPPORTED_JOB_SCHEMA_VERSION: i32 = 1;

/// Job status enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

impl FromStr for JobStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(JobStatus::Queued),
            "running" => Ok(JobStatus::Running),
            "completed" => Ok(JobStatus::Completed),
            "failed" => Ok(JobStatus::Failed),
            "cancelled" => Ok(JobStatus::Cancelled),
            _ => Err(anyhow!("Unknown job status: {}", s)),
        }
    }
}

/// A durable job in the queue.
#[derive(Clone, Debug)]
pub struct Job {
    pub job_id: String,
    pub job_schema_version: i32,
    pub item_id: String,
    pub job_type: String,
    pub source_revision: i32,
    pub profile_version: Option<String>,
    pub request_version: Option<String>,
    pub status: JobStatus,
    pub failure_reason: Option<String>,
    pub attempt_count: i32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Job({})", self.job_id)
    }
}

fn timestamp_error(column: usize, detail: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, detail.into())
}

/// Parse an optional RFC 3339 column. A malformed durable timestamp is a read error: silently
/// treating it as absent would make a backed-off job immediately claimable.
fn parse_timestamp_column(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(column)?;
    raw.map(|text| {
        DateTime::parse_from_rfc3339(&text)
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|error| {
                timestamp_error(column, format!("malformed timestamp {text:?}: {error}"))
            })
    })
    .transpose()
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let status_str: String = row.get(6)?;
    let status = match JobStatus::from_str(&status_str) {
        Ok(s) => s,
        Err(e) => {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid status: {}",
                e
            )))
        }
    };

    let created_at = parse_timestamp_column(row, 12)?
        .ok_or_else(|| timestamp_error(12, "created_at is NULL".to_string()))?;

    Ok(Job {
        job_id: row.get(0)?,
        job_schema_version: row.get(1)?,
        item_id: row.get(2)?,
        job_type: row.get(3)?,
        source_revision: row.get(4)?,
        profile_version: row.get(5)?,
        status,
        failure_reason: row.get(7)?,
        attempt_count: row.get(8)?,
        next_attempt_at: parse_timestamp_column(row, 9)?,
        lease_expires_at: parse_timestamp_column(row, 10)?,
        request_version: row.get(11)?,
        created_at,
    })
}

const JOB_COLUMNS: &str = "job_id, job_schema_version, item_id, job_type, source_revision, \
    profile_version, status, failure_reason, attempt_count, next_attempt_at, lease_expires_at, \
    request_version, created_at";

/// Enqueue a new job. The job must have a unique job_id per item/job_type/source_revision/profile_version
/// combination to prevent duplicates. Returns the enqueued job.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_job(
    db: &mut Database,
    job_id: String,
    item_id: String,
    job_type: String,
    source_revision: i32,
    profile_version: Option<String>,
    request_version: Option<String>,
    job_schema_version: i32,
    now: DateTime<Utc>,
) -> Result<Job> {
    let tx = db.immediate_transaction()?;
    let result = enqueue_job_in_tx(
        &tx,
        job_id,
        item_id,
        job_type,
        source_revision,
        profile_version,
        request_version,
        job_schema_version,
        now,
    )?;
    tx.commit()?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub fn enqueue_job_in_tx(
    tx: &Transaction<'_>,
    job_id: String,
    item_id: String,
    job_type: String,
    source_revision: i32,
    profile_version: Option<String>,
    request_version: Option<String>,
    job_schema_version: i32,
    now: DateTime<Utc>,
) -> Result<Job> {
    // Check if this job_id already exists
    let existing: Option<Job> = tx
        .query_row(
            &format!("SELECT {} FROM jobs WHERE job_id = ?", JOB_COLUMNS),
            [job_id.as_str()],
            job_from_row,
        )
        .optional()?;

    if existing.is_some() {
        return Err(anyhow!("Job with ID {} already exists", job_id));
    }

    // Check if the same logical work (item + type + source + versions) already exists
    // Prevent duplicate delivery across all terminal and non-terminal states
    let logical_dup: Option<(String,)> = tx
        .query_row(
            "SELECT job_id FROM jobs WHERE item_id = ? AND job_type = ? AND source_revision = ? AND profile_version IS ? AND request_version IS ?",
            rusqlite::params![
                &item_id,
                &job_type,
                source_revision,
                &profile_version,
                &request_version
            ],
            |row| Ok((row.get(0)?,)),
        )
        .optional()?;

    if let Some((dup_id,)) = logical_dup {
        return Err(anyhow!(
            "Logical job already exists: item={}, type={}, source={}, profile={:?}, request={:?} (job_id={})",
            item_id,
            job_type,
            source_revision,
            profile_version,
            request_version,
            dup_id
        ));
    }

    let created_at_str = now.to_rfc3339();

    tx.execute(
        "INSERT INTO jobs (
            job_id, job_schema_version, item_id, job_type, source_revision,
            profile_version, request_version, status, attempt_count, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &job_id,
            job_schema_version,
            &item_id,
            &job_type,
            source_revision,
            &profile_version,
            &request_version,
            JobStatus::Queued.as_str(),
            0,
            &created_at_str
        ],
    )?;

    Ok(Job {
        job_id,
        job_schema_version,
        item_id,
        job_type,
        source_revision,
        profile_version,
        request_version,
        status: JobStatus::Queued,
        failure_reason: None,
        attempt_count: 0,
        next_attempt_at: None,
        lease_expires_at: None,
        created_at: now,
    })
}

/// Get a job by ID.
pub fn get_job(db: &Database, job_id: &str) -> Result<Option<Job>> {
    db.conn()
        .query_row(
            &format!("SELECT {} FROM jobs WHERE job_id = ?", JOB_COLUMNS),
            [job_id],
            job_from_row,
        )
        .optional()
        .map_err(|e| anyhow!(e))
}

fn get_job_internal(tx: &Transaction<'_>, job_id: &str) -> Result<Option<Job>> {
    tx.query_row(
        &format!("SELECT {} FROM jobs WHERE job_id = ?", JOB_COLUMNS),
        [job_id],
        job_from_row,
    )
    .optional()
    .map_err(|e| anyhow!(e))
}

/// Claim the oldest eligible job: a queued job whose `next_attempt_at` has passed, or a running
/// job whose lease expired (interrupted-lease recovery). The returned job is `Running` and its
/// `attempt_count` is the lease token that `complete_job` / `fail_job_with_backoff` must present.
/// Candidates that can never run are resolved durably inside the claim transaction and skipped:
/// unsupported schema versions become `failed` (`unsupported_job_version`); jobs whose item is
/// missing, deleted or revised past `source_revision` become `cancelled` with the reason recorded.
/// Returns None if nothing is eligible.
pub fn claim_job_with_lease(
    db: &mut Database,
    lease_duration: Duration,
    now: DateTime<Utc>,
) -> Result<Option<Job>> {
    let tx = db.immediate_transaction()?;
    let result = claim_job_with_lease_in_tx(&tx, lease_duration, now)?;
    tx.commit()?;
    Ok(result)
}

pub fn claim_job_with_lease_in_tx(
    tx: &Transaction<'_>,
    lease_duration: Duration,
    now: DateTime<Utc>,
) -> Result<Option<Job>> {
    let selected_job = 'search: loop {
        // Find oldest queued job ready for attempt, or expired lease
        let job: Option<Job> = tx
            .query_row(
                &format!(
                    "SELECT {} FROM jobs WHERE (status = ? AND (next_attempt_at IS NULL OR next_attempt_at <= ?)) \
                     OR (status = ? AND lease_expires_at < ?) \
                     ORDER BY created_at ASC LIMIT 1",
                    JOB_COLUMNS
                ),
                rusqlite::params![
                    JobStatus::Queued.as_str(),
                    now.to_rfc3339(),
                    JobStatus::Running.as_str(),
                    now.to_rfc3339()
                ],
                job_from_row,
            )
            .optional()?;

        let Some(job) = job else {
            break 'search None;
        };

        // Check for unsupported job schema version
        if job.job_schema_version != SUPPORTED_JOB_SCHEMA_VERSION {
            // Mark as failed with unsupported_job_version reason
            tx.execute(
                "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL WHERE job_id = ? AND status IN (?, ?)",
                rusqlite::params![
                    JobStatus::Failed.as_str(),
                    "unsupported_job_version",
                    &job.job_id,
                    JobStatus::Queued.as_str(),
                    JobStatus::Running.as_str()
                ],
            )?;
            continue;
        }

        // Check item exists and is not deleted, and check revision staleness
        let item_result: Option<(bool, i32)> = tx
            .query_row(
                "SELECT lifecycle_state != 'deleted', revision FROM items WHERE item_id = ?",
                [&job.item_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i32>(1)?)),
            )
            .optional()?;

        let cancel_reason = match item_result {
            None => "item_not_found",
            Some((false, _)) => "item_deleted",
            Some((true, item_revision)) if item_revision != job.source_revision => "stale_revision",
            Some((true, _)) => break 'search Some(job),
        };

        // The target can no longer be mutated: cancel durably (with a reason visible offline)
        // so this job cannot block later work, then look at the next candidate.
        let cancelled = tx.execute(
            "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
             WHERE job_id = ? AND status IN (?, ?)",
            rusqlite::params![
                JobStatus::Cancelled.as_str(),
                cancel_reason,
                &job.job_id,
                JobStatus::Queued.as_str(),
                JobStatus::Running.as_str()
            ],
        )?;
        if cancelled != 1 {
            return Err(anyhow!(
                "Job {} could not be cancelled as {}",
                job.job_id,
                cancel_reason
            ));
        }
    };

    let Some(job) = selected_job else {
        return Ok(None);
    };

    // The lease token is the attempt number this claim produces: every claim increments
    // attempt_count, so a stale holder's token can never match a later lease.
    let lease_attempt = job.attempt_count + 1;
    let lease_expires_at = now + lease_duration;
    let lease_expires_at_str = lease_expires_at.to_rfc3339();

    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = ?, attempt_count = ? \
         WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Running.as_str(),
            &lease_expires_at_str,
            lease_attempt,
            &job.job_id,
            job.status.as_str(),
            job.attempt_count
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "Job {} changed while being claimed; claim aborted",
            job.job_id
        ));
    }

    Ok(Some(Job {
        status: JobStatus::Running,
        attempt_count: lease_attempt,
        lease_expires_at: Some(lease_expires_at),
        ..job
    }))
}

/// Explain why a lease-guarded transition affected no rows.
fn lease_rejection(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    action: &str,
) -> anyhow::Error {
    match get_job_internal(tx, job_id) {
        Err(error) => error,
        Ok(None) => anyhow!("Job {} not found", job_id),
        Ok(Some(job)) => match job.status {
            JobStatus::Completed => anyhow!("Job {} already completed; cannot {}", job_id, action),
            JobStatus::Cancelled => {
                anyhow!("Job {} is cancelled and cannot be {}", job_id, action)
            }
            JobStatus::Failed => anyhow!("Job {} has failed terminally; cannot {}", job_id, action),
            JobStatus::Queued => anyhow!("Job {} is not running; cannot {}", job_id, action),
            JobStatus::Running => anyhow!(
                "Job {} lease mismatch: holder presented attempt {}, current lease is attempt {}",
                job_id,
                lease_attempt,
                job.attempt_count
            ),
        },
    }
}

/// Mark a job as completed. `lease_attempt` is the `attempt_count` of the job returned by the
/// claim; a stale holder whose lease was reclaimed presents an older attempt and is rejected.
pub fn complete_job(db: &mut Database, job_id: &str, lease_attempt: i32) -> Result<()> {
    let tx = db.immediate_transaction()?;
    complete_job_in_tx(&tx, job_id, lease_attempt)?;
    tx.commit()?;
    Ok(())
}

pub fn complete_job_in_tx(tx: &Transaction<'_>, job_id: &str, lease_attempt: i32) -> Result<()> {
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = NULL \
         WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Completed.as_str(),
            job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;

    if affected == 0 {
        return Err(lease_rejection(tx, job_id, lease_attempt, "be completed"));
    }

    Ok(())
}

/// Mark a job as failed with a reason and schedule the next retry using exponential backoff.
/// Backoff formula: min(max_backoff_seconds, base_backoff_seconds * 2^attempt_count)
/// `lease_attempt` fences stale holders exactly as in [`complete_job`].
#[allow(clippy::too_many_arguments)]
pub fn fail_job_with_backoff(
    db: &mut Database,
    job_id: &str,
    failure_reason: String,
    base_backoff_seconds: i64,
    max_backoff_seconds: i64,
    now: DateTime<Utc>,
    lease_attempt: i32,
) -> Result<()> {
    let tx = db.immediate_transaction()?;
    fail_job_with_backoff_in_tx(
        &tx,
        job_id,
        failure_reason,
        base_backoff_seconds,
        max_backoff_seconds,
        now,
        lease_attempt,
    )?;
    tx.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn fail_job_with_backoff_in_tx(
    tx: &Transaction<'_>,
    job_id: &str,
    failure_reason: String,
    base_backoff_seconds: i64,
    max_backoff_seconds: i64,
    now: DateTime<Utc>,
    lease_attempt: i32,
) -> Result<()> {
    // Backoff: min(max, base * 2^attempts), saturating so large attempt counts cannot overflow.
    let exponent = u32::try_from(lease_attempt).unwrap_or(0);
    let backoff_seconds = std::cmp::min(
        max_backoff_seconds,
        base_backoff_seconds.saturating_mul(2i64.saturating_pow(exponent)),
    );

    let next_attempt_at = now + Duration::seconds(backoff_seconds);
    let next_attempt_at_str = next_attempt_at.to_rfc3339();

    let affected = tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, next_attempt_at = ?, lease_expires_at = NULL \
         WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Queued.as_str(),
            failure_reason,
            next_attempt_at_str,
            job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;

    if affected == 0 {
        return Err(lease_rejection(tx, job_id, lease_attempt, "be failed"));
    }

    Ok(())
}

/// Cancel a job durably. Cancellation is idempotent; already-cancelled or completed jobs remain unchanged.
pub fn cancel_job(db: &mut Database, job_id: &str) -> Result<()> {
    let tx = db.immediate_transaction()?;
    cancel_job_in_tx(&tx, job_id)?;
    tx.commit()?;
    Ok(())
}

pub fn cancel_job_in_tx(tx: &Transaction<'_>, job_id: &str) -> Result<()> {
    // Only cancel queued or running jobs; don't overwrite completed
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = NULL WHERE job_id = ? AND status IN (?, ?)",
        rusqlite::params![
            JobStatus::Cancelled.as_str(),
            job_id,
            JobStatus::Queued.as_str(),
            JobStatus::Running.as_str()
        ],
    )?;

    if affected == 0 {
        let job = get_job_internal(tx, job_id)?;
        if job.is_none() {
            return Err(anyhow!("Job {} not found", job_id));
        }
        // Job exists but is already terminal (completed or cancelled) - idempotent, return ok
    }

    Ok(())
}

/// Get all jobs with a specific status. Useful for inspecting offline jobs.
pub fn get_jobs_by_status(db: &Database, status: JobStatus) -> Result<Vec<Job>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {} FROM jobs WHERE status = ? ORDER BY created_at DESC",
        JOB_COLUMNS
    ))?;
    let jobs = stmt
        .query_map([status.as_str()], job_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}

/// Get all jobs for an item, useful for checking if an item has pending work.
pub fn get_jobs_for_item(db: &Database, item_id: &str) -> Result<Vec<Job>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {} FROM jobs WHERE item_id = ? ORDER BY created_at DESC",
        JOB_COLUMNS
    ))?;
    let jobs = stmt
        .query_map([item_id], job_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}

/// Get all jobs with expired leases. These are candidates for recovery/retry.
pub fn get_expired_lease_jobs(db: &Database, now: DateTime<Utc>) -> Result<Vec<Job>> {
    let mut stmt = db.conn().prepare(&format!(
        "SELECT {} FROM jobs WHERE status = ? AND lease_expires_at < ? ORDER BY lease_expires_at ASC",
        JOB_COLUMNS
    ))?;
    let jobs = stmt
        .query_map(
            rusqlite::params![JobStatus::Running.as_str(), now.to_rfc3339()],
            job_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}
