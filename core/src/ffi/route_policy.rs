//! Route processing policy over the core handle (V02b).
//!
//! Three operations, each one transaction on the handle's worker thread:
//!
//! - `ohand_core_start_grant_route_capability` allows one capability on one route to send to the
//!   destinations of one stored profile version. The caller names the route, the capability and
//!   the profile version; destinations are derived in the core and can never be supplied.
//! - `ohand_core_start_revoke_route_capability` removes that capability's grant and drops the
//!   destinations no remaining grant on the route uses.
//! - `ohand_core_start_list_route_policy` reads each route's name, scope, granted capabilities
//!   and destination classes.
//!
//! Only these explicit calls change policy; nothing here runs from a classifier or any inferred
//! signal. Failure messages are fixed strings and never echo the request.
//!
//! Success events: grant `{"operation_id","disposition"}` (`granted` or `unchanged`), revoke
//! `{"operation_id","disposition"}` (`revoked` or `unchanged`), list
//! `{"operation_id","routes":[{"route_id","route_name","scope","granted_capabilities":
//! [{"capability","destination_classes","covered_profile_versions"}]}]}`.
//! `covered_profile_versions` lists the unrevoked profile versions whose every origin the grant
//! allows (ordered by version identifier), the same test `authorize_job` applies; a grant for one provider does not
//! cover another's version.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::privacy::routing::{
    capability_from_key, grant_route_capability, list_route_policy, revoke_route_capability,
    GrantError, GrantOutcome, ProcessingCapability, RevokeOutcome,
};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Database;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Mutex;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantRequest {
    route_id: String,
    capability: String,
    profile_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    route_id: String,
    capability: String,
}

#[derive(Serialize)]
struct DispositionOutcome {
    operation_id: u64,
    disposition: &'static str,
}

#[derive(Serialize)]
struct ListedCapability {
    capability: &'static str,
    destination_classes: Vec<&'static str>,
    covered_profile_versions: Vec<String>,
}

#[derive(Serialize)]
struct ListedRoute {
    route_id: String,
    route_name: String,
    scope: String,
    granted_capabilities: Vec<ListedCapability>,
}

#[derive(Serialize)]
struct PolicyListing {
    operation_id: u64,
    routes: Vec<ListedRoute>,
}

const fn permanent(code: &'static str, message: &'static str) -> AbiFailure {
    AbiFailure {
        class: ErrorClass::Permanent,
        code,
        message: Cow::Borrowed(message),
    }
}

const PROFILE_UNAVAILABLE: AbiFailure =
    permanent("profile_unavailable", "the profile version does not exist");
const PROFILE_REVOKED: AbiFailure =
    permanent("profile_revoked", "the profile version has been revoked");
const PROFILE_MALFORMED: AbiFailure =
    permanent("profile_malformed", "the profile has no valid destination");
const ROUTE_UNKNOWN: AbiFailure = permanent("route_unknown", "the route does not exist");
const CAPABILITY_NOT_GRANTABLE: AbiFailure = permanent(
    "capability_not_grantable",
    "the capability runs on device only",
);
const ROUTE_POLICY_MALFORMED: AbiFailure =
    permanent("route_policy_malformed", "stored route policy is malformed");

fn grant_failure(error: &GrantError) -> AbiFailure {
    match error {
        GrantError::ProfileUnavailable => PROFILE_UNAVAILABLE,
        GrantError::ProfileRevoked => PROFILE_REVOKED,
        GrantError::ProfileMalformed => PROFILE_MALFORMED,
        GrantError::UnknownRoute => ROUTE_UNKNOWN,
        GrantError::CapabilityNotGrantable => CAPABILITY_NOT_GRANTABLE,
        GrantError::PolicyMalformed => ROUTE_POLICY_MALFORMED,
        GrantError::Storage(cause) => AbiFailure::from_store(cause, AbiFailure::STORAGE_ERROR),
    }
}

fn grant_job(request: GrantRequest, capability: ProcessingCapability, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let outcome = grant_route_capability(
            &mut database,
            &request.route_id,
            capability,
            &request.profile_version,
            &created_at,
        )
        .map_err(|error| grant_failure(&error))?;
        serde_json::to_vec(&DispositionOutcome {
            operation_id,
            disposition: match outcome {
                GrantOutcome::Granted => "granted",
                GrantOutcome::Unchanged => "unchanged",
            },
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

fn revoke_job(
    request: RevokeRequest,
    capability: ProcessingCapability,
    operation_id: u64,
) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let outcome = revoke_route_capability(&mut database, &request.route_id, capability)
            .map_err(|error| grant_failure(&error))?;
        serde_json::to_vec(&DispositionOutcome {
            operation_id,
            disposition: match outcome {
                RevokeOutcome::Revoked => "revoked",
                RevokeOutcome::Unchanged => "unchanged",
            },
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

fn list_job(operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let routes = list_route_policy(database.conn())
            .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR))?
            .into_iter()
            .map(|route| ListedRoute {
                route_id: route.route_id,
                route_name: route.route_name,
                scope: route.scope,
                granted_capabilities: route
                    .granted_capabilities
                    .into_iter()
                    .map(|granted| ListedCapability {
                        capability: granted.capability.as_str(),
                        destination_classes: granted
                            .destination_classes
                            .into_iter()
                            .map(|class| class.as_str())
                            .collect(),
                        covered_profile_versions: granted.covered_profile_versions,
                    })
                    .collect(),
            })
            .collect();
        serde_json::to_vec(&PolicyListing {
            operation_id,
            routes,
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Parses the `request_len` JSON bytes at `request`, enforcing the shared size and null rules.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
unsafe fn parse_request<'a, T: Deserialize<'a>>(
    request: *const u8,
    request_len: usize,
) -> Result<T, AbiFailure> {
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
    serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)
}

fn parse_capability(key: &str) -> Result<ProcessingCapability, AbiFailure> {
    capability_from_key(key).ok_or(AbiFailure::INVALID_REQUEST)
}

/// Queues a grant described by the `request_len` JSON bytes at `request`:
/// `{"route_id","capability","profile_version"}` (unknown fields are rejected; `capability` is
/// `text_interpretation`, `review` or `transcription`; identifiers must be non-empty). The
/// outcome event for `operation_id` is success with `{"operation_id","disposition"}`
/// (`granted` or `unchanged`), or a normalized failure: `profile_unavailable`,
/// `profile_revoked`, `profile_malformed`, `route_unknown`, `capability_not_grantable` (on-device
/// capabilities such as transcription) or `route_policy_malformed`, with nothing changed.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_grant_route_capability(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let grant: GrantRequest = parse_request(request, request_len)?;
        if grant.route_id.trim().is_empty() || grant.profile_version.trim().is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let capability = parse_capability(&grant.capability)?;
        core.submit(operation_id, grant_job(grant, capability, operation_id))
    })
}

/// Queues the revocation described by the `request_len` JSON bytes at `request`:
/// `{"route_id","capability"}`. The outcome event for `operation_id` is success with
/// `{"operation_id","disposition"}` (`revoked` or `unchanged` when the capability had no grant),
/// or a normalized failure `route_unknown`. Jobs already queued for the capability then fail
/// authorization at dispatch.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_revoke_route_capability(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let revoke: RevokeRequest = parse_request(request, request_len)?;
        if revoke.route_id.trim().is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let capability = parse_capability(&revoke.capability)?;
        core.submit(operation_id, revoke_job(revoke, capability, operation_id))
    })
}

/// Queues a read of every route's processing policy. The outcome event for `operation_id` is
/// `{"operation_id","routes":[...]}`; each route lists its id, name, scope and granted
/// capabilities with their destination classes (`cloud`, `private_server`, `unknown`).
#[no_mangle]
pub extern "C" fn ohand_core_start_list_route_policy(
    handle: OhandCoreHandle,
    operation_id: u64,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        core.submit(operation_id, list_job(operation_id))
    })
}

#[cfg(test)]
mod tests;
