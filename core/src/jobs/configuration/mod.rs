//! Configuration management and profile pinning for durable jobs.
//!
//! Every job is pinned to a profile_version at enqueue time. Changes to provider configuration
//! (switching default provider, editing profile, or revoking access) never silently reroute
//! existing queued work. Authorization always looks up the exact pinned profile_version.
//!
//! Credentials are resolved at execution time from native secure storage, never persisted into
//! the job payload. The job payload contains only the opaque profile_version reference.

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
/// Returns `true` if the profile has been explicitly revoked (revoked_at is set).
/// Returns `false` if the profile doesn't exist, has never been revoked, or the job is local-only.
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
                None => Ok(false),         // Profile doesn't exist (not explicitly revoked)
                Some(None) => Ok(false),   // Profile exists and has not been revoked
                Some(Some(_)) => Ok(true), // Profile has been explicitly revoked
            }
        }
    }
}

/// Revoke a profile, preventing future dispatch of jobs pinned to it and making
/// any results submitted after revocation ineligible for application.
/// Also retires all queued jobs pinned to this profile in a single transaction.
/// Running jobs are cancelled by the claim loop when their lease expires (or immediately
/// if the next claim attempt discovers them).
pub fn revoke_profile(
    db: &mut crate::store::schema::Database,
    profile_version: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let tx = db.immediate_transaction()?;
    revoke_profile_and_retire_jobs_in_tx(&tx, profile_version, now)?;
    tx.commit()?;
    Ok(())
}

/// Revoke a profile and retire queued jobs pinned to it in a single transaction.
/// Queued jobs are cancelled immediately. Running jobs are cancelled by the next claim attempt
/// that selects them (the claim loop checks revoked_at and cancels). Late results are rejected
/// by complete_job_in_tx.
pub fn revoke_profile_and_retire_jobs_in_tx(
    tx: &Transaction<'_>,
    profile_version: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let revoked_at_str = now.to_rfc3339();
    tx.execute(
        "UPDATE provider_profiles SET revoked_at = ? WHERE profile_version = ?",
        [revoked_at_str.as_str(), profile_version],
    )
    .context("revoking profile")?;

    // Retire all queued jobs pinned to this profile immediately. Running jobs will be
    // cancelled by the claim loop when it encounters them (the revoked_at check).
    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE profile_version = ? AND status = 'queued'",
        rusqlite::params!["cancelled", "profile_revoked", profile_version],
    )
    .context("retiring queued jobs for revoked profile")?;

    Ok(())
}

/// Retire a specific job and create a new requeue job in one transaction.
/// The old job is marked as cancelled with reason 'requeued'. The new job is created pinned
/// to a new profile version, with all other fields copied from the old job.
/// This ensures atomicity and creates an inspectable record: the old job's failure_reason
/// is set to 'requeued', and the new job can be located through the item_id and job_type.
pub fn requeue_job_to_new_profile_in_tx(
    tx: &Transaction<'_>,
    old_job_id: &str,
    new_job_id: String,
    new_profile_version: String,
    now: DateTime<Utc>,
) -> Result<()> {
    // Load the old job to copy its fields
    let (item_id, job_type, source_revision, job_schema_version, request_version): (
        String,
        String,
        i32,
        i32,
        Option<String>,
    ) = tx
        .query_row(
            "SELECT item_id, job_type, source_revision, job_schema_version, request_version \
             FROM jobs WHERE job_id = ?",
            [old_job_id],
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
        .context("loading old job for requeue")?;

    // Validate that the new profile exists and is not revoked
    let profile_exists: Option<Option<String>> = tx
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [&new_profile_version],
            |row| row.get(0),
        )
        .optional()
        .context("checking new profile availability")?;

    match profile_exists {
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

    // Retire the old job: mark as cancelled with reason 'requeued'
    let affected = tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE job_id = ? AND status IN ('queued', 'running')",
        rusqlite::params!["cancelled", "requeued", old_job_id],
    )?;

    if affected == 0 {
        return Err(anyhow!(
            "Job {} not found or not in a state that can be requeued",
            old_job_id
        ));
    }

    // Create the new job pinned to the new profile version, copying fields from old job
    let created_at_str = now.to_rfc3339();
    tx.execute(
        "INSERT INTO jobs (
            job_id, job_schema_version, item_id, job_type, source_revision,
            profile_version, request_version, status, attempt_count, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &new_job_id,
            job_schema_version,
            &item_id,
            &job_type,
            source_revision,
            &new_profile_version,
            &request_version,
            "queued",
            0,
            &created_at_str
        ],
    )?;

    Ok(())
}
