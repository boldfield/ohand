//! Processing health read over the core handle (J03).
//!
//! `ohand_core_start_processing_health` queues one read-only snapshot of
//! [`crate::jobs::health::ProcessingHealth`] on the handle's worker thread and delivers it as the
//! outcome event of `operation_id`: `{"operation_id", ...snapshot}`. The snapshot holds counts,
//! ages, timestamps and machine labels only. Reading changes no job, and saving a capture never
//! waits on or depends on it.

use super::core_handle::exports::{guarded, OhandCoreHandle, OhandCoreResult};
use super::core_handle::failure::AbiFailure;
use super::core_handle::instance::{self, lock, JobFn};
use crate::jobs::health::{read_processing_health, HealthConfig, ProcessingHealth};
use crate::store::schema::Database;
use chrono::{Duration, Utc};
use serde::Serialize;
use std::sync::Mutex;

/// Longest stall or lease threshold accepted, in seconds (one week).
pub const OHAND_CORE_MAX_HEALTH_THRESHOLD_SECONDS: u32 = 604_800;

#[derive(Serialize)]
struct HealthReadout {
    operation_id: u64,
    #[serde(flatten)]
    health: ProcessingHealth,
}

fn health_job(config: HealthConfig, operation_id: u64) -> JobFn {
    Box::new(move |database: &Mutex<Database>| {
        let database = lock(database);
        let health = read_processing_health(&database, &config, Utc::now())
            .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORAGE_ERROR))?;
        serde_json::to_vec(&HealthReadout {
            operation_id,
            health,
        })
        .map_err(|_| AbiFailure::INTERNAL)
    })
}

/// Queues a health read. `stall_after_seconds` is how long due work may wait before it counts as
/// a stall and `lease_grace_seconds` how long a lapsed lease may stay unrecovered; 0 selects the
/// core's default for either, and a value above `OHAND_CORE_MAX_HEALTH_THRESHOLD_SECONDS` is
/// refused as `invalid_request`. The outcome event holds the snapshot JSON, or a normalized
/// failure.
#[no_mangle]
pub extern "C" fn ohand_core_start_processing_health(
    handle: OhandCoreHandle,
    operation_id: u64,
    stall_after_seconds: u32,
    lease_grace_seconds: u32,
) -> OhandCoreResult {
    guarded(|| {
        let core = instance::lookup(handle)?;
        let defaults = HealthConfig::default();
        let threshold = |seconds: u32, default: Duration| {
            if seconds > OHAND_CORE_MAX_HEALTH_THRESHOLD_SECONDS {
                Err(AbiFailure::INVALID_REQUEST)
            } else if seconds == 0 {
                Ok(default)
            } else {
                Ok(Duration::seconds(i64::from(seconds)))
            }
        };
        let config = HealthConfig {
            stall_after: threshold(stall_after_seconds, defaults.stall_after)?,
            lease_grace: threshold(lease_grace_seconds, defaults.lease_grace)?,
        };
        core.submit(operation_id, health_job(config, operation_id))
    })
}

#[cfg(test)]
mod tests;
