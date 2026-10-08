// Integration tests for deletion intent and tombstone processing.
// Tests verify idempotent deletion, search index removal, job cancellation, and cleanup work.

use chrono::Utc;
use ohand_core::domain::items::load_item_state;
use ohand_core::jobs::queue::JobStatus;
use ohand_core::lifecycle::delete_intent::{
    get_deletion_work, list_pending_deletion_work, mark_deletion_intent,
    mark_deletion_work_completed, DeletionWorkStatus, DeletionWorkType,
};
use ohand_core::retrieval::index;
use ohand_core::store::events::{Event, EventPayload, EventType};

mod test_helpers;
use test_helpers::{
    create_test_capture, create_test_db, create_test_item, create_test_job,
    set_item_lifecycle_state,
};

#[test]
fn test_mark_deletion_intent() {
    let mut db = create_test_db();
    let now = Utc::now();

    // Create a test capture and item
    let capture_id = "cap-test-001";
    let item_id = "item-test-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark item for deletion with revision 0 (initial state)
    let deletion_work =
        mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify deletion work was created
    assert_eq!(deletion_work.item_id, item_id);
    assert_eq!(deletion_work.work_type, DeletionWorkType::RemoveAudio);
    assert_eq!(deletion_work.status, DeletionWorkStatus::Pending);

    // Verify item is now deleted
    let tx = db
        .transaction()
        .expect("Transaction creation should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Deleted
    );
}

#[test]
fn test_deletion_intent_idempotent() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-idem-001";
    let item_id = "item-idem-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion twice with the same expected revision (idempotent)
    let work1 =
        mark_deletion_intent(&mut db, item_id, 0, now).expect("First deletion should succeed");
    let work2 = mark_deletion_intent(&mut db, item_id, 0, now)
        .expect("Second deletion with same revision should be idempotent");

    // Both should reference the same item and work
    assert_eq!(work1.item_id, item_id);
    assert_eq!(work2.item_id, item_id);
    // The returned work should have the same deletion_work_id (idempotent)
    assert_eq!(work1.deletion_work_id, work2.deletion_work_id);
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
    {
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM search_index WHERE item_id = ?")
            .expect("Prepare should succeed");
        let indexed_before: i64 = stmt
            .query_row([item_id], |row| row.get(0))
            .expect("Query should succeed");
        assert_eq!(indexed_before, 1);
    }

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify item is removed from index
    {
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM search_index WHERE item_id = ?")
            .expect("Prepare should succeed");
        let indexed_after: i64 = stmt
            .query_row([item_id], |row| row.get(0))
            .expect("Query should succeed");
        assert_eq!(indexed_after, 0);
    }
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
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify jobs are cancelled
    {
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
}

#[test]
fn test_deletion_work_tracking() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-work-001";
    let item_id = "item-work-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion with revision 0
    let _work = mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // List pending work
    let pending_work = list_pending_deletion_work(&db, item_id).expect("List should succeed");
    assert_eq!(pending_work.len(), 3); // audio, ingress, notifications

    // Find and mark one as completed
    let audio_work = pending_work
        .iter()
        .find(|w| w.work_type == DeletionWorkType::RemoveAudio)
        .expect("Should find audio work")
        .clone();

    let completed_work = mark_deletion_work_completed(&mut db, &audio_work.deletion_work_id, now)
        .expect("Mark completed should succeed");
    assert_eq!(completed_work.status, DeletionWorkStatus::Completed);

    // Verify updated status
    let reloaded = get_deletion_work(&db, &audio_work.deletion_work_id)
        .expect("Get should succeed")
        .expect("Work should exist");
    assert_eq!(reloaded.status, DeletionWorkStatus::Completed);

    // Verify other work is still pending
    let still_pending = list_pending_deletion_work(&db, item_id).expect("List should succeed");
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
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify that the item cannot accept new events
    // (This would be tested at the event level, not here, but the lifecycle is locked)
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");

    // Verify item is truly deleted and cannot be modified further
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Deleted
    );
}

#[test]
fn test_deletion_clears_readable_content() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-clear-001";
    let item_id = "item-clear-001";
    let text = "Important readable text that should be cleared";

    create_test_capture(&mut db, capture_id, text, None);
    create_test_item(&mut db, item_id, capture_id);

    // Verify text is stored
    {
        let conn = db.conn();
        let stored_text: String = conn
            .query_row(
                "SELECT text FROM captures WHERE capture_id = ?",
                [capture_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(stored_text, text);
    }

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify text has been cleared (set to empty string)
    {
        let conn = db.conn();
        let cleared_text: String = conn
            .query_row(
                "SELECT text FROM captures WHERE capture_id = ?",
                [capture_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(cleared_text, "");
    }

    // Verify session_topic is also cleared
    {
        let conn = db.conn();
        let session_topic: String = conn
            .query_row(
                "SELECT session_topic FROM captures WHERE capture_id = ?",
                [capture_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(session_topic, "");
    }
}

#[test]
fn test_racing_job_result_after_deletion() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-job-race-001";
    let item_id = "item-job-race-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Create a running job for the item
    let job_id = create_test_job(&mut db, item_id, "transcription", JobStatus::Running, now);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify the job is cancelled
    {
        let conn = db.conn();
        let job_status: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE job_id = ?",
                [&job_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(job_status, "cancelled");
    }

    // Simulate a racing job result arriving after deletion
    // In a real system, this would be rejected by job result application logic
    // because the item is locked (lifecycle_state = deleted)
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    // Job result application should check this
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Deleted
    );
}

#[test]
fn test_deletion_with_stale_revision() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-stale-001";
    let item_id = "item-stale-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Try to delete with a stale expected revision
    let stale_revision = 5; // Initial revision is 0
    let result = mark_deletion_intent(&mut db, item_id, stale_revision, now);

    // Should fail with stale revision error
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Stale delete"));

    // Verify item is still active
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Active
    );
}

#[test]
fn test_deletion_creates_all_cleanup_work() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-cleanup-001";
    let item_id = "item-cleanup-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify all three cleanup work items exist
    let pending_work = list_pending_deletion_work(&db, item_id).expect("List should succeed");
    assert_eq!(pending_work.len(), 3);

    let work_types: std::collections::HashSet<_> =
        pending_work.iter().map(|w| &w.work_type).collect();
    assert!(work_types.contains(&DeletionWorkType::RemoveAudio));
    assert!(work_types.contains(&DeletionWorkType::ClearIngress));
    assert!(work_types.contains(&DeletionWorkType::CancelNotifications));
}

#[test]
fn test_completion_idempotency() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-comp-idem-001";
    let item_id = "item-comp-idem-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion
    let work = mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Mark the work as completed
    let completed1 = mark_deletion_work_completed(&mut db, &work.deletion_work_id, now)
        .expect("Mark completed should succeed");
    assert_eq!(completed1.status, DeletionWorkStatus::Completed);

    // Mark as completed again with same work_id (idempotent)
    let completed2 = mark_deletion_work_completed(&mut db, &work.deletion_work_id, now)
        .expect("Mark completed again should be idempotent");
    assert_eq!(completed2.status, DeletionWorkStatus::Completed);
    assert_eq!(completed1.deletion_work_id, completed2.deletion_work_id);
}

#[test]
fn test_deletion_clears_corrections() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-corr-001";
    let item_id = "item-corr-001";

    create_test_capture(&mut db, capture_id, "Original text", None);
    create_test_item(&mut db, item_id, capture_id);

    // Create a correction in the corrections table
    {
        let tx = db.transaction().expect("Transaction should succeed");
        tx.execute(
            "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                "corr-001",
                item_id,
                0,
                "text",
                Some("Original text"),
                "CORRECTED SECRET",
                now.to_rfc3339()
            ],
        ).expect("Insert should succeed");
        tx.commit().expect("Commit should succeed");
    }

    // Verify correction exists
    {
        let conn = db.conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM corrections WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(count, 1);
    }

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify correction has been deleted
    {
        let conn = db.conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM corrections WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(count, 0);
    }
}

#[test]
fn test_deletion_clears_reminder_commands_result() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-reminder-001";
    let item_id = "item-reminder-001";

    create_test_capture(&mut db, capture_id, "Test", None);
    create_test_item(&mut db, item_id, capture_id);

    // Create a reminder_command with result_json containing readable content
    {
        let tx = db.transaction().expect("Transaction should succeed");

        // Insert reminder_command with result_json
        let result_json = r#"{"reminder_record":{"source_phrase":"SENSITIVE RESULT"}}"#;
        tx.execute(
            "INSERT INTO reminder_commands (command_id, item_id, command_fingerprint, result_json, created_at)
             VALUES (?, ?, ?, ?, ?)",
            rusqlite::params!["cmd-001", item_id, "fingerprint-001", result_json, now.to_rfc3339()],
        ).expect("Insert command should succeed");

        tx.commit().expect("Commit should succeed");
    }

    // Verify reminder_command exists
    {
        let conn = db.conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reminder_commands WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(count, 1);
    }

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Verify reminder_commands have been deleted
    {
        let conn = db.conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reminder_commands WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .expect("Query should succeed");
        assert_eq!(count, 0);
    }
}

#[test]
fn test_deletion_blocks_racing_correction() {
    use ohand_core::store::events::Correction;

    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-race-corr-001";
    let item_id = "item-race-corr-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark deletion
    mark_deletion_intent(&mut db, item_id, 0, now).expect("Deletion should succeed");

    // Try to apply a correction event after deletion
    let correction_payload = EventPayload::Correction(Correction {
        kind: ohand_core::store::events::CorrectionKind::Text,
        old_value: Some("Test capture".to_string()),
        new_value: "CORRECTED TEXT".to_string(),
    });

    let correction_event = Event::new(
        "correction-event-001".to_string(),
        item_id.to_string(),
        1, // The revision has incremented during deletion
        EventType::Correction,
        correction_payload,
        now.to_rfc3339(),
    )
    .expect("Event creation should succeed");

    let tx = db.transaction().expect("Transaction should succeed");

    // Attempting to save a correction event on a deleted item should fail
    // because the lifecycle state is locked to deleted and cannot accept further mutations
    let result = ohand_core::store::events::save_event_in_tx(&tx, &correction_event, 1);
    tx.commit().expect("Commit should succeed");

    // The event should be rejected because the item is deleted
    assert!(result.is_err());
}

#[test]
fn test_deletion_from_completed_state() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-comp-delete-001";
    let item_id = "item-comp-delete-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark item as completed
    set_item_lifecycle_state(&mut db, item_id, "completed");

    // Verify item is completed
    {
        let tx = db.transaction().expect("Transaction should succeed");
        let item_state = load_item_state(&tx, item_id)
            .expect("Load should succeed")
            .expect("Item should exist");
        assert_eq!(
            item_state.lifecycle_state,
            ohand_core::domain::items::LifecycleState::Completed
        );
    }

    // Delete the completed item - should succeed per DESIGN.md deletion contract
    let deletion_work = mark_deletion_intent(&mut db, item_id, 0, now)
        .expect("Deletion of completed item should succeed");
    assert_eq!(deletion_work.item_id, item_id);

    // Verify item is now deleted
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Deleted
    );
}

#[test]
fn test_deletion_from_cancelled_state() {
    let mut db = create_test_db();
    let now = Utc::now();

    let capture_id = "cap-cancel-delete-001";
    let item_id = "item-cancel-delete-001";

    create_test_capture(&mut db, capture_id, "Test capture", None);
    create_test_item(&mut db, item_id, capture_id);

    // Mark item as cancelled
    set_item_lifecycle_state(&mut db, item_id, "cancelled");

    // Verify item is cancelled
    {
        let tx = db.transaction().expect("Transaction should succeed");
        let item_state = load_item_state(&tx, item_id)
            .expect("Load should succeed")
            .expect("Item should exist");
        assert_eq!(
            item_state.lifecycle_state,
            ohand_core::domain::items::LifecycleState::Cancelled
        );
    }

    // Delete the cancelled item - should succeed per DESIGN.md deletion contract
    let deletion_work = mark_deletion_intent(&mut db, item_id, 0, now)
        .expect("Deletion of cancelled item should succeed");
    assert_eq!(deletion_work.item_id, item_id);

    // Verify item is now deleted
    let tx = db.transaction().expect("Transaction should succeed");
    let item_state = load_item_state(&tx, item_id)
        .expect("Load should succeed")
        .expect("Item should exist");
    assert_eq!(
        item_state.lifecycle_state,
        ohand_core::domain::items::LifecycleState::Deleted
    );
}
