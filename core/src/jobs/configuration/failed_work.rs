//! Reading the interpretation work that ended `failed`, and running it again on purpose.
//!
//! A drain never reruns a job that ended `failed`, and [`super::repin_waiting_interpretations`]
//! moves only unpinned waiting work. [`list_failed_interpretations`] names the active items whose
//! newest `interpret` job ended `failed` and was not since replaced, by machine label only, and
//! [`rerun_failed_interpretations`] gives each of them a new `interpret` job at the item's current
//! revision, pinned to whatever [`resolve_profile_pin`] selects now (the same rule `repin.rs`
//! uses) and linked to the failed job in `job_requeues`. The failed job itself is never modified.
//!
//! A failed job fenced for an unknown job schema version (`unsupported_job_version`) is neither
//! listed nor rerun: it waits for a build that supports it, as in `repin.rs`.
//!
//! Both operations are idempotent: once a failed job has a replacement it is no longer listed, so
//! repeating a rerun before the new job runs creates nothing.

use crate::interpretation::enqueue::{
    enqueue_planned_interpretation, resolve_profile_pin, InterpretationPlan,
};
use crate::interpretation::instructions::M1_INSTRUCTION_VERSION;
use crate::jobs::health::KNOWN_REASONS;
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use crate::retrieval::recent::parse_cursor;
use crate::store::events::ItemScope;
use crate::store::schema::Database;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Most items one call reruns. A caller with more failed work calls again.
pub const MAX_RERUN_BATCH: usize = 100;

const RERUN_JOB_ID_DOMAIN: &[u8] = b"ohand.interpretation-rerun.v1";
const RERUN_REQUEST_VERSION_DOMAIN: &[u8] = b"ohand.interpretation-rerun-request.v1";
const UNSUPPORTED_JOB_VERSION_REASON: &str = "unsupported_job_version";
const CURSOR_SEPARATOR: char = '|';
/// Reported when a stored failure reason is not a plain machine label.
const OTHER_REASON: &str = "other";

/// Whether running the failed work again could succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedWorkRecovery {
    /// A provider is allowed now, so a new job can be pinned and sent.
    RunAgain,
    /// No provider is allowed now: the person has to change a setting first.
    ChangeSettings,
}

impl FailedWorkRecovery {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailedWorkRecovery::RunAgain => "run_again",
            FailedWorkRecovery::ChangeSettings => "change_settings",
        }
    }
}

/// One active item whose newest `interpret` job ended `failed`. Carries no text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedInterpretation {
    pub item_id: String,
    pub captured_at: String,
    /// The job's failure reason as a fixed machine label.
    pub failure_reason: String,
    /// The profile version the failed job was pinned to, `None` when it was unpinned.
    pub pinned_profile_version: Option<String>,
    pub recovery: FailedWorkRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedInterpretationPage {
    pub failed: Vec<FailedInterpretation>,
    pub next_cursor: Option<String>,
}

/// Why an item was not moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerunSkipReason {
    /// The item has no failed, unreplaced `interpret` job the caller may read: unknown, deleted,
    /// out of scope, not failed, or already run again.
    NotFailed,
    /// No provider is allowed for the item now, so there is nothing to pin the new job to.
    NoProviderAllowed,
}

impl RerunSkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RerunSkipReason::NotFailed => "not_failed",
            RerunSkipReason::NoProviderAllowed => "no_provider_allowed",
        }
    }
}

/// Which failed work a rerun covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RerunSelection {
    /// These items, at most [`MAX_RERUN_BATCH`] of them.
    Items(Vec<String>),
    /// Every item [`list_failed_interpretations`] lists, newest first, at most
    /// [`MAX_RERUN_BATCH`] of them.
    AllListed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RerunOutcome {
    pub moved: usize,
    pub skipped: Vec<(String, RerunSkipReason)>,
}

struct FailedRow {
    item_id: String,
    revision: i32,
    captured_at: String,
    captured_instant: DateTime<Utc>,
    failed_job_id: String,
    failure_reason: String,
    pinned_profile_version: Option<String>,
}

/// The failed, unreplaced newest `interpret` jobs of active items in `read_scopes` (optionally
/// only `item_id`), newest capture first, then item id.
fn failed_rows(
    tx: &Transaction<'_>,
    read_scopes: &[ItemScope],
    item_id: Option<&str>,
) -> Result<Vec<FailedRow>> {
    if read_scopes.is_empty() {
        return Ok(Vec::new());
    }
    let scope_placeholders = vec!["?"; read_scopes.len()].join(", ");
    let sql = format!(
        "SELECT i.item_id, i.revision, c.capture_instant, j.job_id, j.failure_reason, \
                j.profile_version \
           FROM items i \
           JOIN captures c ON c.capture_id = i.capture_id \
           JOIN jobs j ON j.job_id = ( \
                SELECT n.job_id FROM jobs n WHERE n.item_id = i.item_id AND n.job_type = ? \
                ORDER BY n.created_at DESC, n.rowid DESC LIMIT 1) \
          WHERE i.lifecycle_state = 'active' \
            AND COALESCE(i.current_scope, c.item_scope) IN ({scope_placeholders}) \
            AND j.status = 'failed' AND j.failure_reason IS NOT NULL \
            AND j.failure_reason != ? \
            AND NOT EXISTS (SELECT 1 FROM job_requeues r WHERE r.old_job_id = j.job_id) \
            AND (? IS NULL OR i.item_id = ?)"
    );
    let mut parameters: Vec<String> = vec![JOB_TYPE_INTERPRET.to_string()];
    parameters.extend(read_scopes.iter().map(|scope| scope.as_str().to_string()));
    parameters.push(UNSUPPORTED_JOB_VERSION_REASON.to_string());
    let mut statement = tx
        .prepare(&sql)
        .context("preparing the failed interpretations query")?;
    let item_filter = item_id.map(str::to_string);
    let mut bound: Vec<&dyn rusqlite::ToSql> = parameters
        .iter()
        .map(|parameter| parameter as &dyn rusqlite::ToSql)
        .collect();
    bound.push(&item_filter);
    bound.push(&item_filter);
    let raw = statement
        .query_map(bound.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .context("querying failed interpretations")?
        .collect::<Result<Vec<_>, _>>()
        .context("reading failed interpretations")?;
    let mut rows: Vec<FailedRow> = raw
        .into_iter()
        .filter_map(
            |(item_id, revision, captured_at, failed_job_id, reason, pinned)| {
                // An instant that is not a valid timestamp cannot be placed in the order.
                let captured_instant = DateTime::parse_from_rfc3339(&captured_at)
                    .ok()?
                    .with_timezone(&Utc);
                Some(FailedRow {
                    item_id,
                    revision,
                    captured_at,
                    captured_instant,
                    failed_job_id,
                    failure_reason: machine_label(&reason),
                    pinned_profile_version: pinned,
                })
            },
        )
        .collect();
    rows.sort_by(|left, right| {
        right
            .captured_instant
            .cmp(&left.captured_instant)
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    Ok(rows)
}

/// Reports a stored reason only when it is in the closed health vocabulary; label-shaped text
/// that is not listed there is reported as `other`, so no text leaves the store through this read.
fn machine_label(reason: &str) -> String {
    if KNOWN_REASONS.contains(&reason) {
        reason.to_string()
    } else {
        OTHER_REASON.to_string()
    }
}

/// Up to `limit` failed interpretations of items in `read_scopes`, newest capture first, starting
/// after `cursor` (a value returned as `next_cursor`, validated with [`parse_cursor`] by the
/// caller). The recovery label reflects the pin the resolver selects now. Nothing is written.
pub fn list_failed_interpretations(
    db: &mut Database,
    read_scopes: &[ItemScope],
    limit: usize,
    cursor: Option<&str>,
) -> Result<FailedInterpretationPage> {
    let cursor_position = match cursor {
        Some(cursor) => {
            let (instant, item_id) = parse_cursor(cursor)
                .ok_or_else(|| anyhow::anyhow!("failed-work cursor is malformed"))?;
            let instant = DateTime::parse_from_rfc3339(instant)
                .map_err(|_| anyhow::anyhow!("failed-work cursor is malformed"))?
                .with_timezone(&Utc);
            Some((instant, item_id.to_string()))
        }
        None => None,
    };
    // The pin check probes authorization inside a savepoint; the transaction is dropped, so
    // nothing is committed.
    let tx = db.immediate_transaction()?;
    let rows: Vec<FailedRow> = failed_rows(&tx, read_scopes, None)?
        .into_iter()
        .filter(|row| match &cursor_position {
            Some((instant, item_id)) => {
                row.captured_instant < *instant
                    || (row.captured_instant == *instant && row.item_id > *item_id)
            }
            None => true,
        })
        .take(limit.saturating_add(1))
        .collect();
    let has_more = rows.len() > limit;
    let mut failed = Vec::new();
    for row in rows.into_iter().take(limit) {
        let recovery = if resolve_profile_pin(&tx, &row.item_id)?.is_some() {
            FailedWorkRecovery::RunAgain
        } else {
            FailedWorkRecovery::ChangeSettings
        };
        failed.push(FailedInterpretation {
            item_id: row.item_id,
            captured_at: row.captured_at,
            failure_reason: row.failure_reason,
            pinned_profile_version: row.pinned_profile_version,
            recovery,
        });
    }
    drop(tx);
    let next_cursor = if has_more {
        failed
            .last()
            .map(|last| format!("{}{}{}", last.captured_at, CURSOR_SEPARATOR, last.item_id))
    } else {
        None
    };
    Ok(FailedInterpretationPage {
        failed,
        next_cursor,
    })
}

/// Give each selected failed item a new `interpret` job, in one transaction. See the module
/// documentation.
pub fn rerun_failed_interpretations(
    db: &mut Database,
    read_scopes: &[ItemScope],
    selection: &RerunSelection,
    now: DateTime<Utc>,
) -> Result<RerunOutcome> {
    let tx = db.immediate_transaction()?;
    let outcome = rerun_failed_interpretations_in_tx(&tx, read_scopes, selection, now)?;
    tx.commit()?;
    Ok(outcome)
}

/// See [`rerun_failed_interpretations`].
pub fn rerun_failed_interpretations_in_tx(
    tx: &Transaction<'_>,
    read_scopes: &[ItemScope],
    selection: &RerunSelection,
    now: DateTime<Utc>,
) -> Result<RerunOutcome> {
    let mut candidates: Vec<FailedRow> = Vec::new();
    let mut skipped: Vec<(String, RerunSkipReason)> = Vec::new();
    match selection {
        RerunSelection::AllListed => {
            candidates = failed_rows(tx, read_scopes, None)?;
            candidates.truncate(MAX_RERUN_BATCH);
        }
        RerunSelection::Items(item_ids) => {
            let mut seen: Vec<&str> = Vec::new();
            for item_id in item_ids.iter().take(MAX_RERUN_BATCH) {
                if seen.contains(&item_id.as_str()) {
                    continue;
                }
                seen.push(item_id);
                match failed_rows(tx, read_scopes, Some(item_id))?.pop() {
                    Some(row) => candidates.push(row),
                    None => skipped.push((item_id.clone(), RerunSkipReason::NotFailed)),
                }
            }
        }
    }
    let mut moved = 0;
    for row in candidates {
        let Some(profile_version) = resolve_profile_pin(tx, &row.item_id)? else {
            skipped.push((row.item_id, RerunSkipReason::NoProviderAllowed));
            continue;
        };
        enqueue_rerun(
            tx,
            &row.item_id,
            row.revision,
            &row.failed_job_id,
            row.pinned_profile_version.as_deref(),
            &profile_version,
            now,
        )?;
        moved += 1;
    }
    Ok(RerunOutcome { moved, skipped })
}

/// Give `item_id` a new `interpret` job at `revision` pinned to `profile_version`, and link it to
/// the job it replaces. Both reruns use this one path.
fn enqueue_rerun(
    tx: &Transaction<'_>,
    item_id: &str,
    revision: i32,
    replaced_job_id: &str,
    replaced_profile_version: Option<&str>,
    profile_version: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let plan = InterpretationPlan {
        job_id: rerun_uuid(
            RERUN_JOB_ID_DOMAIN,
            replaced_job_id,
            profile_version,
            revision,
        ),
        profile_version: Some(profile_version.to_string()),
        // The queue refuses a second job with the same item, revision, profile and request
        // version, so a rerun pinned to the replaced job's own profile needs its own.
        request_version: rerun_uuid(
            RERUN_REQUEST_VERSION_DOMAIN,
            replaced_job_id,
            profile_version,
            revision,
        ),
    };
    enqueue_planned_interpretation(tx, item_id, revision, &plan, now)?;
    tx.execute(
        "INSERT INTO job_requeues (
            new_job_id, old_job_id, from_profile_version, to_profile_version, requeued_at
        ) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            &plan.job_id,
            replaced_job_id,
            replaced_profile_version,
            profile_version,
            now.to_rfc3339()
        ],
    )
    .context("linking the replaced job to its replacement")?;
    Ok(())
}

/// What running one abstained note again did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbstainedRerun {
    /// A new `interpret` job was created.
    Queued,
    /// No provider is allowed for the item now, so there is nothing to pin the new job to.
    NoProviderAllowed,
    /// The item is unknown, deleted, out of scope, or its current interpretation is not
    /// `abstained`.
    NotAbstained,
}

impl AbstainedRerun {
    pub fn as_str(&self) -> &'static str {
        match self {
            AbstainedRerun::Queued => "queued",
            AbstainedRerun::NoProviderAllowed => "no_provider_allowed",
            AbstainedRerun::NotAbstained => "not_abstained",
        }
    }
}

/// Run one abstained note again: a new `interpret` job at the item's current revision, pinned by
/// [`resolve_profile_pin`] and linked to the completed job that produced the abstention. Abstained
/// items are never listed by [`list_failed_interpretations`] and never part of
/// [`RerunSelection::AllListed`]. Repeating the call before the new job runs finds the item's
/// newest `interpret` job already replaced and answers `not_abstained`, creating nothing.
pub fn rerun_abstained_interpretation(
    db: &mut Database,
    read_scopes: &[ItemScope],
    item_id: &str,
    now: DateTime<Utc>,
) -> Result<AbstainedRerun> {
    let tx = db.immediate_transaction()?;
    let outcome = rerun_abstained_interpretation_in_tx(&tx, read_scopes, item_id, now)?;
    tx.commit()?;
    Ok(outcome)
}

/// See [`rerun_abstained_interpretation`].
pub fn rerun_abstained_interpretation_in_tx(
    tx: &Transaction<'_>,
    read_scopes: &[ItemScope],
    item_id: &str,
    now: DateTime<Utc>,
) -> Result<AbstainedRerun> {
    if read_scopes.is_empty() {
        return Ok(AbstainedRerun::NotAbstained);
    }
    let scope_placeholders = vec!["?"; read_scopes.len()].join(", ");
    let sql = format!(
        "SELECT i.revision, j.job_id, j.profile_version \
           FROM items i \
           JOIN captures c ON c.capture_id = i.capture_id \
           JOIN jobs j ON j.job_id = ( \
                SELECT n.job_id FROM jobs n WHERE n.item_id = i.item_id AND n.job_type = ? \
                ORDER BY n.created_at DESC, n.rowid DESC LIMIT 1) \
          WHERE i.item_id = ? AND i.lifecycle_state = 'active' \
            AND i.processing_state = 'abstained' \
            AND COALESCE(i.current_scope, c.item_scope) IN ({scope_placeholders}) \
            AND j.status = 'completed' \
            AND NOT EXISTS (SELECT 1 FROM job_requeues r WHERE r.old_job_id = j.job_id)"
    );
    let mut parameters: Vec<String> = vec![JOB_TYPE_INTERPRET.to_string(), item_id.to_string()];
    parameters.extend(read_scopes.iter().map(|scope| scope.as_str().to_string()));
    let found: Option<(i32, String, Option<String>)> = tx
        .query_row(&sql, rusqlite::params_from_iter(parameters.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()
        .context("reading the abstained interpretation")?;
    let Some((revision, abstained_job_id, pinned_profile_version)) = found else {
        return Ok(AbstainedRerun::NotAbstained);
    };
    let Some(profile_version) = resolve_profile_pin(tx, item_id)? else {
        return Ok(AbstainedRerun::NoProviderAllowed);
    };
    enqueue_rerun(
        tx,
        item_id,
        revision,
        &abstained_job_id,
        pinned_profile_version.as_deref(),
        &profile_version,
        now,
    )?;
    Ok(AbstainedRerun::Queued)
}

/// A UUID derived from the failed job, so repeating a rerun names the same replacement.
fn rerun_uuid(domain: &[u8], failed_job_id: &str, profile_version: &str, revision: i32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(M1_INSTRUCTION_VERSION.as_bytes());
    for part in [failed_job_id.as_bytes(), profile_version.as_bytes()] {
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
