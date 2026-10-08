use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use ohand_core::domain::items::{
    apply_proposal, load_item_state, rebuild_state_from_events, validate_state_transition,
    FieldProvenance, ItemScope, ItemState, ItemType, LifecycleState, ProposalApplicationError,
    StateTransition, TextState, TransitionValidity,
};
use ohand_core::store::events::{
    get_item_snapshot, item_can_carry_obligation, save_event, Correction, CorrectionKind, Event,
    EventPayload, EventType,
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
        "{}/test_item_state_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn insert_test_item(
    tx: &rusqlite::Transaction<'_>,
    item_id: &str,
    capture_text: &str,
) -> Result<String> {
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
         VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        rusqlite::params![
            item_id,
            &capture_id,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z",
        ],
    )?;
    Ok(capture_id)
}

#[test]
fn test_load_untyped_active_item() -> Result<()> {
    let path = temp_db_path("load_untyped");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "hello world")?;
    tx.commit()?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?;
    tx.commit()?;

    let state = state.expect("item should exist");
    assert_eq!(state.item_id, "item-1");
    assert_eq!(state.revision, 0);
    assert_eq!(state.item_type, None);
    assert_eq!(state.scope, ItemScope::Personal);
    assert_eq!(state.session_topic, None);
    assert_eq!(state.lifecycle_state, LifecycleState::Active);
    assert_eq!(state.current_text.text(), Some("hello world"));
    Ok(())
}

#[test]
fn test_load_missing_item_returns_none() -> Result<()> {
    let path = temp_db_path("load_missing");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "nonexistent")?;
    assert_eq!(state, None);
    Ok(())
}

#[test]
fn test_type_correction_updates_state() -> Result<()> {
    let path = temp_db_path("type_correction");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Apply a type correction.
    let correction_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &correction_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?;
    tx.commit()?;

    let state = state.expect("item should exist");
    assert_eq!(state.item_type, Some(ItemType::Action));
    assert_eq!(state.revision, 1);
    Ok(())
}

#[test]
fn test_text_correction_tracked_separately() -> Result<()> {
    let path = temp_db_path("text_correction");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "original text")?;
    tx.commit()?;

    // Apply a text correction.
    let correction_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("original text".to_string()),
            new_value: "corrected text".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &correction_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?;
    tx.commit()?;

    let state = state.expect("item should exist");
    match &state.current_text {
        TextState::Corrected { text, corrected_at } => {
            assert_eq!(text, "corrected text");
            assert_eq!(corrected_at, "2026-01-15T10:30:00Z");
        }
        _ => panic!("expected TextState::Corrected"),
    }
    Ok(())
}

#[test]
fn test_completion_transitions_lifecycle() -> Result<()> {
    let path = temp_db_path("completion");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Mark as completed.
    let completion_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &completion_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?;
    tx.commit()?;

    let state = state.expect("item should exist");
    assert_eq!(state.lifecycle_state, LifecycleState::Completed);
    assert_eq!(state.revision, 1);
    Ok(())
}

#[test]
fn test_cancellation_transitions_lifecycle() -> Result<()> {
    let path = temp_db_path("cancellation");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "maybe a roof garden")?;
    tx.commit()?;

    // Mark as cancelled.
    let cancellation_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Cancellation,
        EventPayload::Cancellation,
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &cancellation_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?;
    tx.commit()?;

    let state = state.expect("item should exist");
    assert_eq!(state.lifecycle_state, LifecycleState::Cancelled);
    Ok(())
}

#[test]
fn test_scope_correction_overrides_capture_default() -> Result<()> {
    let path = temp_db_path("scope_correction");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "project notes")?;
    tx.commit()?;

    // Item starts with capture's scope (personal).
    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    assert_eq!(state.scope, ItemScope::Personal);
    tx.commit()?;

    // Apply a scope correction to work.
    let correction_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Scope,
            old_value: Some("personal".to_string()),
            new_value: "work".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &correction_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    assert_eq!(state.scope, ItemScope::Work);
    tx.commit()?;

    Ok(())
}

#[test]
fn test_rebuild_state_equals_stored_after_corrections() -> Result<()> {
    let path = temp_db_path("rebuild_integrity");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "original")?;
    tx.commit()?;

    // Apply type and text corrections.
    let type_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &type_event, 0)?;

    let text_event = Event::new(
        "evt-2".to_string(),
        "item-1".to_string(),
        1,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("original".to_string()),
            new_value: "corrected".to_string(),
        }),
        "2026-01-15T10:30:01Z".to_string(),
    )?;
    save_event(&mut db, &text_event, 1)?;

    // Complete it.
    let completion_event = Event::new(
        "evt-3".to_string(),
        "item-1".to_string(),
        2,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:02Z".to_string(),
    )?;
    save_event(&mut db, &completion_event, 2)?;

    // Verify integrity: stored and rebuilt should match.
    let tx = db.transaction()?;
    let issue = ohand_core::domain::items::verify_state_integrity(&tx, "item-1")?;
    assert_eq!(issue, None);
    tx.commit()?;

    // Explicitly verify they match.
    let tx = db.transaction()?;
    let stored = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    assert_eq!(stored, rebuilt);
    tx.commit()?;

    Ok(())
}

#[test]
fn test_model_cannot_override_completion() -> Result<()> {
    let path = temp_db_path("model_override_blocked");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Mark as completed.
    let completion_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &completion_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    // Model tries to set type on completed item.
    let result = validate_state_transition(
        &state,
        StateTransition::ModelAnnotation {
            annotation_type: Some(ItemType::Action),
        },
    );

    assert_eq!(result, TransitionValidity::ForbiddenOverride);
    Ok(())
}

#[test]
fn test_model_cannot_override_cancellation() -> Result<()> {
    let path = temp_db_path("model_override_cancelled");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "maybe a roof garden")?;
    tx.commit()?;

    // Mark as cancelled.
    let cancellation_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Cancellation,
        EventPayload::Cancellation,
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &cancellation_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    // Model tries to set type on cancelled item.
    let result = validate_state_transition(
        &state,
        StateTransition::ModelAnnotation {
            annotation_type: Some(ItemType::Idea),
        },
    );

    assert_eq!(result, TransitionValidity::ForbiddenOverride);
    Ok(())
}

#[test]
fn test_model_can_annotate_active_item() -> Result<()> {
    let path = temp_db_path("model_annotate_active");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    // Model can annotate an active item.
    let result = validate_state_transition(
        &state,
        StateTransition::ModelAnnotation {
            annotation_type: Some(ItemType::Action),
        },
    );

    assert_eq!(result, TransitionValidity::Valid);
    Ok(())
}

#[test]
fn test_user_corrections_allowed_on_completed() -> Result<()> {
    let path = temp_db_path("corrections_on_completed");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Mark as completed.
    let completion_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &completion_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    // User corrections are still allowed.
    let text_result = validate_state_transition(
        &state,
        StateTransition::TextCorrected("actually called the electrician".to_string()),
    );
    assert_eq!(text_result, TransitionValidity::Valid);

    let scope_result =
        validate_state_transition(&state, StateTransition::ScopeSet(ItemScope::Work));
    assert_eq!(scope_result, TransitionValidity::Valid);

    Ok(())
}

#[test]
fn test_deleted_item_rejects_all_transitions() -> Result<()> {
    let path = temp_db_path("deleted_readonly");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let _db = make_test_db(&path, instant)?;

    let state = ItemState {
        item_id: "item-1".to_string(),
        capture_id: "cap-1".to_string(),
        revision: 0,
        item_type: Some(ItemType::Action),
        scope: ItemScope::Personal,
        session_topic: None,
        lifecycle_state: LifecycleState::Deleted,
        current_text: TextState::Original {
            text: Some("deleted".to_string()),
        },
        provenance: FieldProvenance::default(),
    };

    // All transitions should be rejected as NotAllowed.
    let transitions = vec![
        StateTransition::TypeSet(Some(ItemType::Idea)),
        StateTransition::ScopeSet(ItemScope::Work),
        StateTransition::SessionTopicSet(Some("topic".to_string())),
        StateTransition::TextCorrected("new text".to_string()),
        StateTransition::Completed,
        StateTransition::Cancelled,
    ];

    for transition in transitions {
        let result = validate_state_transition(&state, transition);
        assert_eq!(result, TransitionValidity::NotAllowed);
    }

    Ok(())
}

#[test]
fn test_multiple_text_corrections_preserves_latest() -> Result<()> {
    let path = temp_db_path("multiple_text_corrections");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "v0")?;
    tx.commit()?;

    // First correction.
    let event1 = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("v0".to_string()),
            new_value: "v1".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &event1, 0)?;

    // Second correction.
    let event2 = Event::new(
        "evt-2".to_string(),
        "item-1".to_string(),
        1,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("v1".to_string()),
            new_value: "v2".to_string(),
        }),
        "2026-01-15T10:30:01Z".to_string(),
    )?;
    save_event(&mut db, &event2, 1)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    // Should have the latest correction.
    match &state.current_text {
        TextState::Corrected { text, corrected_at } => {
            assert_eq!(text, "v2");
            assert_eq!(corrected_at, "2026-01-15T10:30:01Z");
        }
        _ => panic!("expected TextState::Corrected"),
    }
    Ok(())
}

#[test]
fn test_session_topic_from_capture_default() -> Result<()> {
    let path = temp_db_path("session_topic_default");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    // Insert item with session topic in capture.
    let tx = db.transaction()?;
    let capture_id = "cap-1";
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            capture_id,
            Some("bring this up in therapy"),
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
            Some("therapy"),
        ],
    )?;

    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        rusqlite::params![
            "item-1",
            capture_id,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    assert_eq!(state.session_topic, Some("therapy".to_string()));
    Ok(())
}

#[test]
fn test_model_cannot_override_user_corrected_type() -> Result<()> {
    let path = temp_db_path("model_override_type");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // User corrects the type to Action.
    let type_correction = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &type_correction, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    assert_eq!(state.item_type, Some(ItemType::Action));
    assert!(state.provenance.type_corrected);
    tx.commit()?;

    // Later, model tries to set type to Idea on this active item.
    // Should be forbidden because user already corrected the type.
    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    tx.commit()?;

    let result = validate_state_transition(
        &state,
        StateTransition::ModelAnnotation {
            annotation_type: Some(ItemType::Idea),
        },
    );

    assert_eq!(result, TransitionValidity::ForbiddenOverride);
    Ok(())
}

#[test]
fn test_rebuild_equals_stored_with_provenance() -> Result<()> {
    let path = temp_db_path("rebuild_provenance");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "original")?;
    tx.commit()?;

    // Apply type, scope, and text corrections.
    let type_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &type_event, 0)?;

    let scope_event = Event::new(
        "evt-2".to_string(),
        "item-1".to_string(),
        1,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Scope,
            old_value: Some("personal".to_string()),
            new_value: "work".to_string(),
        }),
        "2026-01-15T10:30:01Z".to_string(),
    )?;
    save_event(&mut db, &scope_event, 1)?;

    let text_event = Event::new(
        "evt-3".to_string(),
        "item-1".to_string(),
        2,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("original".to_string()),
            new_value: "corrected".to_string(),
        }),
        "2026-01-15T10:30:02Z".to_string(),
    )?;
    save_event(&mut db, &text_event, 2)?;

    // Verify rebuild.
    let tx = db.transaction()?;
    let stored = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    tx.commit()?;

    // Both should track the same provenance.
    assert_eq!(stored.provenance, rebuilt.provenance);
    assert!(stored.provenance.type_corrected);
    assert!(stored.provenance.scope_corrected);
    assert!(stored.provenance.text_corrected);
    assert!(!stored.provenance.session_topic_corrected);

    // They should be equal.
    assert_eq!(stored, rebuilt);
    Ok(())
}

#[test]
fn test_apply_proposal_rejects_stale_proposal() -> Result<()> {
    let path = temp_db_path("apply_stale_proposal");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Add a proposal with source_revision = 0.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-1",
            "item-1",
            "cap-item-1",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Apply a correction to increment revision.
    let correction_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &correction_event, 0)?;

    // Now item revision is 1, but proposal's source_revision is 0 (stale).
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-1");
    tx.commit()?;

    // Should fail with StaleProposal error.
    match result {
        Err(ProposalApplicationError::StaleProposal {
            source_revision: 0,
            current_revision: 1,
        }) => Ok(()),
        _ => Err(anyhow::anyhow!(
            "Expected StaleProposal error, got {:?}",
            result
        )),
    }
}

#[test]
fn test_apply_proposal_rejects_user_corrected_field() -> Result<()> {
    let path = temp_db_path("apply_user_corrected");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // User corrects the type.
    let type_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &type_event, 0)?;

    // Add a proposal for type at the current revision (after the correction).
    let tx = db.transaction()?;
    let current_state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let current_revision = current_state.revision;
    tx.commit()?;

    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-2",
            "item-1",
            "cap-item-1",
            current_revision,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "idea",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Try to apply the proposal. Should fail because user already corrected the type.
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-2");
    tx.commit()?;

    match result {
        Err(ProposalApplicationError::ForbiddenByUserCorrection(field)) => {
            if field == "type" {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Expected field 'type', got '{}'", field))
            }
        }
        _ => Err(anyhow::anyhow!(
            "Expected ForbiddenByUserCorrection error, got {:?}",
            result
        )),
    }
}

#[test]
fn test_apply_proposal_successful_type() -> Result<()> {
    let path = temp_db_path("apply_proposal_success_type");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Add a proposal for type at revision 0.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-1",
            "item-1",
            "cap-item-1",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Apply the proposal successfully.
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-1")?;
    tx.commit()?;

    // Verify the type was applied.
    assert_eq!(result.item_type, Some(ItemType::Action));
    Ok(())
}

#[test]
fn test_apply_proposal_with_abstained_rejected() -> Result<()> {
    let path = temp_db_path("apply_abstained_proposal");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Add an abstained proposal.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-2",
            "item-1",
            "cap-item-1",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            None::<String>,
            None::<String>,
            None::<String>,
            None::<String>,
            1,  // abstained = 1
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Try to apply the abstained proposal.
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-2");
    tx.commit()?;

    // Should fail with InvalidProposal error.
    match result {
        Err(ProposalApplicationError::InvalidProposal(msg)) if msg.contains("abstained") => Ok(()),
        _ => Err(anyhow::anyhow!(
            "Expected InvalidProposal error for abstained proposal, got {:?}",
            result
        )),
    }
}

#[test]
fn test_apply_proposal_cross_item_rejected() -> Result<()> {
    let path = temp_db_path("apply_cross_item_proposal");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    // Create two items.
    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    insert_test_item(&tx, "item-2", "call the electrician")?;
    tx.commit()?;

    // Add a proposal for item-2.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-item2",
            "item-2",
            "cap-item-2",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Try to apply item-2's proposal to item-1 (ownership check).
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-item2");
    tx.commit()?;

    // Should fail with InvalidProposal error.
    match result {
        Err(ProposalApplicationError::InvalidProposal(msg)) if msg.contains("another item") => {
            Ok(())
        }
        _ => Err(anyhow::anyhow!(
            "Expected InvalidProposal error for cross-item proposal, got {:?}",
            result
        )),
    }
}

#[test]
fn test_apply_proposal_with_user_correction_precedence() -> Result<()> {
    let path = temp_db_path("apply_proposal_correction_precedence");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // User corrects the type to idea.
    let type_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "idea".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(&mut db, &type_event, 0)?;

    // Add a proposal to set type to action at the current revision (after the correction).
    let tx = db.transaction()?;
    let current_state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let current_revision = current_state.revision;
    tx.commit()?;

    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-type-override",
            "item-1",
            "cap-item-1",
            current_revision,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Try to apply the proposal.
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-type-override");
    tx.commit()?;

    // Should fail because user already corrected the type.
    match result {
        Err(ProposalApplicationError::ForbiddenByUserCorrection(field)) => {
            if field == "type" {
                Ok(())
            } else {
                Err(anyhow::anyhow!("Expected field 'type', got '{}'", field))
            }
        }
        _ => Err(anyhow::anyhow!(
            "Expected ForbiddenByUserCorrection error, got {:?}",
            result
        )),
    }
}

#[test]
fn test_rebuild_equals_projection_after_model_apply() -> Result<()> {
    let path = temp_db_path("rebuild_after_model_apply");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Add a proposal for type at revision 0.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-1",
            "item-1",
            "cap-item-1",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Apply the proposal.
    let tx = db.transaction()?;
    let _result = apply_proposal(&tx, "item-1", "prop-1")?;
    tx.commit()?;

    // Verify that stored and rebuilt states match.
    let tx = db.transaction()?;
    let stored = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    tx.commit()?;

    // Both should have the applied type.
    assert_eq!(stored.item_type, Some(ItemType::Action));
    assert_eq!(rebuilt.item_type, Some(ItemType::Action));
    assert_eq!(stored, rebuilt);
    Ok(())
}

#[test]
fn test_obligation_gating_after_model_apply() -> Result<()> {
    let path = temp_db_path("obligation_after_apply");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;

    // Initially, the item is untyped and cannot carry obligation.
    let tx = db.transaction()?;
    let can_carry_before = item_can_carry_obligation(&tx, "item-1")?;
    tx.commit()?;
    assert!(
        !can_carry_before,
        "Untyped item should not carry obligation"
    );

    // Add a proposal to set type to action at revision 0.
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            "prop-action",
            "item-1",
            "cap-item-1",
            0,
            1,
            "capture",
            None::<String>,
            "unapplied",
            "action",
            None::<String>,
            None::<String>,
            None::<String>,
            0,
            None::<String>,
            "2026-01-15T10:30:00Z",
        ],
    )?;
    tx.commit()?;

    // Apply the proposal.
    let tx = db.transaction()?;
    apply_proposal(&tx, "item-1", "prop-action")?;
    tx.commit()?;

    // After applying, the item should be typed as action in the authoritative store.
    let tx = db.transaction()?;
    let snapshot = get_item_snapshot(&tx, "item-1")?.expect("snapshot should exist");
    let can_carry_after = item_can_carry_obligation(&tx, "item-1")?;
    tx.commit()?;

    // The snapshot should show item_type = action.
    assert_eq!(
        snapshot.item_type,
        Some(ItemType::Action),
        "After model apply, item_type should be Action in the snapshot"
    );

    // Obligation gating should now return true.
    assert!(
        can_carry_after,
        "After model apply to action, item should carry obligation"
    );

    Ok(())
}

fn insert_proposal(
    db: &mut Database,
    proposal_id: &str,
    item_id: &str,
    source_revision: i32,
    schema_version: i32,
    proposal_type: Option<&str>,
    session_topic_proposal: Option<&str>,
) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, session_topic_proposal, source_spans, abstained, request_version, created_at)
         VALUES (?, ?, ?, ?, ?, 'capture', NULL, 'unapplied', ?, NULL, ?, NULL, 0, NULL, '2026-01-15T10:30:00Z')",
        rusqlite::params![
            proposal_id,
            item_id,
            format!("cap-{}", item_id),
            source_revision,
            schema_version,
            proposal_type,
            session_topic_proposal,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

fn proposal_applied_state(db: &mut Database, proposal_id: &str) -> Result<String> {
    let tx = db.transaction()?;
    let state = tx.query_row(
        "SELECT applied_state FROM proposals WHERE proposal_id = ?",
        [proposal_id],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(state)
}

#[test]
fn test_apply_proposal_rejects_unsupported_schema_version() -> Result<()> {
    let path = temp_db_path("apply_unsupported_schema");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;
    insert_proposal(
        &mut db,
        "prop-v999",
        "item-1",
        0,
        999,
        Some("action"),
        Some("home"),
    )?;

    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-v999");
    tx.commit()?;
    assert_eq!(
        result.unwrap_err(),
        ProposalApplicationError::UnsupportedSchemaVersion(999)
    );

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let snapshot = get_item_snapshot(&tx, "item-1")?.expect("snapshot should exist");
    tx.commit()?;
    assert_eq!(state.item_type, None);
    assert_eq!(state.session_topic, None);
    assert_eq!(snapshot.item_type, None);
    assert_eq!(proposal_applied_state(&mut db, "prop-v999")?, "unapplied");
    Ok(())
}

#[test]
fn test_apply_proposal_rejects_unparseable_type_and_preserves_prior_state() -> Result<()> {
    let path = temp_db_path("apply_unparseable_type");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;
    insert_proposal(&mut db, "prop-good", "item-1", 0, 1, Some("idea"), None)?;
    let tx = db.transaction()?;
    apply_proposal(&tx, "item-1", "prop-good")?;
    tx.commit()?;

    // A newer proposal for the same revision carries an invalid type: the prior idea stays.
    insert_proposal(&mut db, "prop-bad", "item-1", 0, 1, Some("garbage!!"), None)?;
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-bad");
    tx.commit()?;
    match result {
        Err(ProposalApplicationError::InvalidProposal(msg)) if msg.contains("proposal_type") => {}
        other => panic!(
            "expected InvalidProposal for unparseable type, got {:?}",
            other
        ),
    }

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    tx.commit()?;
    assert_eq!(state.item_type, Some(ItemType::Idea));
    assert_eq!(state, rebuilt);
    assert_eq!(proposal_applied_state(&mut db, "prop-good")?, "applied");
    assert_eq!(proposal_applied_state(&mut db, "prop-bad")?, "unapplied");
    Ok(())
}

#[test]
fn test_newer_proposal_for_same_revision_supersedes_older() -> Result<()> {
    let path = temp_db_path("apply_supersede");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;
    insert_proposal(
        &mut db,
        "prop-old",
        "item-1",
        0,
        1,
        Some("idea"),
        Some("home"),
    )?;
    insert_proposal(&mut db, "prop-new", "item-1", 0, 1, Some("action"), None)?;

    let tx = db.transaction()?;
    apply_proposal(&tx, "item-1", "prop-old")?;
    tx.commit()?;
    let tx = db.transaction()?;
    let applied = apply_proposal(&tx, "item-1", "prop-new")?;
    tx.commit()?;

    assert_eq!(applied.item_type, Some(ItemType::Action));
    assert_eq!(applied.session_topic, None);
    assert_eq!(proposal_applied_state(&mut db, "prop-old")?, "superseded");
    assert_eq!(proposal_applied_state(&mut db, "prop-new")?, "applied");

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    let snapshot = get_item_snapshot(&tx, "item-1")?.expect("snapshot should exist");
    tx.commit()?;
    assert_eq!(state, applied);
    assert_eq!(state, rebuilt);
    assert_eq!(snapshot.item_type, Some(ItemType::Action));

    // A superseded proposal cannot be re-applied.
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-old");
    tx.commit()?;
    assert!(matches!(
        result,
        Err(ProposalApplicationError::InvalidProposal(_))
    ));
    Ok(())
}

#[test]
fn test_capture_session_topic_outranks_model_topic_proposal() -> Result<()> {
    let path = temp_db_path("capture_topic_precedence");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "bring this up in therapy")?;
    tx.execute(
        "UPDATE captures SET session_topic = 'therapy' WHERE capture_id = 'cap-item-1'",
        [],
    )?;
    tx.commit()?;
    insert_proposal(
        &mut db,
        "prop-1",
        "item-1",
        0,
        1,
        Some("note"),
        Some("model-topic"),
    )?;

    let tx = db.transaction()?;
    let applied = apply_proposal(&tx, "item-1", "prop-1")?;
    tx.commit()?;
    assert_eq!(applied.session_topic, Some("therapy".to_string()));
    assert_eq!(applied.item_type, Some(ItemType::Note));

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    let snapshot = get_item_snapshot(&tx, "item-1")?.expect("snapshot should exist");
    tx.commit()?;
    assert_eq!(state.session_topic, Some("therapy".to_string()));
    assert_eq!(state, rebuilt);
    assert_eq!(snapshot.item_type, Some(ItemType::Note));
    Ok(())
}

#[test]
fn test_model_topic_applies_when_capture_has_none_and_rebuilds() -> Result<()> {
    let path = temp_db_path("model_topic_applies");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;
    insert_proposal(
        &mut db,
        "prop-1",
        "item-1",
        0,
        1,
        None,
        Some("home repairs"),
    )?;

    let tx = db.transaction()?;
    let applied = apply_proposal(&tx, "item-1", "prop-1")?;
    tx.commit()?;
    assert_eq!(applied.session_topic, Some("home repairs".to_string()));
    assert_eq!(applied.item_type, None);

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    tx.commit()?;
    assert_eq!(state, rebuilt);
    Ok(())
}

#[test]
fn test_user_correction_after_model_apply_records_model_value_as_old_value() -> Result<()> {
    let path = temp_db_path("correction_after_model_apply");
    let instant = DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")?.with_timezone(&Utc);
    let mut db = make_test_db(&path, instant)?;

    let tx = db.transaction()?;
    insert_test_item(&tx, "item-1", "call the roofer")?;
    tx.commit()?;
    insert_proposal(&mut db, "prop-1", "item-1", 0, 1, Some("action"), None)?;
    let tx = db.transaction()?;
    apply_proposal(&tx, "item-1", "prop-1")?;
    tx.commit()?;

    let correction_event = Event::new(
        "evt-1".to_string(),
        "item-1".to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: Some("action".to_string()),
            new_value: "idea".to_string(),
        }),
        "2026-01-15T10:31:00Z".to_string(),
    )?;
    save_event(&mut db, &correction_event, 0)?;

    let tx = db.transaction()?;
    let state = load_item_state(&tx, "item-1")?.expect("item should exist");
    let rebuilt = rebuild_state_from_events(&tx, "item-1")?.expect("rebuilt should exist");
    tx.commit()?;
    assert_eq!(state.item_type, Some(ItemType::Idea));
    assert!(state.provenance.type_corrected);
    assert_eq!(state, rebuilt);

    // The correction records the model-set value as its old value.
    let tx = db.transaction()?;
    let old_value: Option<String> = tx.query_row(
        "SELECT old_value FROM corrections WHERE item_id = 'item-1' AND kind = 'type'",
        [],
        |row| row.get(0),
    )?;
    tx.commit()?;
    assert_eq!(old_value, Some("action".to_string()));

    // Reprocessing cannot undo the correction: a fresh proposal at the new revision is refused.
    insert_proposal(&mut db, "prop-2", "item-1", 1, 1, Some("action"), None)?;
    let tx = db.transaction()?;
    let result = apply_proposal(&tx, "item-1", "prop-2");
    tx.commit()?;
    assert_eq!(
        result.unwrap_err(),
        ProposalApplicationError::ForbiddenByUserCorrection("type".to_string())
    );
    Ok(())
}
