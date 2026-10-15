//! Provider profiles and their per-capability defaults over the core handle (V01b).
//!
//! `ohand_core_start_provider_profiles` exposes the core's production write path for versioned
//! provider profiles (`providers::profiles`) as one tagged JSON command per call. Each command
//! runs on the handle's worker thread, and its outcome event for `operation_id` is
//! `{"operation_id", ...}`:
//!
//! * `create_profile` (`profile`: `{profile_id, protocol, endpoint?, model, credential_ref,
//!   timeout_seconds, retry_policy?, capabilities}`) -> `profile_id`, `profile_version`: store a
//!   new immutable version. The caller names no destinations; the core derives them.
//! * `set_default` (`capability`, `profile_version`) -> `capability`, `profile_version`: point the
//!   capability's default at one unrevoked version.
//! * `clear_default` (`capability`) -> `capability`: remove the default (allowed when none is set).
//! * `list` -> `profiles`, `defaults`: every profile's versions with protocol, endpoint, model,
//!   capability support, whether a credential reference is bound (never the reference), the
//!   revocation flag and the capabilities each version is the default for.
//!
//! A refusal is a normalized failure whose `code` is a fixed string from `providers::profiles`
//! (for example `unknown_protocol`, `credential_required` or `credential_pending_removal`). Failure
//! messages never echo the request.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::providers::contracts::ErrorClass;
use crate::providers::profiles::{
    clear_default_profile, create_profile_version, list_profiles, parse_default_capability,
    set_default_profile, ProfileDraft, ProfileFailure,
};
use crate::store::schema::Database;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::Mutex;

/// Longest identifier accepted in a command, in bytes.
const MAX_IDENTIFIER_BYTES: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    CreateProfile {
        profile: ProfileDraft,
    },
    SetDefault {
        capability: String,
        profile_version: String,
    },
    ClearDefault {
        capability: String,
    },
    List {},
}

impl Command {
    fn validate(&self) -> Result<(), AbiFailure> {
        let within_bound = |text: &str| text.len() <= MAX_IDENTIFIER_BYTES;
        let valid = match self {
            Command::CreateProfile { .. } | Command::List {} => true,
            Command::SetDefault {
                capability,
                profile_version,
            } => {
                !profile_version.is_empty()
                    && within_bound(profile_version)
                    && within_bound(capability)
            }
            Command::ClearDefault { capability } => within_bound(capability),
        };
        if valid {
            Ok(())
        } else {
            Err(AbiFailure::INVALID_REQUEST)
        }
    }
}

fn failure_for(failure: &ProfileFailure) -> AbiFailure {
    match failure {
        ProfileFailure::Storage(cause) => AbiFailure::from_store(cause, AbiFailure::STORAGE_ERROR),
        refusal => AbiFailure {
            class: ErrorClass::Permanent,
            code: refusal.code(),
            message: Cow::Borrowed(refusal.message()),
        },
    }
}

fn storage_failure(error: &anyhow::Error) -> AbiFailure {
    AbiFailure::from_store(error, AbiFailure::STORAGE_ERROR)
}

fn run(database: &mut Database, command: &Command) -> Result<Value, AbiFailure> {
    match command {
        Command::CreateProfile { profile } => {
            let stored = create_profile_version(database, profile, Utc::now())
                .map_err(|failure| failure_for(&failure))?;
            Ok(json!({
                "profile_id": stored.profile_id,
                "profile_version": stored.profile_version,
            }))
        }
        Command::SetDefault {
            capability,
            profile_version,
        } => {
            let parsed = parse_default_capability(capability).map_err(|f| failure_for(&f))?;
            set_default_profile(database, parsed, profile_version, Utc::now())
                .map_err(|failure| failure_for(&failure))?;
            Ok(json!({
                "capability": parsed.as_str(),
                "profile_version": profile_version,
            }))
        }
        Command::ClearDefault { capability } => {
            let parsed = parse_default_capability(capability).map_err(|f| failure_for(&f))?;
            clear_default_profile(database, parsed).map_err(|failure| failure_for(&failure))?;
            Ok(json!({ "capability": parsed.as_str() }))
        }
        Command::List {} => {
            let listing = list_profiles(database.conn()).map_err(|e| storage_failure(&e))?;
            serde_json::to_value(listing).map_err(|_| AbiFailure::INTERNAL)
        }
    }
}

fn profiles_job(command: Command, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let mut body = run(&mut database, &command)?;
        if let Value::Object(fields) = &mut body {
            fields.insert("operation_id".to_string(), json!(operation_id));
        }
        serde_json::to_vec(&body).map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues one provider-profile command described by the `request_len` JSON bytes at `request`
/// (`{"command": "...", ...}`; unknown commands and fields are refused as `invalid_request` before
/// anything is queued). The outcome event for `operation_id` is the JSON documented in the module
/// documentation, or a normalized failure with a fixed `code`.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_provider_profiles(
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
        core.submit(operation_id, profiles_job(command, operation_id))
    })
}

#[cfg(test)]
mod tests;
