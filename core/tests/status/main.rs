use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;

use ohand_core::domain::status::{
    ItemStatus, ProcessingState, ReminderAcknowledgmentState, ReminderDeliveryState,
    ReminderRequestState, ReminderScheduleState, SaveState, SyncState, TranscriptionState,
    UnschedulableReason,
};
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

fn temp_db_path(name: &str) -> String {
    format!(
        "{}/ohand_test_{}_{}.db",
        std::env::temp_dir().display(),
        name,
        Uuid::new_v4()
    )
}

#[test]
fn test_load_item_status() -> Result<()> {
    let path = temp_db_path("load");
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
    let path = temp_db_path("m1sync");
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
    let path = temp_db_path("processing");
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
    let path = temp_db_path("notfound");
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
    let path = temp_db_path("savestates");
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
    let path = temp_db_path("transcription");
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
    let path = temp_db_path("textonly");
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
    let path = temp_db_path("saveindep");
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
    let path = temp_db_path("reminderr");
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

#[test]
fn test_reminder_delivery_never_claims_attention() -> Result<()> {
    let path = temp_db_path("delivery");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "delivered",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "delivered",
        "resolved",
        "scheduled",
        "delivered",
        "not_acknowledged",
        None,
    )?;

    insert_test_item_with_status(
        &tx,
        "opened",
        "text",
        "saved_local",
        "not_configured",
        "processed",
        Some("transcribed"),
    )?;
    insert_test_reminder(
        &tx,
        "opened",
        "resolved",
        "scheduled",
        "opened",
        "acknowledged",
        None,
    )?;

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
        Some("permission_denied"),
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

    tx.commit()?;

    let tx = db.transaction()?;
    let delivered = ItemStatus::load(&tx, "delivered")?.expect("item should exist");
    let opened = ItemStatus::load(&tx, "opened")?.expect("item should exist");
    let unschedulable = ItemStatus::load(&tx, "unschedulable")?.expect("item should exist");
    let ambiguous = ItemStatus::load(&tx, "ambiguous")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        delivered.reminder_delivery_state,
        Some(ReminderDeliveryState::Delivered)
    );
    assert_eq!(
        opened.reminder_delivery_state,
        Some(ReminderDeliveryState::Opened)
    );

    assert_eq!(
        delivered.reminder_acknowledgment_state,
        Some(ReminderAcknowledgmentState::NotAcknowledged)
    );
    assert_eq!(
        opened.reminder_acknowledgment_state,
        Some(ReminderAcknowledgmentState::Acknowledged)
    );

    assert!(
        unschedulable.never_claims_attention(),
        "unschedulable state should not claim attention"
    );
    assert!(
        ambiguous.never_claims_attention(),
        "ambiguous state should not claim attention"
    );

    Ok(())
}
