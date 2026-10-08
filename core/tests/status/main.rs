use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

use ohand_core::domain::status::{
    ItemStatus, ProcessingJobStatus, ProcessingState, ReminderDeliveryState, ReminderRequestState,
    ReminderScheduleState, SaveState, SyncState, TranscriptionState, UnschedulableReason,
};
use ohand_core::jobs::queue::{
    claim_job_with_lease, complete_job, enqueue_job, fail_job_with_backoff, get_job, Job, JobStatus,
};
use ohand_core::privacy::routing::{JOB_TYPE_INTERPRET, JOB_TYPE_TRANSCRIPTION_ATTACHMENT};
use ohand_core::store::schema::{Clock, Database};

struct TestClock {
    instant: DateTime<Utc>,
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

fn make_test_db(path: &str, instant: DateTime<Utc>) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(TestClock { instant });
    Database::open(path, clock)
}

fn insert_test_item_with_status(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
    capture_text: &str,
    save_state: &str,
    sync_state: &str,
    processing_state: &str,
    transcription_state: Option<&str>,
) -> Result<()> {
    let capture_id = format!("cap-{}", item_id);

    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &capture_id,
            Some(capture_text),
            None::<String>,
            "2026-01-15T10:30:00Z",
            "UTC",
            0,
            "en",
            "gregorian",
            "personal",
            "route-1",
            0,
            "2026-01-15T10:30:00Z",
            None::<String>,
        ],
    )?;

    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            item_id,
            &capture_id,
            save_state,
            sync_state,
            processing_state,
            transcription_state,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z",
        ],
    )?;
    Ok(())
}

fn insert_test_reminder(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
    request_state: &str,
    schedule_state: &str,
    delivery_state: &str,
    acknowledgment_state: &str,
    unschedulable_reason: Option<&str>,
) -> Result<()> {
    let reminder_id = Uuid::new_v4().to_string();

    tx.execute(
        "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, acknowledgment_state, unschedulable_reason, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &reminder_id,
            item_id,
            request_state,
            schedule_state,
            delivery_state,
            acknowledgment_state,
            unschedulable_reason,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z",
        ],
    )?;
    Ok(())
}

fn temp_db_path(tmpdir: &TempDir, name: &str) -> String {
    tmpdir
        .path()
        .join(format!("ohand_test_{}.db", name))
        .to_string_lossy()
        .to_string()
}

#[test]
fn test_load_item_status() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "load");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "item-1",
        "hello world",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "item-1")?;
    tx.commit()?;

    let status = status.expect("item should exist");
    assert_eq!(status.item_id, "item-1");
    assert_eq!(status.save_state, SaveState::SavedLocal);
    assert_eq!(status.sync_state, SyncState::NotConfigured);
    assert_eq!(status.processing_state, ProcessingState::Processed);
    assert_eq!(status.transcription_state, TranscriptionState::Transcribed);
    assert_eq!(status.reminder_request_state, None);
    Ok(())
}

#[test]
fn test_m1_sync_not_configured() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "m1sync");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "item-1",
        "capture text",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("audio_pending"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "item-1")?;
    tx.commit()?;

    let status = status.expect("item should exist");
    assert_eq!(status.sync_state, SyncState::NotConfigured);
    assert_eq!(
        status.sync_state.as_str(),
        "not_configured",
        "M1 sync truthfully reads not_configured"
    );

    Ok(())
}

#[test]
fn test_processing_state_values() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "processing");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "abstained",
        "text 1",
        "saved_local",
        "not_configured",
        "abstained",
        Some("transcribed"),
    )?;
    insert_test_item_with_status(
        &tx,
        "uninterpreted",
        "text 2",
        "saved_local",
        "not_configured",
        "uninterpreted",
        Some("transcribed"),
    )?;
    insert_test_item_with_status(
        &tx,
        "unprocessed",
        "text 3",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("transcribed"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let abstained = ItemStatus::load(&tx, "abstained")?.expect("item should exist");
    let uninterpreted = ItemStatus::load(&tx, "uninterpreted")?.expect("item should exist");
    let unprocessed = ItemStatus::load(&tx, "unprocessed")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(abstained.processing_state, ProcessingState::Abstained);
    assert_eq!(
        uninterpreted.processing_state,
        ProcessingState::Uninterpreted
    );
    assert_eq!(unprocessed.processing_state, ProcessingState::Unprocessed);

    assert_ne!(abstained.processing_state, uninterpreted.processing_state);
    assert_ne!(uninterpreted.processing_state, unprocessed.processing_state);

    Ok(())
}

#[test]
fn test_item_not_found() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "notfound");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "nonexistent")?;
    tx.commit()?;

    assert_eq!(status, None);

    Ok(())
}

#[test]
fn test_save_state_values() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "savestates");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "saved_local",
        "text",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("audio_pending"),
    )?;
    insert_test_item_with_status(
        &tx,
        "not_saved",
        "text",
        "not_saved",
        "not_configured",
        "unprocessed",
        Some("audio_pending"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let saved = ItemStatus::load(&tx, "saved_local")?.expect("item should exist");
    let not_saved = ItemStatus::load(&tx, "not_saved")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(saved.save_state, SaveState::SavedLocal);
    assert_eq!(not_saved.save_state, SaveState::NotSaved);

    Ok(())
}

#[test]
fn test_transcription_state_values() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "transcription");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "audio-pending",
        "text with audio",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("audio_pending"),
    )?;
    insert_test_item_with_status(
        &tx,
        "transcription-unsupported",
        "audio",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("transcription_unsupported"),
    )?;
    insert_test_item_with_status(
        &tx,
        "transcription-failed",
        "audio",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("transcription_failed"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let audio_pending = ItemStatus::load(&tx, "audio-pending")?.expect("item should exist");
    let unsupported =
        ItemStatus::load(&tx, "transcription-unsupported")?.expect("item should exist");
    let failed = ItemStatus::load(&tx, "transcription-failed")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        audio_pending.transcription_state,
        TranscriptionState::AudioPending
    );
    assert_eq!(
        unsupported.transcription_state,
        TranscriptionState::TranscriptionUnsupported
    );
    assert_eq!(
        failed.transcription_state,
        TranscriptionState::TranscriptionFailed
    );

    Ok(())
}

#[test]
fn test_text_capture_without_transcription() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "textonly");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "text-only",
        "pure text capture",
        "saved_local",
        "not_configured",
        "processed",
        Some("not_applicable"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "text-only")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        status.transcription_state,
        TranscriptionState::NotApplicable
    );
    assert_eq!(status.save_state, SaveState::SavedLocal);
    Ok(())
}

#[test]
fn test_save_and_processing_independent_from_reminder() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "saveindep");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "saved-processed-no-reminder",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;

    insert_test_item_with_status(
        &tx,
        "saved-processed-unscheduled-reminder",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "saved-processed-unscheduled-reminder",
        "not_scheduled_yet",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        None,
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let no_reminder =
        ItemStatus::load(&tx, "saved-processed-no-reminder")?.expect("item should exist");
    let unscheduled_reminder =
        ItemStatus::load(&tx, "saved-processed-unscheduled-reminder")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(no_reminder.save_state, SaveState::SavedLocal);
    assert_eq!(no_reminder.processing_state, ProcessingState::Processed);
    assert_eq!(no_reminder.sync_state, SyncState::NotConfigured);
    assert_eq!(no_reminder.reminder_request_state, None);
    assert_eq!(no_reminder.reminder_schedule_state, None);

    assert_eq!(unscheduled_reminder.save_state, SaveState::SavedLocal);
    assert_eq!(
        unscheduled_reminder.processing_state,
        ProcessingState::Processed
    );
    assert_eq!(
        unscheduled_reminder.reminder_request_state,
        Some(ReminderRequestState::NotScheduledYet)
    );
    assert_eq!(
        unscheduled_reminder.reminder_schedule_state,
        Some(ReminderScheduleState::NotScheduled)
    );

    Ok(())
}

#[test]
fn test_distinguishable_reminder_error_states() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "reminderr");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "expired",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "expired",
        "unschedulable",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        Some("time_in_past"),
    )?;

    insert_test_item_with_status(
        &tx,
        "ambiguous",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "ambiguous",
        "not_scheduled_yet",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        None,
    )?;

    insert_test_item_with_status(
        &tx,
        "permission-denied",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "permission-denied",
        "unschedulable",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        Some("permission_denied"),
    )?;

    tx.commit()?;

    let tx = db.transaction()?;
    let expired = ItemStatus::load(&tx, "expired")?.expect("item should exist");
    let ambiguous = ItemStatus::load(&tx, "ambiguous")?.expect("item should exist");
    let perm_denied = ItemStatus::load(&tx, "permission-denied")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        expired.reminder_request_state,
        Some(ReminderRequestState::Unschedulable)
    );
    assert_eq!(
        ambiguous.reminder_request_state,
        Some(ReminderRequestState::NotScheduledYet)
    );
    assert_eq!(
        perm_denied.reminder_request_state,
        Some(ReminderRequestState::Unschedulable)
    );

    assert_ne!(
        expired.reminder_request_state,
        ambiguous.reminder_request_state
    );
    assert_ne!(
        ambiguous.reminder_request_state,
        perm_denied.reminder_request_state
    );

    assert_eq!(
        expired.unschedulable_reason,
        Some(UnschedulableReason::TimeInPast)
    );
    assert_eq!(
        perm_denied.unschedulable_reason,
        Some(UnschedulableReason::PermissionDenied)
    );
    assert_eq!(ambiguous.unschedulable_reason, None);

    assert_ne!(
        expired.unschedulable_reason, perm_denied.unschedulable_reason,
        "expired and permission-denied must be distinguishable"
    );

    assert_eq!(
        expired.reminder_delivery_state,
        Some(ReminderDeliveryState::Unknown)
    );
    assert_eq!(
        ambiguous.reminder_delivery_state,
        Some(ReminderDeliveryState::Unknown)
    );
    assert_eq!(
        perm_denied.reminder_delivery_state,
        Some(ReminderDeliveryState::Unknown)
    );

    Ok(())
}

fn enqueue_for_item(
    db: &mut Database,
    item_id: &str,
    job_type: &str,
    job_schema_version: i32,
    now: DateTime<Utc>,
) -> Result<Job> {
    // Jobs pinned to a missing profile are retired at claim (V03), so the pin must exist.
    db.conn().execute(
        "INSERT OR IGNORE INTO provider_profiles (
            profile_id, profile_version, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at
        ) VALUES ('profile', 'profile-1', 'anthropic', 'synthetic-model', 30, '{}', '[]', '{}', ?)",
        [now.to_rfc3339()],
    )?;
    enqueue_job(
        db,
        format!("job-{}-{}", item_id, job_type),
        item_id.to_string(),
        job_type.to_string(),
        0,
        Some("profile-1".to_string()),
        Some("request-1".to_string()),
        job_schema_version,
        now,
    )
}

fn insert_unprocessed_text_item(db: &mut Database, item_id: &str) -> Result<()> {
    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        item_id,
        "text",
        "saved_local",
        "not_configured",
        "unprocessed",
        Some("not_applicable"),
    )?;
    tx.commit()?;
    Ok(())
}

fn load_status(db: &mut Database, item_id: &str) -> Result<ItemStatus> {
    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, item_id)?.expect("item should exist");
    tx.commit()?;
    Ok(status)
}

fn fail_claimed_job(db: &mut Database, now: DateTime<Utc>, failure_reason: &str) -> Result<Job> {
    let claimed = claim_job_with_lease(db, Duration::seconds(60), now)?.expect("job is claimable");
    fail_job_with_backoff(
        db,
        &claimed.job_id,
        failure_reason.to_string(),
        30,
        600,
        now,
        claimed.attempt_count,
    )?;
    Ok(get_job(db, &claimed.job_id)?.expect("job exists"))
}

#[test]
fn test_outage_retry_distinguishable_from_never_attempted_and_config_wait() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "outage");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    for item_id in [
        "outage-retry",
        "unauthorized-wait",
        "unsupported-version-wait",
        "never-attempted",
    ] {
        insert_unprocessed_text_item(&mut db, item_id)?;
    }

    // Outage: a transient failure recorded through the production queue API. Each job is
    // driven to its state before the next is enqueued so the oldest-first claim picks it.
    enqueue_for_item(&mut db, "outage-retry", JOB_TYPE_INTERPRET, 1, instant)?;
    let retried = fail_claimed_job(&mut db, instant, "transient")?;
    assert_eq!(retried.status, JobStatus::Queued);
    assert!(retried.next_attempt_at.is_some());

    // Configuration wait: an unauthorized failure on the same queue API.
    enqueue_for_item(&mut db, "unauthorized-wait", JOB_TYPE_INTERPRET, 1, instant)?;
    fail_claimed_job(&mut db, instant, "unauthorized")?;

    // Configuration wait: the queue itself stops a job with an unsupported schema version.
    enqueue_for_item(
        &mut db,
        "unsupported-version-wait",
        JOB_TYPE_INTERPRET,
        99,
        instant,
    )?;
    assert!(claim_job_with_lease(&mut db, Duration::seconds(60), instant)?.is_none());
    let stopped = get_job(&db, "job-unsupported-version-wait-interpret")?.expect("job exists");
    assert_eq!(stopped.status, JobStatus::Failed);

    // Never attempted: freshly queued job.
    enqueue_for_item(&mut db, "never-attempted", JOB_TYPE_INTERPRET, 1, instant)?;

    let outage = load_status(&mut db, "outage-retry")?;
    let unauthorized = load_status(&mut db, "unauthorized-wait")?;
    let unsupported = load_status(&mut db, "unsupported-version-wait")?;
    let never_attempted = load_status(&mut db, "never-attempted")?;

    for status in [&outage, &unauthorized, &unsupported, &never_attempted] {
        assert_eq!(status.processing_state, ProcessingState::Unprocessed);
        assert!(!status.claims_user_attention());
    }

    assert_eq!(
        outage.processing_job_status,
        Some(ProcessingJobStatus::RetryingAfterTransient)
    );
    assert_eq!(
        unauthorized.processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    assert_eq!(
        unsupported.processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    assert_eq!(never_attempted.processing_job_status, None);

    Ok(())
}

#[test]
fn test_only_interpretation_jobs_drive_processing_job_status() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "jobtype");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    insert_unprocessed_text_item(&mut db, "item-1")?;
    enqueue_for_item(&mut db, "item-1", JOB_TYPE_INTERPRET, 1, instant)?;
    fail_claimed_job(&mut db, instant, "transient")?;

    // A newer job of another type for the same item must not change the interpretation status.
    let later = instant + Duration::seconds(5);
    enqueue_for_item(
        &mut db,
        "item-1",
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
        1,
        later,
    )?;
    assert_eq!(
        load_status(&mut db, "item-1")?.processing_job_status,
        Some(ProcessingJobStatus::RetryingAfterTransient)
    );

    // Once the interpretation retry succeeds the wait disappears.
    let retry_time = instant + Duration::seconds(600);
    let claimed =
        claim_job_with_lease(&mut db, Duration::seconds(60), retry_time)?.expect("retry is due");
    assert_eq!(claimed.job_type, JOB_TYPE_INTERPRET);
    complete_job(&mut db, &claimed.job_id, claimed.attempt_count)?;
    assert_eq!(load_status(&mut db, "item-1")?.processing_job_status, None);

    Ok(())
}

#[test]
fn test_unschedulable_reason_must_match_request_state() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "reasonpair");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "no-reason",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("not_applicable"),
    )?;
    insert_test_reminder(
        &tx,
        "no-reason",
        "unschedulable",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        None,
    )?;
    insert_test_item_with_status(
        &tx,
        "stray-reason",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("not_applicable"),
    )?;
    insert_test_reminder(
        &tx,
        "stray-reason",
        "resolved",
        "scheduled",
        "unknown",
        "not_acknowledged",
        Some("time_in_past"),
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    assert!(ItemStatus::load(&tx, "no-reason").is_err());
    assert!(ItemStatus::load(&tx, "stray-reason").is_err());
    tx.commit()?;
    Ok(())
}

#[test]
fn test_attention_predicate_only_acknowledged_claims_attention() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = temp_db_path(&tmpdir, "attention");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    // Acknowledged: claims attention
    insert_test_item_with_status(
        &tx,
        "acknowledged",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "acknowledged",
        "resolved",
        "scheduled",
        "opened",
        "acknowledged",
        None,
    )?;

    // Delivered but not acknowledged: does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "delivered-not-ack",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "delivered-not-ack",
        "resolved",
        "scheduled",
        "delivered",
        "not_acknowledged",
        None,
    )?;

    // Opened but not acknowledged: does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "opened-not-ack",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "opened-not-ack",
        "resolved",
        "scheduled",
        "opened",
        "not_acknowledged",
        None,
    )?;

    // Unschedulable (expired): does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "unschedulable",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "unschedulable",
        "unschedulable",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        Some("time_in_past"),
    )?;

    // Ambiguous: does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "ambiguous",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "ambiguous",
        "not_scheduled_yet",
        "not_scheduled",
        "unknown",
        "not_acknowledged",
        None,
    )?;

    // Pending schedule: does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "pending-schedule",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "pending-schedule",
        "resolved",
        "pending_schedule",
        "unknown",
        "not_acknowledged",
        None,
    )?;

    // Schedule failed: does NOT claim attention
    insert_test_item_with_status(
        &tx,
        "schedule-failed",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "schedule-failed",
        "resolved",
        "schedule_failed",
        "unknown",
        "not_acknowledged",
        None,
    )?;

    tx.commit()?;

    let tx = db.transaction()?;
    let acknowledged = ItemStatus::load(&tx, "acknowledged")?.expect("item should exist");
    let delivered_not_ack = ItemStatus::load(&tx, "delivered-not-ack")?.expect("item should exist");
    let opened_not_ack = ItemStatus::load(&tx, "opened-not-ack")?.expect("item should exist");
    let unschedulable = ItemStatus::load(&tx, "unschedulable")?.expect("item should exist");
    let ambiguous = ItemStatus::load(&tx, "ambiguous")?.expect("item should exist");
    let pending_schedule = ItemStatus::load(&tx, "pending-schedule")?.expect("item should exist");
    let schedule_failed = ItemStatus::load(&tx, "schedule-failed")?.expect("item should exist");
    tx.commit()?;

    // Only acknowledged claims attention
    assert!(acknowledged.claims_user_attention());
    assert!(!delivered_not_ack.claims_user_attention());
    assert!(!opened_not_ack.claims_user_attention());
    assert!(!unschedulable.claims_user_attention());
    assert!(!ambiguous.claims_user_attention());
    assert!(!pending_schedule.claims_user_attention());
    assert!(!schedule_failed.claims_user_attention());

    Ok(())
}
