//! Queueing of `interpret` jobs for saved captures (I08).
//!
//! Every capture with text gets one `interpret` job at the revision its text first exists: the
//! initial revision for a typed or imported capture, and the post-transcript revision for a voice
//! capture. The job's identity is derived from the item and revision, so a retry of the import or
//! the transcript attachment can never queue a second job for the same source.
//!
//! [`resolve_profile_pin`] is the one place that chooses the provider profile version a new job is
//! pinned to. Native code and the callers in `ingress` never choose a destination. Until stored
//! provider profiles exist it returns `None`, and an unpinned job runs only the offline fast path
//! and is `Local` under `privacy::routing::authorize_job`.
//!
//! Queueing never waits on interpretation and never reaches a provider; it only writes the job row
//! inside the caller's transaction.

use crate::interpretation::instructions::M1_INSTRUCTION_VERSION;
use crate::jobs::queue::{enqueue_job_in_tx, get_job_internal, Job};
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Version of the job record layout that the queue supports.
const INTERPRETATION_JOB_SCHEMA_VERSION: i32 = 1;
const JOB_ID_DOMAIN: &[u8] = b"ohand.interpretation-job.v1";
const REQUEST_VERSION_DOMAIN: &[u8] = b"ohand.interpretation-request.v1:";

/// What a new `interpret` job for an item revision is created with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpretationPlan {
    pub job_id: String,
    /// `None` queues the offline fast path only; `Some` pins the job to a stored profile version.
    pub profile_version: Option<String>,
    pub request_version: String,
}

/// The profile version a new `interpret` job for `item_id` is pinned to, or `None`.
///
/// This is the only function that chooses a pin. It runs inside the caller's transaction so the
/// choice and the job commit together.
pub fn resolve_profile_pin(
    _transaction: &Transaction<'_>,
    _item_id: &str,
) -> Result<Option<String>> {
    Ok(None)
}

/// The job identity for interpreting `item_id` at `source_revision`: a UUID derived from both.
pub fn interpretation_job_id(item_id: &str, source_revision: i32) -> String {
    derived_uuid(JOB_ID_DOMAIN, item_id, source_revision)
}

/// The request version of that job. Provider request validation requires a UUID, so the
/// instruction version [`M1_INSTRUCTION_VERSION`] (a SHA-256 digest) is bound into a UUID that
/// changes with the instruction text, the item and the revision.
pub fn interpretation_request_version(item_id: &str, source_revision: i32) -> String {
    let mut domain = REQUEST_VERSION_DOMAIN.to_vec();
    domain.extend_from_slice(M1_INSTRUCTION_VERSION.as_bytes());
    derived_uuid(&domain, item_id, source_revision)
}

fn derived_uuid(domain: &[u8], item_id: &str, source_revision: i32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update([0]);
    hasher.update(item_id.as_bytes());
    hasher.update([0]);
    hasher.update(source_revision.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// The core's own plan for interpreting `item_id` at `source_revision`.
pub fn plan_interpretation(
    transaction: &Transaction<'_>,
    item_id: &str,
    source_revision: i32,
) -> Result<InterpretationPlan> {
    Ok(InterpretationPlan {
        job_id: interpretation_job_id(item_id, source_revision),
        profile_version: resolve_profile_pin(transaction, item_id)?,
        request_version: interpretation_request_version(item_id, source_revision),
    })
}

/// Queue the planned `interpret` job unless a job with the plan's identity already exists, in
/// which case that job is returned unchanged. Safe to call again for the same item and revision.
pub fn enqueue_planned_interpretation(
    transaction: &Transaction<'_>,
    item_id: &str,
    source_revision: i32,
    plan: &InterpretationPlan,
    now: DateTime<Utc>,
) -> Result<Job> {
    if let Some(existing) = get_job_internal(transaction, &plan.job_id)? {
        return Ok(existing);
    }
    enqueue_job_in_tx(
        transaction,
        plan.job_id.clone(),
        item_id.to_string(),
        JOB_TYPE_INTERPRET.to_string(),
        source_revision,
        plan.profile_version.clone(),
        Some(plan.request_version.clone()),
        INTERPRETATION_JOB_SCHEMA_VERSION,
        now,
    )
}

/// Plan and queue the interpretation of `item_id` at `source_revision` in one step.
pub fn enqueue_default_interpretation(
    transaction: &Transaction<'_>,
    item_id: &str,
    source_revision: i32,
    now: DateTime<Utc>,
) -> Result<Job> {
    let plan = plan_interpretation(transaction, item_id, source_revision)?;
    enqueue_planned_interpretation(transaction, item_id, source_revision, &plan, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_deterministic_distinct_uuids() {
        let job_id = interpretation_job_id("item-1", 0);
        assert_eq!(job_id, interpretation_job_id("item-1", 0));
        assert_ne!(job_id, interpretation_job_id("item-1", 1));
        assert_ne!(job_id, interpretation_job_id("item-2", 0));

        let request_version = interpretation_request_version("item-1", 0);
        assert_eq!(request_version, interpretation_request_version("item-1", 0));
        assert_ne!(request_version, job_id);
        assert!(Uuid::parse_str(&job_id).is_ok());
        assert!(Uuid::parse_str(&request_version).is_ok());
    }
}
