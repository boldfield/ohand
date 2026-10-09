use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::policy::ShadowPolicy;
use super::record::{find_logical_record, load_shadow_record, ShadowRecord};
use crate::jobs::queue::{enqueue_job_in_tx, JobStatus};
use crate::privacy::routing::{
    authorize_job, AuthorizationDecision, DenialReason, JOB_TYPE_SHADOW_REVIEW,
};
use crate::store::schema::Database;

const SHADOW_JOB_SCHEMA_VERSION: i32 = 1;

/// The case being offered for sampling: one interpretation request's result on one item revision,
/// to be reviewed through one separately approved review profile version.
#[derive(Debug, Clone, Copy)]
pub struct ShadowCandidate<'a> {
    pub item_id: &'a str,
    pub source_revision: i32,
    pub request_version: &'a str,
    pub review_profile_version: &'a str,
}

/// Why nothing was reserved. Every skip leaves storage unchanged and dispatches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowSkip {
    /// Shadow review is off, which is the default.
    Disabled,
    InvalidPolicy,
    /// The item is missing, deleted, or no longer at the candidate's revision.
    ItemUnavailable,
    NotSampled,
    BudgetExhausted {
        requests_in_use: u32,
        requests_needed: u32,
        limit: u32,
    },
    /// The route, destination or review profile is not approved for the `review` capability.
    NotAuthorized(DenialReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowSelection {
    /// A new case was reserved; its job is queued.
    Selected(ShadowRecord),
    /// The same case was already reserved by an earlier call; nothing was added.
    AlreadySelected(ShadowRecord),
    Skipped(ShadowSkip),
}

/// Provider requests reserved or spent that count against the window ending at `now`.
///
/// A case that is still open (queued, running or waiting to retry) holds its full per-sample
/// reservation however old it is, because none of that reservation is settled yet. A finished
/// case holds only the requests it actually used, counted while its latest activity is inside
/// the window. Latest activity is the later of the case's creation and the last dispatch
/// authorization stamped by [`super::authorize_shadow_dispatch`], so requests are accounted when
/// they are sent, not only when the case was selected.
pub fn requests_in_use(
    conn: &Connection,
    policy: &ShadowPolicy,
    now: DateTime<Utc>,
) -> Result<u32> {
    let window_start = now - Duration::seconds(policy.window_seconds);
    let mut statement = conn
        .prepare(
            "SELECT job_id, status, attempt_count, created_at, next_attempt_at \
             FROM jobs WHERE job_type = ?",
        )
        .context("preparing shadow budget read")?;
    let mut rows = statement.query([JOB_TYPE_SHADOW_REVIEW])?;
    let mut in_use: u64 = 0;
    while let Some(row) = rows.next()? {
        let job_id: String = row.get(0)?;
        let status: JobStatus = row.get::<_, String>(1)?.parse()?;
        let attempts = u64::try_from(row.get::<_, i32>(2)?).unwrap_or(0);
        let created_text: String = row.get(3)?;
        let stamped_text: Option<String> = row.get(4)?;
        match status {
            JobStatus::Queued | JobStatus::Running => {
                in_use += attempts.max(u64::from(policy.max_attempts_per_sample));
            }
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled => {
                let mut latest_activity = parse_instant(&job_id, &created_text)?;
                if let Some(stamped_text) = stamped_text {
                    latest_activity = latest_activity.max(parse_instant(&job_id, &stamped_text)?);
                }
                if latest_activity > window_start {
                    in_use += attempts;
                }
            }
        }
    }
    Ok(u32::try_from(in_use).unwrap_or(u32::MAX))
}

fn parse_instant(job_id: &str, text: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(text)
        .with_context(|| format!("shadow job {job_id} has a malformed timestamp"))?
        .with_timezone(&Utc))
}

/// Offer one case for shadow review. A job is queued only when, in order: the policy is enabled
/// and valid, the item still exists at the candidate revision, the case is sampled, the window
/// budget can reserve a full per-sample allowance, and the route has a separate stored `review`
/// grant covering the review profile's destinations. The check and the reservation happen in one
/// immediate transaction, so concurrent callers cannot overspend the budget, and a refusal rolls
/// back everything. Offering the same case again returns the existing record and reserves
/// nothing more.
pub fn select_for_shadow_review(
    db: &mut Database,
    candidate: &ShadowCandidate<'_>,
    policy: &ShadowPolicy,
    now: DateTime<Utc>,
) -> Result<ShadowSelection> {
    if !policy.enabled {
        return Ok(ShadowSelection::Skipped(ShadowSkip::Disabled));
    }
    if policy.validate().is_err()
        || candidate.request_version.trim().is_empty()
        || candidate.review_profile_version.trim().is_empty()
    {
        return Ok(ShadowSelection::Skipped(ShadowSkip::InvalidPolicy));
    }

    let tx = db.immediate_transaction()?;

    let item_current: Option<bool> = tx
        .query_row(
            "SELECT lifecycle_state != 'deleted' AND revision = ? FROM items WHERE item_id = ?",
            rusqlite::params![candidate.source_revision, candidate.item_id],
            |row| row.get(0),
        )
        .optional()
        .context("reading item for shadow selection")?;
    if item_current != Some(true) {
        return Ok(ShadowSelection::Skipped(ShadowSkip::ItemUnavailable));
    }

    if let Some(existing) = find_logical_record(
        &tx,
        candidate.item_id,
        candidate.source_revision,
        candidate.request_version,
        candidate.review_profile_version,
    )? {
        return Ok(ShadowSelection::AlreadySelected(existing));
    }

    if !policy.samples(
        candidate.item_id,
        candidate.source_revision,
        candidate.request_version,
        candidate.review_profile_version,
    ) {
        return Ok(ShadowSelection::Skipped(ShadowSkip::NotSampled));
    }

    let requests_in_use = requests_in_use(&tx, policy, now)?;
    let requests_needed = policy.max_attempts_per_sample;
    if u64::from(requests_in_use) + u64::from(requests_needed)
        > u64::from(policy.max_requests_per_window)
    {
        return Ok(ShadowSelection::Skipped(ShadowSkip::BudgetExhausted {
            requests_in_use,
            requests_needed,
            limit: policy.max_requests_per_window,
        }));
    }

    let job_id = Uuid::new_v4().to_string();
    enqueue_job_in_tx(
        &tx,
        job_id.clone(),
        candidate.item_id.to_string(),
        JOB_TYPE_SHADOW_REVIEW.to_string(),
        candidate.source_revision,
        Some(candidate.review_profile_version.to_string()),
        Some(candidate.request_version.to_string()),
        SHADOW_JOB_SCHEMA_VERSION,
        now,
    )?;
    match authorize_job(&tx, &job_id)? {
        AuthorizationDecision::Authorized(authorization) if !authorization.is_local() => {}
        AuthorizationDecision::Authorized(_) => {
            return Ok(ShadowSelection::Skipped(ShadowSkip::NotAuthorized(
                DenialReason::ProfileUnavailable,
            )));
        }
        AuthorizationDecision::Denied(reason) => {
            return Ok(ShadowSelection::Skipped(ShadowSkip::NotAuthorized(reason)));
        }
    }

    let record = load_shadow_record(&tx, &job_id)?
        .context("shadow job vanished inside its own transaction")?;
    tx.commit()?;
    Ok(ShadowSelection::Selected(record))
}
