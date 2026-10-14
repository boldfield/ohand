//! Fresh-install local-only capture route over the core handle (U01a).
//!
//! `ohand_core_start_ensure_local_capture_route` queues one idempotent transaction on the
//! handle's worker thread that makes sure the configured capture route exists, so a fresh install
//! can save before any processing policy is set up. A new route has an empty destination set,
//! `preview_safe = 0` and no `route_authorizations`: creating it never authorizes upload. A route
//! that already exists with the same scope is left exactly as the user configured it, and one
//! with a different scope is refused as `route_scope_mismatch` with nothing changed.
//!
//! Success event: `{"operation_id","disposition"}` where `disposition` is `created` or
//! `unchanged`. Failure messages are fixed strings and never echo the request.

use super::capture::OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES;
use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::providers::contracts::ErrorClass;
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Mutex;

/// Stored `routes.processing_destinations` of a route that may contact no destination.
const NO_PROCESSING_DESTINATIONS: &str = "[]";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteRequest {
    route_id: String,
    route_name: String,
    scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provisioning {
    Created,
    Unchanged,
}

impl Provisioning {
    fn as_str(self) -> &'static str {
        match self {
            Provisioning::Created => "created",
            Provisioning::Unchanged => "unchanged",
        }
    }
}

#[derive(Debug)]
enum ProvisioningError {
    ScopeMismatch,
    Storage(anyhow::Error),
}

#[derive(Serialize)]
struct ProvisioningOutcome {
    operation_id: u64,
    disposition: &'static str,
}

const ROUTE_SCOPE_MISMATCH: AbiFailure = AbiFailure {
    class: ErrorClass::Permanent,
    code: "route_scope_mismatch",
    message: Cow::Borrowed("the route already exists for a different item scope"),
};

/// Inserts the route when absent; never modifies an existing one. One immediate transaction, so
/// concurrent callers (including other connections) serialize and exactly one inserts.
fn ensure_local_capture_route(
    database: &mut Database,
    route_id: &str,
    route_name: &str,
    scope: ItemScope,
    created_at: &str,
) -> Result<Provisioning, ProvisioningError> {
    let transaction = database
        .immediate_transaction()
        .map_err(ProvisioningError::Storage)?;
    let existing_scope: Option<String> = transaction
        .query_row(
            "SELECT scope FROM routes WHERE route_id = ?",
            [route_id],
            |row| row.get(0),
        )
        .optional()
        .context("reading route")
        .map_err(ProvisioningError::Storage)?;
    let outcome = match existing_scope {
        Some(existing) if existing == scope.as_str() => Provisioning::Unchanged,
        Some(_) => return Err(ProvisioningError::ScopeMismatch),
        None => {
            transaction
                .execute(
                    "INSERT INTO routes \
                     (route_id, route_name, scope, processing_destinations, preview_safe, created_at) \
                     VALUES (?, ?, ?, ?, 0, ?)",
                    rusqlite::params![
                        route_id,
                        route_name,
                        scope.as_str(),
                        NO_PROCESSING_DESTINATIONS,
                        created_at
                    ],
                )
                .context("creating route")
                .map_err(ProvisioningError::Storage)?;
            Provisioning::Created
        }
    };
    transaction
        .commit()
        .context("committing route")
        .map_err(ProvisioningError::Storage)?;
    Ok(outcome)
}

fn provisioning_failure(error: &ProvisioningError) -> AbiFailure {
    match error {
        ProvisioningError::ScopeMismatch => ROUTE_SCOPE_MISMATCH,
        ProvisioningError::Storage(cause) => {
            AbiFailure::from_store(cause, AbiFailure::STORAGE_ERROR)
        }
    }
}

fn provisioning_job(request: RouteRequest, scope: ItemScope, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let mut database = lock(database);
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let disposition = ensure_local_capture_route(
            &mut database,
            &request.route_id,
            &request.route_name,
            scope,
            &created_at,
        )
        .map_err(|error| provisioning_failure(&error))?;
        serde_json::to_vec(&ProvisioningOutcome {
            operation_id,
            disposition: disposition.as_str(),
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues the idempotent provisioning of the local-only capture route described by the
/// `request_len` JSON bytes at `request`: `{"route_id","route_name","scope"}` (unknown fields are
/// rejected; `scope` is `personal` or `work`; the identifiers must be non-empty). The outcome
/// event for `operation_id` is success with JSON `{"operation_id","disposition"}` (`created` or
/// `unchanged`), or a normalized failure: `route_scope_mismatch` when the route exists for a
/// different scope, with nothing changed.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_ensure_local_capture_route(
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
        let route: RouteRequest =
            serde_json::from_slice(bytes).map_err(|_| AbiFailure::INVALID_REQUEST)?;
        if route.route_id.trim().is_empty() || route.route_name.trim().is_empty() {
            return Err(AbiFailure::INVALID_REQUEST);
        }
        let scope = route
            .scope
            .parse::<ItemScope>()
            .map_err(|_| AbiFailure::INVALID_REQUEST)?;
        core.submit(operation_id, provisioning_job(route, scope, operation_id))
    })
}

#[cfg(test)]
mod tests;
