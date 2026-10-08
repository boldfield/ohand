// Integration tests for suggestion eligibility (S01)

use chrono::Utc;
use ohand_core::store::captures;
use ohand_core::store::captures::Capture;
use ohand_core::store::events::{
    self, Correction, CorrectionKind, Event, EventPayload, SuggestionControlKind,
};
use ohand_core::store::schema::{Database, SystemClock};
use ohand_core::suggestions::eligibility::{self, EligibilityReason};
use std::sync::Arc;

fn test_db() -> anyhow::Result<Database> {
    let db = Database::open(":memory:", Arc::new(SystemClock))?;
    Ok(db)
}

fn make_test_capture(id: &str, text: &str) -> anyhow::Result<Capture> {
    Capture::new(
        id.to_string(),
        Some(text.to_string()),
        None,
        "2026-10-08T10:00:00Z".to_string(),
        "America/New_York".to_string(),
        -240,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-default".to_string(),
        false,
        "2026-10-08T10:00:00Z".to_string(),
        None,
    )
}

fn create_test_item(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
    capture_id: &str,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO items (
            item_id, capture_id, revision, lifecycle_state, save_state,
            sync_state, processing_state, transcription_state, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            item_id,
            capture_id,
            0,
            "active",
            "saved",
            "not_configured",
            "not_configured",
            "not_configured",
            "2026-10-08T10:00:00Z",
            "2026-10-08T10:00:00Z",
        ],
    )?;
    Ok(())
}

#[test]
fn test_active_action_is_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-1";
    let item_id = "item-1";

    let capture = make_test_capture(capture_id, "Buy groceries")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_completed_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-2";
    let item_id = "item-2";

    let capture = make_test_capture(capture_id, "Call mom")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;

    // Complete the item
    let event = Event::new(
        "event-2".to_string(),
        item_id.to_string(),
        1,
        events::EventType::Completion,
        EventPayload::Completion,
        "2026-10-08T11:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 1)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Completed);
    Ok(())
}

#[test]
fn test_cancelled_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-3";
    let item_id = "item-3";

    let capture = make_test_capture(capture_id, "Learn French")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;

    // Cancel the item
    let event = Event::new(
        "event-2".to_string(),
        item_id.to_string(),
        1,
        events::EventType::Cancellation,
        EventPayload::Cancellation,
        "2026-10-08T11:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 1)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Cancelled);
    Ok(())
}

#[test]
fn test_uninterpreted_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-4";
    let item_id = "item-4";

    let capture = make_test_capture(capture_id, "Maybe I should think about this")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Uninterpreted);
    Ok(())
}

#[test]
fn test_idea_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-5";
    let item_id = "item-5";

    let capture = make_test_capture(capture_id, "A roof garden would be nice")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as idea (not action)
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "idea".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::NotAnAction);
    Ok(())
}

#[test]
fn test_stop_suggesting_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-6";
    let item_id = "item-6";

    let capture = make_test_capture(capture_id, "Review project docs")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;

    // User stops suggesting
    let event = Event::new(
        "event-2".to_string(),
        item_id.to_string(),
        1,
        events::EventType::SuggestionControl,
        EventPayload::SuggestionControl(events::SuggestionControlPayload {
            kind: SuggestionControlKind::StopSuggesting,
        }),
        "2026-10-08T11:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 1)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::StopSuggesting);
    Ok(())
}

#[test]
fn test_not_now_within_cooldown_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-7";
    let item_id = "item-7";

    let capture = make_test_capture(capture_id, "Finish report")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;

    // User marks as "not now"
    let event = Event::new(
        "event-2".to_string(),
        item_id.to_string(),
        1,
        events::EventType::SuggestionControl,
        EventPayload::SuggestionControl(events::SuggestionControlPayload {
            kind: SuggestionControlKind::NotNow,
        }),
        "2026-10-08T11:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 1)?;

    // Set snooze in eligibility table
    let now = Utc::now();
    let cooldown = chrono::Duration::hours(1);
    eligibility::set_snooze(&tx, item_id, cooldown, now)?;

    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(!elig.eligible);
    match &elig.reason {
        EligibilityReason::SnoozedUntil(_) => {}
        _ => panic!("Expected SnoozedUntil, got {:?}", elig.reason),
    }
    Ok(())
}

#[test]
fn test_not_now_after_cooldown_is_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-8";
    let item_id = "item-8";

    let capture = make_test_capture(capture_id, "Water plants")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;

    // Type as action
    let event = Event::new(
        "event-1".to_string(),
        item_id.to_string(),
        0,
        events::EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-10-08T10:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 0)?;

    // User marks as "not now"
    let event = Event::new(
        "event-2".to_string(),
        item_id.to_string(),
        1,
        events::EventType::SuggestionControl,
        EventPayload::SuggestionControl(events::SuggestionControlPayload {
            kind: SuggestionControlKind::NotNow,
        }),
        "2026-10-08T11:00:00Z".to_string(),
    )?;

    events::save_event_in_tx(&tx, &event, 1)?;

    // Set snooze that expired in the past
    let now = Utc::now();
    let past_time = now - chrono::Duration::hours(2);
    eligibility::set_snooze(&tx, item_id, chrono::Duration::hours(-1), past_time)?;

    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id)?;

    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}
