//! Configuration management and profile pinning for durable jobs.
//!
//! Every job is pinned to a profile_version at enqueue time. Changes to provider configuration
//! (switching default provider, editing profile, or revoking access) never silently reroute
//! existing queued work. Authorization always looks up the exact pinned profile_version.
//!
//! Credentials are resolved at execution time through [`resolve_execution_target`], which reads
//! the opaque credential reference from the pinned profile after authorization. The reference is
//! never copied into the job row; the job stores only the `profile_version` pin.
//!
//! Moving queued work to a different profile is an explicit act ([`requeue_job_to_new_profile`])
//! that retires the old job and records the old-to-new link in `job_requeues`. Revocation
//! ([`revoke_profile`]) stamps `revoked_at` on the profile and retires every queued or running
//! job pinned to it.

use crate::jobs::queue::{enqueue_job_in_tx, get_job_internal, Job};
use crate::privacy::routing::{authorize_job, AuthorizationDecision, DenialReason};
use crate::store::schema::Database;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};

/// Check if a profile version is currently available (not revoked).
/// Returns `true` if the profile exists and has not been revoked.
/// Returns `false` if the profile doesn't exist or has been revoked.
pub fn is_profile_available(conn: &Connection, profile_version: &str) -> Result<bool> {
    let revoked_at: Option<Option<String>> = conn
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| row.get(0),
        )
        .optional()
        .context("checking profile availability")?;

    match revoked_at {
        None => Ok(false),          // Profile doesn't exist
        Some(None) => Ok(true),     // Profile exists and not revoked
        Some(Some(_)) => Ok(false), // Profile exists but is revoked
    }
}

/// Get the profile_id of a given profile_version to help identify what was revoked.
pub fn get_profile_id(conn: &Connection, profile_version: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT profile_id FROM provider_profiles WHERE profile_version = ?",
        [profile_version],
        |row| row.get(0),
    )
    .optional()
    .context("getting profile ID")
}

/// Get all jobs currently pinned to a specific profile_version.
/// Used to identify which jobs will be affected by a profile change.
pub fn get_jobs_pinned_to_profile(conn: &Connection, profile_version: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT job_id FROM jobs WHERE profile_version = ? AND status IN ('queued', 'running')",
        )
        .context("preparing statement")?;

    let job_ids = stmt
        .query_map([profile_version], |row| row.get(0))
        .context("querying jobs")?
        .collect::<Result<Vec<String>, _>>()
        .context("collecting job IDs")?;

    Ok(job_ids)
}

/// Get all jobs for a given item that are pinned to a specific profile_version.
/// Used to check if an item's queued work should be requeued after profile change.
pub fn get_item_jobs_for_profile(
    conn: &Connection,
    item_id: &str,
    profile_version: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT job_id FROM jobs WHERE item_id = ? AND profile_version = ? AND status IN ('queued', 'running')",
        )
        .context("preparing statement")?;

    let job_ids = stmt
        .query_map([item_id, profile_version], |row| row.get(0))
        .context("querying item jobs")?
        .collect::<Result<Vec<String>, _>>()
        .context("collecting job IDs")?;

    Ok(job_ids)
}

/// Check if a job's profile has been revoked. Used during authorization to make late results
/// from a revoked profile ineligible for application.
/// Returns `true` if the pinned profile has been revoked or no longer exists, matching
/// [`is_profile_available`] (a missing pin target is never dispatchable).
/// Returns `false` if the pinned profile is live or the job is local-only (no pin).
pub fn is_job_profile_revoked(conn: &Connection, job_id: &str) -> Result<bool> {
    let profile_version: Option<Option<String>> = conn
        .query_row(
            "SELECT profile_version FROM jobs WHERE job_id = ?",
            [job_id],
            |row| row.get(0),
        )
        .optional()
        .context("getting job profile")?;

    match profile_version {
        None => Err(anyhow!("Job {} not found", job_id)),
        Some(None) => Ok(false), // Job has no profile pin (local only, never revoked)
        Some(Some(pin)) => {
            // Check if pinned profile is revoked
            let revoked_at: Option<Option<String>> = conn
                .query_row(
                    "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
                    [&pin],
                    |row| row.get(0),
                )
                .optional()
                .context("checking if job profile is revoked")?;

            match revoked_at {
                None => Ok(true),          // Pinned profile is missing: unavailable
                Some(None) => Ok(false),   // Profile exists and has not been revoked
                Some(Some(_)) => Ok(true), // Profile has been explicitly revoked
            }
        }
    }
}

/// Revoke a profile in its own transaction. See [`revoke_profile_and_retire_jobs_in_tx`].
pub fn revoke_profile(db: &mut Database, profile_version: &str, now: DateTime<Utc>) -> Result<()> {
    let tx = db.immediate_transaction()?;
    revoke_profile_and_retire_jobs_in_tx(&tx, profile_version, now)?;
    tx.commit()?;
    Ok(())
}

/// Revoke a profile and retire every queued or running job pinned to it in one transaction.
/// Retired jobs become `cancelled` with failure_reason `profile_revoked`, so they are never
/// claimed again, and a late result for a retired lease is rejected because the job is no
/// longer `running`. Revoking an already-revoked profile keeps the original `revoked_at`.
/// Fails if `profile_version` does not exist.
pub fn revoke_profile_and_retire_jobs_in_tx(
    tx: &Transaction<'_>,
    profile_version: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let revoked_at_str = now.to_rfc3339();
    let profile_exists = tx
        .query_row(
            "SELECT 1 FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |_| Ok(()),
        )
        .optional()
        .context("looking up profile to revoke")?
        .is_some();
    if !profile_exists {
        return Err(anyhow!(
            "Cannot revoke unknown profile version {}",
            profile_version
        ));
    }

    tx.execute(
        "UPDATE provider_profiles SET revoked_at = ? WHERE profile_version = ? AND revoked_at IS NULL",
        [revoked_at_str.as_str(), profile_version],
    )
    .context("revoking profile")?;

    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE profile_version = ? AND status IN ('queued', 'running')",
        rusqlite::params!["cancelled", "profile_revoked", profile_version],
    )
    .context("retiring jobs for revoked profile")?;

    Ok(())
}

/// Durable record of one explicit requeue: `old_job_id` was retired and `new_job_id` replaced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequeueRecord {
    pub old_job_id: String,
    pub new_job_id: String,
    pub from_profile_version: Option<String>,
    pub to_profile_version: String,
    pub requeued_at: DateTime<Utc>,
}

/// Find the requeue record in which `job_id` is either the retired job or its replacement.
pub fn get_requeue_record(conn: &Connection, job_id: &str) -> Result<Option<RequeueRecord>> {
    let row: Option<(String, String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT old_job_id, new_job_id, from_profile_version, to_profile_version, requeued_at \
             FROM job_requeues WHERE old_job_id = ?1 OR new_job_id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .context("reading requeue record")?;

    row.map(
        |(old_job_id, new_job_id, from_profile_version, to_profile_version, requeued_at)| {
            let requeued_at = DateTime::parse_from_rfc3339(&requeued_at)
                .context("malformed requeued_at")?
                .with_timezone(&Utc);
            Ok(RequeueRecord {
                old_job_id,
                new_job_id,
                from_profile_version,
                to_profile_version,
                requeued_at,
            })
        },
    )
    .transpose()
}

/// Explicitly move a queued or running job to another profile version in its own transaction.
/// See [`requeue_job_to_new_profile_in_tx`].
pub fn requeue_job_to_new_profile(
    db: &mut Database,
    old_job_id: &str,
    new_job_id: String,
    new_profile_version: String,
    now: DateTime<Utc>,
) -> Result<Job> {
    let tx = db.immediate_transaction()?;
    let replacement =
        requeue_job_to_new_profile_in_tx(&tx, old_job_id, new_job_id, new_profile_version, now)?;
    tx.commit()?;
    Ok(replacement)
}

/// Retire `old_job_id` (`cancelled`, failure_reason `requeued`) and enqueue a replacement pinned
/// to `new_profile_version`, recording the link in `job_requeues`.
///
/// The replacement copies item, job type, source revision, schema version and request version
/// from the old job and goes through the shared enqueue duplicate guard, so equivalent work that
/// already exists on the target profile makes the whole requeue fail without retiring anything.
/// The target profile must exist, must not be revoked and must differ from the current pin;
/// local-only jobs (no pin) cannot be moved to a provider. Retrying a requeue that already
/// happened to the same target returns the existing replacement; a different target is an error.
pub fn requeue_job_to_new_profile_in_tx(
    tx: &Transaction<'_>,
    old_job_id: &str,
    new_job_id: String,
    new_profile_version: String,
    now: DateTime<Utc>,
) -> Result<Job> {
    if let Some(record) =
        get_requeue_record(tx, old_job_id)?.filter(|record| record.old_job_id == old_job_id)
    {
        if record.to_profile_version == new_profile_version {
            return get_job_internal(tx, &record.new_job_id)?
                .ok_or_else(|| anyhow!("Replacement job {} is missing", record.new_job_id));
        }
        return Err(anyhow!(
            "Job {} was already requeued to {} ({}) and cannot be requeued again",
            old_job_id,
            record.to_profile_version,
            record.new_job_id
        ));
    }

    let old_job =
        get_job_internal(tx, old_job_id)?.ok_or_else(|| anyhow!("Job {} not found", old_job_id))?;
    if !matches!(
        old_job.status,
        crate::jobs::queue::JobStatus::Queued | crate::jobs::queue::JobStatus::Running
    ) {
        return Err(anyhow!(
            "Job {} is {} and cannot be requeued",
            old_job_id,
            old_job.status.as_str()
        ));
    }
    let Some(from_profile_version) = old_job.profile_version.clone() else {
        return Err(anyhow!(
            "Job {} is local-only and cannot be requeued to a provider profile",
            old_job_id
        ));
    };
    if from_profile_version == new_profile_version {
        return Err(anyhow!(
            "Job {} is already pinned to profile {}",
            old_job_id,
            new_profile_version
        ));
    }

    let target_revoked_at: Option<Option<String>> = tx
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [&new_profile_version],
            |row| row.get(0),
        )
        .optional()
        .context("checking new profile availability")?;
    match target_revoked_at {
        None => {
            return Err(anyhow!(
                "New profile {} does not exist",
                new_profile_version
            ))
        }
        Some(Some(_)) => {
            return Err(anyhow!(
                "New profile {} is revoked and cannot be used for requeue",
                new_profile_version
            ))
        }
        Some(None) => {}
    }

    // Enqueue first: the duplicate guard fails before anything is retired.
    let replacement = enqueue_job_in_tx(
        tx,
        new_job_id,
        old_job.item_id.clone(),
        old_job.job_type.clone(),
        old_job.source_revision,
        Some(new_profile_version.clone()),
        old_job.request_version.clone(),
        old_job.job_schema_version,
        now,
    )?;

    let retired = tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE job_id = ? AND status IN ('queued', 'running')",
        rusqlite::params!["cancelled", "requeued", old_job_id],
    )?;
    if retired == 0 {
        return Err(anyhow!(
            "Job {} could not be retired for requeue",
            old_job_id
        ));
    }

    tx.execute(
        "INSERT INTO job_requeues (
            new_job_id, old_job_id, from_profile_version, to_profile_version, requeued_at
        ) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            &replacement.job_id,
            old_job_id,
            &from_profile_version,
            &new_profile_version,
            now.to_rfc3339()
        ],
    )?;

    Ok(replacement)
}

/// What a job may do at execution time, resolved from the job's pinned profile.
#[derive(Clone, PartialEq, Eq)]
pub enum ExecutionResolution {
    /// On-device work: no provider, destination or credential is involved.
    Local,
    /// Remote work, authorized against the pinned profile version.
    Remote(ExecutionTarget),
    /// Dispatch is not allowed; nothing was resolved and no credential reference is exposed.
    Denied(DenialReason),
}

impl std::fmt::Debug for ExecutionResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionResolution::Local => f.write_str("Local"),
            ExecutionResolution::Remote(target) => f.debug_tuple("Remote").field(target).finish(),
            ExecutionResolution::Denied(reason) => f.debug_tuple("Denied").field(reason).finish(),
        }
    }
}

/// Provider settings for one dispatch, read from the pinned profile row at execution time.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecutionTarget {
    pub job_id: String,
    pub profile_version: String,
    pub provider_type: String,
    pub model: String,
    /// Bare https origins the payload may be sent to.
    pub destinations: Vec<String>,
    /// Opaque reference the native credential service resolves; never the secret itself.
    pub credential_ref: Option<String>,
}

impl std::fmt::Debug for ExecutionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionTarget")
            .field("job_id", &self.job_id)
            .field("profile_version", &self.profile_version)
            .field("provider_type", &self.provider_type)
            .field("model", &self.model)
            .field("destinations", &self.destinations)
            .field(
                "credential_ref",
                &self.credential_ref.as_ref().map(|_| "<ref>"),
            )
            .finish()
    }
}

/// Resolve how `job_id` may execute: authorize it (revocation, route and capability policy), then
/// read the credential reference from the profile version the job is pinned to. The reference is
/// looked up on every call, so a later change to the stored profile is observed at execution time
/// and nothing credential-related is ever stored on the job.
pub fn resolve_execution_target(conn: &Connection, job_id: &str) -> Result<ExecutionResolution> {
    let authorization = match authorize_job(conn, job_id)? {
        AuthorizationDecision::Denied(reason) => return Ok(ExecutionResolution::Denied(reason)),
        AuthorizationDecision::Authorized(authorization) => authorization,
    };
    let Some(profile_version) = authorization.profile_version() else {
        return Ok(ExecutionResolution::Local);
    };

    let (provider_type, model, credential_ref): (String, String, Option<String>) = conn
        .query_row(
            "SELECT provider_type, model, credential_ref FROM provider_profiles \
             WHERE profile_version = ?",
            [profile_version],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("reading pinned profile for execution")?;

    Ok(ExecutionResolution::Remote(ExecutionTarget {
        job_id: job_id.to_string(),
        profile_version: profile_version.to_string(),
        provider_type,
        model,
        destinations: authorization.destinations().to_vec(),
        credential_ref: credential_ref.filter(|reference| !reference.is_empty()),
    }))
}
