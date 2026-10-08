// Idempotent deletion intent and tombstone processing.
// Implementation owned by L01.
//
// Deletion is a one-way transition recorded by `mark_deletion_intent` in a single immediate
// transaction: tombstone event, readable-content erasure, index removal, job and reminder
// cancellation, and durable cleanup work. A retry with the original expected revision returns
// the existing intent; any other revision is a conflict.
//
// `deletion_progress` derives completion only from durable `deletion_work` rows, so a crash
// between the tombstone and the native cleanup effects can never read as complete.

use crate::domain::items::{load_item_state, LifecycleState};
use crate::jobs::queue::JobStatus;
use crate::reminders::state;
use crate::retrieval::index;
use crate::store::events::{Event, EventPayload, EventType};
use crate::store::schema::{Clock, Database};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction};

/// Simple clock wrapper for passing a fixed time to the N01 API.
struct FixedClock {
    instant: DateTime<Utc>,
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

/// Work type for deletion cleanup tasks.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// Handle returned by [`mark_deletion_intent`]: the revision at which the deletion was recorded
/// and every cleanup effect that must finish before deletion may be reported complete.
#[derive(Clone, Debug)]
pub struct DeletionIntent {
    pub item_id: String,
    /// Item revision the delete command was applied against (the command's `expected_revision`).
    pub deletion_revision: i32,
    pub work: Vec<DeletionWork>,
}

/// Aggregate state of an item's deletion, derived only from durable rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeletionProgress {
    /// The item has not been deleted.
    NotRequested,
    /// Deletion is recorded but at least one required cleanup effect has not finished.
    InProgress {
        pending: Vec<DeletionWorkType>,
        failed: Vec<DeletionWorkType>,
    },
    /// Every other effect finished, but at least one terminally failed and needs attention.
    Failed { failed: Vec<DeletionWorkType> },
    /// Every required cleanup effect completed.
    Complete,
}

/// Native cleanup identities for a deletion work row. The audio reference stays on the
/// tombstoned capture, as non-content recovery metadata, until its removal is acknowledged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupTarget {
    pub capture_id: String,
    pub audio_reference: Option<String>,
}

/// Mark an item for deletion in a single idempotent transaction.
/// This atomically:
/// 1. Sets the item's lifecycle_state to "deleted" via an event (compare-and-set on revision)
/// 2. Removes readable/indexed content: clears capture text and correction content
/// 3. Removes the item from the search index
/// 4. Cancels all pending jobs for this item
/// 5. Cancels all reminders (moves to cancelled state and enqueues reminder cancellation),
///    re-arming any earlier generation's failed native cancel so it is retried
/// 6. Enqueues cleanup work for audio (only when the capture has audio), ingress and
///    notifications
///
/// `expected_revision` enforces compare-and-set semantics: a stale delete command cannot
/// delete a newer corrected item. A retry of a delete that already committed must present the
/// same `expected_revision` and returns the existing intent; any other revision is a conflict.
pub fn mark_deletion_intent(
    db: &mut Database,
    item_id: &str,
    expected_revision: i32,
    now: DateTime<Utc>,
) -> Result<DeletionIntent> {
    let tx = db.immediate_transaction()?;

    let current_state = load_item_state(&tx, item_id)
        .map_err(|e| anyhow!("Failed to load item state: {}", e))?
        .ok_or_else(|| anyhow!("Item {} not found", item_id))?;

    if current_state.lifecycle_state == LifecycleState::Deleted {
        let deletion_revision: i32 = tx
            .query_row(
                "SELECT revision FROM events WHERE item_id = ? AND event_type = 'deletion'",
                [item_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("Item {} is deleted but has no deletion event", item_id))?;
        if deletion_revision != expected_revision {
            return Err(anyhow!(
                "Stale delete: item was deleted at revision {} but command expected {}",
                deletion_revision,
                expected_revision
            ));
        }
        let work = list_deletion_work_in_tx(&tx, item_id)?;
        if work.is_empty() {
            return Err(anyhow!(
                "Item {} is deleted but has no cleanup work recorded",
                item_id
            ));
        }
        return Ok(DeletionIntent {
            item_id: item_id.to_string(),
            deletion_revision,
            work,
        });
    }

    if current_state.revision != expected_revision {
        return Err(anyhow!(
            "Stale delete: expected revision {} but current is {}",
            expected_revision,
            current_state.revision
        ));
    }

    let deletion_event = Event::new(
        unused_deletion_event_id(&tx, item_id, current_state.revision)?,
        item_id.to_string(),
        current_state.revision,
        EventType::Deletion,
        EventPayload::Deletion,
        now.to_rfc3339(),
    )
    .map_err(|e| anyhow!("Failed to create deletion event: {}", e))?;

    crate::store::events::save_deletion_event_in_tx(&tx, &deletion_event, expected_revision)
        .map_err(|e| anyhow!("Failed to save deletion event: {}", e))?;

    let has_audio: bool = tx
        .query_row(
            "SELECT captures.audio_reference IS NOT NULL FROM captures
             JOIN items ON items.capture_id = captures.capture_id WHERE items.item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);

    remove_readable_content_in_tx(&tx, item_id)
        .map_err(|e| anyhow!("Failed to remove readable content: {}", e))?;
    index::remove_item_from_index(&tx, item_id)
        .map_err(|e| anyhow!("Failed to remove item from search index: {}", e))?;
    cancel_item_jobs_in_tx(&tx, item_id).map_err(|e| anyhow!("Failed to cancel jobs: {}", e))?;
    cancel_item_reminders_in_tx(&tx, item_id, &now)
        .map_err(|e| anyhow!("Failed to cancel reminders: {}", e))?;
    retry_failed_notification_cancels_in_tx(&tx, item_id)
        .map_err(|e| anyhow!("Failed to retry notification cancels: {}", e))?;

    let mut required_work = Vec::new();
    if has_audio {
        required_work.push(DeletionWorkType::RemoveAudio);
    }
    required_work.push(DeletionWorkType::ClearIngress);
    required_work.push(DeletionWorkType::CancelNotifications);
    for work_type in required_work {
        enqueue_deletion_work_in_tx(&tx, item_id, work_type, now)
            .map_err(|e| anyhow!("Failed to enqueue deletion cleanup: {}", e))?;
    }

    let work = list_deletion_work_in_tx(&tx, item_id)?;
    tx.commit()?;

    Ok(DeletionIntent {
        item_id: item_id.to_string(),
        deletion_revision: expected_revision,
        work,
    })
}

/// Report how far an item's deletion has progressed. Complete only when work rows exist and
/// every one of them has been durably recorded as completed, so a crash between the tombstone
/// and the cleanup effects can never read as complete.
pub fn deletion_progress(db: &mut Database, item_id: &str) -> Result<DeletionProgress> {
    let tx = db.immediate_transaction()?;
    let lifecycle_state: Option<String> = tx
        .query_row(
            "SELECT lifecycle_state FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .optional()?;
    match lifecycle_state.as_deref() {
        None => return Err(anyhow!("Item {} not found", item_id)),
        Some("deleted") => {}
        Some(_) => return Ok(DeletionProgress::NotRequested),
    }

    let work = list_deletion_work_in_tx(&tx, item_id)?;
    if work.is_empty() {
        return Err(anyhow!(
            "Item {} is deleted but has no cleanup work recorded",
            item_id
        ));
    }
    let types_with_status = |wanted: DeletionWorkStatus| -> Vec<DeletionWorkType> {
        work.iter()
            .filter(|entry| entry.status == wanted)
            .map(|entry| entry.work_type.clone())
            .collect()
    };
    let pending = types_with_status(DeletionWorkStatus::Pending);
    let failed = types_with_status(DeletionWorkStatus::FailedNoRetry);
    Ok(if !pending.is_empty() {
        DeletionProgress::InProgress { pending, failed }
    } else if !failed.is_empty() {
        DeletionProgress::Failed { failed }
    } else {
        DeletionProgress::Complete
    })
}

/// Mark a deletion work task as completed. Idempotent for an already-completed task; rejected
/// for a terminally failed task. Completing `CancelNotifications` is refused while any reminder
/// operation for the item is still pending or a cancel of any of the reminder's generations
/// has failed (see `unfinished_notification_operations`), and completing `RemoveAudio` releases
/// the audio reference held on the tombstoned capture.
pub fn mark_deletion_work_completed(
    db: &mut Database,
    deletion_work_id: &str,
    now: DateTime<Utc>,
) -> Result<DeletionWork> {
    let tx = db.immediate_transaction()?;

    let work = get_deletion_work_in_tx(&tx, deletion_work_id)
        .map_err(|e| anyhow!("Failed to load deletion work: {}", e))?
        .ok_or_else(|| anyhow!("Deletion work {} not found", deletion_work_id))?;

    match work.status {
        DeletionWorkStatus::Completed => return Ok(work),
        DeletionWorkStatus::FailedNoRetry => {
            return Err(anyhow!(
                "Cannot complete deletion work with terminal failure status"
            ))
        }
        DeletionWorkStatus::Pending => {}
    }

    match work.work_type {
        DeletionWorkType::CancelNotifications => {
            let unfinished_operations = unfinished_notification_operations(&tx, &work.item_id)?;
            if unfinished_operations > 0 {
                return Err(anyhow!(
                    "Cannot complete notification cleanup: {} reminder operation(s) pending or failed",
                    unfinished_operations
                ));
            }
        }
        DeletionWorkType::RemoveAudio => {
            tx.execute(
                "UPDATE captures SET audio_reference = NULL WHERE capture_id IN
                 (SELECT capture_id FROM items WHERE item_id = ?)",
                [&work.item_id],
            )?;
        }
        DeletionWorkType::ClearIngress => {}
    }

    tx.execute(
        "UPDATE deletion_work SET status = ?, attempted_at = ? WHERE deletion_work_id = ?",
        rusqlite::params![
            DeletionWorkStatus::Completed.as_str(),
            now.to_rfc3339(),
            deletion_work_id
        ],
    )?;
    tx.commit()?;

    Ok(DeletionWork {
        status: DeletionWorkStatus::Completed,
        attempted_at: Some(now),
        ..work
    })
}

/// Count the reminder operations that keep notification cleanup unfinished: every pending
/// operation, plus every failed cancel of any generation. Each generation has its own native
/// identifier, so a failed cancel means that generation's notification may still be on the
/// device whatever happened to later generations. Failed schedules installed nothing and do
/// not block.
fn unfinished_notification_operations(tx: &Transaction<'_>, item_id: &str) -> Result<usize> {
    let Some(reminder) =
        state::get_reminder(tx, item_id).map_err(|e| anyhow!("Failed to load reminder: {}", e))?
    else {
        return Ok(0);
    };
    let operations = state::list_operations(tx, &reminder.reminder_id)
        .map_err(|e| anyhow!("Failed to load reminder operations: {}", e))?;
    Ok(operations
        .iter()
        .filter(|operation| match operation.operation_state {
            state::OperationState::Pending => true,
            state::OperationState::Failed => {
                operation.operation_type == state::OperationType::Cancel
            }
            _ => false,
        })
        .count())
}

/// Re-arm every failed cancel of the item's reminder, from any generation, as pending so the
/// native layer retries removing each notification that may still be installed. Without this
/// an earlier generation's failed cancel would leave its notification on the device with no
/// durable effect left to remove it.
fn retry_failed_notification_cancels_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<usize> {
    Ok(tx.execute(
        "UPDATE reminder_operations SET operation_state = ?
         WHERE operation_type = ? AND operation_state = ? AND reminder_id IN
         (SELECT reminder_id FROM reminders WHERE item_id = ?)",
        rusqlite::params![
            state::OperationState::Pending.as_str(),
            state::OperationType::Cancel.as_str(),
            state::OperationState::Failed.as_str(),
            item_id,
        ],
    )?)
}

/// Record that a cleanup task terminally failed. Idempotent for an already-failed task; a
/// completed task cannot be failed. The item stays tombstoned and unreadable either way.
pub fn mark_deletion_work_failed(
    db: &mut Database,
    deletion_work_id: &str,
    now: DateTime<Utc>,
) -> Result<DeletionWork> {
    let tx = db.immediate_transaction()?;

    let work = get_deletion_work_in_tx(&tx, deletion_work_id)
        .map_err(|e| anyhow!("Failed to load deletion work: {}", e))?
        .ok_or_else(|| anyhow!("Deletion work {} not found", deletion_work_id))?;

    match work.status {
        DeletionWorkStatus::FailedNoRetry => return Ok(work),
        DeletionWorkStatus::Completed => {
            return Err(anyhow!("Cannot fail deletion work that already completed"))
        }
        DeletionWorkStatus::Pending => {}
    }

    tx.execute(
        "UPDATE deletion_work SET status = ?, attempted_at = ? WHERE deletion_work_id = ?",
        rusqlite::params![
            DeletionWorkStatus::FailedNoRetry.as_str(),
            now.to_rfc3339(),
            deletion_work_id
        ],
    )?;
    tx.commit()?;

    Ok(DeletionWork {
        status: DeletionWorkStatus::FailedNoRetry,
        attempted_at: Some(now),
        ..work
    })
}

/// Identities the native layer needs to perform a cleanup task.
pub fn cleanup_target(db: &Database, item_id: &str) -> Result<Option<CleanupTarget>> {
    Ok(db
        .conn()
        .query_row(
            "SELECT captures.capture_id, captures.audio_reference FROM captures
             JOIN items ON items.capture_id = captures.capture_id WHERE items.item_id = ?",
            [item_id],
            |row| {
                Ok(CleanupTarget {
                    capture_id: row.get(0)?,
                    audio_reference: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// List all pending deletion work for an item.
pub fn list_pending_deletion_work(db: &Database, item_id: &str) -> Result<Vec<DeletionWork>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT deletion_work_id, item_id, work_type, status, attempted_at, created_at
         FROM deletion_work WHERE item_id = ? AND status = ? ORDER BY deletion_work_id",
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

fn list_deletion_work_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<Vec<DeletionWork>> {
    let mut stmt = tx.prepare(
        "SELECT deletion_work_id, item_id, work_type, status, attempted_at, created_at
         FROM deletion_work WHERE item_id = ? ORDER BY deletion_work_id",
    )?;
    let work = stmt
        .query_map([item_id], deletion_work_from_row)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("Failed to query deletion work: {}", e))?;
    Ok(work)
}

/// Event ids share one unrestricted namespace, so a caller-chosen id could already occupy the
/// natural deletion id; probe for an unused one inside the fencing transaction.
fn unused_deletion_event_id(tx: &Transaction<'_>, item_id: &str, revision: i32) -> Result<String> {
    let base = format!("deletion:{}:{}", item_id, revision);
    let mut candidate = base.clone();
    let mut suffix = 1;
    loop {
        let taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?)",
            [&candidate],
            |row| row.get(0),
        )?;
        if !taken {
            return Ok(candidate);
        }
        candidate = format!("{}#{}", base, suffix);
        suffix += 1;
    }
}

/// Remove all readable content from an item: clear capture text, correction content, and other content-bearing fields.
/// This ensures deleted items cannot restore readable text after marking as deleted.
/// Uses empty strings/NULLs to satisfy schema constraints while removing readable content.
fn remove_readable_content_in_tx(tx: &Transaction<'_>, item_id: &str) -> Result<()> {
    // Clear capture text and session topic (set to empty string to satisfy NOT NULL constraints)
    tx.execute(
        "UPDATE captures SET text = '', session_topic = '' WHERE capture_id IN
         (SELECT capture_id FROM items WHERE item_id = ?)",
        rusqlite::params![item_id],
    )?;

    // Clear item session topic
    tx.execute(
        "UPDATE items SET current_session_topic = NULL WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    // Redact correction values in the events table. The event reader requires a non-null
    // new value for correction events, so redact to an empty string (like capture text) and
    // keep the non-content kind/revision metadata readable.
    tx.execute(
        "UPDATE events SET correction_new_value = '', correction_old_value = NULL
         WHERE item_id = ? AND event_type = 'correction'",
        rusqlite::params![item_id],
    )?;

    // Delete the authoritative corrections table rows
    tx.execute(
        "DELETE FROM corrections WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    // Delete proposal rows (suggestions, interpretation results)
    tx.execute(
        "DELETE FROM proposals WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    // Clear reminder source phrase (readable content in reminders)
    tx.execute(
        "UPDATE reminders SET source_phrase = NULL WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    // Delete reminder_commands rows (contains serialized ReminderRecord with source_phrase)
    tx.execute(
        "DELETE FROM reminder_commands WHERE item_id = ?",
        rusqlite::params![item_id],
    )?;

    Ok(())
}

/// Cancel all reminders for an item using the N01 API.
/// Uses cancel_for_inactive_item which properly sets the reminder state to cancelled and creates the necessary operations.
fn cancel_item_reminders_in_tx(
    tx: &Transaction<'_>,
    item_id: &str,
    now: &DateTime<Utc>,
) -> Result<()> {
    let clock = FixedClock { instant: *now };
    state::cancel_for_inactive_item(tx, &clock, item_id)
        .map_err(|e| anyhow!("Failed to cancel reminder: {}", e))?;
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
    let work_id = format!("deletion-work:{}:{}", item_id, work_type.as_str());
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
