//! Configuration management and profile pinning for durable jobs.
//!
//! Every job is pinned to a profile_version at enqueue time. Changes to provider configuration
//! (switching default provider, editing profile, or revoking access) never silently reroute
//! existing queued work. Authorization always looks up the exact pinned profile_version.
//!
//! Credentials are resolved at execution time from native secure storage, never persisted into
//! the job payload. The job payload contains only the opaque profile_version reference.

use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// Check if a profile version is currently available (not revoked).
/// Returns `true` if the profile exists, `false` if it's been removed.
pub fn is_profile_available(conn: &Connection, profile_version: &str) -> Result<bool> {
    let exists: Option<i32> = conn
        .query_row(
            "SELECT 1 FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| row.get(0),
        )
        .optional()
        .context("checking profile availability")?;

    Ok(exists.is_some())
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
    // Query returns Option<Option<String>> because the column is nullable
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
        Some(None) => Ok(false), // Job has no profile pin (local only)
        Some(Some(pin)) => {
            // Check if pinned profile still exists
            is_profile_available(conn, &pin).map(|exists| !exists) // Revoked if NOT available
        }
    }
}
