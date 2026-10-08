// Integration tests for suggestion eligibility (S01)

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::items;
use ohand_core::store::captures;
use ohand_core::store::captures::Capture;
use ohand_core::store::events::{
    self, Correction, CorrectionKind, Event, EventPayload, ItemScope, SuggestionControlKind,
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
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

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
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_completed_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-2";
    let item_id = "item-2";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

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
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Completed);
    Ok(())
}

#[test]
fn test_cancelled_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-3";
    let item_id = "item-3";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

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
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Cancelled);
    Ok(())
}

#[test]
fn test_uninterpreted_item_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-4";
    let item_id = "item-4";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture = make_test_capture(capture_id, "Maybe I should think about this")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, item_id, capture_id)?;
    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::Uninterpreted);
    Ok(())
}

#[test]
fn test_idea_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-5";
    let item_id = "item-5";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

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
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::NotAnAction);
    Ok(())
}

#[test]
fn test_stop_suggesting_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-6";
    let item_id = "item-6";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

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
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::StopSuggesting);
    Ok(())
}

#[test]
fn test_not_now_within_cooldown_not_eligible() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-7";
    let item_id = "item-7";
    let base_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let eval_time = base_time + Duration::minutes(30); // Within 1 hour cooldown

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

    // Record NotNow response atomically (event + cooldown in one call)
    eligibility::record_not_now(&tx, item_id, "event-2", 1, base_time)?;

    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

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
    let base_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let eval_time = base_time + Duration::hours(2); // After 1 hour cooldown

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

    // Record NotNow response atomically (event + cooldown in one call)
    eligibility::record_not_now(&tx, item_id, "event-2", 1, base_time)?;

    tx.commit()?;

    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;

    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

// End-to-end tests for selection and rotation

#[test]
fn test_select_eligible_item_finds_unselected_action() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-select-1";
    let item_id = "item-select-1";
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

    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;

    assert_eq!(selected, Some(item_id.to_string()));

    // Verify selection was recorded with the actual eligibility reason
    let reason: String = tx.query_row(
        "SELECT selection_reason FROM suggestion_eligibility WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    assert_eq!(reason, "active_action");
    Ok(())
}

#[test]
fn test_select_eligible_item_rotation_order() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    // Create two actions
    for i in 1..=2 {
        let capture_id = format!("capture-rot-{}", i);
        let item_id = format!("item-rot-{}", i);
        let capture = make_test_capture(&capture_id, &format!("Action {}", i))?;
        captures::save_capture(&mut db, &capture)?;

        let tx = db.immediate_transaction()?;
        create_test_item(&tx, &item_id, &capture_id)?;

        let event = Event::new(
            format!("event-{}", i),
            item_id,
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
    }

    // First selection should get item-rot-1
    let tx = db.immediate_transaction()?;
    let first = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(first, Some("item-rot-1".to_string()));
    tx.commit()?;

    // Second selection should get item-rot-2 (rotation)
    let tx = db.immediate_transaction()?;
    let second = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(second, Some("item-rot-2".to_string()));
    tx.commit()?;

    // Third selection should cycle back to item-rot-1
    let tx = db.immediate_transaction()?;
    let third = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(third, Some("item-rot-1".to_string()));
    Ok(())
}

#[test]
fn test_deleted_item_not_selected() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-del";
    let item_id = "item-del";
    let capture = make_test_capture(capture_id, "Task to delete")?;
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

    // Mark as deleted by updating lifecycle_state
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [item_id],
    )?;
    tx.commit()?;

    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(selected, None);
    Ok(())
}

#[test]
fn test_dated_action_with_reminder_not_selected() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-dated";
    let item_id = "item-dated";
    let reminder_id = "reminder-dated";
    let capture = make_test_capture(capture_id, "Remind me tomorrow")?;
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

    // Create a reminder (dated action)
    tx.execute(
        "INSERT INTO reminders (
            reminder_id, item_id, request_state, schedule_state, delivery_state,
            acknowledgment_state, resolved_instant, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            reminder_id,
            item_id,
            "configured",
            "scheduled",
            "pending",
            "unacknowledged",
            "2026-10-09T10:00:00Z",
            "2026-10-08T10:00:00Z",
            "2026-10-08T10:00:00Z",
        ],
    )?;
    tx.commit()?;

    // Should not be selected because it has a reminder (dated)
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(selected, None);
    Ok(())
}

#[test]
fn test_pull_only_not_selected() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-pull";
    let item_id = "item-pull";
    let capture = make_test_capture(capture_id, "Pull-only item")?;
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

    // Mark as pull-only
    eligibility::set_pull_only(&tx, item_id, eval_time)?;
    tx.commit()?;

    // Should not be selected because it's marked pull-only
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(selected, None);
    Ok(())
}

#[test]
fn test_snoozed_item_not_selected() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let base_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let eval_time = base_time + Duration::minutes(30); // Within snooze

    let capture_id = "capture-snooze";
    let item_id = "item-snooze";
    let capture = make_test_capture(capture_id, "Snoozed item")?;
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

    // Mark as not-now with cooldown (atomically in one call)
    eligibility::record_not_now(&tx, item_id, "event-2", 1, base_time)?;
    tx.commit()?;

    // Should not be selected while snoozed
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(selected, None);
    Ok(())
}

#[test]
fn test_nonresponse_preserves_lifecycle() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-nonresp";
    let item_id = "item-nonresp";
    let capture = make_test_capture(capture_id, "Nonresponse test")?;
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

    // Select the item multiple times
    for _ in 0..3 {
        let tx = db.immediate_transaction()?;
        let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
        assert_eq!(selected, Some(item_id.to_string()));
        tx.commit()?;
    }

    // Verify item is still active with correct lifecycle
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;
    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_privacy_scope_filter_excludes_items() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    // Create a personal action
    let capture_id = "capture-personal";
    let item_id = "item-personal";
    let capture = make_test_capture(capture_id, "Personal task")?;
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

    // Without scope filter, it should be eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;
    assert!(elig.eligible);
    drop(tx);

    // With work scope filter, it should be excluded
    let tx = db.transaction()?;
    let elig =
        eligibility::check_eligibility_with_scope(&tx, item_id, eval_time, Some(ItemScope::Work))?;
    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ScopeExcluded);
    drop(tx);

    // With personal scope filter, it should be eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility_with_scope(
        &tx,
        item_id,
        eval_time,
        Some(ItemScope::Personal),
    )?;
    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_selection_records_correct_reason() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture_id = "capture-reason";
    let item_id = "item-reason";
    let capture = make_test_capture(capture_id, "Action for reason test")?;
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

    // Select the item
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
    assert_eq!(selected, Some(item_id.to_string()));
    tx.commit()?;

    // Verify the stored selection reason is the eligibility reason, not "rotation"
    let tx = db.transaction()?;
    let reason: String = tx.query_row(
        "SELECT selection_reason FROM suggestion_eligibility WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    assert_eq!(reason, "active_action");
    Ok(())
}

#[test]
fn test_dated_action_reason_is_has_reminder() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-dated-reason";
    let item_id = "item-dated-reason";
    let reminder_id = "reminder-dated-reason";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture = make_test_capture(capture_id, "Task with reminder")?;
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

    // Create a reminder (makes it dated)
    tx.execute(
        "INSERT INTO reminders (
            reminder_id, item_id, request_state, schedule_state, delivery_state,
            acknowledgment_state, resolved_instant, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            reminder_id,
            item_id,
            "configured",
            "scheduled",
            "pending",
            "unacknowledged",
            "2026-10-09T10:00:00Z",
            "2026-10-08T10:00:00Z",
            "2026-10-08T10:00:00Z",
        ],
    )?;
    tx.commit()?;

    // Check eligibility - should be ineligible with HasReminder reason
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, eval_time)?;
    assert!(!elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::HasReminder);
    Ok(())
}

#[test]
fn test_not_now_only_becomes_eligible_after_cooldown() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-not-now-only";
    let item_id = "item-not-now-only";
    let not_now_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let during_cooldown = not_now_time + Duration::minutes(30);
    let after_cooldown = not_now_time + Duration::hours(2);

    let capture = make_test_capture(capture_id, "Not-now test action")?;
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

    // Record NotNow response atomically (event + cooldown in one call)
    eligibility::record_not_now(&tx, item_id, "event-2", 1, not_now_time)?;
    tx.commit()?;

    // During cooldown: not eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, during_cooldown)?;
    assert!(!elig.eligible);
    drop(tx);

    // After cooldown: eligible again (no separate write needed)
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, after_cooldown)?;
    assert!(elig.eligible);
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_not_now_exact_boundary() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-boundary";
    let item_id = "item-boundary";
    let not_now_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let one_sec_before = not_now_time + Duration::hours(1) - Duration::seconds(1);
    let exactly_at = not_now_time + Duration::hours(1);
    let one_sec_after = not_now_time + Duration::hours(1) + Duration::seconds(1);

    let capture = make_test_capture(capture_id, "Boundary test action")?;
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

    // Record NotNow response atomically
    eligibility::record_not_now(&tx, item_id, "event-2", 1, not_now_time)?;
    tx.commit()?;

    // One second before cooldown expires: not eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, one_sec_before)?;
    assert!(
        !elig.eligible,
        "Should be ineligible one second before cooldown expiry"
    );
    drop(tx);

    // Exactly at cooldown expiry: eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, exactly_at)?;
    assert!(
        elig.eligible,
        "Should be eligible at exactly cooldown expiry"
    );
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    drop(tx);

    // One second after: eligible
    let tx = db.transaction()?;
    let elig = eligibility::check_eligibility(&tx, item_id, one_sec_after)?;
    assert!(
        elig.eligible,
        "Should be eligible one second after cooldown expiry"
    );
    assert_eq!(elig.reason, EligibilityReason::ActiveAction);
    Ok(())
}

#[test]
fn test_rotation_with_fixed_clock() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let base_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    // Create three test items
    for i in 1..=3 {
        let capture_id = format!("capture-rot-{}", i);
        let item_id = format!("item-rot-{}", i);
        let capture = make_test_capture(&capture_id, &format!("Rotation test action {}", i))?;
        captures::save_capture(&mut db, &capture)?;

        let tx = db.immediate_transaction()?;
        create_test_item(&tx, &item_id, &capture_id)?;

        // Type as action
        let event = Event::new(
            format!("event-1-{}", i),
            item_id.clone(),
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
    }

    // Perform multiple selections at the same clock time
    let tx = db.immediate_transaction()?;

    // Selection 1: should get item-rot-1 (first by item_id)
    let selected = eligibility::select_eligible_item(&tx, base_time, None)?;
    assert_eq!(selected, Some("item-rot-1".to_string()));
    tx.commit()?;

    // Selection 2 (same instant)
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, base_time, None)?;
    assert_eq!(
        selected,
        Some("item-rot-2".to_string()),
        "Should rotate to item-rot-2"
    );
    tx.commit()?;

    // Selection 3 (same instant)
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, base_time, None)?;
    assert_eq!(
        selected,
        Some("item-rot-3".to_string()),
        "Should rotate to item-rot-3"
    );
    tx.commit()?;

    // Selection 4 (same instant) - should complete the cycle
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, base_time, None)?;
    assert_eq!(
        selected,
        Some("item-rot-1".to_string()),
        "Should rotate back to item-rot-1"
    );
    tx.commit()?;

    // Selection 5 - continue rotation
    let tx = db.immediate_transaction()?;
    let selected = eligibility::select_eligible_item(&tx, base_time, None)?;
    assert_eq!(
        selected,
        Some("item-rot-2".to_string()),
        "Should continue to item-rot-2"
    );
    tx.commit()?;

    Ok(())
}

#[test]
fn test_nonresponse_preserves_lifecycle_and_importance() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let capture_id = "capture-nonresp";
    let item_id = "item-nonresp";
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;

    let capture = make_test_capture(capture_id, "Nonresponse test action")?;
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

    let (revision_before, event_count_before, type_before) = {
        let tx = db.transaction()?;
        let counts = item_write_counts(&tx, item_id)?;
        let state = items::load_item_state(&tx, item_id)?.expect("Item should exist");
        (counts.0, counts.1, state.item_type)
    };

    // Select the item three times - nonresponse should not change lifecycle
    for _ in 1..=3 {
        let tx = db.immediate_transaction()?;
        let selected = eligibility::select_eligible_item(&tx, eval_time, None)?;
        assert_eq!(selected, Some(item_id.to_string()));

        // Verify item is still active and typed
        let item_state = items::load_item_state(&tx, item_id)?.expect("Item should exist");
        assert_eq!(item_state.lifecycle_state, items::LifecycleState::Active);
        assert_eq!(item_state.item_type, Some(events::ItemType::Action));

        tx.commit()?;
    }

    // Selection bookkeeping writes no events or revisions and never alters type or lifecycle
    let tx = db.transaction()?;
    let (revision_after, event_count_after) = item_write_counts(&tx, item_id)?;
    assert_eq!(revision_after, revision_before);
    assert_eq!(event_count_after, event_count_before);
    let state_after = items::load_item_state(&tx, item_id)?.expect("Item should exist");
    assert_eq!(state_after.item_type, type_before);
    assert_eq!(state_after.lifecycle_state, items::LifecycleState::Active);

    Ok(())
}

fn item_write_counts(tx: &rusqlite::Transaction<'_>, item_id: &str) -> anyhow::Result<(i64, i64)> {
    let revision: i64 = tx.query_row(
        "SELECT revision FROM items WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    let event_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM events WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?;
    Ok((revision, event_count))
}

#[test]
fn test_malformed_last_selected_at_is_an_error() -> anyhow::Result<()> {
    let mut db = test_db()?;
    let eval_time = "2026-10-08T10:00:00Z".parse::<DateTime<Utc>>()?;
    let capture = make_test_capture("capture-bad-stamp", "Malformed stamp")?;
    captures::save_capture(&mut db, &capture)?;

    let tx = db.immediate_transaction()?;
    create_test_item(&tx, "item-bad-stamp", "capture-bad-stamp")?;
    let event = Event::new(
        "event-1".to_string(),
        "item-bad-stamp".to_string(),
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
    tx.execute(
        "INSERT INTO suggestion_eligibility (item_id, eligible, snoozed, pull_only, last_selected_at, created_at, updated_at)
         VALUES ('item-bad-stamp', 1, 0, 0, 'not-a-timestamp', '2026-10-08T10:00:00Z', '2026-10-08T10:00:00Z')",
        [],
    )?;
    assert!(eligibility::select_eligible_item(&tx, eval_time, None).is_err());
    Ok(())
}
