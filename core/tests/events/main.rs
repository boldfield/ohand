use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Barrier};
use std::thread;

use ohand_core::store::events::{
    delete_events_for_item, get_event, get_events_for_item, save_event, save_event_in_tx, Event,
    EventType,
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
        "{}/test_events_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn make_test_event(event_id: &str, item_id: &str, revision: i32, event_type: EventType) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        event_type,
        "2026-01-15T10:30:00Z".to_string(),
    )
    .unwrap()
}

fn insert_test_item(tx: &rusqlite::Transaction<'_>, item_id: &str) -> Result<()> {
    let capture_id = format!("cap-{}", item_id);

    // First insert the capture (required by items foreign key)
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            &capture_id,
            Some("test"),
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

    // Then insert the item
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        rusqlite::params![
            item_id,
            &capture_id,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z",
        ],
    )?;
    Ok(())
}

#[test]
fn test_save_and_retrieve_event() -> Result<()> {
    let path = temp_db_path("save_retrieve");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-1", "item-1", 0, EventType::Completion);
    let saved = save_event(&mut db, &event)?;

    assert_eq!(saved, event);

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-1")?;
    assert_eq!(retrieved, Some(event));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_idempotent_save_returns_same_record() -> Result<()> {
    let path = temp_db_path("idempotent");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-1", "item-1", 0, EventType::Completion);

    let saved1 = save_event(&mut db, &event)?;
    let saved2 = save_event(&mut db, &event)?;

    assert_eq!(saved1, saved2);
    assert_eq!(saved1, event);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_conflict_on_different_content_same_id() -> Result<()> {
    let path = temp_db_path("conflict");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    insert_test_item(&tx, "item-2")?;
    tx.commit()?;

    let event1 = make_test_event("evt-1", "item-1", 0, EventType::Completion);
    save_event(&mut db, &event1)?;

    let event2 = make_test_event("evt-1", "item-2", 0, EventType::Completion);
    let result = save_event(&mut db, &event2);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("already exists with different content"));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_stale_update_rejected() -> Result<()> {
    let path = temp_db_path("stale_update");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let newer_event = make_test_event("evt-new", "item-1", 2, EventType::Completion);
    save_event(&mut db, &newer_event)?;

    let stale_event = make_test_event("evt-stale", "item-1", 0, EventType::Cancellation);
    let result = save_event(&mut db, &stale_event);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Stale event"));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_event_retries_have_one_effect() -> Result<()> {
    let path = temp_db_path("one_effect");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-1", "item-1", 0, EventType::Completion);

    save_event(&mut db, &event.clone())?;
    save_event(&mut db, &event.clone())?;
    save_event(&mut db, &event.clone())?;

    let tx = db.transaction()?;
    let events = get_events_for_item(&tx, "item-1")?;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0], event);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_correction_event_type() -> Result<()> {
    let path = temp_db_path("correction_type");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let correction_event = make_test_event("evt-correct", "item-1", 0, EventType::Correction);
    save_event(&mut db, &correction_event)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-correct")?;
    let evt = retrieved.unwrap();

    assert_eq!(evt.event_type, EventType::Correction);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_get_events_for_item_empty() -> Result<()> {
    let path = temp_db_path("empty_item");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    let events = get_events_for_item(&tx, "nonexistent")?;

    assert!(events.is_empty());

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_get_events_for_item_ordered() -> Result<()> {
    let path = temp_db_path("ordered_events");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let evt0 = make_test_event("evt-0", "item-1", 0, EventType::Correction);
    let evt1 = make_test_event("evt-1", "item-1", 1, EventType::Cancellation);
    let evt2 = make_test_event("evt-2", "item-1", 2, EventType::Completion);

    save_event(&mut db, &evt0)?;
    save_event(&mut db, &evt1)?;
    save_event(&mut db, &evt2)?;

    let tx = db.transaction()?;
    let events = get_events_for_item(&tx, "item-1")?;

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].revision, 0);
    assert_eq!(events[1].revision, 1);
    assert_eq!(events[2].revision, 2);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_multiple_items_independent() -> Result<()> {
    let path = temp_db_path("multiple_items");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    insert_test_item(&tx, "item-2")?;
    tx.commit()?;

    let evt1_i1 = make_test_event("evt-1-i1", "item-1", 0, EventType::Completion);
    let evt1_i2 = make_test_event("evt-1-i2", "item-2", 0, EventType::Cancellation);

    save_event(&mut db, &evt1_i1)?;
    save_event(&mut db, &evt1_i2)?;

    let tx = db.transaction()?;
    let events_i1 = get_events_for_item(&tx, "item-1")?;
    let events_i2 = get_events_for_item(&tx, "item-2")?;

    assert_eq!(events_i1.len(), 1);
    assert_eq!(events_i1[0].item_id, "item-1");

    assert_eq!(events_i2.len(), 1);
    assert_eq!(events_i2[0].item_id, "item-2");

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_user_corrections_separate_from_source() -> Result<()> {
    let path = temp_db_path("corrections_separate");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let correction = make_test_event("evt-correct", "item-1", 0, EventType::Correction);
    save_event(&mut db, &correction)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-correct")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    assert_eq!(evt.event_type, EventType::Correction);
    assert_eq!(evt.revision, 0);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_notes_ideas_actions_distinct() -> Result<()> {
    let path = temp_db_path("intent_distinct");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let correction = make_test_event("evt-correct", "item-1", 0, EventType::Correction);
    let completion = make_test_event("evt-done", "item-1", 1, EventType::Completion);
    let cancellation = make_test_event("evt-cancel", "item-1", 2, EventType::Cancellation);

    save_event(&mut db, &correction)?;
    save_event(&mut db, &completion)?;
    save_event(&mut db, &cancellation)?;

    let tx = db.transaction()?;
    let events = get_events_for_item(&tx, "item-1")?;

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_type, EventType::Correction);
    assert_eq!(events[1].event_type, EventType::Completion);
    assert_eq!(events[2].event_type, EventType::Cancellation);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_delete_events_for_item() -> Result<()> {
    let path = temp_db_path("delete_events");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    insert_test_item(&tx, "item-2")?;
    tx.commit()?;

    let evt1 = make_test_event("evt-1", "item-1", 0, EventType::Correction);
    let evt2 = make_test_event("evt-2", "item-1", 1, EventType::Completion);
    let evt3 = make_test_event("evt-3", "item-2", 0, EventType::Cancellation);

    save_event(&mut db, &evt1)?;
    save_event(&mut db, &evt2)?;
    save_event(&mut db, &evt3)?;

    {
        let tx = db.transaction()?;
        delete_events_for_item(&tx, "item-1")?;
        tx.commit()?;
    }

    let tx = db.transaction()?;
    let events_i1 = get_events_for_item(&tx, "item-1")?;
    let events_i2 = get_events_for_item(&tx, "item-2")?;

    assert!(events_i1.is_empty());
    assert_eq!(events_i2.len(), 1);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_suggestion_control_event() -> Result<()> {
    let path = temp_db_path("suggestion_control");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let control_event = make_test_event("evt-control", "item-1", 0, EventType::SuggestionControl);
    save_event(&mut db, &control_event)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-control")?;

    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().event_type, EventType::SuggestionControl);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_revision_zero_allowed() -> Result<()> {
    let path = temp_db_path("revision_zero");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-1", "item-1", 0, EventType::Correction);
    let saved = save_event(&mut db, &event)?;

    assert_eq!(saved.revision, 0);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_high_revision_numbers() -> Result<()> {
    let path = temp_db_path("high_revision");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-1", "item-1", 9999, EventType::Completion);
    let saved = save_event(&mut db, &event)?;

    assert_eq!(saved.revision, 9999);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_clear_error_states_on_conflict() -> Result<()> {
    let path = temp_db_path("error_clear");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let evt1 = make_test_event("evt-1", "item-1", 0, EventType::Completion);
    save_event(&mut db, &evt1)?;

    let evt_conflict = make_test_event("evt-1", "item-1", 0, EventType::Cancellation);
    let result = save_event(&mut db, &evt_conflict);

    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("evt-1"));
    assert!(err_msg.contains("different content"));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_concurrent_saves_idempotent() -> Result<()> {
    let path = temp_db_path("concurrent_saves");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);

    let _ = make_test_db(&path, instant)?;

    {
        let mut db = make_test_db(&path, instant)?;
        let tx = db.transaction()?;
        insert_test_item(&tx, "item-1")?;
        tx.commit()?;
    }

    let event = make_test_event("evt-concurrent", "item-1", 0, EventType::Completion);

    let num_threads = 6;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = vec![];

    for _ in 0..num_threads {
        let path_clone = path.clone();
        let event_clone = event.clone();
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || -> Result<Event> {
            let mut db = make_test_db(&path_clone, instant)?;
            barrier_clone.wait();
            let result = save_event(&mut db, &event_clone)?;
            Ok(result)
        });

        handles.push(handle);
    }

    let mut results = vec![];
    for handle in handles {
        match handle.join() {
            Ok(Ok(evt)) => results.push(evt),
            Ok(Err(e)) => return Err(anyhow::anyhow!("Thread failed: {}", e)),
            Err(_) => return Err(anyhow::anyhow!("Thread panicked")),
        }
    }

    assert_eq!(results.len(), num_threads);
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result, &event, "Thread {} got different event", i);
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_save_event_in_transaction() -> Result<()> {
    let path = temp_db_path("save_in_tx");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_test_event("evt-tx", "item-1", 0, EventType::Correction);

    {
        let tx = db.transaction()?;
        let saved = save_event_in_tx(&tx, &event)?;
        assert_eq!(saved, event);
        tx.commit()?;
    }

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-tx")?;
    assert_eq!(retrieved, Some(event));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_empty_event_id_rejected() -> Result<()> {
    let result = Event::new(
        "".to_string(),
        "item-1".to_string(),
        0,
        EventType::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("event_id"));

    Ok(())
}

#[test]
fn test_empty_item_id_rejected() -> Result<()> {
    let result = Event::new(
        "evt-1".to_string(),
        "".to_string(),
        0,
        EventType::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("item_id"));

    Ok(())
}

#[test]
fn test_negative_revision_rejected() -> Result<()> {
    let result = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        -1,
        EventType::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("revision"));

    Ok(())
}

#[test]
fn test_compare_and_set_same_revision_different_id() -> Result<()> {
    let path = temp_db_path("cas_same_rev");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    insert_test_item(&tx, "item-2")?;
    tx.commit()?;

    let evt1 = make_test_event("evt-1", "item-1", 0, EventType::Completion);
    save_event(&mut db, &evt1)?;

    let evt2 = make_test_event("evt-2", "item-2", 0, EventType::Completion);
    let result = save_event(&mut db, &evt2);

    assert!(result.is_ok());

    let _ = std::fs::remove_file(&path);
    Ok(())
}
