use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Barrier};
use std::thread;

use ohand_core::store::events::{
    delete_events_for_item, get_event, get_events_for_item, get_item_snapshot,
    item_can_carry_obligation, save_event, save_event_in_tx, Correction, CorrectionKind, Event,
    EventError, EventPayload, EventType, ItemType, SuggestionControlKind, SuggestionControlPayload,
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
    kind: CorrectionKind,
    old_value: Option<&str>,
    new_value: &str,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind,
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
    kind: SuggestionControlKind,
) -> Event {
    Event::new(
        event_id.to_string(),
        item_id.to_string(),
        revision,
        EventType::SuggestionControl,
        EventPayload::SuggestionControl(SuggestionControlPayload { kind }),
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
    assert!(matches!(
        result,
        Err(EventError::Conflict { ref event_id }) if event_id == "evt-1"
    ));

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

    // Try to save event2 with revision 0 (stale) expecting item revision 0 (stale)
    let event2 = make_cancellation_event("evt-2", "item-1", 0);
    let result = save_event(&mut db, &event2, 0);

    match result {
        Err(EventError::StaleRevision {
            item_id,
            expected,
            current,
        }) => {
            assert_eq!(item_id, "item-1");
            assert_eq!(expected, 0);
            assert_eq!(current.revision, 1);
            assert_eq!(current.lifecycle_state, "completed");
            assert_eq!(current.item_type, None);
        }
        other => panic!("expected StaleRevision, got {:?}", other),
    }

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

    // Verify original capture text
    let tx = db.transaction()?;
    let original_text: String = tx.query_row(
        "SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = ?)",
        ["item-1"],
        |row| row.get(0),
    )?;
    assert_eq!(original_text, "test");
    tx.commit()?;

    let correction = make_correction_event(
        "evt-correct",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("test"),
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
    assert_eq!(corrections_row.2, Some("test".to_string()));
    assert_eq!(corrections_row.3, "new text");

    // Verify original capture text is unchanged
    let capture_text: String = tx.query_row(
        "SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = ?)",
        ["item-1"],
        |row| row.get(0),
    )?;
    assert_eq!(capture_text, "test");

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_first_topic_assignment_has_no_old_value() -> Result<()> {
    let path = temp_db_path("topic_no_old");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let correction = make_correction_event(
        "evt-correct",
        "item-1",
        0,
        CorrectionKind::SessionTopic,
        None,
        "new topic",
    );
    save_event(&mut db, &correction, 0)?;

    let tx = db.transaction()?;
    let corrections_row: (String, Option<String>, String) = tx.query_row(
        "SELECT kind, old_value, new_value FROM corrections WHERE item_id = ?",
        ["item-1"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    assert_eq!(corrections_row.0, "session_topic");
    assert_eq!(corrections_row.1, None);
    assert_eq!(corrections_row.2, "new topic");

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn assert_no_obligation_rows(tx: &rusqlite::Transaction<'_>, item_id: &str) -> Result<()> {
    let reminders: i64 = tx.query_row(
        "SELECT COUNT(*) FROM reminders WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    let suggestions: i64 = tx.query_row(
        "SELECT COUNT(*) FROM suggestion_eligibility WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        reminders, 0,
        "events must not create reminders for {item_id}"
    );
    assert_eq!(
        suggestions, 0,
        "events must not create suggestion eligibility for {item_id}"
    );
    Ok(())
}

#[test]
fn test_notes_ideas_actions_distinct() -> Result<()> {
    let path = temp_db_path("intent_distinct");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let cases = [
        ("item-broad", ItemType::BroadIntention, false),
        ("item-note", ItemType::Note, false),
        ("item-idea", ItemType::Idea, false),
        ("item-action", ItemType::Action, true),
    ];

    let tx = db.transaction()?;
    for (item_id, _, _) in cases {
        insert_test_item(&tx, item_id)?;
    }
    tx.commit()?;

    // An untyped item cannot carry an obligation either.
    let tx = db.transaction()?;
    assert_eq!(
        get_item_snapshot(&tx, "item-note")?.unwrap().item_type,
        None
    );
    assert!(!item_can_carry_obligation(&tx, "item-note")?);
    tx.commit()?;

    for (item_id, item_type, _) in cases {
        let correction = make_correction_event(
            &format!("evt-{item_id}"),
            item_id,
            0,
            CorrectionKind::Type,
            None,
            item_type.as_str(),
        );
        save_event(&mut db, &correction, 0)?;
    }

    let tx = db.transaction()?;
    for (item_id, item_type, carries_obligation) in cases {
        let snapshot = get_item_snapshot(&tx, item_id)?.unwrap();
        assert_eq!(snapshot.item_type, Some(item_type), "{item_id}");
        assert_eq!(snapshot.revision, 1);
        assert_eq!(
            item_can_carry_obligation(&tx, item_id)?,
            carries_obligation,
            "{item_id}"
        );
        // Typing an item as an action is not an explicit reminder request: no obligation rows.
        assert_no_obligation_rows(&tx, item_id)?;

        // The source capture is untouched by the type correction.
        let capture_text: String = tx.query_row(
            "SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = ?)",
            [item_id],
            |row| row.get(0),
        )?;
        assert_eq!(capture_text, "test");

        let events = get_events_for_item(&tx, item_id)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Correction);
    }

    // Correcting action -> idea removes the ability to carry an obligation.
    tx.commit()?;
    let retype = make_correction_event(
        "evt-retype",
        "item-action",
        1,
        CorrectionKind::Type,
        Some("action"),
        "idea",
    );
    save_event(&mut db, &retype, 1)?;
    let tx = db.transaction()?;
    assert_eq!(
        get_item_snapshot(&tx, "item-action")?.unwrap().item_type,
        Some(ItemType::Idea)
    );
    assert!(!item_can_carry_obligation(&tx, "item-action")?);
    assert_no_obligation_rows(&tx, "item-action")?;
    // The correction history keeps both values.
    let history: Vec<(String, Option<String>, String)> = tx
        .prepare("SELECT kind, old_value, new_value FROM corrections WHERE item_id = 'item-action' ORDER BY revision")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        history,
        vec![
            ("type".to_string(), None, "action".to_string()),
            (
                "type".to_string(),
                Some("action".to_string()),
                "idea".to_string()
            ),
        ]
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_unsupported_type_values_rejected() -> Result<()> {
    let path = temp_db_path("bad_type");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    // Event::new rejects an unsupported type value.
    let built = Event::new(
        "evt-bad".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "banana".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    );
    assert!(built.is_err());

    // Public fields allow bypassing Event::new; persistence still rejects before any write.
    let bypass = Event {
        event_id: "evt-bad".to_string(),
        item_id: "item-1".to_string(),
        revision: 0,
        event_type: EventType::Correction,
        payload: EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "banana".to_string(),
        }),
        happened_at: "2026-01-15T10:30:00Z".to_string(),
    };
    assert!(matches!(
        save_event(&mut db, &bypass, 0),
        Err(EventError::Invalid(_))
    ));

    let bad_old = Event {
        payload: EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: Some("banana".to_string()),
            new_value: "note".to_string(),
        }),
        ..bypass.clone()
    };
    assert!(matches!(
        save_event(&mut db, &bad_old, 0),
        Err(EventError::Invalid(_))
    ));

    let tx = db.transaction()?;
    let snapshot = get_item_snapshot(&tx, "item-1")?.unwrap();
    assert_eq!(snapshot.revision, 0);
    assert_eq!(snapshot.item_type, None);
    assert!(get_events_for_item(&tx, "item-1")?.is_empty());

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_mismatched_type_and_payload_rejected_at_persistence() -> Result<()> {
    let path = temp_db_path("bypass_mismatch");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let mismatched = Event {
        event_id: "evt-1".to_string(),
        item_id: "item-1".to_string(),
        revision: 0,
        event_type: EventType::Completion,
        payload: EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: None,
            new_value: "x".to_string(),
        }),
        happened_at: "2026-01-15T10:30:00Z".to_string(),
    };
    assert!(matches!(
        save_event(&mut db, &mismatched, 0),
        Err(EventError::Invalid(_))
    ));

    let tx = db.transaction()?;
    let snapshot = get_item_snapshot(&tx, "item-1")?.unwrap();
    assert_eq!(snapshot.revision, 0);
    assert_eq!(snapshot.lifecycle_state, "active");
    let correction_rows: i64 =
        tx.query_row("SELECT COUNT(*) FROM corrections", [], |row| row.get(0))?;
    assert_eq!(correction_rows, 0);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_deleted_item_rejects_every_event() -> Result<()> {
    let path = temp_db_path("deleted_rejects");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = 'item-1'",
        [],
    )?;
    tx.commit()?;

    let events = [
        make_correction_event(
            "evt-1",
            "item-1",
            0,
            CorrectionKind::Text,
            None,
            "resurrected",
        ),
        make_completion_event("evt-2", "item-1", 0),
        make_cancellation_event("evt-3", "item-1", 0),
        make_suggestion_control_event("evt-4", "item-1", 0, SuggestionControlKind::NotNow),
    ];
    for event in &events {
        assert!(matches!(
            save_event(&mut db, event, 0),
            Err(EventError::NotAllowedInState { ref lifecycle_state, .. })
                if lifecycle_state == "deleted"
        ));
    }

    let tx = db.transaction()?;
    assert!(get_events_for_item(&tx, "item-1")?.is_empty());
    let correction_rows: i64 =
        tx.query_row("SELECT COUNT(*) FROM corrections", [], |row| row.get(0))?;
    assert_eq!(correction_rows, 0);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_completed_item_stays_correctable_but_takes_no_suggestion_control() -> Result<()> {
    let path = temp_db_path("completed_correctable");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    save_event(&mut db, &make_completion_event("evt-1", "item-1", 0), 0)?;

    let control =
        make_suggestion_control_event("evt-2", "item-1", 1, SuggestionControlKind::NotNow);
    assert!(matches!(
        save_event(&mut db, &control, 1),
        Err(EventError::NotAllowedInState { .. })
    ));

    let correction =
        make_correction_event("evt-3", "item-1", 1, CorrectionKind::Type, None, "note");
    save_event(&mut db, &correction, 1)?;

    let tx = db.transaction()?;
    let snapshot = get_item_snapshot(&tx, "item-1")?.unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.lifecycle_state, "completed");
    assert_eq!(snapshot.item_type, Some(ItemType::Note));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_null_correction_new_value_is_corruption() -> Result<()> {
    let path = temp_db_path("null_new_value");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.execute(
        "INSERT INTO events (event_id, item_id, revision, event_type, happened_at, correction_kind)
         VALUES ('evt-1', 'item-1', 0, 'correction', '2026-01-15T10:30:00Z', 'text')",
        [],
    )?;
    assert!(get_event(&tx, "evt-1").is_err());

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

    let control =
        make_suggestion_control_event("evt-control", "item-1", 0, SuggestionControlKind::NotNow);
    save_event(&mut db, &control, 0)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-control")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    assert_eq!(evt.event_type, EventType::SuggestionControl);
    if let EventPayload::SuggestionControl(payload) = evt.payload {
        assert_eq!(payload.kind, SuggestionControlKind::NotNow);
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

    let control = make_suggestion_control_event(
        "evt-control",
        "item-1",
        0,
        SuggestionControlKind::StopSuggesting,
    );
    save_event(&mut db, &control, 0)?;

    let tx = db.transaction()?;
    let retrieved = get_event(&tx, "evt-control")?;

    assert!(retrieved.is_some());
    let evt = retrieved.unwrap();
    if let EventPayload::SuggestionControl(payload) = evt.payload {
        assert_eq!(payload.kind, SuggestionControlKind::StopSuggesting);
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

    let evt1 = make_correction_event(
        "evt-1",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("test"),
        "new",
    );
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

    // Create three non-terminal events that preserve ordering
    let evt0 = make_correction_event(
        "evt-0",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("test"),
        "val0",
    );
    let evt1 = make_correction_event("evt-1", "item-1", 1, CorrectionKind::Type, None, "idea");
    let evt2 = make_correction_event(
        "evt-2",
        "item-1",
        2,
        CorrectionKind::Scope,
        Some("personal"),
        "work",
    );

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

    let event = make_correction_event(
        "evt-tx",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("test"),
        "val",
    );

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

#[test]
fn test_invalid_event_type_payload_combination() -> Result<()> {
    // Completion event with Correction payload should be rejected
    let result = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Completion,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: None,
            new_value: "test".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    );

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Invalid event type and payload combination"));

    Ok(())
}

#[test]
fn test_no_transitions_after_completion() -> Result<()> {
    let path = temp_db_path("completion_terminal");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let completion = make_completion_event("evt-1", "item-1", 0);
    save_event(&mut db, &completion, 0)?;

    // Attempt to apply another event after completion should fail
    let cancellation = make_cancellation_event("evt-2", "item-1", 1);
    let result = save_event(&mut db, &cancellation, 1);

    assert!(matches!(
        result,
        Err(EventError::NotAllowedInState { ref lifecycle_state, .. })
            if lifecycle_state == "completed" || lifecycle_state == "cancelled"
    ));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_no_transitions_after_cancellation() -> Result<()> {
    let path = temp_db_path("cancellation_terminal");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let cancellation = make_cancellation_event("evt-1", "item-1", 0);
    save_event(&mut db, &cancellation, 0)?;

    // Attempt to apply another event after cancellation should fail
    let completion = make_completion_event("evt-2", "item-1", 1);
    let result = save_event(&mut db, &completion, 1);

    assert!(matches!(
        result,
        Err(EventError::NotAllowedInState { ref lifecycle_state, .. })
            if lifecycle_state == "completed" || lifecycle_state == "cancelled"
    ));

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_event_revision_must_match_expected() -> Result<()> {
    let path = temp_db_path("revision_mismatch");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    // Create event with revision 9999 but expected revision 0
    let event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        9999,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    )?;

    let result = save_event(&mut db, &event, 0);

    assert!(matches!(
        result,
        Err(EventError::RevisionMismatch {
            event_revision: 9999,
            expected: 0
        })
    ));
    let tx = db.transaction()?;
    assert_eq!(get_item_snapshot(&tx, "item-1")?.unwrap().revision, 0);

    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn item_effective_columns(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    Ok(tx.query_row(
        "SELECT current_scope, current_session_topic FROM items WHERE item_id = ?",
        [item_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

#[test]
fn test_scope_correction_validated_and_applied() -> Result<()> {
    let path = temp_db_path("scope_correction");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    // Values outside personal/work are rejected, both at construction and at persistence.
    let built = Event::new(
        "evt-bad".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Scope,
            old_value: Some("personal".to_string()),
            new_value: "public_internet".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    );
    assert!(built.is_err());
    let bypass = Event {
        event_id: "evt-bad".to_string(),
        item_id: "item-1".to_string(),
        revision: 0,
        event_type: EventType::Correction,
        payload: EventPayload::Correction(Correction {
            kind: CorrectionKind::Scope,
            old_value: Some("galaxy".to_string()),
            new_value: "work".to_string(),
        }),
        happened_at: "2026-01-15T10:30:00Z".to_string(),
    };
    assert!(matches!(
        save_event(&mut db, &bypass, 0),
        Err(EventError::Invalid(_))
    ));

    let tx = db.transaction()?;
    assert_eq!(item_effective_columns(&tx, "item-1")?, (None, None));
    assert!(get_events_for_item(&tx, "item-1")?.is_empty());
    tx.commit()?;

    // A valid scope correction sets the effective scope; the capture keeps its original scope.
    let to_work = make_correction_event(
        "evt-work",
        "item-1",
        0,
        CorrectionKind::Scope,
        Some("personal"),
        "work",
    );
    save_event(&mut db, &to_work, 0)?;
    let tx = db.transaction()?;
    assert_eq!(
        item_effective_columns(&tx, "item-1")?,
        (Some("work".to_string()), None)
    );
    let capture_scope: String = tx.query_row(
        "SELECT item_scope FROM captures WHERE capture_id = 'cap-item-1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(capture_scope, "personal");
    tx.commit()?;

    // The next correction must state the real previous scope (work), not the capture's.
    let stale_old = make_correction_event(
        "evt-back-bad",
        "item-1",
        1,
        CorrectionKind::Scope,
        Some("personal"),
        "personal",
    );
    assert!(matches!(
        save_event(&mut db, &stale_old, 1),
        Err(EventError::OldValueMismatch { .. })
    ));
    let back = make_correction_event(
        "evt-back",
        "item-1",
        1,
        CorrectionKind::Scope,
        Some("work"),
        "personal",
    );
    save_event(&mut db, &back, 1)?;
    let tx = db.transaction()?;
    assert_eq!(
        item_effective_columns(&tx, "item-1")?.0,
        Some("personal".to_string())
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_session_topic_correction_applied_without_changing_scope() -> Result<()> {
    let path = temp_db_path("topic_correction");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    let blank = Event {
        event_id: "evt-blank".to_string(),
        item_id: "item-1".to_string(),
        revision: 0,
        event_type: EventType::Correction,
        payload: EventPayload::Correction(Correction {
            kind: CorrectionKind::SessionTopic,
            old_value: None,
            new_value: "  ".to_string(),
        }),
        happened_at: "2026-01-15T10:30:00Z".to_string(),
    };
    assert!(matches!(
        save_event(&mut db, &blank, 0),
        Err(EventError::Invalid(_))
    ));

    let assign = make_correction_event(
        "evt-topic",
        "item-1",
        0,
        CorrectionKind::SessionTopic,
        None,
        "synthetic-session",
    );
    save_event(&mut db, &assign, 0)?;
    let tx = db.transaction()?;
    assert_eq!(
        item_effective_columns(&tx, "item-1")?,
        (None, Some("synthetic-session".to_string()))
    );
    tx.commit()?;

    let reassign = make_correction_event(
        "evt-topic-2",
        "item-1",
        1,
        CorrectionKind::SessionTopic,
        Some("synthetic-session"),
        "other-session",
    );
    save_event(&mut db, &reassign, 1)?;
    let tx = db.transaction()?;
    assert_eq!(
        item_effective_columns(&tx, "item-1")?.1,
        Some("other-session".to_string())
    );

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_old_value_must_match_current_effective_value() -> Result<()> {
    let path = temp_db_path("old_value_mismatch");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1")?;
    tx.commit()?;

    // Item has no type yet: claiming a previous "note" type is rejected and writes nothing.
    let fabricated = make_correction_event(
        "evt-fake",
        "item-1",
        0,
        CorrectionKind::Type,
        Some("note"),
        "action",
    );
    match save_event(&mut db, &fabricated, 0) {
        Err(EventError::OldValueMismatch {
            kind,
            stated,
            actual,
            ..
        }) => {
            assert_eq!(kind, "type");
            assert_eq!(stated, Some("note".to_string()));
            assert_eq!(actual, None);
        }
        other => panic!("expected OldValueMismatch, got {other:?}"),
    }
    // Text previous value must be the capture text until a text correction exists.
    let wrong_text = make_correction_event(
        "evt-wrong-text",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("not the capture"),
        "edited",
    );
    assert!(matches!(
        save_event(&mut db, &wrong_text, 0),
        Err(EventError::OldValueMismatch { .. })
    ));
    let tx = db.transaction()?;
    let snapshot = get_item_snapshot(&tx, "item-1")?.unwrap();
    assert_eq!((snapshot.revision, snapshot.item_type), (0, None));
    let history_rows: i64 =
        tx.query_row("SELECT COUNT(*) FROM corrections", [], |row| row.get(0))?;
    assert_eq!(history_rows, 0);
    tx.commit()?;

    // After a text correction the next one must state the corrected text as previous value.
    let first = make_correction_event(
        "evt-text-1",
        "item-1",
        0,
        CorrectionKind::Text,
        Some("test"),
        "edited once",
    );
    save_event(&mut db, &first, 0)?;
    let stale_previous = make_correction_event(
        "evt-text-2-bad",
        "item-1",
        1,
        CorrectionKind::Text,
        Some("test"),
        "edited twice",
    );
    assert!(matches!(
        save_event(&mut db, &stale_previous, 1),
        Err(EventError::OldValueMismatch { .. })
    ));
    let second = make_correction_event(
        "evt-text-2",
        "item-1",
        1,
        CorrectionKind::Text,
        Some("edited once"),
        "edited twice",
    );
    save_event(&mut db, &second, 1)?;

    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn test_suggestion_control_kinds_are_not_now_and_stop_only() {
    assert_eq!(SuggestionControlKind::NotNow.as_str(), "not_now");
    assert_eq!(
        SuggestionControlKind::StopSuggesting.as_str(),
        "stop_suggesting"
    );
    assert!("cooldown".parse::<SuggestionControlKind>().is_err());
}
