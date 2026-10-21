//! Job configuration controls over the core handle (A07).
//!
//! `ohand_core_start_job_configuration` carries one tagged JSON command, runs it on the handle's
//! worker thread, and answers `{"operation_id", ...}`:
//!
//! * `jobs_pinned_to_profile` (`profile_version`, optional `limit`) -> `jobs`, `total`,
//!   `truncated`: the queued or running jobs pinned to one profile version
//!   (`get_jobs_pinned_to_profile`), at most `limit` of them (default and ceiling
//!   [`MAX_LISTED_JOBS`]). Each entry carries `job_id` (the handle the other commands address),
//!   `item_id` and `status`, and nothing about the item's content. `total` is the full count.
//! * `jobs_retired_by_revocation` (`profile_version`, optional `limit`, optional `after`) ->
//!   `jobs`, `total`, `truncated`, `next_cursor`: the jobs pinned to a revoked version and retired
//!   with failure reason `profile_revoked` (`jobs_retired_by_revocation`), ordered by `job_id`,
//!   at most `limit` per page (default and ceiling [`MAX_LISTED_JOBS`]). Each entry carries
//!   `job_id`, `item_id`, `retired_at` (the revocation time) and `requeued_to` (the replacement
//!   job, or `null`). `after` is the previous page's `next_cursor`, which is `null` on the last
//!   page. A version that is not revoked answers an empty list; an unknown one is `not_found`.
//!   Read it after `revoke_profile`: the set is then final and survives a relaunch.
//! * `requeue_job` (`job_id`, `new_job_id`, `new_profile_version`) -> `job_id`, `item_id`,
//!   `status`, `profile_version` of the replacement (`requeue_job_to_new_profile`). `new_job_id`
//!   is the immutable identity of the user action, reused unchanged by each retry: repeating the
//!   same requeue answers with the existing replacement.
//! * `requeue_record` (`job_id`) -> `record`: the link (`old_job_id`, `new_job_id`,
//!   `from_profile_version`, `to_profile_version`, `requeued_at`) in which the job is either side,
//!   or `null` (`get_requeue_record`).
//! * `revoke_profile` (`profile_version`) -> `profile_version`: revoke the version and retire the
//!   jobs pinned to it (`revoke_profile`), as the reset flow does. Revoking only stamps
//!   `revoked_at`, so a credential claim of an explicit "remove credentials" choice neither blocks
//!   it nor changes. Revoking again keeps the first `revoked_at`.
//! * `repin_waiting_work` (`route_id`) -> `moved`: the explicit move of the route's waiting
//!   unpinned `interpret` work, and of its uninterpreted or unprocessed items that have no live
//!   job, onto the profile the resolver selects now, only when that profile is authorized for the
//!   route (`repin_waiting_interpretations`). It runs only when called, moves at most
//!   `MAX_REPIN_BATCH` items, is idempotent and never changes a job pinned to a profile.
//!
//! A refusal is a normalized failure: `not_found` for an unknown job or profile,
//! `invalid_request` for a malformed command, `job_configuration_refused` when the core's rules
//! refuse the change (for example a revoked or identical target profile, or a job in a state that
//! cannot move). Failure messages never echo the request.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::jobs::configuration::{
    get_jobs_pinned_to_profile, get_profile_id, get_requeue_record, jobs_retired_by_revocation,
    repin_waiting_interpretations, requeue_job_to_new_profile, revoke_profile,
};
use crate::jobs::queue::get_job;
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Database;
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;
/// Most jobs one listing returns.
pub const MAX_LISTED_JOBS: usize = crate::jobs::configuration::MAX_LISTED_JOBS;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    JobsPinnedToProfile {
        profile_version: String,
        #[serde(default)]
        limit: Option<u32>,
    },
    JobsRetiredByRevocation {
        profile_version: String,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        after: Option<String>,
    },
    RequeueJob {
        job_id: String,
        new_job_id: String,
        new_profile_version: String,
    },
    RequeueRecord {
        job_id: String,
    },
    RevokeProfile {
        profile_version: String,
    },
    RepinWaitingWork {
        route_id: String,
    },
}

impl Command {
    fn validate(&self) -> Result<(), AbiFailure> {
        let acceptable =
            |identifier: &str| !identifier.is_empty() && identifier.len() <= MAX_IDENTIFIER_BYTES;
        let valid = match self {
            Command::JobsPinnedToProfile {
                profile_version,
                limit,
            } => acceptable(profile_version) && limit.is_none_or(|limit| limit >= 1),
            Command::JobsRetiredByRevocation {
                profile_version,
                limit,
                after,
            } => {
                acceptable(profile_version)
                    && limit.is_none_or(|limit| limit >= 1)
                    && after.as_deref().is_none_or(acceptable)
            }
            Command::RequeueJob {
                job_id,
                new_job_id,
                new_profile_version,
            } => acceptable(job_id) && acceptable(new_job_id) && acceptable(new_profile_version),
            Command::RequeueRecord { job_id } => acceptable(job_id),
            Command::RevokeProfile { profile_version } => acceptable(profile_version),
            Command::RepinWaitingWork { route_id } => acceptable(route_id),
        };
        if valid {
            Ok(())
        } else {
            Err(AbiFailure::INVALID_REQUEST)
        }
    }
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

/// A change the core's rules refuse, as opposed to a storage failure that is retried.
const fn refused() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Permanent,
        code: "job_configuration_refused",
        message: Cow::Borrowed("the job configuration change is not allowed"),
    }
}

/// A rule violation is a plain error without a SQLite cause; a genuine storage failure keeps its
/// normalized class so it is retried instead of being reported as refused.
fn refusal_or_store_failure(error: &anyhow::Error) -> AbiFailure {
    let mapped = store_failure(error);
    let has_sqlite_cause = error
        .chain()
        .any(|cause| cause.downcast_ref::<rusqlite::Error>().is_some());
    if mapped == AbiFailure::STORAGE_ERROR && !has_sqlite_cause {
        refused()
    } else {
        mapped
    }
}

fn job_exists(database: &Database, job_id: &str) -> Result<bool, AbiFailure> {
    Ok(get_job(database, job_id)
        .map_err(|error| store_failure(&error))?
        .is_some())
}

fn jobs_pinned_to_profile(
    database: &Database,
    profile_version: &str,
    limit: Option<u32>,
) -> Result<Value, AbiFailure> {
    let limit = limit
        .and_then(|limit| usize::try_from(limit).ok())
        .map_or(MAX_LISTED_JOBS, |limit| limit.min(MAX_LISTED_JOBS));
    let job_ids = get_jobs_pinned_to_profile(database.conn(), profile_version)
        .map_err(|error| store_failure(&error))?;
    let mut jobs = Vec::new();
    for job_id in job_ids.iter().take(limit) {
        let Some(job) = get_job(database, job_id).map_err(|error| store_failure(&error))? else {
            continue;
        };
        jobs.push(json!({
            "job_id": job.job_id,
            "item_id": job.item_id,
            "status": job.status.as_str(),
        }));
    }
    Ok(json!({
        "jobs": jobs,
        "total": job_ids.len(),
        "truncated": job_ids.len() > limit,
    }))
}

fn retired_by_revocation(
    database: &Database,
    profile_version: &str,
    limit: Option<u32>,
    after: Option<&str>,
) -> Result<Value, AbiFailure> {
    let known = get_profile_id(database.conn(), profile_version)
        .map_err(|error| store_failure(&error))?
        .is_some();
    if !known {
        return Err(AbiFailure::NOT_FOUND);
    }
    let limit = limit
        .and_then(|limit| usize::try_from(limit).ok())
        .map_or(MAX_LISTED_JOBS, |limit| limit.min(MAX_LISTED_JOBS));
    let page = jobs_retired_by_revocation(database.conn(), profile_version, limit, after)
        .map_err(|error| store_failure(&error))?;
    let jobs: Vec<Value> = page
        .jobs
        .iter()
        .map(|job| {
            json!({
                "job_id": job.job_id,
                "item_id": job.item_id,
                "retired_at": job.retired_at,
                "requeued_to": job.requeued_to,
            })
        })
        .collect();
    Ok(json!({
        "jobs": jobs,
        "total": page.total,
        "truncated": page.next_cursor.is_some(),
        "next_cursor": page.next_cursor,
    }))
}

fn requeue_job(
    database: &mut Database,
    job_id: &str,
    new_job_id: &str,
    new_profile_version: &str,
) -> Result<Value, AbiFailure> {
    if !job_exists(database, job_id)? {
        return Err(AbiFailure::NOT_FOUND);
    }
    let replacement = requeue_job_to_new_profile(
        database,
        job_id,
        new_job_id.to_string(),
        new_profile_version.to_string(),
        Utc::now(),
    )
    .map_err(|error| refusal_or_store_failure(&error))?;
    Ok(json!({
        "job_id": replacement.job_id,
        "item_id": replacement.item_id,
        "status": replacement.status.as_str(),
        "profile_version": replacement.profile_version,
    }))
}

fn requeue_record(database: &Database, job_id: &str) -> Result<Value, AbiFailure> {
    let record = get_requeue_record(database.conn(), job_id)
        .map_err(|error| store_failure(&error))?
        .map(|record| {
            json!({
                "old_job_id": record.old_job_id,
                "new_job_id": record.new_job_id,
                "from_profile_version": record.from_profile_version,
                "to_profile_version": record.to_profile_version,
                "requeued_at": record.requeued_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            })
        });
    Ok(json!({ "record": record }))
}

fn revoke(database: &mut Database, profile_version: &str) -> Result<Value, AbiFailure> {
    let known = get_profile_id(database.conn(), profile_version)
        .map_err(|error| store_failure(&error))?
        .is_some();
    if !known {
        return Err(AbiFailure::NOT_FOUND);
    }
    revoke_profile(database, profile_version, Utc::now())
        .map_err(|error| refusal_or_store_failure(&error))?;
    Ok(json!({ "profile_version": profile_version }))
}

fn repin(database: &mut Database, route_id: &str) -> Result<Value, AbiFailure> {
    let moved = repin_waiting_interpretations(database, route_id, Utc::now())
        .map_err(|error| store_failure(&error))?;
    Ok(json!({ "moved": moved }))
}

fn run(database: &mut Database, command: &Command) -> Result<Value, AbiFailure> {
    match command {
        Command::JobsPinnedToProfile {
            profile_version,
            limit,
        } => jobs_pinned_to_profile(database, profile_version, *limit),
        Command::JobsRetiredByRevocation {
            profile_version,
            limit,
            after,
        } => retired_by_revocation(database, profile_version, *limit, after.as_deref()),
        Command::RequeueJob {
            job_id,
            new_job_id,
            new_profile_version,
        } => requeue_job(database, job_id, new_job_id, new_profile_version),
        Command::RequeueRecord { job_id } => requeue_record(database, job_id),
        Command::RevokeProfile { profile_version } => revoke(database, profile_version),
        Command::RepinWaitingWork { route_id } => repin(database, route_id),
    }
}

fn configuration_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one job-configuration command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields and empty or oversized values are
/// refused as `invalid_request` before anything is queued). The outcome event for `operation_id`
/// is the JSON documented in the module documentation, or a normalized failure.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_job_configuration(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        if request_len > OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if request_len == 0 {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        if request.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let bytes = std::slice::from_raw_parts(request, request_len);
        let command: Command =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        command.validate()?;
        core.submit(operation_id, configuration_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
