// Integration tests for deletion intent and tombstone processing.
// Tests verify idempotent deletion, search index removal, job cancellation, and cleanup work.

use oh_and_core::domain::items::load_item_state;
use oh_and_core::jobs::queue::JobStatus;
use oh_and_core::lifecycle::delete_intent::{
    mark_deletion_intent, mark_deletion_work_completed, list_pending_deletion_work,
    DeletionWorkStatus, DeletionWorkType, get_deletion_work,
};
use oh_and_core::retrieval::index;
use oh_and_core::store::schema::{Database, SystemClock};
use oh_and_core::store::events::save_event;
use oh_and_core::store::captures::save_capture;
use std::sync::Arc;
use chrono::Utc;

mod test_helpers;
use test_helpers::{create_test_db, create_test_capture, create_test_item, create_test_job};

#[test]
fn test_mark_deletion_intent() {
    let mut db = create_test_db();
    let now = Utc::now();

    // Create a test capture and item
    let capture_id = "cap-test-001";
    let item_id = "item-test-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark item for deletion
    let deletion_work = mark_deletion_intent(&mut db, item_id, now).expect("Deletion should succeed");

    // Verify deletion work was created
    assert_eq!(deletion_work.item_id, item_id);
    assert_eq!(deletion_work.work_type, DeletionWorkType::RemoveAudio);
    assert_eq!(deletion_work.status, DeletionWorkStatus::Pending);

    // Verify item is now deleted
    let tx = db.transaction().expect("Transaction creation should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    assert_eq!(item_state.lifecycle_state, oh_and_core::domain::items::LifecycleState::Deleted);
}

#[test]
fn test_deletion_intent_idempotent() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-idem-001";
    let item_id = "item-idem-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion twice
    let work1 = mark_deletion_intent(&mut db, item_id, now).expect("First deletion should succeed");
    let work2 = mark_deletion_intent(&mut db, item_id, now).expect("Second deletion should also succeed");

    // Both should reference the same item
    assert_eq!(work1.item_id, work2.item_id);
    assert_eq!(work1.item_id, item_id);
}

#[test]
fn test_deletion_removes_from_index() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-index-001";
    let item_id = "item-index-001";

    create_test_capture(&mut db, capture_id, "Searchable text", None);
    create_test_item(&mut db, item_id, capture_id);

    // Sync item to search index
    let tx = db.transaction().expect("Transaction should succeed");
    index::sync_item_in_tx(&tx, item_id).expect("Index sync should succeed");
    tx.commit().expect("Commit should succeed");

    // Verify item is in index by doing a quick search
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM search_index WHERE item_id = ?")
        .expect("Prepare should succeed");
    let indexed_before: i64 = stmt
        .query_row([item_id], |row| row.get(0))
        .expect("Query should succeed");
    assert_eq!(indexed_before, 1);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, now).expect("Deletion should succeed");

    // Verify item is removed from index
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM search_index WHERE item_id = ?")
        .expect("Prepare should succeed");
    let indexed_after: i64 = stmt
        .query_row([item_id], |row| row.get(0))
        .expect("Query should succeed");
    assert_eq!(indexed_after, 0);
}

#[test]
fn test_deletion_cancels_jobs() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-jobs-001";
    let item_id = "item-jobs-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Create pending jobs for the item
    let job1_id = create_test_job(&mut db, item_id, "transcription", JobStatus::Queued, now);
    let job2_id = create_test_job(&mut db, item_id, "interpretation", JobStatus::Running, now);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, now).expect("Deletion should succeed");

    // Verify jobs are cancelled
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT status FROM jobs WHERE job_id = ?")
        .expect("Prepare should succeed");

    let job1_status: String = stmt
        .query_row([&job1_id], |row| row.get(0))
        .expect("Query should succeed");
    assert_eq!(job1_status, "cancelled");

    let job2_status: String = stmt
        .query_row([&job2_id], |row| row.get(0))
        .expect("Query should succeed");
    assert_eq!(job2_status, "cancelled");
}

#[test]
fn test_deletion_work_tracking() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-work-001";
    let item_id = "item-work-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion
    let work = mark_deletion_intent(&mut db, item_id, now).expect("Deletion should succeed");

    // List pending work
    let pending_work = list_pending_deletion_work(&db, item_id)
        .expect("List should succeed");
    assert_eq!(pending_work.len(), 3); // audio, ingress, notifications

    // Find and mark one as completed
    let audio_work = pending_work
        .iter()
        .find(|w| w.work_type == DeletionWorkType::RemoveAudio)
        .expect("Should find audio work");

    let completed_work = mark_deletion_work_completed(&mut db, &audio_work.deletion_work_id)
        .expect("Mark completed should succeed");
    assert_eq!(completed_work.status, DeletionWorkStatus::Completed);

    // Verify updated status
    let reloaded = get_deletion_work(&db, &audio_work.deletion_work_id)
        .expect("Get should succeed")
        .expect("Work should exist");
    assert_eq!(reloaded.status, DeletionWorkStatus::Completed);

    // Verify other work is still pending
    let still_pending = list_pending_deletion_work(&db, item_id)
        .expect("List should succeed");
    assert_eq!(still_pending.len(), 2); // 3 - 1 completed
}

#[test]
fn test_deleted_item_readonly() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-ro-001";
    let item_id = "item-ro-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, now).expect("Deletion should succeed");

    // Verify that the item cannot accept new events
    // (This would be tested at the event level, not here, but the lifecycle is locked)
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");

    // Verify item is truly deleted and cannot be modified further
    assert_eq!(item_state.lifecycle_state, oh_and_core::domain::items::LifecycleState::Deleted);
}
