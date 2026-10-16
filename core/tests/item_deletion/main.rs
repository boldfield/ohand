//! Behavior tests for the user's per-item deletion (A05) against the real core store. Each
//! test reopens the file to assert on durable state, as a restarted process would see it.

use chrono::{Duration, Utc};
use ohand_core::ffi::item_deletion::{
    read_item_deletion_progress, record_item_deletion, DeletionOutcome,
};
use ohand_core::jobs::queue::{
    claim_job_with_lease, complete_job, enqueue_job, get_job, JobStatus,
};
use ohand_core::lifecycle::delete_intent::{
    list_pending_deletion_work, mark_deletion_work_completed, DeletionProgress, DeletionWorkType,
};
use ohand_core::retrieval::index::sync_item_in_tx;
use ohand_core::retrieval::query::{scoped_query_direct, QueryFilter, QueryPagination};
use ohand_core::store::captures::{get_capture, save_capture, Capture};
use ohand_core::store::events::ItemScope;
use ohand_core::store::schema::{Database, SystemClock};
use std::sync::Arc;
use tempfile::TempDir;

const ITEM_ID: &str = "item-1";
const CAPTURE_ID: &str = "capture-1";
const SECRET: &str = "SECRET-PARSNIP-TEXT";
const AUDIO_REFERENCE: &str = "FinalizedAudio/capture-1.m4a";

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn path(&self) -> String {
        self.directory
            .path()
            .join("core.sqlite")
            .to_string_lossy()
            .into_owned()
    }

    fn open(&self) -> Database {
        Database::open(&self.path(), Arc::new(SystemClock)).unwrap()
    }

    /// An active item with audio, a searchable capture and a queued job.
    fn new() -> Fixture {
        let fixture = Fixture {
            directory: tempfile::tempdir().unwrap(),
        };
        let mut database = fixture.open();
        let capture = Capture::new(
            CAPTURE_ID.to_string(),
            Some(SECRET.to_string()),
            Some(AUDIO_REFERENCE.to_string()),
            "2026-10-10T09:30:00+00:00".to_string(),
            "UTC".to_string(),
            0,
            "en_US".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            "route-personal".to_string(),
            false,
            "2026-10-10T09:30:01+00:00".to_string(),
            None,
        )
        .unwrap();
        save_capture(&mut database, &capture).unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type,
                                    save_state, sync_state, processing_state,
                                    transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'active', 'note', 'saved', 'not_configured', 'pending',
                         'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                [ITEM_ID, CAPTURE_ID],
            )
            .unwrap();
        enqueue_job(
            &mut database,
            "job-1".to_string(),
            ITEM_ID.to_string(),
            "interpretation".to_string(),
            0,
            None,
            None,
            1,
            Utc::now(),
        )
        .unwrap();
        let transaction = database.immediate_transaction().unwrap();
        sync_item_in_tx(&transaction, ITEM_ID).unwrap();
        transaction.commit().unwrap();
        fixture
    }
}

fn search_hits(database: &Database) -> usize {
    let filter = QueryFilter {
        read_scopes: vec![ItemScope::Personal],
        lifecycle_states: vec!["deleted".to_string(), "active".to_string()],
        ..QueryFilter::default()
    };
    let pagination = QueryPagination {
        limit: 10,
        offset: 0,
    };
    scoped_query_direct(database.conn(), "SECRET-PARSNIP-TEXT", &filter, &pagination)
        .unwrap()
        .hits
        .len()
}

fn recorded(outcome: DeletionOutcome) -> (i32, bool, DeletionProgress) {
    match outcome {
        DeletionOutcome::Recorded {
            deletion_revision,
            replayed,
            progress,
        } => (deletion_revision, replayed, progress),
        other => panic!("expected a recorded deletion, got {other:?}"),
    }
}

#[test]
fn deleting_an_item_removes_it_from_search_and_detail_reads_and_cancels_its_work() {
    let fixture = Fixture::new();
    let mut database = fixture.open();
    assert_eq!(search_hits(&database), 1);

    let (revision, replayed, progress) =
        recorded(record_item_deletion(&mut database, ITEM_ID, 0).unwrap());
    assert_eq!(revision, 0);
    assert!(!replayed);
    assert_eq!(
        progress,
        DeletionProgress::InProgress {
            pending: vec![
                DeletionWorkType::CancelNotifications,
                DeletionWorkType::ClearIngress,
                DeletionWorkType::RemoveAudio,
            ],
            failed: vec![],
        }
    );
    drop(database);

    let mut database = fixture.open();
    assert_eq!(
        search_hits(&database),
        0,
        "a deleted item is not found by search"
    );
    let transaction = database.immediate_transaction().unwrap();
    let capture = get_capture(&transaction, CAPTURE_ID).unwrap().unwrap();
    drop(transaction);
    assert_eq!(
        capture.text.as_deref().unwrap_or(""),
        "",
        "the detail read has no content"
    );
    assert_eq!(
        get_job(&database, "job-1").unwrap().unwrap().status,
        JobStatus::Cancelled
    );
}

#[test]
fn a_late_job_result_for_a_deleted_item_is_refused() {
    let fixture = Fixture::new();
    let mut database = fixture.open();
    let claimed = claim_job_with_lease(&mut database, Duration::minutes(5), Utc::now())
        .unwrap()
        .expect("the worker claims the job");

    record_item_deletion(&mut database, ITEM_ID, 0).unwrap();

    assert!(
        complete_job(&mut database, "job-1", claimed.attempt_count).is_err(),
        "the worker's late result must be refused"
    );
    assert_eq!(
        get_job(&database, "job-1").unwrap().unwrap().status,
        JobStatus::Cancelled
    );
}

#[test]
fn a_repeat_after_a_restart_reports_the_same_intent_and_keeps_the_progress() {
    let fixture = Fixture::new();
    let mut database = fixture.open();
    record_item_deletion(&mut database, ITEM_ID, 0).unwrap();
    for row in list_pending_deletion_work(&database, ITEM_ID).unwrap() {
        if row.work_type == DeletionWorkType::RemoveAudio {
            mark_deletion_work_completed(&mut database, &row.deletion_work_id, Utc::now()).unwrap();
        }
    }
    drop(database);

    let mut database = fixture.open();
    let (revision, replayed, progress) =
        recorded(record_item_deletion(&mut database, ITEM_ID, 0).unwrap());
    assert_eq!(revision, 0);
    assert!(replayed);
    assert_eq!(
        progress,
        DeletionProgress::InProgress {
            pending: vec![
                DeletionWorkType::CancelNotifications,
                DeletionWorkType::ClearIngress,
            ],
            failed: vec![],
        },
        "a repeat never re-opens finished cleanup"
    );
}

#[test]
fn progress_reaches_complete_only_when_every_row_is_durably_completed() {
    let fixture = Fixture::new();
    let mut database = fixture.open();
    assert_eq!(
        read_item_deletion_progress(&mut database, ITEM_ID).unwrap(),
        DeletionProgress::NotRequested
    );
    record_item_deletion(&mut database, ITEM_ID, 0).unwrap();

    for row in list_pending_deletion_work(&database, ITEM_ID).unwrap() {
        assert!(matches!(
            read_item_deletion_progress(&mut database, ITEM_ID).unwrap(),
            DeletionProgress::InProgress { .. }
        ));
        mark_deletion_work_completed(&mut database, &row.deletion_work_id, Utc::now()).unwrap();
    }
    assert_eq!(
        read_item_deletion_progress(&mut database, ITEM_ID).unwrap(),
        DeletionProgress::Complete
    );
    let audio_reference: Option<String> = database
        .conn()
        .query_row(
            "SELECT audio_reference FROM captures WHERE capture_id = ?",
            [CAPTURE_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        audio_reference, None,
        "completing the audio row releases the reference"
    );
}

#[test]
fn a_stale_revision_never_deletes_a_newer_item() {
    let fixture = Fixture::new();
    let mut database = fixture.open();
    assert_eq!(
        record_item_deletion(&mut database, ITEM_ID, 4).unwrap(),
        DeletionOutcome::StaleRevision {
            current_revision: 0
        }
    );
    assert_eq!(search_hits(&database), 1);
    assert_eq!(
        read_item_deletion_progress(&mut database, ITEM_ID).unwrap(),
        DeletionProgress::NotRequested
    );
}
