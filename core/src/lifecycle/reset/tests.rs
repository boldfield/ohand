use super::*;
use crate::domain::items::{
    apply_proposal, load_item_state, LifecycleState, ProposalApplicationError,
};
use crate::ingress::import::{import_foreground_ingress, ImportDisposition, IngressErrorKind};
use crate::ingress::transcription::{
    apply_transcription_outcome, DiscardReason, RecognizedTranscript, RecognizerResult,
    TranscriptionDisposition, TranscriptionOutcome,
};
use crate::jobs::queue::{
    claim_job_with_lease, complete_job, enqueue_job, get_job, Job, JobStatus,
};
use crate::lifecycle::delete_intent::{
    deletion_progress, mark_deletion_work_completed, mark_deletion_work_failed, DeletionProgress,
};
use crate::privacy::routing::JOB_TYPE_TRANSCRIPTION_ATTACHMENT;
use crate::reminders::reconcile::{
    apply_completion, CompletionOutcome, EffectCompletion, EffectOutcome,
};
use crate::reminders::state::{
    apply_derived_request, desired_notifications, get_reminder, list_operations,
    DerivedReminderRequest, OperationType, RequestState,
};
use crate::store::captures::Capture;
use crate::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use crate::store::schema::{Clock, SystemClock};
use chrono::{Duration, TimeZone};
use std::sync::Arc;
use tempfile::TempDir;

const SECRET: &str = "SECRET-RESET-TEXT";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const PERSONAL_ROUTE: &str = "route-personal";
const WORK_ROUTE: &str = "route-work";

fn instant(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 15, 10, minute, 0)
        .single()
        .unwrap()
}

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

struct Store {
    _directory: TempDir,
    path: String,
}

impl Store {
    fn new() -> Store {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("ohand.db").display().to_string();
        let store = Store {
            _directory: directory,
            path,
        };
        let database = store.open();
        for (route_id, scope) in [(PERSONAL_ROUTE, "personal"), (WORK_ROUTE, "work")] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                     VALUES (?, ?, ?, '[]', '2026-01-15T09:00:00Z')",
                    rusqlite::params![route_id, route_id, scope],
                )
                .unwrap();
        }
        store
    }

    /// Opening again stands in for a process restart.
    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.open()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn secret_hits(&self) -> i64 {
        let like = format!("%{SECRET}%");
        let database = self.open();
        [
            "SELECT COUNT(*) FROM captures WHERE text LIKE ?1 OR session_topic LIKE ?1",
            "SELECT COUNT(*) FROM items WHERE current_session_topic LIKE ?1",
            "SELECT COUNT(*) FROM events WHERE correction_new_value LIKE ?1 OR correction_old_value LIKE ?1",
            "SELECT COUNT(*) FROM corrections WHERE new_value LIKE ?1 OR old_value LIKE ?1",
            "SELECT COUNT(*) FROM proposals WHERE session_topic_proposal LIKE ?1",
            "SELECT COUNT(*) FROM reminders WHERE source_phrase LIKE ?1",
            "SELECT COUNT(*) FROM transcript_attachments WHERE text LIKE ?1",
            "SELECT COUNT(*) FROM search_index WHERE original_text LIKE ?1 OR current_text LIKE ?1",
        ]
        .iter()
        .map(|sql| {
            database
                .conn()
                .query_row(sql, [&like], |row| row.get::<_, i64>(0))
                .unwrap_or_else(|error| panic!("{sql}: {error}"))
        })
        .sum()
    }

    fn item_lifecycle(&self, item_id: &str) -> LifecycleState {
        let mut database = self.open();
        let transaction = database.transaction().unwrap();
        load_item_state(&transaction, item_id)
            .unwrap()
            .unwrap()
            .lifecycle_state
    }
}

fn text_capture(capture_id: &str, text: &str, scope: &str, route_id: &str) -> Capture {
    Capture::new(
        capture_id.to_string(),
        Some(text.to_string()),
        None,
        "2026-01-15T09:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        route_id.to_string(),
        false,
        "2026-01-15T09:30:01Z".to_string(),
        Some(format!("{SECRET} topic")),
    )
    .unwrap()
}

fn voice_capture(capture_id: &str, audio_reference: &str) -> Capture {
    Capture {
        text: None,
        audio_reference: Some(audio_reference.to_string()),
        session_topic: None,
        ..text_capture(capture_id, "unused", "personal", PERSONAL_ROUTE)
    }
}

fn import(store: &Store, capture: &Capture) -> String {
    let acknowledgment = import_foreground_ingress(&mut store.open(), capture).unwrap();
    assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);
    // These tests script their own jobs, so drop the interpretation job an import queues.
    store
        .open()
        .conn()
        .execute(
            "DELETE FROM jobs WHERE item_id = ? AND job_type = 'interpret'",
            [&acknowledgment.item_id],
        )
        .unwrap();
    acknowledgment.item_id
}

fn enqueue(store: &Store, job_id: &str, item_id: &str, job_type: &str) {
    let mut database = store.open();
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .unwrap();
    enqueue_job(
        &mut database,
        job_id.to_string(),
        item_id.to_string(),
        job_type.to_string(),
        revision,
        None,
        None,
        1,
        instant(1),
    )
    .unwrap();
}

fn claim(store: &Store) -> Job {
    claim_job_with_lease(&mut store.open(), Duration::minutes(30), instant(2))
        .unwrap()
        .expect("a job is eligible")
}

fn transcription_outcome(job: &Job, audio_reference: &str, text: &str) -> TranscriptionOutcome {
    TranscriptionOutcome {
        job_id: job.job_id.clone(),
        lease_attempt: job.attempt_count,
        audio_reference: audio_reference.to_string(),
        result: RecognizerResult::Transcript(RecognizedTranscript {
            text: text.to_string(),
            confidence: Some(0.9),
            detected_language: Some("en-US".to_string()),
            recognizer: "on-device-recognizer/1".to_string(),
            audio_sha256: AUDIO_SHA256.to_string(),
        }),
        interpretation: None,
    }
}

fn save_text_correction(store: &Store, item_id: &str, revision: i32, event_id: &str, value: &str) {
    let event = Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some(format!("{SECRET} work note")),
            new_value: value.to_string(),
        }),
        instant(3).to_rfc3339(),
    )
    .unwrap();
    save_event(&mut store.open(), &event, revision).unwrap();
}

fn insert_proposal(
    store: &Store,
    proposal_id: &str,
    item_id: &str,
    source_revision: i32,
    topic: &str,
) {
    let database = store.open();
    let capture_id: String = database
        .conn()
        .query_row(
            "SELECT capture_id FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .unwrap();
    database
        .conn()
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
                topic,
                instant(1).to_rfc3339()
            ],
        )
        .unwrap();
}

/// Everything a populated store holds that a reset must reach, plus the in-flight work that can
/// still report back afterwards.
struct Populated {
    personal_item: String,
    work_item: String,
    attached_voice_item: String,
    inflight_voice_item: String,
    reminder_item: String,
    inflight_transcription: Job,
    running_interpretation: Job,
    queued_interpretation_id: String,
    schedule_effect_identity: String,
    reminder_id: String,
    personal_capture: Capture,
}

const INFLIGHT_AUDIO: &str = "staging/2026-01-15/inflight.m4a";

fn populate(store: &Store) -> Populated {
    let personal_capture = text_capture(
        "3f9c1e52-0000-4000-8000-000000000001",
        &format!("{SECRET} personal note"),
        "personal",
        PERSONAL_ROUTE,
    );
    let personal_item = import(store, &personal_capture);
    let work_item = import(
        store,
        &text_capture(
            "3f9c1e52-0000-4000-8000-000000000002",
            &format!("{SECRET} work note"),
            "work",
            WORK_ROUTE,
        ),
    );

    let attached_voice_item = import(
        store,
        &voice_capture(
            "3f9c1e52-0000-4000-8000-000000000003",
            "staging/attached.m4a",
        ),
    );
    enqueue(
        store,
        "transcription-attached",
        &attached_voice_item,
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
    );
    let attached_job = claim(store);
    apply_transcription_outcome(
        &mut store.open(),
        &transcription_outcome(
            &attached_job,
            "staging/attached.m4a",
            &format!("{SECRET} spoken"),
        ),
        instant(4),
    )
    .unwrap();

    let inflight_voice_item = import(
        store,
        &voice_capture("3f9c1e52-0000-4000-8000-000000000004", INFLIGHT_AUDIO),
    );
    enqueue(
        store,
        "transcription-inflight",
        &inflight_voice_item,
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
    );
    let inflight_transcription = claim(store);

    enqueue(
        store,
        "interpretation-running",
        &personal_item,
        "interpretation",
    );
    let running_interpretation = claim(store);
    enqueue(store, "interpretation-queued", &work_item, "interpretation");

    save_text_correction(
        store,
        &work_item,
        0,
        "evt-correction-work",
        &format!("{SECRET} edited"),
    );
    insert_proposal(store, "proposal-work", &work_item, 1, SECRET);

    let reminder_item = import(
        store,
        &text_capture(
            "3f9c1e52-0000-4000-8000-000000000005",
            "call the roofer 2026-01-16 09:00:00",
            "personal",
            PERSONAL_ROUTE,
        ),
    );
    let type_event = Event::new(
        "evt-type-reminder".to_string(),
        reminder_item.clone(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        instant(1).to_rfc3339(),
    )
    .unwrap();
    save_event(&mut store.open(), &type_event, 0).unwrap();
    let mut database = store.open();
    let transaction = database.immediate_transaction().unwrap();
    let reminder = apply_derived_request(
        &transaction,
        &FixedClock(instant(0)),
        &DerivedReminderRequest {
            item_id: reminder_item.clone(),
            source_revision: 1,
            phrase: "2026-01-16 09:00:00".to_string(),
        },
    )
    .unwrap();
    let schedule_effect_identity = list_operations(&transaction, &reminder.reminder_id)
        .unwrap()
        .into_iter()
        .find(|operation| operation.operation_type == OperationType::Schedule)
        .expect("a schedule operation is pending")
        .effect_identity;
    transaction.commit().unwrap();

    Populated {
        personal_item,
        work_item,
        attached_voice_item,
        inflight_voice_item,
        reminder_item,
        inflight_transcription,
        running_interpretation,
        queued_interpretation_id: "interpretation-queued".to_string(),
        schedule_effect_identity,
        reminder_id: reminder.reminder_id,
        personal_capture,
    }
}

fn acknowledge_all_cleanup(store: &Store, now: DateTime<Utc>) {
    let mut database = store.open();
    let work_ids: Vec<String> = database
        .conn()
        .prepare("SELECT deletion_work_id FROM deletion_work WHERE status = 'pending' ORDER BY deletion_work_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for work_id in work_ids {
        // Acknowledge the native notification cancel first, as native code does, so the
        // notification cleanup row may complete.
        database
            .conn()
            .execute(
                "UPDATE reminder_operations SET operation_state = 'acknowledged'
                 WHERE operation_state = 'pending'",
                [],
            )
            .unwrap();
        mark_deletion_work_completed(&mut database, &work_id, now).unwrap();
    }
}

const ITEM_COUNT: i64 = 5;

#[test]
fn start_records_generation_boundary_and_tombstones_every_item_across_routes() {
    let store = Store::new();
    let populated = populate(&store);
    assert!(store.secret_hits() > 0);
    assert!(store.count("SELECT COUNT(*) FROM search_index") > 0);

    let status = start_reset(&mut store.open(), instant(10)).unwrap();

    assert_eq!(status.generation, 1);
    assert_eq!(status.phase, ResetPhase::Pending);
    assert_eq!(status.boundary_rowid, ITEM_COUNT);
    assert_eq!(status.items_awaiting_tombstone, 0);
    assert!(
        status.cleanup_pending > 0,
        "cleanup is not acknowledged yet"
    );
    assert_eq!(status.completed_at, None);

    for item_id in [
        &populated.personal_item,
        &populated.work_item,
        &populated.attached_voice_item,
        &populated.inflight_voice_item,
        &populated.reminder_item,
    ] {
        assert_eq!(store.item_lifecycle(item_id), LifecycleState::Deleted);
    }
    assert_eq!(
        store.count("SELECT COUNT(*) FROM items WHERE lifecycle_state != 'deleted'"),
        0
    );
    assert_eq!(store.secret_hits(), 0);
    assert_eq!(store.count("SELECT COUNT(*) FROM search_index"), 0);
    assert_eq!(
        store.count("SELECT COUNT(*) FROM transcript_attachments"),
        0
    );
    assert_eq!(
        store.count("SELECT COUNT(*) FROM jobs WHERE status IN ('queued', 'running')"),
        0,
        "old in-flight work is invalidated"
    );
    let mut database = store.open();
    let transaction = database.transaction().unwrap();
    assert!(desired_notifications(&transaction).unwrap().is_empty());
    let reminder = get_reminder(&transaction, &populated.reminder_item)
        .unwrap()
        .unwrap();
    assert_eq!(reminder.request_state, RequestState::Cancelled);
}

#[test]
fn reset_stays_pending_until_every_cleanup_is_acknowledged_across_restarts() {
    let store = Store::new();
    populate(&store);
    start_reset(&mut store.open(), instant(10)).unwrap();

    let restarted = advance_reset(&mut store.open(), instant(11)).unwrap();
    assert_eq!(restarted.phase, ResetPhase::Pending);
    assert!(restarted.cleanup_pending > 0);

    let mut database = store.open();
    let first_work: String = database
        .conn()
        .query_row(
            "SELECT deletion_work_id FROM deletion_work WHERE work_type = 'clear_ingress' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_work_completed(&mut database, &first_work, instant(12)).unwrap();
    drop(database);
    let partial = advance_reset(&mut store.open(), instant(13)).unwrap();
    assert_eq!(partial.phase, ResetPhase::Pending);
    assert_eq!(partial.cleanup_pending, restarted.cleanup_pending - 1);

    acknowledge_all_cleanup(&store, instant(14));
    let completed = advance_reset(&mut store.open(), instant(15)).unwrap();
    assert_eq!(completed.phase, ResetPhase::Completed);
    assert_eq!(completed.completed_at, Some(instant(15)));
    assert_eq!(
        (completed.cleanup_pending, completed.cleanup_failed),
        (0, 0)
    );

    let after_restart = reset_status(&store.open()).unwrap().unwrap();
    assert_eq!(after_restart.phase, ResetPhase::Completed);
    let replay = advance_reset(&mut store.open(), instant(20)).unwrap();
    assert_eq!(
        replay.completed_at,
        Some(instant(15)),
        "completion time is not rewritten"
    );
}

#[test]
fn a_terminally_failed_cleanup_is_reported_and_never_completes_the_reset() {
    let store = Store::new();
    populate(&store);
    start_reset(&mut store.open(), instant(10)).unwrap();
    let mut database = store.open();
    let failing: String = database
        .conn()
        .query_row(
            "SELECT deletion_work_id FROM deletion_work WHERE work_type = 'clear_ingress' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_work_failed(&mut database, &failing, instant(11)).unwrap();
    drop(database);
    acknowledge_all_cleanup(&store, instant(12));

    let status = advance_reset(&mut store.open(), instant(13)).unwrap();
    assert_eq!(status.phase, ResetPhase::Pending);
    assert_eq!((status.cleanup_pending, status.cleanup_failed), (0, 1));
}

#[test]
fn starting_again_while_pending_resumes_the_same_generation() {
    let store = Store::new();
    populate(&store);
    let first = start_reset(&mut store.open(), instant(10)).unwrap();

    let second = start_reset(&mut store.open(), instant(20)).unwrap();

    assert_eq!(second.generation, first.generation);
    assert_eq!(second.boundary_rowid, first.boundary_rowid);
    assert_eq!(second.started_at, first.started_at);
    assert_eq!(store.count("SELECT COUNT(*) FROM reset_generations"), 1);
}

#[test]
fn a_crash_before_the_reset_commits_leaves_no_half_reset_and_a_retry_completes_it() {
    let store = Store::new();
    populate(&store);

    let crashed = start_with_fault(&mut store.open(), instant(10), Some(2));
    assert!(crashed.is_err());

    assert_eq!(store.count("SELECT COUNT(*) FROM reset_generations"), 0);
    assert_eq!(
        store.count("SELECT COUNT(*) FROM items WHERE lifecycle_state = 'deleted'"),
        0
    );
    assert_eq!(
        store.count("SELECT COUNT(*) FROM events WHERE event_type = 'deletion'"),
        0
    );

    let retried = start_reset(&mut store.open(), instant(11)).unwrap();
    assert_eq!(retried.generation, 1);
    assert_eq!(retried.items_awaiting_tombstone, 0);
    assert_eq!(store.secret_hits(), 0);
    assert_eq!(
        store.count("SELECT COUNT(*) FROM events WHERE event_type = 'deletion'"),
        ITEM_COUNT,
        "each item is tombstoned exactly once"
    );
    let replay = start_reset(&mut store.open(), instant(12)).unwrap();
    assert_eq!(replay.generation, 1);
    assert_eq!(
        store.count("SELECT COUNT(*) FROM events WHERE event_type = 'deletion'"),
        ITEM_COUNT
    );
}

#[test]
fn a_committed_pending_generation_never_exposes_a_live_item_inside_its_boundary() {
    let store = Store::new();
    populate(&store);

    let started = start_reset(&mut store.open(), instant(10)).unwrap();
    assert_eq!(started.phase, ResetPhase::Pending);

    let reopened = store.open();
    let live_inside_boundary: i64 = reopened
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM items
             WHERE lifecycle_state != 'deleted'
               AND rowid <= (SELECT boundary_rowid FROM reset_generations WHERE phase = 'pending')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_inside_boundary, 0);
}

#[test]
fn a_pending_generation_found_with_live_items_is_resumed_in_one_transaction() {
    let store = Store::new();
    populate(&store);
    store
        .open()
        .conn()
        .execute(
            "INSERT INTO reset_generations (phase, boundary_rowid, started_at)
             VALUES ('pending', (SELECT MAX(rowid) FROM items), ?)",
            [instant(9).to_rfc3339()],
        )
        .unwrap();

    let crashed = advance_reset_with_fault(&mut store.open(), instant(10), Some(3));
    assert!(crashed.is_err());
    assert_eq!(
        store.count("SELECT COUNT(*) FROM items WHERE lifecycle_state = 'deleted'"),
        0
    );

    let resumed = advance_reset(&mut store.open(), instant(11)).unwrap();
    assert_eq!(resumed.generation, 1);
    assert_eq!(resumed.items_awaiting_tombstone, 0);
    assert_eq!(store.count("SELECT COUNT(*) FROM reset_generations"), 1);
    assert_eq!(store.secret_hits(), 0);
}

#[test]
fn an_item_already_deleted_by_the_user_keeps_its_own_cleanup_in_the_reset() {
    let store = Store::new();
    let populated = populate(&store);
    crate::lifecycle::delete_intent::mark_deletion_intent(
        &mut store.open(),
        &populated.attached_voice_item,
        1,
        instant(5),
    )
    .unwrap();

    let status = start_reset(&mut store.open(), instant(10)).unwrap();

    assert_eq!(status.items_awaiting_tombstone, 0);
    acknowledge_all_cleanup(&store, instant(11));
    assert_eq!(
        advance_reset(&mut store.open(), instant(12)).unwrap().phase,
        ResetPhase::Completed
    );
}

#[test]
fn a_capture_after_reset_started_belongs_to_the_fresh_generation_and_survives_retries() {
    let store = Store::new();
    let populated = populate(&store);
    start_reset(&mut store.open(), instant(10)).unwrap();

    let fresh_item = import(
        &store,
        &text_capture(
            "3f9c1e52-0000-4000-8000-000000000006",
            "a fresh note after the reset tap",
            "personal",
            PERSONAL_ROUTE,
        ),
    );
    start_reset(&mut store.open(), instant(11)).unwrap();
    acknowledge_all_cleanup(&store, instant(12));
    let completed = advance_reset(&mut store.open(), instant(13)).unwrap();

    assert_eq!(completed.phase, ResetPhase::Completed);
    assert_eq!(store.item_lifecycle(&fresh_item), LifecycleState::Active);
    assert_eq!(store.count("SELECT COUNT(*) FROM search_index"), 1);
    assert_eq!(
        generation_of_item(&store.open(), &populated.personal_item).unwrap(),
        0
    );
    assert_eq!(generation_of_item(&store.open(), &fresh_item).unwrap(), 1);
    assert_eq!(current_generation(&store.open()).unwrap(), 1);

    let after_completion = import(
        &store,
        &text_capture(
            "3f9c1e52-0000-4000-8000-000000000007",
            "a note after the reset completed",
            "work",
            WORK_ROUTE,
        ),
    );
    assert_eq!(
        store.item_lifecycle(&after_completion),
        LifecycleState::Active
    );
    assert_eq!(
        generation_of_item(&store.open(), &after_completion).unwrap(),
        1
    );

    let second = start_reset(&mut store.open(), instant(30)).unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(second.boundary_rowid, ITEM_COUNT + 2);
    assert_eq!(store.item_lifecycle(&fresh_item), LifecycleState::Deleted);
    assert_eq!(
        store.item_lifecycle(&after_completion),
        LifecycleState::Deleted
    );
    let third = import(
        &store,
        &text_capture(
            "3f9c1e52-0000-4000-8000-000000000008",
            "after the second reset",
            "personal",
            PERSONAL_ROUTE,
        ),
    );
    assert_eq!(generation_of_item(&store.open(), &third).unwrap(), 2);
}

#[test]
fn resetting_an_empty_store_completes_immediately() {
    let store = Store::new();
    let status = start_reset(&mut store.open(), instant(10)).unwrap();
    assert_eq!(status.phase, ResetPhase::Completed);
    assert_eq!(status.boundary_rowid, 0);
    assert_eq!(current_generation(&store.open()).unwrap(), 1);
}

#[test]
fn advancing_before_any_reset_is_an_error_and_status_is_empty() {
    let store = Store::new();
    assert!(reset_status(&store.open()).unwrap().is_none());
    assert!(advance_reset(&mut store.open(), instant(1)).is_err());
    assert_eq!(current_generation(&store.open()).unwrap(), 0);
}

#[test]
fn per_item_deletion_progress_is_visible_for_every_reset_item() {
    let store = Store::new();
    let populated = populate(&store);
    start_reset(&mut store.open(), instant(10)).unwrap();
    assert!(matches!(
        deletion_progress(&mut store.open(), &populated.work_item).unwrap(),
        DeletionProgress::InProgress { .. }
    ));
}

// ---------------------------------------------------------------------------
// Late results arriving after reset started are rejected by the existing per-item guard.
// ---------------------------------------------------------------------------

#[test]
fn late_results_after_reset_started_revive_nothing_even_after_a_restart() {
    let store = Store::new();
    let populated = populate(&store);
    start_reset(&mut store.open(), instant(10)).unwrap();
    // Process restart: nothing but the database file carries the reset forward.
    let mut database = store.open();

    // A job result from work that was running when reset started.
    let result = complete_job(
        &mut database,
        &populated.running_interpretation.job_id,
        populated.running_interpretation.attempt_count,
    );
    assert!(result.is_err(), "the running job's late result is rejected");
    assert_eq!(
        get_job(&database, &populated.running_interpretation.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Cancelled
    );
    assert_eq!(
        get_job(&database, &populated.queued_interpretation_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Cancelled
    );

    // A job enqueued for a pre-reset item after the reset is never run.
    enqueue_job(
        &mut database,
        "late-enqueue".to_string(),
        populated.personal_item.clone(),
        "interpretation".to_string(),
        1,
        None,
        None,
        1,
        instant(11),
    )
    .unwrap();
    assert!(
        claim_job_with_lease(&mut database, Duration::minutes(5), instant(12))
            .unwrap()
            .is_none()
    );

    // A user correction racing the reset.
    for revision in [0, 1] {
        let racing = Event::new(
            format!("evt-race-{revision}"),
            populated.work_item.clone(),
            revision,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind: CorrectionKind::Text,
                old_value: None,
                new_value: SECRET.to_string(),
            }),
            instant(11).to_rfc3339(),
        )
        .unwrap();
        assert!(save_event(&mut database, &racing, revision).is_err());
    }

    // A transcript for audio whose recognition was in flight.
    let attachment = apply_transcription_outcome(
        &mut database,
        &transcription_outcome(&populated.inflight_transcription, INFLIGHT_AUDIO, SECRET),
        instant(11),
    );
    if let Ok(acknowledgment) = attachment {
        assert_eq!(
            acknowledgment.disposition,
            TranscriptionDisposition::Discarded(DiscardReason::ItemDeleted)
        );
    }

    // An interpretation proposal landing against the tombstone.
    drop(database);
    insert_proposal(
        &store,
        "proposal-late",
        &populated.work_item,
        2,
        "a late topic",
    );
    let mut database = store.open();
    let transaction = database.transaction().unwrap();
    let applied = apply_proposal(&transaction, &populated.work_item, "proposal-late");
    assert!(
        matches!(applied, Err(ProposalApplicationError::ForbiddenByLifecycle)),
        "{applied:?}"
    );
    drop(transaction);

    // A late native confirmation that the reminder was installed.
    let transaction = database.immediate_transaction().unwrap();
    let completion = apply_completion(
        &transaction,
        &FixedClock(instant(11)),
        &EffectCompletion {
            reminder_id: populated.reminder_id.clone(),
            effect_identity: populated.schedule_effect_identity.clone(),
            outcome: EffectOutcome::Succeeded,
        },
    )
    .unwrap();
    assert_eq!(
        completion,
        CompletionOutcome::Stale {
            cancel_required: true
        }
    );
    assert!(desired_notifications(&transaction).unwrap().is_empty());
    assert_eq!(
        get_reminder(&transaction, &populated.reminder_item)
            .unwrap()
            .unwrap()
            .request_state,
        RequestState::Cancelled
    );
    transaction.commit().unwrap();

    // Redelivery of a pre-reset capture ID, including one whose item was imported before.
    let error = import_foreground_ingress(&mut database, &populated.personal_capture).unwrap_err();
    assert!(
        matches!(&error.kind, IngressErrorKind::ItemDeleted { item_id } if item_id == &populated.personal_item)
    );
    drop(database);

    assert_eq!(store.secret_hits(), 0);
    assert_eq!(store.count("SELECT COUNT(*) FROM search_index"), 0);
    assert_eq!(
        store.count("SELECT COUNT(*) FROM transcript_attachments"),
        0
    );
    assert_eq!(
        store.count("SELECT COUNT(*) FROM proposals WHERE applied_state = 'applied'"),
        0
    );
    assert_eq!(
        store.count("SELECT COUNT(*) FROM items WHERE lifecycle_state != 'deleted'"),
        0
    );
}
