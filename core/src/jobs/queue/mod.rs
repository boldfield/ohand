// Durable job queue with lease management.
// Implementation owned by J01.

use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;
use std::str::FromStr;

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

    let created_at_str: String = row.get(12)?;
    let created_at = match created_at_str.parse::<DateTime<Utc>>() {
        Ok(dt) => dt,
        Err(_) => {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid datetime format".to_string(),
            ))
        }
    };

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
        next_attempt_at: row
            .get::<_, Option<String>>(9)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
        lease_expires_at: row
            .get::<_, Option<String>>(10)?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
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
) -> Result<Job> {
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

    let now = Utc::now();
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

/// Claim a job for execution with a lease. The job must be queued, and its item must not be deleted.
/// Returns the leased job with lease_expires_at set to now + lease_duration, and status set to Running.
/// If no queued job exists, returns None.
/// If the job's item is deleted/stale, returns an error.
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
    // Find oldest queued job or expired lease
    let job: Option<Job> = tx
        .query_row(
            &format!(
                "SELECT {} FROM jobs WHERE (status = ? OR (status = ? AND lease_expires_at < ?)) \
                 ORDER BY created_at ASC LIMIT 1",
                JOB_COLUMNS
            ),
            rusqlite::params![
                JobStatus::Queued.as_str(),
                JobStatus::Running.as_str(),
                now.to_rfc3339()
            ],
            job_from_row,
        )
        .optional()?;

    let Some(job) = job else {
        return Ok(None);
    };

    // Check that the job's item is not deleted
    let item_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE item_id = ? AND lifecycle_state != 'deleted')",
        [&job.item_id],
        |row| row.get(0),
    )?;

    if !item_exists {
        return Err(anyhow!(
            "Cannot claim job {}: target item {} is deleted or does not exist",
            job.job_id,
            job.item_id
        ));
    }

    let lease_expires_at = now + lease_duration;
    let lease_expires_at_str = lease_expires_at.to_rfc3339();

    tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = ?, attempt_count = attempt_count + 1 \
         WHERE job_id = ?",
        rusqlite::params![
            JobStatus::Running.as_str(),
            &lease_expires_at_str,
            &job.job_id
        ],
    )?;

    Ok(Some(Job {
        job_id: job.job_id,
        job_schema_version: job.job_schema_version,
        item_id: job.item_id,
        job_type: job.job_type,
        source_revision: job.source_revision,
        profile_version: job.profile_version,
        request_version: job.request_version,
        status: JobStatus::Running,
        failure_reason: job.failure_reason,
        attempt_count: job.attempt_count + 1,
        next_attempt_at: job.next_attempt_at,
        lease_expires_at: Some(lease_expires_at),
        created_at: job.created_at,
    }))
}

/// Mark a job as completed.
pub fn complete_job(db: &mut Database, job_id: &str) -> Result<()> {
    let tx = db.transaction()?;
    complete_job_in_tx(&tx, job_id)?;
    tx.commit()?;
    Ok(())
}

pub fn complete_job_in_tx(tx: &Transaction<'_>, job_id: &str) -> Result<()> {
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = NULL WHERE job_id = ?",
        rusqlite::params![JobStatus::Completed.as_str(), job_id],
    )?;

    if affected == 0 {
        return Err(anyhow!("Job {} not found", job_id));
    }

    Ok(())
}

/// Mark a job as failed with a reason and schedule the next retry using exponential backoff.
/// Backoff formula: min(max_backoff_seconds, base_backoff_seconds * 2^attempt_count)
#[allow(clippy::too_many_arguments)]
pub fn fail_job_with_backoff(
    db: &mut Database,
    job_id: &str,
    failure_reason: String,
    base_backoff_seconds: i64,
    max_backoff_seconds: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let tx = db.transaction()?;
    fail_job_with_backoff_in_tx(
        &tx,
        job_id,
        failure_reason,
        base_backoff_seconds,
        max_backoff_seconds,
        now,
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
) -> Result<()> {
    // Get current attempt count to calculate backoff
    let attempt_count: i32 = tx.query_row(
        "SELECT attempt_count FROM jobs WHERE job_id = ?",
        [job_id],
        |row| row.get(0),
    )?;

    // Calculate backoff: min(max, base * 2^attempts)
    let backoff_seconds = std::cmp::min(
        max_backoff_seconds,
        base_backoff_seconds * 2i64.saturating_pow(attempt_count as u32),
    );

    let next_attempt_at = now + Duration::seconds(backoff_seconds);
    let next_attempt_at_str = next_attempt_at.to_rfc3339();

    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, next_attempt_at = ?, lease_expires_at = NULL \
         WHERE job_id = ?",
        rusqlite::params![JobStatus::Queued.as_str(), failure_reason, next_attempt_at_str, job_id],
    )?;

    Ok(())
}

/// Cancel a job durably. The job must exist.
pub fn cancel_job(db: &mut Database, job_id: &str) -> Result<()> {
    let tx = db.transaction()?;
    cancel_job_in_tx(&tx, job_id)?;
    tx.commit()?;
    Ok(())
}

pub fn cancel_job_in_tx(tx: &Transaction<'_>, job_id: &str) -> Result<()> {
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = NULL WHERE job_id = ?",
        rusqlite::params![JobStatus::Cancelled.as_str(), job_id],
    )?;

    if affected == 0 {
        return Err(anyhow!("Job {} not found", job_id));
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
