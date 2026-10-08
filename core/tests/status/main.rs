use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use ohand_core::domain::status::{
    ItemStatus, ProcessingState, SaveState, SyncState, TranscriptionState,
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

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_status_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn insert_test_item_with_status(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
    capture_text: &str,
    save_state: &str,
    sync_state: &str,
    processing_state: &str,
    transcription_state: &str,
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

#[test]
fn test_load_item_status() -> Result<()> {
    let path = temp_db_path("load_status");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "item-1",
        "hello world",
        "saved",
        "not_configured",
        "processed",
        "transcribed",
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "item-1")?;
    tx.commit()?;

    let status = status.expect("item should exist");
    assert_eq!(status.item_id, "item-1");
    assert_eq!(status.save_state, SaveState::Saved);
    assert_eq!(status.sync_state, SyncState::NotConfigured);
    assert_eq!(status.processing_state, ProcessingState::Processed);
    assert_eq!(status.transcription_state, TranscriptionState::Transcribed);

    Ok(())
}

#[test]
fn test_m1_sync_not_configured() -> Result<()> {
    let path = temp_db_path("m1_sync");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "item-1",
        "capture text",
        "saved",
        "not_configured",
        "unprocessed",
        "unprocessed",
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
fn test_distinguishable_processing_errors() -> Result<()> {
    let path = temp_db_path("processing_errors");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "permission-denied",
        "text 1",
        "saved",
        "not_configured",
        "permission_denied",
        "not_applicable",
    )?;
    insert_test_item_with_status(
        &tx,
        "provider-outage",
        "text 2",
        "saved",
        "not_configured",
        "provider_unavailable",
        "not_applicable",
    )?;
    insert_test_item_with_status(
        &tx,
        "pending-ambiguity",
        "text 3",
        "saved",
        "not_configured",
        "awaiting_authorization",
        "not_applicable",
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let perm_denied = ItemStatus::load(&tx, "permission-denied")?.expect("item should exist");
    let outage = ItemStatus::load(&tx, "provider-outage")?.expect("item should exist");
    let ambiguity = ItemStatus::load(&tx, "pending-ambiguity")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        perm_denied.processing_state,
        ProcessingState::PermissionDenied
    );
    assert_eq!(
        outage.processing_state,
        ProcessingState::ProviderUnavailable
    );
    assert_eq!(
        ambiguity.processing_state,
        ProcessingState::AwaitingAuthorization
    );

    assert_ne!(perm_denied.processing_state, outage.processing_state);
    assert_ne!(outage.processing_state, ambiguity.processing_state);

    Ok(())
}

#[test]
fn test_item_not_found() -> Result<()> {
    let path = temp_db_path("not_found");
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
    let path = temp_db_path("save_states");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "saved",
        "text",
        "saved",
        "not_configured",
        "unprocessed",
        "unprocessed",
    )?;
    insert_test_item_with_status(
        &tx,
        "pending",
        "text",
        "pending",
        "not_configured",
        "unprocessed",
        "unprocessed",
    )?;
    insert_test_item_with_status(
        &tx,
        "failed",
        "text",
        "failed",
        "not_configured",
        "unprocessed",
        "unprocessed",
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let saved = ItemStatus::load(&tx, "saved")?.expect("item should exist");
    let pending = ItemStatus::load(&tx, "pending")?.expect("item should exist");
    let failed = ItemStatus::load(&tx, "failed")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(saved.save_state, SaveState::Saved);
    assert_eq!(pending.save_state, SaveState::Pending);
    assert_eq!(failed.save_state, SaveState::Failed);

    Ok(())
}

#[test]
fn test_transcription_state_values() -> Result<()> {
    let path = temp_db_path("transcription_states");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "not-applicable",
        "text only",
        "saved",
        "not_configured",
        "processed",
        "not_applicable",
    )?;
    insert_test_item_with_status(
        &tx,
        "language-not-supported",
        "audio",
        "saved",
        "not_configured",
        "processed",
        "language_not_supported",
    )?;
    insert_test_item_with_status(
        &tx,
        "permission-denied",
        "audio",
        "saved",
        "not_configured",
        "processed",
        "permission_denied",
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let not_applicable = ItemStatus::load(&tx, "not-applicable")?.expect("item should exist");
    let lang_not_supported =
        ItemStatus::load(&tx, "language-not-supported")?.expect("item should exist");
    let perm_denied = ItemStatus::load(&tx, "permission-denied")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(
        not_applicable.transcription_state,
        TranscriptionState::NotApplicable
    );
    assert_eq!(
        lang_not_supported.transcription_state,
        TranscriptionState::LanguageNotSupported
    );
    assert_eq!(
        perm_denied.transcription_state,
        TranscriptionState::PermissionDenied
    );

    Ok(())
}

#[test]
fn test_no_save_implies_no_reminder() -> Result<()> {
    // Acceptance criterion: neither a source save nor a model response implies an installed reminder.
    // This test demonstrates that SaveState and ProcessingState are independent facts.
    let path = temp_db_path("save_processing_independent");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item_with_status(
        &tx,
        "processed-no-save",
        "text",
        "pending",
        "not_configured",
        "processed",
        "transcribed",
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, "processed-no-save")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(status.save_state, SaveState::Pending);
    assert_eq!(status.processing_state, ProcessingState::Processed);

    assert_eq!(status.sync_state, SyncState::NotConfigured);

    Ok(())
}
