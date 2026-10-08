// Integration tests for deletion intent and tombstone processing.
// They drive the production write paths (events, jobs, proposals, captures, index, reminders)
// against a real SQLite file, including close/reopen to model a crash.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::items::{apply_proposal, load_item_state, LifecycleState};
use ohand_core::jobs::queue::{
    claim_job_with_lease, complete_job, enqueue_job, get_job, JobStatus,
};
use ohand_core::lifecycle::delete_intent::{
    cleanup_target, deletion_progress, get_deletion_work, list_pending_deletion_work,
    mark_deletion_intent, mark_deletion_work_completed, mark_deletion_work_failed, DeletionIntent,
    DeletionProgress, DeletionWork, DeletionWorkStatus, DeletionWorkType,
};
use ohand_core::reminders::state::{
    apply_derived_request, get_reminder, list_operations, DerivedReminderRequest, OperationState,
    OperationType, RequestState,
};
use ohand_core::retrieval::index;
use ohand_core::store::captures::save_capture;
use ohand_core::store::events::{
    save_event, save_event_in_tx, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use ohand_core::store::schema::{Clock, Database};

mod test_helpers;
use test_helpers::{
    build_capture, count_rows, create_queued_job, create_test_capture, create_test_db,
    create_test_item, fixed_instant, set_item_lifecycle_state, TestDb,
};

struct FixedClock {
    instant: DateTime<Utc>,
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

const SECRET: &str = "SECRET-READABLE-TEXT";

fn item_lifecycle(db: &mut Database, item_id: &str) -> LifecycleState {
    let tx = db.transaction().expect("transaction");
    load_item_state(&tx, item_id)
        .expect("load")
        .expect("item exists")
        .lifecycle_state
}

fn work_of_type(intent: &DeletionIntent, work_type: DeletionWorkType) -> DeletionWork {
    intent
        .work
        .iter()
        .find(|work| work.work_type == work_type)
        .unwrap_or_else(|| panic!("intent should contain {work_type:?}"))
        .clone()
}

fn correction_event(
    event_id: &str,
    item_id: &str,
    revision: i32,
    old_value: Option<&str>,
    new_value: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: old_value.map(str::to_string),
            new_value: new_value.to_string(),
        }),
        fixed_instant(2).to_rfc3339(),
    )
    .expect("event")
}

fn type_as_action(db: &mut Database, item_id: &str) {
    let event = Event::new(
        format!("evt-type-{item_id}"),
        item_id.to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        fixed_instant(1).to_rfc3339(),
    )
    .expect("event");
    save_event(db, &event, 0).expect("type correction");
}

fn insert_proposal(db: &mut Database, proposal_id: &str, item_id: &str, source_revision: i32) {
    let capture_id: String = db
        .conn()
        .query_row(
            "SELECT capture_id FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .expect("capture id");
    db.conn()
        .execute(
            "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version,
                text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal,
                session_topic_proposal, source_spans, abstained, request_version, created_at)
             VALUES (?, ?, ?, ?, 1, 'capture', NULL, 'unapplied', 'action', NULL, ?, NULL, 0, NULL, ?)",
            rusqlite::params![
                proposal_id,
                item_id,
                capture_id,
                source_revision,
                SECRET,
                fixed_instant(1).to_rfc3339()
            ],
        )
        .expect("proposal insert");
}

fn index_item(db: &mut Database, item_id: &str) {
    let tx = db.transaction().expect("transaction");
    index::sync_item_in_tx(&tx, item_id).expect("index sync");
    tx.commit().expect("commit");
}

fn indexed_rows(db: &Database, item_id: &str) -> i64 {
    count_rows(
        db,
        "SELECT COUNT(*) FROM search_index WHERE item_id = ?",
        item_id,
    )
}

fn captured_text(db: &Database, capture_id: &str) -> String {
    db.conn()
        .query_row(
            "SELECT text FROM captures WHERE capture_id = ?",
            [capture_id],
            |row| row.get(0),
        )
        .expect("capture text")
}

fn delete_fresh_item(db: &mut TestDb, suffix: &str, audio: Option<&str>) -> DeletionIntent {
    let capture_id = format!("cap-{suffix}");
    let item_id = format!("item-{suffix}");
    create_test_capture(db, &capture_id, "Test capture", audio);
    create_test_item(db, &item_id, &capture_id);
    mark_deletion_intent(db, &item_id, 0, fixed_instant(5)).expect("deletion")
}

fn work_types(intent: &DeletionIntent) -> Vec<DeletionWorkType> {
    let mut types: Vec<_> = intent
        .work
        .iter()
        .map(|work| work.work_type.clone())
        .collect();
    types.sort_by_key(|work_type| work_type.as_str());
    types
}

// ---------------------------------------------------------------------------
// Atomic tombstone and cleanup enqueue
// ---------------------------------------------------------------------------

#[test]
fn deletion_tombstones_item_and_returns_progress_handle() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "handle", Some("audio/handle.m4a"));

    assert_eq!(intent.item_id, "item-handle");
    assert_eq!(intent.deletion_revision, 0);
    assert_eq!(
        work_types(&intent),
        vec![
            DeletionWorkType::CancelNotifications,
            DeletionWorkType::ClearIngress,
            DeletionWorkType::RemoveAudio
        ]
    );
    assert!(intent
        .work
        .iter()
        .all(|work| work.status == DeletionWorkStatus::Pending));
    assert_eq!(
        item_lifecycle(&mut db, "item-handle"),
        LifecycleState::Deleted
    );
    assert_eq!(
        deletion_progress(&mut db, "item-handle").unwrap(),
        DeletionProgress::InProgress {
            pending: vec![
                DeletionWorkType::CancelNotifications,
                DeletionWorkType::ClearIngress,
                DeletionWorkType::RemoveAudio
            ],
            failed: vec![]
        }
    );
}

#[test]
fn text_only_capture_enqueues_no_audio_cleanup() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "textonly", None);
    assert_eq!(
        work_types(&intent),
        vec![
            DeletionWorkType::CancelNotifications,
            DeletionWorkType::ClearIngress
        ]
    );
}

#[test]
fn work_ids_are_deterministic_from_item_and_type() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "ids", Some("audio/ids.m4a"));
    let audio = work_of_type(&intent, DeletionWorkType::RemoveAudio);
    assert_eq!(
        audio.deletion_work_id,
        "deletion-work:item-ids:remove_audio"
    );
    assert_eq!(audio.created_at, fixed_instant(5));
}

#[test]
fn deletion_removes_item_from_index() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-index", "Searchable text", None);
    create_test_item(&mut db, "item-index", "cap-index");
    index_item(&mut db, "item-index");
    assert_eq!(indexed_rows(&db, "item-index"), 1);

    mark_deletion_intent(&mut db, "item-index", 0, fixed_instant(5)).expect("deletion");
    assert_eq!(indexed_rows(&db, "item-index"), 0);
}

#[test]
fn deletion_cancels_queued_and_running_jobs() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-jobs", "Test", None);
    create_test_item(&mut db, "item-jobs", "cap-jobs");
    let queued = create_queued_job(&mut db, "item-jobs", "interpretation");
    let running = create_queued_job(&mut db, "item-jobs", "transcription");
    db.conn()
        .execute(
            "UPDATE jobs SET status = 'running' WHERE job_id = ?",
            [&running],
        )
        .unwrap();

    mark_deletion_intent(&mut db, "item-jobs", 0, fixed_instant(5)).expect("deletion");

    for job_id in [queued, running] {
        let job = get_job(&db, &job_id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }
}

#[test]
fn deletion_works_from_completed_and_cancelled_items() {
    for (state, suffix) in [("completed", "done"), ("cancelled", "cancel")] {
        let mut db = create_test_db();
        create_test_capture(&mut db, &format!("cap-{suffix}"), "Test capture", None);
        create_test_item(&mut db, &format!("item-{suffix}"), &format!("cap-{suffix}"));
        set_item_lifecycle_state(&mut db, &format!("item-{suffix}"), state);

        mark_deletion_intent(&mut db, &format!("item-{suffix}"), 0, fixed_instant(5))
            .unwrap_or_else(|error| panic!("{state} item should be deletable: {error}"));
        assert_eq!(
            item_lifecycle(&mut db, &format!("item-{suffix}")),
            LifecycleState::Deleted
        );
    }
}

#[test]
fn deletion_event_id_survives_a_colliding_caller_chosen_event_id() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-collide", "Test capture", None);
    create_test_item(&mut db, "item-collide", "cap-collide");
    type_as_action(&mut db, "item-collide");

    // A valid completion event that happens to use the natural deletion id for revision 2.
    let completion = Event::new(
        "deletion:item-collide:2".to_string(),
        "item-collide".to_string(),
        1,
        EventType::Completion,
        EventPayload::Completion,
        fixed_instant(3).to_rfc3339(),
    )
    .unwrap();
    save_event(&mut db, &completion, 1).expect("completion");

    mark_deletion_intent(&mut db, "item-collide", 2, fixed_instant(5))
        .expect("deletion must not be blocked by an unrelated event id");
    let deletion_event_id: String = db
        .conn()
        .query_row(
            "SELECT event_id FROM events WHERE item_id = 'item-collide' AND event_type = 'deletion'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(deletion_event_id, "deletion:item-collide:2#1");
    assert_eq!(
        item_lifecycle(&mut db, "item-collide"),
        LifecycleState::Deleted
    );
}

// ---------------------------------------------------------------------------
// Expected revision, duplicate and bypass handling
// ---------------------------------------------------------------------------

#[test]
fn stale_delete_after_newer_correction_reports_conflict() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-stale", "Original", None);
    create_test_item(&mut db, "item-stale", "cap-stale");
    save_event(
        &mut db,
        &correction_event("evt-fix", "item-stale", 0, Some("Original"), "Fixed"),
        0,
    )
    .unwrap();

    let error = mark_deletion_intent(&mut db, "item-stale", 0, fixed_instant(5)).unwrap_err();
    assert!(error.to_string().contains("Stale delete"), "{error}");
    assert_eq!(
        item_lifecycle(&mut db, "item-stale"),
        LifecycleState::Active
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM deletion_work WHERE item_id = ?",
            "item-stale"
        ),
        0
    );
}

#[test]
fn duplicate_delete_with_original_revision_returns_the_existing_intent() {
    let mut db = create_test_db();
    let first = delete_fresh_item(&mut db, "dup", Some("audio/dup.m4a"));

    let second = mark_deletion_intent(&mut db, "item-dup", 0, fixed_instant(30))
        .expect("retry of the committed command is idempotent");

    assert_eq!(second.deletion_revision, first.deletion_revision);
    let first_ids: Vec<_> = first.work.iter().map(|w| &w.deletion_work_id).collect();
    let second_ids: Vec<_> = second.work.iter().map(|w| &w.deletion_work_id).collect();
    assert_eq!(first_ids, second_ids);
    assert_eq!(second.work[0].created_at, fixed_instant(5));
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM deletion_work WHERE item_id = ?",
            "item-dup"
        ),
        3
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM events WHERE item_id = ? AND event_type = 'deletion'",
            "item-dup"
        ),
        1
    );
}

#[test]
fn duplicate_delete_with_a_different_revision_is_a_conflict() {
    let mut db = create_test_db();
    delete_fresh_item(&mut db, "dupconflict", None);

    for wrong_revision in [1, 2, 99] {
        let error = mark_deletion_intent(
            &mut db,
            "item-dupconflict",
            wrong_revision,
            fixed_instant(6),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Stale delete"), "{error}");
    }
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM deletion_work WHERE item_id = ?",
            "item-dupconflict"
        ),
        2
    );
}

#[test]
fn public_event_writers_cannot_set_the_tombstone() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-bypass", SECRET, None);
    create_test_item(&mut db, "item-bypass", "cap-bypass");

    let bypass = Event::new(
        "evt-bypass".to_string(),
        "item-bypass".to_string(),
        0,
        EventType::Deletion,
        EventPayload::Deletion,
        fixed_instant(2).to_rfc3339(),
    )
    .unwrap();

    assert!(save_event(&mut db, &bypass, 0).is_err());
    let tx = db.transaction().unwrap();
    assert!(save_event_in_tx(&tx, &bypass, 0).is_err());
    tx.commit().unwrap();

    assert_eq!(
        item_lifecycle(&mut db, "item-bypass"),
        LifecycleState::Active
    );
    assert_eq!(captured_text(&db, "cap-bypass"), SECRET);
}

// ---------------------------------------------------------------------------
// Readable content removal
// ---------------------------------------------------------------------------

fn secret_hits(db: &Database) -> i64 {
    let like = format!("%{SECRET}%");
    let queries = [
        "SELECT COUNT(*) FROM captures WHERE text LIKE ?1 OR session_topic LIKE ?1",
        "SELECT COUNT(*) FROM items WHERE current_session_topic LIKE ?1",
        "SELECT COUNT(*) FROM events WHERE correction_new_value LIKE ?1 OR correction_old_value LIKE ?1",
        "SELECT COUNT(*) FROM corrections WHERE new_value LIKE ?1 OR old_value LIKE ?1",
        "SELECT COUNT(*) FROM proposals WHERE session_topic_proposal LIKE ?1",
        "SELECT COUNT(*) FROM reminders WHERE source_phrase LIKE ?1",
        "SELECT COUNT(*) FROM reminder_commands WHERE result_json LIKE ?1",
        "SELECT COUNT(*) FROM search_index WHERE original_text LIKE ?1 OR current_text LIKE ?1",
    ];
    queries
        .iter()
        .map(|sql| {
            db.conn()
                .query_row(sql, [&like], |row| row.get::<_, i64>(0))
                .unwrap_or_else(|error| panic!("{sql}: {error}"))
        })
        .sum()
}

#[test]
fn deletion_erases_every_content_bearing_row() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-erase", SECRET, None);
    create_test_item(&mut db, "item-erase", "cap-erase");
    db.conn()
        .execute(
            "UPDATE captures SET session_topic = ? WHERE capture_id = 'cap-erase'",
            [SECRET],
        )
        .unwrap();
    db.conn()
        .execute(
            "UPDATE items SET current_session_topic = ? WHERE item_id = 'item-erase'",
            [SECRET],
        )
        .unwrap();
    save_event(
        &mut db,
        &correction_event("evt-secret", "item-erase", 0, Some(SECRET), "CORRECTED"),
        0,
    )
    .unwrap();
    insert_proposal(&mut db, "prop-secret", "item-erase", 1);
    db.conn()
        .execute(
            "INSERT INTO reminder_commands (command_id, item_id, command_fingerprint, result_json, created_at)
             VALUES ('cmd-1', 'item-erase', 'fp-1', ?, ?)",
            rusqlite::params![format!(r#"{{"source_phrase":"{SECRET}"}}"#), fixed_instant(1).to_rfc3339()],
        )
        .unwrap();
    index_item(&mut db, "item-erase");

    assert!(
        secret_hits(&db) > 0,
        "fixture must start with readable content"
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM corrections WHERE item_id = ?",
            "item-erase"
        ),
        1
    );

    mark_deletion_intent(&mut db, "item-erase", 1, fixed_instant(5)).expect("deletion");

    assert_eq!(secret_hits(&db), 0, "no readable content may remain");
    assert_eq!(captured_text(&db, "cap-erase"), "");
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM corrections WHERE item_id = ?",
            "item-erase"
        ),
        0
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM proposals WHERE item_id = ?",
            "item-erase"
        ),
        0
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM reminder_commands WHERE item_id = ?",
            "item-erase"
        ),
        0
    );
    // The same state is visible after a restart.
    db.reopen();
    assert_eq!(secret_hits(&db), 0);
}

// ---------------------------------------------------------------------------
// Reminder cancellation through the N01 state machine
// ---------------------------------------------------------------------------

fn create_item_with_reminder(db: &mut TestDb, suffix: &str) -> String {
    let item_id = format!("item-{suffix}");
    create_test_capture(
        db,
        &format!("cap-{suffix}"),
        "call the roofer 2026-01-16 09:00:00",
        None,
    );
    create_test_item(db, &item_id, &format!("cap-{suffix}"));
    type_as_action(db, &item_id);
    let clock = FixedClock {
        instant: fixed_instant(0),
    };
    let tx = db.immediate_transaction().unwrap();
    apply_derived_request(
        &tx,
        &clock,
        &DerivedReminderRequest {
            item_id: item_id.clone(),
            source_revision: 1,
            phrase: "2026-01-16 09:00:00".to_string(),
        },
    )
    .expect("reminder");
    tx.commit().unwrap();
    item_id
}

#[test]
fn deletion_cancels_the_reminder_and_gates_notification_cleanup() {
    let mut db = create_test_db();
    let item_id = create_item_with_reminder(&mut db, "remind");

    let intent = mark_deletion_intent(&mut db, &item_id, 1, fixed_instant(5)).expect("deletion");

    let tx = db.transaction().unwrap();
    let reminder = get_reminder(&tx, &item_id)
        .expect("reminder state stays decodable")
        .expect("reminder exists");
    assert_eq!(reminder.request_state, RequestState::Cancelled);
    assert_eq!(reminder.source_phrase, None);
    let operations = list_operations(&tx, &reminder.reminder_id).unwrap();
    assert!(operations
        .iter()
        .any(|op| op.operation_type == OperationType::Cancel
            && op.operation_state == OperationState::Pending));
    drop(tx);

    let notifications = work_of_type(&intent, DeletionWorkType::CancelNotifications);
    let blocked =
        mark_deletion_work_completed(&mut db, &notifications.deletion_work_id, fixed_instant(6));
    assert!(blocked.is_err(), "pending native cancel blocks completion");
    assert_eq!(
        get_deletion_work(&db, &notifications.deletion_work_id)
            .unwrap()
            .unwrap()
            .status,
        DeletionWorkStatus::Pending
    );

    db.conn()
        .execute(
            "UPDATE reminder_operations SET operation_state = 'acknowledged'
             WHERE operation_state = 'pending'",
            [],
        )
        .unwrap();
    let completed =
        mark_deletion_work_completed(&mut db, &notifications.deletion_work_id, fixed_instant(7))
            .expect("completes once the native cancel is acknowledged");
    assert_eq!(completed.status, DeletionWorkStatus::Completed);
}

#[test]
fn failed_native_cancel_never_completes_notification_cleanup() {
    let mut db = create_test_db();
    let item_id = create_item_with_reminder(&mut db, "remind-failed");

    let intent = mark_deletion_intent(&mut db, &item_id, 1, fixed_instant(5)).expect("deletion");
    let notifications = work_of_type(&intent, DeletionWorkType::CancelNotifications);

    db.conn()
        .execute(
            "UPDATE reminder_operations SET operation_state = 'failed'
             WHERE operation_type = 'cancel' AND operation_state = 'pending'",
            [],
        )
        .unwrap();

    let blocked =
        mark_deletion_work_completed(&mut db, &notifications.deletion_work_id, fixed_instant(6));
    assert!(blocked.is_err(), "a failed native cancel blocks completion");
    assert_eq!(
        get_deletion_work(&db, &notifications.deletion_work_id)
            .unwrap()
            .unwrap()
            .status,
        DeletionWorkStatus::Pending
    );

    for work in &intent.work {
        if work.work_type != DeletionWorkType::CancelNotifications {
            mark_deletion_work_completed(&mut db, &work.deletion_work_id, fixed_instant(7))
                .expect("other cleanup completes");
        }
    }
    assert!(
        !matches!(
            deletion_progress(&mut db, &item_id).unwrap(),
            DeletionProgress::Complete
        ),
        "deletion is never complete while the native cancel has failed"
    );
}

// ---------------------------------------------------------------------------
// Racing and replayed writes cannot restore readable text
// ---------------------------------------------------------------------------

#[test]
fn late_job_result_is_rejected_after_deletion() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-late", SECRET, None);
    create_test_item(&mut db, "item-late", "cap-late");
    let job_id = create_queued_job(&mut db, "item-late", "interpretation");
    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), fixed_instant(2))
        .unwrap()
        .expect("job claimed by a worker");
    assert_eq!(claimed.job_id, job_id);

    mark_deletion_intent(&mut db, "item-late", 0, fixed_instant(5)).expect("deletion");

    let result = complete_job(&mut db, &job_id, claimed.attempt_count);
    assert!(result.is_err(), "the worker's late result must be rejected");
    assert_eq!(
        get_job(&db, &job_id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
    assert_eq!(secret_hits(&db), 0);
}

#[test]
fn job_enqueued_after_deletion_is_never_run() {
    let mut db = create_test_db();
    delete_fresh_item(&mut db, "latejob", None);

    let job = enqueue_job(
        &mut db,
        "job-after-delete".to_string(),
        "item-latejob".to_string(),
        "interpretation".to_string(),
        1,
        None,
        None,
        1,
        fixed_instant(6),
    )
    .expect("enqueue itself is durable");
    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), fixed_instant(7)).unwrap();
    assert!(claimed.is_none());
    assert_eq!(
        get_job(&db, &job.job_id).unwrap().unwrap().status,
        JobStatus::Cancelled
    );
}

#[test]
fn late_proposal_cannot_be_applied_to_a_deleted_item() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-prop", "Test capture", None);
    create_test_item(&mut db, "item-prop", "cap-prop");
    insert_proposal(&mut db, "prop-before", "item-prop", 0);

    mark_deletion_intent(&mut db, "item-prop", 0, fixed_instant(5)).expect("deletion");

    // The pre-deletion proposal was erased with the item.
    let tx = db.transaction().unwrap();
    assert!(apply_proposal(&tx, "item-prop", "prop-before").is_err());
    drop(tx);

    // A worker racing the delete lands a proposal at the post-deletion revision.
    insert_proposal(&mut db, "prop-after", "item-prop", 1);
    let tx = db.transaction().unwrap();
    let outcome = apply_proposal(&tx, "item-prop", "prop-after");
    assert!(
        matches!(
            outcome,
            Err(ohand_core::domain::items::ProposalApplicationError::ForbiddenByLifecycle)
        ),
        "{outcome:?}"
    );
    drop(tx);
    let applied: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM proposals WHERE applied_state = 'applied'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(applied, 0);
}

#[test]
fn racing_correction_cannot_restore_text() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-racecorr", "Test capture", None);
    create_test_item(&mut db, "item-racecorr", "cap-racecorr");
    mark_deletion_intent(&mut db, "item-racecorr", 0, fixed_instant(5)).expect("deletion");

    for revision in [0, 1] {
        let racing = correction_event(
            &format!("evt-race-{revision}"),
            "item-racecorr",
            revision,
            Some("Test capture"),
            SECRET,
        );
        let error = save_event(&mut db, &racing, revision).unwrap_err();
        if revision == 1 {
            assert!(error.to_string().contains("deleted"), "{error}");
        }
    }

    assert_eq!(secret_hits(&db), 0);
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM corrections WHERE item_id = ?",
            "item-racecorr"
        ),
        0
    );
    assert_eq!(indexed_rows(&db, "item-racecorr"), 0);
    assert_eq!(
        item_lifecycle(&mut db, "item-racecorr"),
        LifecycleState::Deleted
    );
}

#[test]
fn stale_source_replay_cannot_restore_text_or_index() {
    let mut db = create_test_db();
    let original = build_capture("cap-replay", SECRET, Some("audio/replay.m4a"));
    save_capture(&mut db, &original).unwrap();
    create_test_item(&mut db, "item-replay", "cap-replay");
    index_item(&mut db, "item-replay");
    mark_deletion_intent(&mut db, "item-replay", 0, fixed_instant(5)).expect("deletion");

    assert!(
        save_capture(&mut db, &original).is_err(),
        "replaying the original capture must not overwrite the tombstone"
    );

    let tx = db.transaction().unwrap();
    assert_eq!(
        index::sync_item_in_tx(&tx, "item-replay").unwrap(),
        index::IndexChange::Removed
    );
    index::rebuild_index(&tx).unwrap();
    tx.commit().unwrap();

    assert_eq!(captured_text(&db, "cap-replay"), "");
    assert_eq!(indexed_rows(&db, "item-replay"), 0);
    assert_eq!(secret_hits(&db), 0);

    db.reopen();
    assert_eq!(captured_text(&db, "cap-replay"), "");
    assert_eq!(indexed_rows(&db, "item-replay"), 0);
}

// ---------------------------------------------------------------------------
// Deletion progress and crash safety
// ---------------------------------------------------------------------------

#[test]
fn active_item_reports_deletion_not_requested() {
    let mut db = create_test_db();
    create_test_capture(&mut db, "cap-active", "Test", None);
    create_test_item(&mut db, "item-active", "cap-active");
    assert_eq!(
        deletion_progress(&mut db, "item-active").unwrap(),
        DeletionProgress::NotRequested
    );
    assert!(deletion_progress(&mut db, "item-missing").is_err());
}

#[test]
fn completion_is_reported_only_after_every_effect_finishes_across_restarts() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "crash", Some("audio/crash.m4a"));

    // Crash immediately after the tombstone: the item is deleted and unreadable, never complete.
    db.reopen();
    assert!(matches!(
        deletion_progress(&mut db, "item-crash").unwrap(),
        DeletionProgress::InProgress { ref pending, .. } if pending.len() == 3
    ));
    assert_eq!(captured_text(&db, "cap-crash"), "");

    let audio = work_of_type(&intent, DeletionWorkType::RemoveAudio);
    let ingress = work_of_type(&intent, DeletionWorkType::ClearIngress);
    let notifications = work_of_type(&intent, DeletionWorkType::CancelNotifications);

    mark_deletion_work_completed(&mut db, &audio.deletion_work_id, fixed_instant(6)).unwrap();
    mark_deletion_work_completed(&mut db, &ingress.deletion_work_id, fixed_instant(6)).unwrap();

    // Crash with one effect outstanding.
    db.reopen();
    assert_eq!(
        deletion_progress(&mut db, "item-crash").unwrap(),
        DeletionProgress::InProgress {
            pending: vec![DeletionWorkType::CancelNotifications],
            failed: vec![]
        }
    );
    assert_eq!(
        list_pending_deletion_work(&db, "item-crash").unwrap().len(),
        1
    );

    mark_deletion_work_completed(&mut db, &notifications.deletion_work_id, fixed_instant(7))
        .unwrap();
    assert_eq!(
        deletion_progress(&mut db, "item-crash").unwrap(),
        DeletionProgress::Complete
    );
    db.reopen();
    assert_eq!(
        deletion_progress(&mut db, "item-crash").unwrap(),
        DeletionProgress::Complete
    );
}

#[test]
fn completing_work_is_idempotent() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "idem", None);
    let ingress = work_of_type(&intent, DeletionWorkType::ClearIngress);

    let first =
        mark_deletion_work_completed(&mut db, &ingress.deletion_work_id, fixed_instant(6)).unwrap();
    let second =
        mark_deletion_work_completed(&mut db, &ingress.deletion_work_id, fixed_instant(9)).unwrap();
    assert_eq!(first.status, DeletionWorkStatus::Completed);
    assert_eq!(second.status, DeletionWorkStatus::Completed);
    assert_eq!(second.attempted_at, Some(fixed_instant(6)));
}

#[test]
fn terminal_failure_is_recorded_and_never_reads_as_complete() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "fail", None);
    let ingress = work_of_type(&intent, DeletionWorkType::ClearIngress);
    let notifications = work_of_type(&intent, DeletionWorkType::CancelNotifications);

    let failed =
        mark_deletion_work_failed(&mut db, &ingress.deletion_work_id, fixed_instant(6)).unwrap();
    assert_eq!(failed.status, DeletionWorkStatus::FailedNoRetry);
    // Idempotent, and a terminal failure cannot be silently flipped to completed.
    mark_deletion_work_failed(&mut db, &ingress.deletion_work_id, fixed_instant(8)).unwrap();
    assert!(
        mark_deletion_work_completed(&mut db, &ingress.deletion_work_id, fixed_instant(8)).is_err()
    );

    assert_eq!(
        deletion_progress(&mut db, "item-fail").unwrap(),
        DeletionProgress::InProgress {
            pending: vec![DeletionWorkType::CancelNotifications],
            failed: vec![DeletionWorkType::ClearIngress]
        }
    );

    mark_deletion_work_completed(&mut db, &notifications.deletion_work_id, fixed_instant(7))
        .unwrap();
    db.reopen();
    assert_eq!(
        deletion_progress(&mut db, "item-fail").unwrap(),
        DeletionProgress::Failed {
            failed: vec![DeletionWorkType::ClearIngress]
        }
    );
    assert!(
        mark_deletion_work_failed(&mut db, &notifications.deletion_work_id, fixed_instant(9))
            .is_err(),
        "completed work cannot be failed"
    );
}

#[test]
fn audio_reference_is_kept_for_cleanup_until_removal_completes() {
    let mut db = create_test_db();
    let intent = delete_fresh_item(&mut db, "audio", Some("audio/audio.m4a"));

    let target = cleanup_target(&db, "item-audio").unwrap().unwrap();
    assert_eq!(target.capture_id, "cap-audio");
    assert_eq!(target.audio_reference.as_deref(), Some("audio/audio.m4a"));

    let audio = work_of_type(&intent, DeletionWorkType::RemoveAudio);
    mark_deletion_work_completed(&mut db, &audio.deletion_work_id, fixed_instant(6)).unwrap();
    let target = cleanup_target(&db, "item-audio").unwrap().unwrap();
    assert_eq!(target.audio_reference, None);
    assert_eq!(captured_text(&db, "cap-audio"), "");
}
