use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};

use super::policy::ShadowPolicy;
use super::record::{
    load_shadow_record, ShadowRecord, UnreviewedReason, ERROR_PREFIX, UNREVIEWED_PREFIX,
};
use crate::jobs::queue::{fail_job_with_backoff_in_tx, JobStatus};
use crate::privacy::routing::{
    authorize_job, Authorization, AuthorizationDecision, DenialReason, JOB_TYPE_SHADOW_REVIEW,
};
use crate::providers::contracts::{FailureKind, ProviderFailure};
use crate::store::schema::Database;

const RETRY_BASE_BACKOFF_SECONDS: i64 = 30;
const RETRY_MAX_BACKOFF_SECONDS: i64 = 900;

/// Why a claimed shadow job may not be sent to the reviewer right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowDispatchDenial {
    Disabled,
    InvalidPolicy,
    JobNotFound,
    NotAShadowJob,
    /// The job is not running, or the caller's lease is not the current attempt.
    NotLeased,
    /// The case already used its per-sample request allowance.
    AttemptsExhausted,
    ItemUnavailable,
    /// The job has no pinned remote review profile.
    NoRemoteProfile,
    /// The stored route, destination or profile no longer authorizes the `review` capability.
    NotAuthorized(DenialReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowDispatchDecision {
    /// The only value that permits a provider request; send only to its destinations, using its
    /// pinned profile version.
    Allowed(Authorization),
    Denied(ShadowDispatchDenial),
}

struct JobGateRow {
    job_type: String,
    status: String,
    attempt_count: i32,
    item_deleted: bool,
}

fn read_gate_row(conn: &Connection, job_id: &str) -> Result<Option<JobGateRow>> {
    conn.query_row(
        "SELECT jobs.job_type, jobs.status, jobs.attempt_count, items.lifecycle_state = 'deleted' \
         FROM jobs JOIN items ON items.item_id = jobs.item_id WHERE jobs.job_id = ?",
        [job_id],
        |row| {
            Ok(JobGateRow {
                job_type: row.get(0)?,
                status: row.get(1)?,
                attempt_count: row.get(2)?,
                item_deleted: row.get(3)?,
            })
        },
    )
    .optional()
    .context("reading shadow job for dispatch")
}

/// Decide, from current durable state, whether the attempt `lease_attempt` of a claimed shadow
/// job may call the reviewer. Run it before every attempt, retries included: the policy switch,
/// the per-sample request allowance and the route's `review` grant are all re-read each time, so
/// disabling shadow review, revoking the profile or removing the grant stops the next request.
///
/// An `Allowed` decision also stamps `now` as the case's latest dispatch time in the same
/// transaction, so the request is accounted to the window it is actually sent in even when the
/// case was selected, or last retried, in an earlier window. A denial writes nothing.
pub fn authorize_shadow_dispatch(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    policy: &ShadowPolicy,
    now: DateTime<Utc>,
) -> Result<ShadowDispatchDecision> {
    let tx = db.immediate_transaction()?;
    let decision = decide_dispatch(&tx, job_id, lease_attempt, policy)?;
    if matches!(decision, ShadowDispatchDecision::Allowed(_)) {
        stamp_dispatch(&tx, job_id, lease_attempt, now)?;
        tx.commit()?;
    }
    Ok(decision)
}

/// The dispatch time lives in `next_attempt_at`, which the queue ignores while a job is running
/// and which a later retry or terminal write leaves intact or replaces with a later instant.
/// The stamp never moves backwards.
fn stamp_dispatch(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    now: DateTime<Utc>,
) -> Result<()> {
    let existing: Option<String> = tx.query_row(
        "SELECT next_attempt_at FROM jobs WHERE job_id = ?",
        [job_id],
        |row| row.get(0),
    )?;
    let stamp = match existing {
        Some(text) => {
            let existing_instant = DateTime::parse_from_rfc3339(&text)
                .with_context(|| format!("shadow job {job_id} has a malformed timestamp"))?
                .with_timezone(&Utc);
            existing_instant.max(now)
        }
        None => now,
    };
    let updated = tx.execute(
        "UPDATE jobs SET next_attempt_at = ? \
         WHERE job_id = ? AND job_type = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            stamp.to_rfc3339(),
            job_id,
            JOB_TYPE_SHADOW_REVIEW,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    if updated != 1 {
        return Err(anyhow!(
            "shadow job {job_id} lost its lease during dispatch"
        ));
    }
    Ok(())
}

fn decide_dispatch(
    conn: &Connection,
    job_id: &str,
    lease_attempt: i32,
    policy: &ShadowPolicy,
) -> Result<ShadowDispatchDecision> {
    use ShadowDispatchDecision::Denied;

    if !policy.enabled {
        return Ok(Denied(ShadowDispatchDenial::Disabled));
    }
    if policy.validate().is_err() {
        return Ok(Denied(ShadowDispatchDenial::InvalidPolicy));
    }
    let Some(job) = read_gate_row(conn, job_id)? else {
        return Ok(Denied(ShadowDispatchDenial::JobNotFound));
    };
    if job.job_type != JOB_TYPE_SHADOW_REVIEW {
        return Ok(Denied(ShadowDispatchDenial::NotAShadowJob));
    }
    if job.status != JobStatus::Running.as_str() || job.attempt_count != lease_attempt {
        return Ok(Denied(ShadowDispatchDenial::NotLeased));
    }
    if i64::from(lease_attempt) > i64::from(policy.max_attempts_per_sample) {
        return Ok(Denied(ShadowDispatchDenial::AttemptsExhausted));
    }
    if job.item_deleted {
        return Ok(Denied(ShadowDispatchDenial::ItemUnavailable));
    }
    match authorize_job(conn, job_id)? {
        AuthorizationDecision::Denied(reason) => {
            Ok(Denied(ShadowDispatchDenial::NotAuthorized(reason)))
        }
        AuthorizationDecision::Authorized(authorization) if authorization.is_local() => {
            Ok(Denied(ShadowDispatchDenial::NoRemoteProfile))
        }
        AuthorizationDecision::Authorized(authorization) => {
            Ok(ShadowDispatchDecision::Allowed(authorization))
        }
    }
}

fn kind_code(kind: FailureKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn require_shadow_job(tx: &Transaction<'_>, job_id: &str) -> Result<()> {
    match read_gate_row(tx, job_id)? {
        Some(job) if job.job_type == JOB_TYPE_SHADOW_REVIEW => Ok(()),
        Some(_) => Err(anyhow!("job {job_id} is not a shadow review job")),
        None => Err(anyhow!("job {job_id} not found")),
    }
}

/// End the leased attempt as a terminal failure with a fixed reason code. Repeating the same
/// report for the same attempt is a no-op; a stale or different report is rejected.
fn finish_terminal(
    tx: &Transaction<'_>,
    job_id: &str,
    lease_attempt: i32,
    reason: &str,
) -> Result<()> {
    let updated = tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE job_id = ? AND job_type = ? AND status = ? AND attempt_count = ?",
        rusqlite::params![
            JobStatus::Failed.as_str(),
            reason,
            job_id,
            JOB_TYPE_SHADOW_REVIEW,
            JobStatus::Running.as_str(),
            lease_attempt
        ],
    )?;
    if updated == 1 {
        return Ok(());
    }
    let already_recorded: Option<bool> = tx
        .query_row(
            "SELECT status = ? AND failure_reason = ? AND attempt_count = ? \
             FROM jobs WHERE job_id = ? AND job_type = ?",
            rusqlite::params![
                JobStatus::Failed.as_str(),
                reason,
                lease_attempt,
                job_id,
                JOB_TYPE_SHADOW_REVIEW
            ],
            |row| row.get(0),
        )
        .optional()?;
    match already_recorded {
        Some(true) => Ok(()),
        _ => Err(anyhow!(
            "shadow job {job_id} is not running under lease attempt {lease_attempt}"
        )),
    }
}

fn record_after<F>(db: &mut Database, job_id: &str, write: F) -> Result<ShadowRecord>
where
    F: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let tx = db.immediate_transaction()?;
    require_shadow_job(&tx, job_id)?;
    write(&tx)?;
    let record = load_shadow_record(&tx, job_id)?
        .ok_or_else(|| anyhow!("shadow record {job_id} missing after write"))?;
    tx.commit()?;
    Ok(record)
}

/// Record that the leased attempt ended with no review verdict.
pub fn record_shadow_unreviewed(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    reason: UnreviewedReason,
) -> Result<ShadowRecord> {
    record_after(db, job_id, |tx| {
        finish_terminal(
            tx,
            job_id,
            lease_attempt,
            &format!("{UNREVIEWED_PREFIX}{}", reason.code()),
        )
    })
}

/// Record a normalized provider failure for the leased attempt. A timeout or cancellation ends
/// the case as unreviewed, with no further round. Another transient failure is retried with
/// backoff while the per-sample allowance lasts, inside the request budget already reserved;
/// everything else ends the case as a terminal error.
pub fn record_shadow_failure(
    db: &mut Database,
    job_id: &str,
    lease_attempt: i32,
    failure: &ProviderFailure,
    policy: &ShadowPolicy,
    now: DateTime<Utc>,
) -> Result<ShadowRecord> {
    match failure.kind {
        FailureKind::Timeout => {
            return record_shadow_unreviewed(db, job_id, lease_attempt, UnreviewedReason::Timeout)
        }
        FailureKind::Cancelled => {
            return record_shadow_unreviewed(db, job_id, lease_attempt, UnreviewedReason::Cancelled)
        }
        _ => {}
    }
    let reason = format!("{ERROR_PREFIX}{}", kind_code(failure.kind));
    let retry =
        failure.retriable && i64::from(lease_attempt) < i64::from(policy.max_attempts_per_sample);
    record_after(db, job_id, |tx| {
        if retry {
            fail_job_with_backoff_in_tx(
                tx,
                job_id,
                reason,
                RETRY_BASE_BACKOFF_SECONDS,
                RETRY_MAX_BACKOFF_SECONDS,
                now,
                lease_attempt,
            )
        } else {
            finish_terminal(tx, job_id, lease_attempt, &reason)
        }
    })
}
