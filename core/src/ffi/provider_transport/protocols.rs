//! Which provider protocols a handle may dispatch, and a typed provider-profile status read (T11c).
//!
//! Registering an adapter confers no permission, and neither does a stored profile: the host
//! decides per handle which protocols may leave the device at all. The default is Anthropic and
//! OpenAI enabled and self-hosted disabled, so an optional protocol stays off unless the build
//! registers it. Enabling a protocol changes no other check: execution resolution, destinations,
//! credentials, staleness and revocation apply to it exactly as to the others.
//!
//! * `ohand_core_set_enabled_provider_protocols` replaces the handle's enabled set with the JSON
//!   array of protocol names (`"anthropic"`, `"open_ai"`, `"self_hosted"`) at `protocols`. An empty
//!   array disables all. The interpretation exchange (`prepare`, `run_exchange`), the job-runner
//!   drain and shadow review each check the set when they dispatch, so disabling a protocol stops
//!   every dispatch that has not yet reached the transport. A refusal is the normalized
//!   `protocol_not_enabled` and nothing is sent. The drain puts such a job back without spending
//!   retry budget so that enabling the protocol later lets it run.
//! * `ohand_core_start_provider_profile_status` answers with one event
//!   `{"operation_id","exists","protocol","revoked","serves_model"}` for the stored profile
//!   version and model named in the JSON request. A missing profile reads `exists: false`,
//!   `protocol: null`, `revoked: false`, `serves_model: false`. It carries no endpoint,
//!   credential or content, and `serves_model` is whether the stored model equals the one asked
//!   about (revocation is reported separately).

use super::shadow_review::{encode, parse_request};
use super::{failures, storage_failure};
use crate::ffi::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use crate::ffi::core_handle::failure::AbiFailure;
use crate::ffi::core_handle::instance::{self, lock};
use crate::providers::contracts::ProviderProtocol;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// The code recorded or reported when a protocol is not enabled for the handle.
pub(crate) const PROTOCOL_NOT_ENABLED: &str = "protocol_not_enabled";

const DEFAULT_ENABLED: [ProviderProtocol; 2] =
    [ProviderProtocol::Anthropic, ProviderProtocol::OpenAi];

static ENABLED: Mutex<BTreeMap<u64, Vec<ProviderProtocol>>> = Mutex::new(BTreeMap::new());

/// Whether `handle` may dispatch `protocol`.
pub(crate) fn is_enabled(handle: u64, protocol: ProviderProtocol) -> bool {
    match lock(&ENABLED).get(&handle) {
        Some(enabled) => enabled.contains(&protocol),
        None => DEFAULT_ENABLED.contains(&protocol),
    }
}

pub(crate) fn ensure_enabled(handle: u64, protocol: ProviderProtocol) -> Result<(), AbiFailure> {
    if is_enabled(handle, protocol) {
        Ok(())
    } else {
        Err(failures::PROTOCOL_NOT_ENABLED)
    }
}

/// Replaces the provider protocols `handle` may dispatch with the JSON array of protocol names at
/// `protocols`. See the module documentation. An unknown name is `invalid_request`.
///
/// # Safety
/// `protocols` must point at `protocols_len` readable bytes (it may be null only when
/// `protocols_len` is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_set_enabled_provider_protocols(
    handle: OhandCoreHandle,
    protocols: *const u8,
    protocols_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        instance::lookup(handle)?;
        let mut requested: Vec<ProviderProtocol> = parse_request(protocols, protocols_len)?;
        requested.sort_by_key(|protocol| *protocol as u8);
        requested.dedup();
        let mut enabled = lock(&ENABLED);
        enabled.retain(|owner, _| instance::lookup(*owner).is_ok());
        enabled.insert(handle, requested);
        Ok(())
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    profile_version: String,
    model: String,
}

#[derive(Serialize)]
struct StatusEvent {
    operation_id: u64,
    exists: bool,
    protocol: Option<ProviderProtocol>,
    revoked: bool,
    serves_model: bool,
}

/// Queues the status read for the stored provider profile version and model named in the JSON
/// `{"profile_version","model"}` at `request`. See the module documentation for the event.
///
/// # Safety
/// `request` must point at `request_len` readable bytes (it may be null only when `request_len`
/// is 0, which is rejected).
#[no_mangle]
pub unsafe extern "C" fn ohand_core_start_provider_profile_status(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: *const u8,
    request_len: usize,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let read: StatusRequest = parse_request(request, request_len)?;
        core.submit(
            operation_id,
            Box::new(move |database| {
                let database = lock(database);
                let stored: Option<(String, String, Option<String>)> = database
                    .conn()
                    .query_row(
                        "SELECT provider_type, model, revoked_at FROM provider_profiles \
                         WHERE profile_version = ?",
                        [&read.profile_version],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(|error| storage_failure(anyhow::Error::from(error)))?;
                let event = match stored {
                    None => StatusEvent {
                        operation_id,
                        exists: false,
                        protocol: None,
                        revoked: false,
                        serves_model: false,
                    },
                    Some((provider_type, model, revoked_at)) => StatusEvent {
                        operation_id,
                        exists: true,
                        protocol: serde_json::from_value(serde_json::Value::String(provider_type))
                            .ok(),
                        revoked: revoked_at.is_some(),
                        serves_model: model == read.model,
                    },
                };
                encode(&event)
            }),
        )
    })
}

/// Test fixtures enable the protocols they rely on through the same export the host uses.
#[cfg(test)]
pub(crate) fn enable_for_test(handle: OhandCoreHandle, names: &[&str]) {
    use crate::ffi::core_handle::failure::OHAND_CORE_STATUS_OK;
    let request = serde_json::to_vec(names).unwrap();
    let outcome = crate::ffi::core_handle::tests::consume(unsafe {
        ohand_core_set_enabled_provider_protocols(handle, request.as_ptr(), request.len())
    });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
}
