//! Lease-fenced settlements the runner applies on behalf of a capability that did not settle the
//! job itself. Every statement names the job's lease (`attempt_count`) and `running` status, so a
//! holder whose lease was reclaimed changes nothing.

use crate::jobs::queue::{get_job, JobStatus};
use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};

/// Job failure reason recorded when the runner's own retry limit is spent.
pub const RETRIES_EXHAUSTED_REASON: &str = "retries_exhausted";
/// Job failure reason recorded while no capability is registered for the job's type.
pub const CAPABILITY_UNAVAILABLE_REASON: &str = "capability_unavailable";
/// Job failure reason recorded when a job is put back at a checkpoint.
pub const INTERRUPTED_REASON: &str = "interrupted";

/// How a counted transient failure was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransientSettlement {
    Requeued {
        retry_at: DateTime<Utc>,
        failures: u32,
    },
    Exhausted,
}

/// Count one transient failure against the job and, in the same transaction, either re-queue it
/// with exponential backoff (`min(max, base * 2^failures)`) or, when this was the
/// `max_attempts`-th failure, end it `failed` with [`RETRIES_EXHAUSTED_REASON`].
#[allow(clippy::too_many_arguments)]
pub(super) fn settle_transient_failure(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    reason: &str,
    max_attempts: u32,
    base_backoff: Duration,
    max_backoff: Duration,
    now: DateTime<Utc>,
) -> Result<TransientSettlement> {
    let tx = db.immediate_transaction()?;
    let counted = tx.execute(
        "UPDATE jobs SET transient_failure_count = transient_failure_count + 1
          WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![job_id, JobStatus::Running.as_str(), lease_attempt],
    )?;
    if counted != 1 {
        return Err(anyhow!("job {job_id} lease is no longer held"));
    }
    let failures: i64 = tx.query_row(
        "SELECT transient_failure_count FROM jobs WHERE job_id = ?",
        [job_id],
        |row| row.get(0),
    )?;
    let failures = u32::try_from(failures).unwrap_or(u32::MAX);

    if failures >= max_attempts {
        tx.execute(
            "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL
              WHERE job_id = ? AND status = ? AND attempt_count = ?",
            rusqlite::params![
                JobStatus::Failed.as_str(),
                RETRIES_EXHAUSTED_REASON,
                job_id,
                JobStatus::Running.as_str(),
                lease_attempt
            ],
        )?;
        tx.commit()?;
        return Ok(TransientSettlement::Exhausted);
    }

    let retry_at = backoff_retry_at(now, failures, base_backoff, max_backoff);
    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, next_attempt_at = ?, lease_expires_at = NULL
          WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Queued.as_str(),
            reason,
            retry_at.to_rfc3339(),
            job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    tx.commit()?;
    Ok(TransientSettlement::Requeued { retry_at, failures })
}

fn backoff_retry_at(
    now: DateTime<Utc>,
    failures: u32,
    base_backoff: Duration,
    max_backoff: Duration,
) -> DateTime<Utc> {
    let base_milliseconds = base_backoff.num_milliseconds().max(0);
    let max_milliseconds = max_backoff.num_milliseconds().max(0);
    let delay_milliseconds = std::cmp::min(
        max_milliseconds,
        base_milliseconds.saturating_mul(2i64.saturating_pow(failures)),
    );
    // Job timestamps are RFC 3339 text compared as strings, which orders correctly only while the
    // year has four digits.
    let latest = DateTime::parse_from_rfc3339("9999-12-31T23:59:59.999+00:00")
        .expect("constant timestamp parses")
        .with_timezone(&Utc);
    Duration::try_milliseconds(delay_milliseconds)
        .and_then(|delay| now.checked_add_signed(delay))
        .map_or(latest, |retry_at| std::cmp::min(retry_at, latest))
}

/// End the job `failed` for a reason retrying cannot fix.
pub(super) fn settle_permanent_failure(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    reason: &str,
) -> Result<()> {
    let affected = db.conn().execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL
          WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Failed.as_str(),
            reason,
            job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!("job {job_id} lease is no longer held"));
    }
    Ok(())
}

/// Put the job back after `delay` without spending any retry budget.
pub(super) fn settle_without_cost(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    reason: &str,
    delay: Duration,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let retry_at = now
        .checked_add_signed(std::cmp::max(delay, Duration::zero()))
        .unwrap_or(now);
    let affected = db.conn().execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, next_attempt_at = ?, lease_expires_at = NULL
          WHERE job_id = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Queued.as_str(),
            reason,
            retry_at.to_rfc3339(),
            job_id,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!("job {job_id} lease is no longer held"));
    }
    Ok(retry_at)
}

/// Whether the job is still running under exactly this lease.
pub(super) fn still_holds_lease(db: &Database, job_id: &str, lease_attempt: i32) -> Result<bool> {
    Ok(get_job(db, job_id)?
        .map(|job| job.status == JobStatus::Running && job.attempt_count == lease_attempt)
        .unwrap_or(false))
}
