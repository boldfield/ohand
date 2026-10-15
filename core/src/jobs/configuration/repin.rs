//! The explicit, user-invoked move of waiting interpretation work onto a newly authorized profile.
//!
//! Work that was captured before any provider was configured, or before the route was granted,
//! ends waiting: its `interpret` job was unpinned, the offline fast path found nothing, and the
//! job stopped for configuration (`unsupported` or `unauthorized`), leaving the item
//! `unprocessed`. Changing a default, creating a profile or granting a route never touches that
//! work. [`repin_waiting_interpretations`] is the one operation that does, and only when the user
//! asks for it.
//!
//! The target is whatever [`resolve_profile_pin`] selects now: the stored default for text
//! interpretation, and only if `authorize_job` would let a job pinned to it dispatch on the
//! route. When it selects nothing (no default, a revoked default, a route without a grant) the
//! operation moves nothing.
//!
//! Each moved item gets a new `interpret` job pinned to that profile at the item's current
//! revision. No existing job is modified: a job pinned to any profile keeps its pin and its
//! state, and an unpinned waiting job is only linked to its replacement in `job_requeues`.

use super::get_requeue_record;
use crate::interpretation::enqueue::{
    enqueue_planned_interpretation, interpretation_request_version, resolve_profile_pin,
    InterpretationPlan,
};
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use crate::store::schema::Database;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Most items one call moves. A caller with more waiting work calls again.
pub const MAX_REPIN_BATCH: usize = 100;

const REPIN_JOB_ID_DOMAIN: &[u8] = b"ohand.interpretation-repin.v1";

/// Failure reasons that mean a job stopped for configuration rather than for the provider.
const CONFIGURATION_WAIT_REASONS: [&str; 3] =
    ["unauthorized", "unsupported", "unsupported_job_version"];

/// Move the waiting, unpinned interpretation work of `route_id` onto the profile the resolver
/// selects now, in one transaction, and return how many items moved (at most
/// [`MAX_REPIN_BATCH`]).
///
/// An item is moved when it is active, belongs to the route, is `unprocessed` or
/// `uninterpreted`, has text to interpret, has no queued or running `interpret` job, and has not
/// already had an `interpret` job at its current revision pinned to the target profile. Calling
/// again therefore moves nothing more once the work has moved, and work already pinned to another
/// profile and still live is never touched.
pub fn repin_waiting_interpretations(
    db: &mut Database,
    route_id: &str,
    now: DateTime<Utc>,
) -> Result<usize> {
    let tx = db.immediate_transaction()?;
    let moved = repin_waiting_interpretations_in_tx(&tx, route_id, now)?;
    tx.commit()?;
    Ok(moved)
}

/// See [`repin_waiting_interpretations`].
pub fn repin_waiting_interpretations_in_tx(
    tx: &Transaction<'_>,
    route_id: &str,
    now: DateTime<Utc>,
) -> Result<usize> {
    let representative: Option<String> = tx
        .query_row(
            "SELECT i.item_id FROM items i JOIN captures c ON c.capture_id = i.capture_id \
             WHERE c.route_id = ? AND i.lifecycle_state = 'active' ORDER BY i.rowid LIMIT 1",
            [route_id],
            |row| row.get(0),
        )
        .optional()
        .context("looking up an item of the route")?;
    let Some(representative) = representative else {
        return Ok(0);
    };
    // Authorization depends on the route, the capability and the profile alone, so one item
    // decides for every item of the route.
    let Some(profile_version) = resolve_profile_pin(tx, &representative)? else {
        return Ok(0);
    };

    let candidates = waiting_items(tx, route_id, &profile_version)?;
    for (item_id, revision) in &candidates {
        let waiting_job_id = waiting_unpinned_job(tx, item_id, *revision)?;
        let plan = InterpretationPlan {
            job_id: repin_job_id(item_id, *revision, &profile_version),
            profile_version: Some(profile_version.clone()),
            request_version: interpretation_request_version(item_id, *revision),
        };
        enqueue_planned_interpretation(tx, item_id, *revision, &plan, now)?;
        if let Some(waiting_job_id) = waiting_job_id {
            tx.execute(
                "INSERT INTO job_requeues (
                    new_job_id, old_job_id, from_profile_version, to_profile_version, requeued_at
                ) VALUES (?, ?, NULL, ?, ?)",
                rusqlite::params![
                    &plan.job_id,
                    waiting_job_id,
                    &profile_version,
                    now.to_rfc3339()
                ],
            )
            .context("linking the waiting job to its replacement")?;
        }
    }
    Ok(candidates.len())
}

fn waiting_items(
    tx: &Transaction<'_>,
    route_id: &str,
    profile_version: &str,
) -> Result<Vec<(String, i32)>> {
    let mut statement = tx
        .prepare(
            "SELECT i.item_id, i.revision FROM items i \
             JOIN captures c ON c.capture_id = i.capture_id \
             WHERE c.route_id = ?1 AND i.lifecycle_state = 'active' \
               AND i.processing_state IN ('unprocessed', 'uninterpreted') \
               AND (c.text IS NOT NULL \
                    OR EXISTS (SELECT 1 FROM transcript_attachments t WHERE t.item_id = i.item_id) \
                    OR EXISTS (SELECT 1 FROM corrections k \
                               WHERE k.item_id = i.item_id AND k.kind = 'text')) \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.item_id = i.item_id \
                               AND j.job_type = ?2 AND j.status IN ('queued', 'running')) \
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.item_id = i.item_id \
                               AND j.job_type = ?2 AND j.source_revision = i.revision \
                               AND j.profile_version = ?3) \
             ORDER BY i.rowid LIMIT ?4",
        )
        .context("preparing the waiting items query")?;
    let limit = i64::try_from(MAX_REPIN_BATCH).unwrap_or(i64::MAX);
    let rows = statement
        .query_map(
            rusqlite::params![route_id, JOB_TYPE_INTERPRET, profile_version, limit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("querying waiting items")?
        .collect::<Result<Vec<(String, i32)>, _>>()
        .context("reading waiting items")?;
    Ok(rows)
}

/// The item's newest `interpret` job at `revision`, when it is an unpinned job that stopped for
/// configuration and has not been linked to a replacement yet.
fn waiting_unpinned_job(
    tx: &Transaction<'_>,
    item_id: &str,
    revision: i32,
) -> Result<Option<String>> {
    let newest: Option<(String, Option<String>, String, Option<String>)> = tx
        .query_row(
            "SELECT job_id, profile_version, status, failure_reason FROM jobs \
             WHERE item_id = ? AND job_type = ? AND source_revision = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![item_id, JOB_TYPE_INTERPRET, revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .context("reading the item's newest interpretation job")?;
    let Some((job_id, None, status, Some(reason))) = newest else {
        return Ok(None);
    };
    if status != "failed" || !CONFIGURATION_WAIT_REASONS.contains(&reason.as_str()) {
        return Ok(None);
    }
    if get_requeue_record(tx, &job_id)?.is_some() {
        return Ok(None);
    }
    Ok(Some(job_id))
}

fn repin_job_id(item_id: &str, revision: i32, profile_version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(REPIN_JOB_ID_DOMAIN);
    for part in [item_id.as_bytes(), profile_version.as_bytes()] {
        hasher.update([0]);
        hasher.update(part);
    }
    hasher.update([0]);
    hasher.update(revision.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
