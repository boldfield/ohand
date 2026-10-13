//! Durable global reset generation (L05a).
//!
//! A reset erases everything captured before it started by composing the per-item deletion
//! guard under one durable generation, instead of adding a second fence to every apply path.
//! Tombstoning an item (`delete_intent`) already cancels its jobs and reminders, erases readable
//! content and the index entry, and queues cleanup; every result and apply path already rejects
//! work for a deleted item, and importing an already imported capture ID returns `ItemDeleted`.
//! A late job result, correction, transcript, interpretation or reminder effect that arrives
//! after reset started therefore revives nothing.
//!
//! * Starting a reset records, in one immediate transaction, a new generation in phase `pending`
//!   and its boundary: the highest `items.rowid` at that moment (0 for an empty store). No clock
//!   is compared. Items are never removed from the table, so rowids only grow: an item created
//!   after the boundary belongs to the fresh generation and no retry ever tombstones it.
//! * The same transaction tombstones every live item at or below the boundary, so no committed
//!   pending generation ever exposes an unfenced pre-reset item: a crash before the commit
//!   leaves no reset at all and one after it leaves every old item tombstoned. Starting again
//!   while a generation is pending resumes that generation and never opens another one (the
//!   schema allows one pending generation); the pass is idempotent.
//! * The generation completes only when every item at or below the boundary is tombstoned and
//!   every required cleanup work row of those items is acknowledged as completed. A terminally
//!   failed cleanup keeps the reset pending and is reported, never counted as acknowledged.
//!   Cleanup that has no item (native ingress staging, export temporaries, credentials) is
//!   orchestrated natively by L05b around this operation.

use crate::lifecycle::delete_intent::tombstone_at_current_revision;
use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

#[cfg(test)]
mod tests;

/// Where a reset generation stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetPhase {
    Pending,
    Completed,
}

impl ResetPhase {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "pending" => Ok(ResetPhase::Pending),
            "completed" => Ok(ResetPhase::Completed),
            other => Err(anyhow!("Unknown reset phase: {}", other)),
        }
    }
}

/// A reset generation and what still keeps it from completing, derived from durable rows only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetStatus {
    /// First reset is generation 1; content captured before any reset is generation 0.
    pub generation: i64,
    pub phase: ResetPhase,
    /// Highest `items.rowid` that existed when the reset started.
    pub boundary_rowid: i64,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Live items at or below the boundary that are not yet tombstoned.
    pub items_awaiting_tombstone: usize,
    /// Cleanup work rows of those items that are not yet acknowledged (including a tombstoned
    /// item with no cleanup row recorded, which can never read as acknowledged).
    pub cleanup_pending: usize,
    /// Cleanup work rows that terminally failed. They keep the reset pending.
    pub cleanup_failed: usize,
}

/// Start a reset, or resume the pending one, and drive it as far as it can go now: the
/// generation and boundary are recorded atomically, every live item at or below the boundary is
/// tombstoned, and the generation completes if all cleanup is already acknowledged.
///
/// Calling this while a generation is pending returns that generation's progress and never
/// starts another. After a completed reset it starts a new generation.
pub fn start_reset(db: &mut Database, now: DateTime<Utc>) -> Result<ResetStatus> {
    start_with_fault(db, now, None)
}

/// Resume the pending reset after a restart or after cleanup acknowledgments arrived:
/// tombstone anything not yet tombstoned and complete the generation once every required
/// cleanup is acknowledged. When no reset is pending, returns the latest (completed) status.
pub fn advance_reset(db: &mut Database, now: DateTime<Utc>) -> Result<ResetStatus> {
    let latest = latest_generation(db)?.ok_or_else(|| anyhow!("No reset has been started"))?;
    if latest.phase == ResetPhase::Completed {
        return Ok(latest);
    }
    advance_with_fault(db, now, latest.generation, None)
}

#[cfg(test)]
pub(crate) fn advance_reset_with_fault(
    db: &mut Database,
    now: DateTime<Utc>,
    fault_after_tombstones: Option<usize>,
) -> Result<ResetStatus> {
    let latest = latest_generation(db)?.ok_or_else(|| anyhow!("No reset has been started"))?;
    advance_with_fault(db, now, latest.generation, fault_after_tombstones)
}

fn advance_with_fault(
    db: &mut Database,
    now: DateTime<Utc>,
    generation: i64,
    fault_after_tombstones: Option<usize>,
) -> Result<ResetStatus> {
    let tx = db.immediate_transaction()?;
    drive_in_tx(&tx, now, generation, fault_after_tombstones)?;
    tx.commit()?;
    status_of(db, generation)
}

/// The latest reset generation, or `None` when no reset has ever started. Read-only.
pub fn reset_status(db: &Database) -> Result<Option<ResetStatus>> {
    latest_generation(db)
}

/// The reset generation a capture belongs to: the newest generation that started before the
/// item was created, or 0 for content created before any reset.
pub fn generation_of_item(db: &Database, item_id: &str) -> Result<i64> {
    let item_rowid: i64 = db
        .conn()
        .query_row(
            "SELECT rowid FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;
    Ok(db.conn().query_row(
        "SELECT COALESCE(MAX(generation), 0) FROM reset_generations WHERE boundary_rowid < ?",
        [item_rowid],
        |row| row.get(0),
    )?)
}

/// The generation new captures belong to right now: the latest started reset, or 0.
pub fn current_generation(db: &Database) -> Result<i64> {
    Ok(db.conn().query_row(
        "SELECT COALESCE(MAX(generation), 0) FROM reset_generations",
        [],
        |row| row.get(0),
    )?)
}

/// `fault_after_tombstones` fails the transaction after that many tombstones, which models a
/// crash before the commit: nothing of the reset survives.
pub(crate) fn start_with_fault(
    db: &mut Database,
    now: DateTime<Utc>,
    fault_after_tombstones: Option<usize>,
) -> Result<ResetStatus> {
    let tx = db.immediate_transaction()?;
    let generation = match pending_generation_in_tx(&tx)? {
        Some(pending) => pending,
        None => {
            tx.execute(
                "INSERT INTO reset_generations (phase, boundary_rowid, started_at)
                 VALUES ('pending', (SELECT COALESCE(MAX(rowid), 0) FROM items), ?)",
                [now.to_rfc3339()],
            )?;
            tx.last_insert_rowid()
        }
    };
    drive_in_tx(&tx, now, generation, fault_after_tombstones)?;
    tx.commit()?;
    status_of(db, generation)
}

fn drive_in_tx(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    generation: i64,
    fault_after_tombstones: Option<usize>,
) -> Result<()> {
    let boundary = boundary_in_tx(tx, generation)?;
    for (tombstoned, item_id) in live_items_up_to(tx, boundary)?.iter().enumerate() {
        if fault_after_tombstones == Some(tombstoned) {
            return Err(anyhow!(
                "simulated crash before the reset transaction commits"
            ));
        }
        tombstone_at_current_revision(tx, item_id, now)?;
    }
    complete_if_acknowledged(tx, generation, boundary, now)
}

fn boundary_in_tx(tx: &Transaction<'_>, generation: i64) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT boundary_rowid FROM reset_generations WHERE generation = ?",
        [generation],
        |row| row.get(0),
    )?)
}

fn pending_generation_in_tx(tx: &Transaction<'_>) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT generation FROM reset_generations WHERE phase = 'pending'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

fn live_items_up_to(tx: &Transaction<'_>, boundary: i64) -> Result<Vec<String>> {
    let mut statement = tx.prepare(
        "SELECT item_id FROM items WHERE rowid <= ? AND lifecycle_state != 'deleted'
         ORDER BY rowid",
    )?;
    let items = statement
        .query_map([boundary], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(items)
}

/// Counts of what still blocks completion: (live items, unacknowledged cleanup, failed cleanup).
fn blockers_in_tx(tx: &Transaction<'_>, boundary: i64) -> Result<(usize, usize, usize)> {
    let count = |sql: &str| -> Result<usize> {
        Ok(tx.query_row(sql, [boundary], |row| row.get::<_, i64>(0))? as usize)
    };
    let live =
        count("SELECT COUNT(*) FROM items WHERE rowid <= ? AND lifecycle_state != 'deleted'")?;
    let pending = count(
        "SELECT COUNT(*) FROM deletion_work
         WHERE status = 'pending' AND item_id IN (SELECT item_id FROM items WHERE rowid <= ?)",
    )?;
    let without_cleanup = count(
        "SELECT COUNT(*) FROM items WHERE rowid <= ? AND lifecycle_state = 'deleted'
         AND NOT EXISTS (SELECT 1 FROM deletion_work WHERE deletion_work.item_id = items.item_id)",
    )?;
    let failed = count(
        "SELECT COUNT(*) FROM deletion_work
         WHERE status = 'failed_no_retry'
           AND item_id IN (SELECT item_id FROM items WHERE rowid <= ?)",
    )?;
    Ok((live, pending + without_cleanup, failed))
}

fn complete_if_acknowledged(
    tx: &Transaction<'_>,
    generation: i64,
    boundary: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let (live, pending, failed) = blockers_in_tx(tx, boundary)?;
    if live == 0 && pending == 0 && failed == 0 {
        tx.execute(
            "UPDATE reset_generations SET phase = 'completed', completed_at = ?
             WHERE generation = ? AND phase = 'pending'",
            rusqlite::params![now.to_rfc3339(), generation],
        )?;
    }
    Ok(())
}

fn status_of(db: &mut Database, generation: i64) -> Result<ResetStatus> {
    let tx = db.transaction()?;
    let status = status_in_tx(&tx, generation)?;
    drop(tx);
    Ok(status)
}

fn latest_generation(db: &Database) -> Result<Option<ResetStatus>> {
    let tx = db.conn().unchecked_transaction()?;
    let latest: Option<i64> = tx
        .query_row("SELECT MAX(generation) FROM reset_generations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    latest
        .map(|generation| status_in_tx(&tx, generation))
        .transpose()
}

fn status_in_tx(tx: &Transaction<'_>, generation: i64) -> Result<ResetStatus> {
    let (phase, boundary_rowid, started_at, completed_at): (String, i64, String, Option<String>) =
        tx.query_row(
            "SELECT phase, boundary_rowid, started_at, completed_at
             FROM reset_generations WHERE generation = ?",
            [generation],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let phase = ResetPhase::parse(&phase)?;
    let (items_awaiting_tombstone, cleanup_pending, cleanup_failed) =
        blockers_in_tx(tx, boundary_rowid)?;
    Ok(ResetStatus {
        generation,
        phase,
        boundary_rowid,
        started_at: parse_instant(&started_at)?,
        completed_at: completed_at.as_deref().map(parse_instant).transpose()?,
        items_awaiting_tombstone,
        cleanup_pending,
        cleanup_failed,
    })
}

fn parse_instant(text: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| anyhow!("malformed timestamp {text:?}: {error}"))
}
