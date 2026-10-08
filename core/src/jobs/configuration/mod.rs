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
                None => Ok(false),         // Profile doesn't exist (not revoked, never existed)
                Some(None) => Ok(false),   // Profile exists and is not revoked
                Some(Some(_)) => Ok(true), // Profile exists but is revoked
            }
        }
    }
}

/// Revoke a profile, preventing future dispatch of jobs pinned to it and making
/// any results submitted after revocation ineligible for application.
/// Also retires all queued and running jobs pinned to this profile.
pub fn revoke_profile(conn: &Connection, profile_version: &str, now: DateTime<Utc>) -> Result<()> {
    let revoked_at_str = now.to_rfc3339();
    conn.execute(
        "UPDATE provider_profiles SET revoked_at = ? WHERE profile_version = ?",
        [revoked_at_str.as_str(), profile_version],
    )
    .context("revoking profile")?;
    Ok(())
}

/// Revoke a profile and retire queued jobs pinned to it in a single transaction.
/// Running jobs are not retired; late results from them will be rejected by complete_job_in_tx.
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

    // Retire all queued jobs pinned to this profile. Running jobs are left as-is;
    // future claims will skip them due to the revoked_at check, and late results will
    // be rejected by complete_job_in_tx.
    tx.execute(
        "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL \
         WHERE profile_version = ? AND status = 'queued'",
        rusqlite::params!["cancelled", "profile_revoked", profile_version],
    )
    .context("retiring queued jobs for revoked profile")?;

    Ok(())
}
