// Idempotent deletion intent and tombstone processing.
// Implementation owned by L01.
//
// Deletion is a one-way transition: an item's lifecycle_state moves to "deleted" and remains
// readonly. The deletion process is idempotent and durable:
//
// 1. Mark intent: set lifecycle to deleted, remove from search index, cancel pending jobs
// 2. Enqueue cleanup work: audio files, ingress records, native notifications
// 3. Track progress: deletion work table records whether each cleanup has completed
//
// A crash between marking intent and completing cleanup does not restore readable text: the
// item's lifecycle_state persists as deleted, the search index is rebuilt without it, and jobs
// remain cancelled. Completion of individual cleanup tasks is recorded in deletion_work table so
// reconciliation can resume without duplication.
//
// Racing job results cannot restore readable text because the item is locked (lifecycle_state
// forbids mutations), and reprocessing of stale source records cannot schedule new reminders
// because the item is already marked completed/deleted.

use crate::domain::items::{load_item_state, LifecycleState};
use crate::jobs::queue::JobStatus;
use crate::retrieval::index;
use crate::store::events::{Event, EventPayload, EventType};
use crate::store::schema::Database;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

/// Work type for deletion cleanup tasks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletionWorkType {
    /// Remove audio file associated with the item
    RemoveAudio,
    /// Clear ingress record associated with the item
    ClearIngress,
    /// Cancel native notifications associated with the item
    CancelNotifications,
}

impl DeletionWorkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeletionWorkType::RemoveAudio => "remove_audio",
            DeletionWorkType::ClearIngress => "clear_ingress",
            DeletionWorkType::CancelNotifications => "cancel_notifications",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "remove_audio" => Ok(DeletionWorkType::RemoveAudio),
            "clear_ingress" => Ok(DeletionWorkType::ClearIngress),
            "cancel_notifications" => Ok(DeletionWorkType::CancelNotifications),
            _ => Err(anyhow!("Unknown deletion work type: {}", s)),
        }
    }
}

/// Status of a deletion cleanup task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletionWorkStatus {
    /// Cleanup work is pending (not yet attempted or failed with retry pending)
    Pending,
    /// Cleanup work was successfully completed
    Completed,
    /// Cleanup work failed and no more retries are planned
    FailedNoRetry,
}

impl DeletionWorkStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeletionWorkStatus::Pending => "pending",
            DeletionWorkStatus::Completed => "completed",
            DeletionWorkStatus::FailedNoRetry => "failed_no_retry",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(DeletionWorkStatus::Pending),
            "completed" => Ok(DeletionWorkStatus::Completed),
            "failed_no_retry" => Ok(DeletionWorkStatus::FailedNoRetry),
            _ => Err(anyhow!("Unknown deletion work status: {}", s)),
        }
    }
}

/// A deletion cleanup task.
#[derive(Clone, Debug)]
pub struct DeletionWork {
    pub deletion_work_id: String,
    pub item_id: String,
    pub work_type: DeletionWorkType,
    pub status: DeletionWorkStatus,
    pub attempted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl DeletionWork {
    pub fn new(
        deletion_work_id: String,
        item_id: String,
        work_type: DeletionWorkType,
        created_at: DateTime<Utc>,
    ) -> Self {
        DeletionWork {
            deletion_work_id,
            item_id,
            work_type,
            status: DeletionWorkStatus::Pending,
            attempted_at: None,
            created_at,
        }
    }
}

fn deletion_work_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeletionWork> {
    let work_type_str: String = row.get(2)?;
    let work_type = DeletionWorkType::parse(&work_type_str)
        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;

    let status_str: String = row.get(3)?;
    let status = DeletionWorkStatus::parse(&status_str)
        .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;

    let attempted_at: Option<String> = row.get(4)?;
    let attempted_at = attempted_at
        .map(|text| {
            DateTime::parse_from_rfc3339(&text)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::InvalidParameterName(format!(
                        "malformed timestamp {text:?}: {e}"
                    ))
                })
        })
        .transpose()?;

    let created_at_str: String = row.get(5)?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::InvalidParameterName(format!(
                "malformed timestamp {created_at_str:?}: {e}"
            ))
        })?;

    Ok(DeletionWork {
        deletion_work_id: row.get(0)?,
        item_id: row.get(1)?,
        work_type,
        status,
        attempted_at,
        created_at,
    })
}

/// Mark an item for deletion in a single idempotent transaction.
/// This atomically:
/// 1. Sets the item's lifecycle_state to "deleted" via an event (compare-and-set on revision)
/// 2. Removes readable/indexed content: clears capture text and correction content
/// 3. Removes the item from the search index
/// 4. Cancels all pending jobs for this item
/// 5. Cancels all reminders (moves to cancelled state and enqueues reminder cancellation)
/// 6. Enqueues cleanup work for audio, ingress, and notifications
///
/// Returns a deletion work record if successful.
/// If the item is already deleted, returns success (idempotent).
///
/// `expected_revision` enforces compare-and-set semantics: a stale delete command cannot
/// delete a newer corrected item.
pub fn mark_deletion_intent(
    db: &mut Database,
    item_id: &str,
    expected_revision: i32,
    now: DateTime<Utc>,
) -> Result<DeletionWork> {
    let tx = db.immediate_transaction()?;

    // Load current item state to check if already deleted and validate revision
    let current_state = load_item_state(&tx, item_id)
        .map_err(|e| anyhow!("Failed to load item state: {}", e))?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;

    // Enforce compare-and-set: reject stale delete commands first, before checking state
    if current_state.revision != expected_revision {
        return Err(anyhow!(
            "Stale delete: expected revision {} but current is {}",
            expected_revision,
            current_state.revision
        ));
    }

    // Check if already deleted (idempotent case)
    if current_state.lifecycle_state == LifecycleState::Deleted {
        // Item is already deleted; return success with existing work
        tx.commit()?;
        let conn = db.conn();
        let existing_work: Option<String> = conn
            .query_row(
                "SELECT deletion_work_id FROM deletion_work WHERE item_id = ? LIMIT 1",
                [item_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(work_id) = existing_work {
            return get_deletion_work(db, &work_id)?
                .ok_or_else(|| anyhow!("Deletion work not found"));
        }

        // No existing work, return empty (shouldn't happen normally)
        return Err(anyhow!("Item already deleted with no pending work"));
    }

    // Create deletion event with command identity (not timestamp)
    let event_id = format!("deletion:{}", item_id);
    let deletion_event = Event::new(
        event_id,
        item_id.to_string(),
        current_state.revision,
        EventType::Deletion,
        EventPayload::Deletion,
        now.to_rfc3339(),
    )
    .map_err(|e| anyhow!("Failed to create deletion event: {}", e))?;

    // Attempt to save the deletion event (compare-and-set on revision)
    // save_event_in_tx will enforce compare-and-set; pass the expected revision
    crate::store::events::save_event_in_tx(&tx, &deletion_event, expected_revision)
        .map_err(|e| anyhow!("Failed to save deletion event: {}", e))?;

    // Remove readable content: clear source text and all content-bearing rows
    remove_readable_content_in_tx(&tx, item_id)
        .map_err(|e| anyhow!("Failed to remove readable content: {}", e))?;

    // Remove from search index (item is now deleted via the event update)
    index::remove_item_from_index(&tx, item_id)
        .map_err(|e| anyhow!("Failed to remove item from search index: {}", e))?;

    // Cancel all pending jobs for this item
    cancel_item_jobs_in_tx(&tx, item_id).map_err(|e| anyhow!("Failed to cancel jobs: {}", e))?;

    // Cancel all reminders and enqueue reminder cancellation
    cancel_item_reminders_in_tx(&tx, item_id, &now)
        .map_err(|e| anyhow!("Failed to cancel reminders: {}", e))?;

    // Enqueue cleanup work (audio, ingress, notifications)
    let work_id_audio =
        enqueue_deletion_work_in_tx(&tx, item_id, DeletionWorkType::RemoveAudio, now)
            .map_err(|e| anyhow!("Failed to enqueue audio cleanup: {}", e))?;

    enqueue_deletion_work_in_tx(&tx, item_id, DeletionWorkType::ClearIngress, now)
        .map_err(|e| anyhow!("Failed to enqueue ingress cleanup: {}", e))?;

    enqueue_deletion_work_in_tx(&tx, item_id, DeletionWorkType::CancelNotifications, now)
        .map_err(|e| anyhow!("Failed to enqueue notification cleanup: {}", e))?;

    tx.commit()?;

    Ok(DeletionWork::new(
        work_id_audio,
        item_id.to_string(),
        DeletionWorkType::RemoveAudio,
        now,
    ))
}

/// Mark a deletion work task as completed.
/// Only updates if the current status is Pending; rejects other states.
pub fn mark_deletion_work_completed(
    db: &mut Database,
    deletion_work_id: &str,
    now: DateTime<Utc>,
) -> Result<DeletionWork> {
    let tx = db.transaction()?;

    let work = get_deletion_work_in_tx(&tx, deletion_work_id)
        .map_err(|e| anyhow!("Failed to load deletion work: {}", e))?
        .ok_or_else(|| anyhow!("Deletion work {} not found", deletion_work_id))?;

    // Only allow transition from Pending to Completed
    if work.status != DeletionWorkStatus::Pending {
        return Err(anyhow!(
            "Cannot complete deletion work with status {:?}",
            work.status
        ));
    }

    let completed_at_str = now.to_rfc3339();

    tx.execute(
        "UPDATE deletion_work SET status = ?, attempted_at = ? WHERE deletion_work_id = ? AND status = ?",
        rusqlite::params![
            DeletionWorkStatus::Completed.as_str(),
            completed_at_str,
            deletion_work_id,
            DeletionWorkStatus::Pending.as_str()
        ],
    )
    .map_err(|e| anyhow!("Failed to update deletion work status: {}", e))?;

    tx.commit()?;

    Ok(DeletionWork {
        status: DeletionWorkStatus::Completed,
        attempted_at: Some(now),
        ..work
    })
}

/// List all pending deletion work for an item.
pub fn list_pending_deletion_work(db: &Database, item_id: &str) -> Result<Vec<DeletionWork>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT deletion_work_id, item_id, work_type, status, attempted_at, created_at
         FROM deletion_work WHERE item_id = ? AND status = ?",
    )?;

    let work_items = stmt
        .query_map(
            rusqlite::params![item_id, DeletionWorkStatus::Pending.as_str()],
            deletion_work_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("Failed to query deletion work: {}", e))?;

    Ok(work_items)
}

/// Get a single deletion work item.
pub fn get_deletion_work(db: &Database, deletion_work_id: &str) -> Result<Option<DeletionWork>> {
    let tx = db.conn().unchecked_transaction()?;
    get_deletion_work_in_tx(&tx, deletion_work_id)
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Remove all readable content from an item: clear capture text and correction content.
/// This ensures deleted items cannot restore readable text after marking as deleted.
/// Uses empty strings to satisfy schema constraints while removing readable content.
fn remove_readable_content_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<()> {
    // Clear capture text (set to empty string to satisfy NOT NULL constraints)
    tx.execute(
        "UPDATE captures SET text = '' WHERE capture_id IN
         (SELECT capture_id FROM items WHERE item_id = ?)",
        rusqlite::params![item_id],
    )?;

    // Clear correction values and proposal content
    tx.execute(
        "UPDATE events SET correction_new_value = NULL, correction_old_value = NULL
         WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    // Delete proposal rows (suggestions, interpretation results)
    tx.execute(
        "DELETE FROM proposals WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    Ok(())
}

/// Cancel all reminders for an item by marking for user cancellation.
/// Updates reminder request_state to 'user_cancelled' so reconciliation can complete it.
fn cancel_item_reminders_in_tx(
    tx: &Transaction<'_>,
    item_id: &str,
    now: &DateTime<Utc>,
) -> Result<()> {
    // Mark all reminders for this item as user_cancelled in the request_state column.
    // Reconciliation will handle the actual notification cancellation.
    tx.execute(
        "UPDATE reminders SET request_state = 'user_cancelled', updated_at = ? WHERE item_id = ?",
        rusqlite::params![now.to_rfc3339(), item_id],
    )?;

    Ok(())
}

/// Cancel all pending and running jobs for an item.
fn cancel_item_jobs_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<usize> {
    Ok(tx.execute(
        "UPDATE jobs SET status = ? WHERE item_id = ? AND status IN (?, ?)",
        rusqlite::params![
            JobStatus::Cancelled.as_str(),
            item_id,
            JobStatus::Queued.as_str(),
            JobStatus::Running.as_str(),
        ],
    )?)
}

/// Enqueue a deletion work task in a transaction.
fn enqueue_deletion_work_in_tx(
    tx: &Transaction<'_>,
    item_id: &str,
    work_type: DeletionWorkType,
    created_at: DateTime<Utc>,
) -> Result<String> {
    let work_id = format!(
        "del-{}-{}-{}",
        item_id,
        work_type.as_str(),
        created_at.timestamp_millis()
    );
    let created_at_str = created_at.to_rfc3339();

    // Check if work already exists (idempotent)
    let existing: Option<String> = tx
        .query_row(
            "SELECT deletion_work_id FROM deletion_work
             WHERE item_id = ? AND work_type = ?",
            rusqlite::params![item_id, work_type.as_str()],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(existing_id) = existing {
        return Ok(existing_id);
    }

    tx.execute(
        "INSERT INTO deletion_work (deletion_work_id, item_id, work_type, status, created_at)
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            &work_id,
            item_id,
            work_type.as_str(),
            DeletionWorkStatus::Pending.as_str(),
            created_at_str
        ],
    )?;

    Ok(work_id)
}

/// Get a deletion work item from a transaction.
fn get_deletion_work_in_tx(
    tx: &Transaction<'_>,
    deletion_work_id: &str,
) -> Result<Option<DeletionWork>> {
    let work: Option<DeletionWork> = tx
        .query_row(
            "SELECT deletion_work_id, item_id, work_type, status, attempted_at, created_at
             FROM deletion_work WHERE deletion_work_id = ?",
            [deletion_work_id],
            deletion_work_from_row,
        )
        .optional()?;

    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deletion_work_type_str() {
        assert_eq!(DeletionWorkType::RemoveAudio.as_str(), "remove_audio");
        assert_eq!(DeletionWorkType::ClearIngress.as_str(), "clear_ingress");
        assert_eq!(
            DeletionWorkType::CancelNotifications.as_str(),
            "cancel_notifications"
        );
    }

    #[test]
    fn test_deletion_work_type_parse() {
        assert_eq!(
            DeletionWorkType::parse("remove_audio").unwrap(),
            DeletionWorkType::RemoveAudio
        );
        assert_eq!(
            DeletionWorkType::parse("clear_ingress").unwrap(),
            DeletionWorkType::ClearIngress
        );
        assert_eq!(
            DeletionWorkType::parse("cancel_notifications").unwrap(),
            DeletionWorkType::CancelNotifications
        );
        assert!(DeletionWorkType::parse("unknown").is_err());
    }

    #[test]
    fn test_deletion_work_status_str() {
        assert_eq!(DeletionWorkStatus::Pending.as_str(), "pending");
        assert_eq!(DeletionWorkStatus::Completed.as_str(), "completed");
        assert_eq!(
            DeletionWorkStatus::FailedNoRetry.as_str(),
            "failed_no_retry"
        );
    }

    #[test]
    fn test_deletion_work_status_parse() {
        assert_eq!(
            DeletionWorkStatus::parse("pending").unwrap(),
            DeletionWorkStatus::Pending
        );
        assert_eq!(
            DeletionWorkStatus::parse("completed").unwrap(),
            DeletionWorkStatus::Completed
        );
        assert_eq!(
            DeletionWorkStatus::parse("failed_no_retry").unwrap(),
            DeletionWorkStatus::FailedNoRetry
        );
        assert!(DeletionWorkStatus::parse("unknown").is_err());
    }

    #[test]
    fn test_deletion_work_new() {
        let now = Utc::now();
        let work = DeletionWork::new(
            "test-id".to_string(),
            "item-123".to_string(),
            DeletionWorkType::RemoveAudio,
            now,
        );

        assert_eq!(work.deletion_work_id, "test-id");
        assert_eq!(work.item_id, "item-123");
        assert_eq!(work.work_type, DeletionWorkType::RemoveAudio);
        assert_eq!(work.status, DeletionWorkStatus::Pending);
        assert_eq!(work.attempted_at, None);
        assert_eq!(work.created_at, now);
    }
}
