use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Barrier};
use std::thread;

use ohand_core::store::events::{
    delete_events_for_item, get_event, get_events_for_item, save_event, save_event_in_tx,
    Correction, Event, EventPayload, EventType, SuggestionControlPayload,
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

fn make_completion_event(event_id: &str, item_id: &str, revision: i32) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    )
    .unwrap()
}

fn make_cancellation_event(event_id: &str, item_id: &str, revision: i32) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Cancellation,
        EventPayload::Cancellation,
        "2026-01-15T10:30:00Z".to_string(),
    )
    .unwrap()
}

fn make_correction_event(
    event_id: &str,
    item_id: &str,
    revision: i32,
    kind: &str,
    old_value: Option<&str>,
    new_value: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: kind.to_string(),
            old_value: old_value.map(|s| s.to_string()),
            new_value: new_value.to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )
    .unwrap()
}

fn make_suggestion_control_event(
    event_id: &str,
    item_id: &str,
    revision: i32,
    kind: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::SuggestionControl,
        EventPayload::SuggestionControl(SuggestionControlPayload {
            kind: kind.to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )
    .unwrap()
}

fn insert_test_item(tx: &rusqlite::Transaction<'_>, item_id: &str) -> Result<()> {
    let capture_id = format!("cap-{}", item_id);

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

    let event = make_completion_event("evt-1", "item-1", 0);
    let saved = save_event(&mut db, &event, 0)?;

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

    let event = make_completion_event("evt-1", "item-1", 0);

    let saved1 = save_event(&mut db, &event, 0)?;
    let saved2 = save_event(&mut db, &event, 0)?;

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

    let event1 = make_completion_event("evt-1", "item-1", 0);
    save_event(&mut db, &event1, 0)?;

    let event2 = make_completion_event("evt-1", "item-2", 0);
    let result = save_event(&mut db, &event2, 0);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("already exists with different content"));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_compare_and_set_stale_revision_rejected() -> Result<()> {
    let path = temp_db_path("cas_stale");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event1 = make_completion_event("evt-1", "item-1", 0);
    save_event(&mut db, &event1, 0)?;

    let tx = db.transaction()?;
    let item_revision: i32 = tx.query_row(
        "SELECT revision FROM items WHERE item_id = ?",
        ["item-1"],
        |row| row.get(0),
    )?;
    tx.commit()?;

    assert_eq!(
        item_revision, 1,
        "Item revision should be 1 after first event"
    );

    let event2 = make_cancellation_event("evt-2", "item-1", 1);
    let result = save_event(&mut db, &event2, 0);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Stale write"));

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

    let event = make_completion_event("evt-1", "item-1", 0);

    save_event(&mut db, &event.clone(), 0)?;
    save_event(&mut db, &event.clone(), 0)?;
    save_event(&mut db, &event.clone(), 0)?;

    let tx = db.transaction()?;
    let events = get_events_for_item(&tx, "item-1")?;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0], event);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_completion_changes_lifecycle_state() -> Result<()> {
    let path = temp_db_path("completion_lifecycle");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_completion_event("evt-1", "item-1", 0);
    save_event(&mut db, &event, 0)?;

    let tx = db.transaction()?;
    let lifecycle_state: String = tx.query_row(
        "SELECT lifecycle_state FROM items WHERE item_id = ?",
        ["item-1"],
        |row| row.get(0),
    )?;
    tx.commit()?;

    assert_eq!(lifecycle_state, "completed");

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_cancellation_changes_lifecycle_state() -> Result<()> {
    let path = temp_db_path("cancellation_lifecycle");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_cancellation_event("evt-1", "item-1", 0);
    save_event(&mut db, &event, 0)?;

    let tx = db.transaction()?;
    let lifecycle_state: String = tx.query_row(
        "SELECT lifecycle_state FROM items WHERE item_id = ?",
        ["item-1"],
        |row| row.get(0),
    )?;
    tx.commit()?;

    assert_eq!(lifecycle_state, "cancelled");

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

    let correction = make_correction_event(
        "evt-correct",
        "item-1",
        0,
        "text",
        Some("old text"),
        "new text",
    );
    save_event(&mut db, &correction, 0)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-correct")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    assert_eq!(evt.event_type, EventType::Correction);

    let corrections_row: (String, String, Option<String>, String) = tx.query_row(
        "SELECT correction_id, kind, old_value, new_value FROM corrections WHERE item_id = ?",
        ["item-1"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    assert_eq!(corrections_row.1, "text");
    assert_eq!(corrections_row.2, Some("old text".to_string()));
    assert_eq!(corrections_row.3, "new text");

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_correction_with_no_old_value() -> Result<()> {
    let path = temp_db_path("correction_no_old");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let correction = make_correction_event("evt-correct", "item-1", 0, "text", None, "new text");
    save_event(&mut db, &correction, 0)?;

    let tx = db.transaction()?;
    let corrections_row: (String, Option<String>, String) = tx.query_row(
        "SELECT kind, old_value, new_value FROM corrections WHERE item_id = ?",
        ["item-1"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(corrections_row.0, "text");
    assert_eq!(corrections_row.1, None);
    assert_eq!(corrections_row.2, "new text");

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

    let correction =
        make_correction_event("evt-correct", "item-1", 0, "note", None, "this is a note");
    let completion = make_completion_event("evt-done", "item-1", 1);
    let cancellation = make_cancellation_event("evt-cancel", "item-1", 2);

    save_event(&mut db, &correction, 0)?;
    save_event(&mut db, &completion, 1)?;
    save_event(&mut db, &cancellation, 2)?;

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
fn test_suggestion_control_not_now_kind() -> Result<()> {
    let path = temp_db_path("suggestion_control_not_now");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let control = make_suggestion_control_event("evt-control", "item-1", 0, "not_now");
    save_event(&mut db, &control, 0)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-control")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    assert_eq!(evt.event_type, EventType::SuggestionControl);
    if let EventPayload::SuggestionControl(payload) = evt.payload {
        assert_eq!(payload.kind, "not_now");
    } else {
        panic!("Expected SuggestionControl payload");
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_suggestion_control_stop_suggesting_kind() -> Result<()> {
    let path = temp_db_path("suggestion_control_stop");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let control = make_suggestion_control_event("evt-control", "item-1", 0, "stop_suggesting");
    save_event(&mut db, &control, 0)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-control")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    if let EventPayload::SuggestionControl(payload) = evt.payload {
        assert_eq!(payload.kind, "stop_suggesting");
    } else {
        panic!("Expected SuggestionControl payload");
    }

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

    let evt1 = make_correction_event("evt-1", "item-1", 0, "text", Some("old"), "new");
    let evt2 = make_completion_event("evt-2", "item-1", 1);
    let evt3 = make_cancellation_event("evt-3", "item-2", 0);

    save_event(&mut db, &evt1, 0)?;
    save_event(&mut db, &evt2, 1)?;
    save_event(&mut db, &evt3, 0)?;

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
fn test_get_events_for_item_ordered() -> Result<()> {
    let path = temp_db_path("ordered_events");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let evt0 = make_correction_event("evt-0", "item-1", 0, "text", None, "val");
    let evt1 = make_cancellation_event("evt-1", "item-1", 1);
    let evt2 = make_completion_event("evt-2", "item-1", 2);

    save_event(&mut db, &evt0, 0)?;
    save_event(&mut db, &evt1, 1)?;
    save_event(&mut db, &evt2, 2)?;

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

    let evt1_i1 = make_completion_event("evt-1-i1", "item-1", 0);
    let evt1_i2 = make_cancellation_event("evt-1-i2", "item-2", 0);

    save_event(&mut db, &evt1_i1, 0)?;
    save_event(&mut db, &evt1_i2, 0)?;

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
fn test_save_event_in_transaction() -> Result<()> {
    let path = temp_db_path("save_in_tx");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let event = make_correction_event("evt-tx", "item-1", 0, "text", None, "val");

    {
        let tx = db.transaction()?;
        let saved = save_event_in_tx(&tx, &event, 0)?;
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
        EventPayload::Completion,
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
        EventPayload::Completion,
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
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("revision"));

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

    let event = make_completion_event("evt-concurrent", "item-1", 0);

    let num_threads = 3;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = vec![];

    for _ in 0..num_threads {
        let path_clone = path.clone();
        let event_clone = event.clone();
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || -> Result<Event> {
            let mut db = make_test_db(&path_clone, instant)?;
            barrier_clone.wait();
            let result = save_event(&mut db, &event_clone, 0)?;
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
