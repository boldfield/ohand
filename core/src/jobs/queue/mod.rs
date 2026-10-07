// Durable job queue with lease management.
// Implementation owned by J01.

use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

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
    pub lease_id: Option<String>,
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

    let created_at_str: String = row.get(13)?;
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
        lease_id: row.get(12)?,
        created_at,
    })
}

const JOB_COLUMNS: &str = "job_id, job_schema_version, item_id, job_type, source_revision, \
    profile_version, status, failure_reason, attempt_count, next_attempt_at, lease_expires_at, \
    request_version, lease_id, created_at";

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
        lease_id: None,
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
    loop {
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
            return Ok(None);
        };

        // Check item exists and is not deleted, and check revision staleness
        let item_result: Option<(bool, i32)> = tx
            .query_row(
                "SELECT lifecycle_state != 'deleted', revision FROM items WHERE item_id = ?",
                [&job.item_id],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i32>(1)?)),
            )
            .optional()?;

        match item_result {
            None => {
                // Item doesn't exist: cancel this job and move to next
                tx.execute(
                    "UPDATE jobs SET status = ? WHERE job_id = ? AND status != ?",
                    rusqlite::params![
                        JobStatus::Cancelled.as_str(),
                        &job.job_id,
                        JobStatus::Completed.as_str()
                    ],
                )?;
                continue;
            }
            Some((false, _)) => {
                // Item is deleted: cancel this job and move to next
                tx.execute(
                    "UPDATE jobs SET status = ? WHERE job_id = ? AND status != ?",
                    rusqlite::params![
                        JobStatus::Cancelled.as_str(),
                        &job.job_id,
                        JobStatus::Completed.as_str()
                    ],
                )?;
                continue;
            }
            Some((true, item_revision)) if item_revision != job.source_revision => {
                // Item has been revised since job was created: skip as stale
                continue;
            }
            Some((true, _)) => {
                // Item is valid, proceed with claim
                break;
            }
        }
    }

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
        return Ok(None);
    };

    let lease_id = Uuid::new_v4().to_string();
    let lease_expires_at = now + lease_duration;
    let lease_expires_at_str = lease_expires_at.to_rfc3339();

    tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = ?, lease_id = ?, attempt_count = attempt_count + 1 \
         WHERE job_id = ?",
        rusqlite::params![
            JobStatus::Running.as_str(),
            &lease_expires_at_str,
            &lease_id,
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
        lease_id: Some(lease_id),
        created_at: job.created_at,
    }))
}

/// Mark a job as completed. Requires the correct lease_id to prevent stale holders from completing.
pub fn complete_job(db: &mut Database, job_id: &str, lease_id: &str) -> Result<()> {
    let tx = db.immediate_transaction()?;
    complete_job_in_tx(&tx, job_id, lease_id)?;
    tx.commit()?;
    Ok(())
}

pub fn complete_job_in_tx(tx: &Transaction<'_>, job_id: &str, lease_id: &str) -> Result<()> {
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, lease_expires_at = NULL, lease_id = NULL WHERE job_id = ? AND lease_id = ? AND status = ?",
        rusqlite::params![JobStatus::Completed.as_str(), job_id, lease_id, JobStatus::Running.as_str()],
    )?;

    if affected == 0 {
        let job = get_job_internal(tx, job_id)?;
        if let Some(j) = job {
            if j.status == JobStatus::Completed {
                return Err(anyhow!("Job {} already completed", job_id));
            } else if j.lease_id.as_deref() != Some(lease_id) {
                return Err(anyhow!(
                    "Job {} lease mismatch: expected {}, got {:?}",
                    job_id,
                    lease_id,
                    j.lease_id
                ));
            } else if j.status == JobStatus::Cancelled {
                return Err(anyhow!(
                    "Job {} is cancelled and cannot be completed",
                    job_id
                ));
            }
        }
        return Err(anyhow!("Job {} not found or not running", job_id));
    }

    Ok(())
}

/// Mark a job as failed with a reason and schedule the next retry using exponential backoff.
/// Backoff formula: min(max_backoff_seconds, base_backoff_seconds * 2^attempt_count)
/// Requires the correct lease_id to prevent stale holders from failing.
#[allow(clippy::too_many_arguments)]
pub fn fail_job_with_backoff(
    db: &mut Database,
    job_id: &str,
    failure_reason: String,
    base_backoff_seconds: i64,
    max_backoff_seconds: i64,
    now: DateTime<Utc>,
    lease_id: &str,
) -> Result<()> {
    let tx = db.immediate_transaction()?;
    fail_job_with_backoff_in_tx(
        &tx,
        job_id,
        failure_reason,
        base_backoff_seconds,
        max_backoff_seconds,
        now,
        lease_id,
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
    lease_id: &str,
) -> Result<()> {
    // Get current job state
    let job = get_job_internal(tx, job_id)?;
    let Some(job) = job else {
        return Err(anyhow!("Job {} not found", job_id));
    };

    if job.status == JobStatus::Completed {
        return Err(anyhow!("Job {} is already completed", job_id));
    }

    if job.status == JobStatus::Cancelled {
        return Err(anyhow!("Job {} is cancelled and cannot be failed", job_id));
    }

    if job.lease_id.as_deref() != Some(lease_id) {
        return Err(anyhow!(
            "Job {} lease mismatch: expected {}, got {:?}",
            job_id,
            lease_id,
            job.lease_id
        ));
    }

    // Check for unsupported job schema version
    if job.job_schema_version > 1 {
        return Err(anyhow!(
            "Job {} has unsupported schema version {}",
            job_id,
            job.job_schema_version
        ));
    }

    // Calculate backoff: min(max, base * 2^attempts)
    let backoff_seconds = std::cmp::min(
        max_backoff_seconds,
        base_backoff_seconds.saturating_mul(2i64.saturating_pow(job.attempt_count as u32)),
    );

    let next_attempt_at = now + Duration::seconds(backoff_seconds);
    let next_attempt_at_str = next_attempt_at.to_rfc3339();

    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, next_attempt_at = ?, lease_expires_at = NULL, lease_id = NULL \
         WHERE job_id = ? AND lease_id = ? AND status = ?",
        rusqlite::params![
            JobStatus::Queued.as_str(),
            failure_reason,
            next_attempt_at_str,
            job_id,
            lease_id,
            JobStatus::Running.as_str()
        ],
    )?;

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
