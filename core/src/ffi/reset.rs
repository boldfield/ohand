//! Durable global reset over the core handle (L05b).
//!
//! `ohand_core_start_reset` exposes exactly what native reset orchestration needs from the
//! durable reset generation (`lifecycle::reset`) and from explicit provider-profile revocation
//! (`jobs::configuration`), as one tagged JSON command per call. Each command runs on the
//! handle's worker thread and its outcome event for `operation_id` is `{"operation_id", ...}`:
//!
//! * `start` -> `status`: record a new reset generation (or resume the pending one), tombstone
//!   every pre-reset item, and complete the generation if all cleanup is already acknowledged.
//! * `advance` -> `status`: resume the pending reset after cleanup acknowledgments; `status` is
//!   null when no reset has ever started.
//! * `status` -> `status`: the latest generation read-only, or null.
//! * `current_generation` -> `generation`: the generation new captures belong to right now.
//! * `item_generation` (`item_id`) -> `generation`: the generation a capture belongs to, or null
//!   for an unknown item.
//! * `profile_credentials` -> `profiles`: `{profile_version, credential_ref, revoked}` rows.
//! * `revoke_profile` (`profile_version`): revoke one profile version and retire its jobs.
//!
//! `status` is `{generation, phase, boundary_rowid, items_awaiting_tombstone, cleanup_pending,
//! cleanup_failed, started_at, completed_at}` with `phase` `pending` or `completed`. The phase
//! is derived from durable rows only; native code never decides that a reset succeeded.
//! Failures are normalized and never echo identifiers or content.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::guarded;
use super::core_handle::exports::{OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::jobs::configuration::revoke_profile;
use crate::lifecycle::reset::{
    advance_reset, current_generation, generation_of_item, reset_status, start_reset, ResetPhase,
    ResetStatus,
};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Start {},
    Advance {},
    Status {},
    CurrentGeneration {},
    ItemGeneration { item_id: String },
    ProfileCredentials {},
    RevokeProfile { profile_version: String },
}

impl Command {
    fn validate(&self) -> Result<(), AbiFailure> {
        let identifier = match self {
            Command::Start {}
            | Command::Advance {}
            | Command::Status {}
            | Command::CurrentGeneration {}
            | Command::ProfileCredentials {} => return Ok(()),
            Command::ItemGeneration { item_id } => item_id,
            Command::RevokeProfile { profile_version } => profile_version,
        };
        if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_BYTES {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        Ok(())
    }
}

fn store_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn sqlite_failure(error: rusqlite::Error) -> AbiFailure {
    store_failure(&anyhow::Error::new(error))
}

/// A profile revocation core refused (unknown profile version).
const fn refused() -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Permanent,
        code: "reset_profile_refused",
        message: Cow::Borrowed("the provider profile cannot be revoked"),
    }
}

fn status_json(status: &ResetStatus) -> Value {
    json!({
        "generation": status.generation,
        "phase": match status.phase {
            ResetPhase::Pending => "pending",
            ResetPhase::Completed => "completed",
        },
        "boundary_rowid": status.boundary_rowid,
        "items_awaiting_tombstone": status.items_awaiting_tombstone,
        "cleanup_pending": status.cleanup_pending,
        "cleanup_failed": status.cleanup_failed,
        "started_at": status.started_at.to_rfc3339(),
        "completed_at": status.completed_at.map(|instant| instant.to_rfc3339()),
    })
}

fn item_generation(database: &Database, item_id: &str) -> Result<Value, AbiFailure> {
    let known = database
        .conn()
        .query_row("SELECT 1 FROM items WHERE item_id = ?", [item_id], |_| {
            Ok(())
        })
        .optional()
        .map_err(sqlite_failure)?
        .is_some();
    if !known {
        return Ok(json!({ "generation": null }));
    }
    let generation = generation_of_item(database, item_id).map_err(|e| store_failure(&e))?;
    Ok(json!({ "generation": generation }))
}

fn profile_credentials(database: &Database) -> Result<Value, AbiFailure> {
    let mut statement = database
        .conn()
        .prepare(
            "SELECT profile_version, credential_ref, revoked_at IS NOT NULL \
             FROM provider_profiles ORDER BY profile_version",
        )
        .map_err(sqlite_failure)?;
    let profiles = statement
        .query_map([], |row| {
            Ok(json!({
                "profile_version": row.get::<_, String>(0)?,
                "credential_ref": row.get::<_, Option<String>>(1)?,
                "revoked": row.get::<_, bool>(2)?,
            }))
        })
        .map_err(sqlite_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_failure)?;
    Ok(json!({ "profiles": profiles }))
}

fn run(database: &mut Database, command: &Command) -> Result<Value, AbiFailure> {
    match command {
        Command::Start {} => {
            let status = start_reset(database, Utc::now()).map_err(|e| store_failure(&e))?;
            Ok(json!({ "status": status_json(&status) }))
        }
        Command::Advance {} => {
            let latest = reset_status(database).map_err(|e| store_failure(&e))?;
            let Some(_) = latest else {
                return Ok(json!({ "status": null }));
            };
            let status = advance_reset(database, Utc::now()).map_err(|e| store_failure(&e))?;
            Ok(json!({ "status": status_json(&status) }))
        }
        Command::Status {} => {
            let latest = reset_status(database).map_err(|e| store_failure(&e))?;
            Ok(json!({ "status": latest.as_ref().map(status_json) }))
        }
        Command::CurrentGeneration {} => {
            let generation = current_generation(database).map_err(|e| store_failure(&e))?;
            Ok(json!({ "generation": generation }))
        }
        Command::ItemGeneration { item_id } => item_generation(database, item_id),
        Command::ProfileCredentials {} => profile_credentials(database),
        Command::RevokeProfile { profile_version } => {
            revoke_profile(database, profile_version, Utc::now()).map_err(|error| {
                let mapped = store_failure(&error);
                if mapped == AbiFailure::STORAGE_ERROR {
                    refused()
                } else {
                    mapped
                }
            })?;
            Ok(json!({}))
        }
    }
}

fn reset_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one reset command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields are refused as `invalid_request`
/// before anything is queued). The outcome event for `operation_id` is the JSON documented in
/// the module documentation, or a normalized failure. `reset_profile_refused` means the profile
/// version is unknown and nothing was revoked.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when
/// `request_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_reset(
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
        core.submit(operation_id, reset_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
