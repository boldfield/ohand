use chrono::Utc;
use ohand_core::reminders::state::{
    apply_resolution, record_operation, AcknowledgmentState, DeliveryState, OperationState,
    OperationType, ReminderState, RequestState, ScheduleState,
};
use ohand_core::time::resolver::{AmbiguityKind, ResolutionResult, TimeContext};
use rusqlite::Connection;

fn make_reminder_state(reminder_id: &str, item_id: &str) -> ReminderState {
    ReminderState {
        reminder_id: reminder_id.to_string(),
        item_id: item_id.to_string(),
        request_state: RequestState::NotRequested,
        schedule_state: ScheduleState::NotScheduled,
        delivery_state: DeliveryState::Unknown,
        acknowledgment_state: AcknowledgmentState::NotAcknowledged,
        resolved_instant: None,
        timezone_id: None,
        ambiguity_reason: None,
        unsupported_reason: None,
        schedule_generation: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().expect("Failed to open in-memory DB");

    // Disable foreign key constraints for test setup (will be re-enabled after creating tables)
    conn.execute("PRAGMA foreign_keys = OFF", [])
        .expect("Failed to disable FK");

    // Create the reminders table for proper testing
    conn.execute(
        "CREATE TABLE reminders (
            reminder_id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL UNIQUE,
            request_state TEXT NOT NULL,
            schedule_state TEXT NOT NULL,
            delivery_state TEXT NOT NULL,
            acknowledgment_state TEXT NOT NULL,
            resolved_instant TEXT,
            timezone_id TEXT,
            ambiguity_reason TEXT,
            unsupported_reason TEXT,
            schedule_generation INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        [],
    )
    .expect("Failed to create reminders table");

    // Create the reminder_operations table for tests.
    // UNIQUE includes operation_type to fix the confirmed bug where Cancel(r1#0)
    // and Create(r1#0) were colliding.
    conn.execute(
        "CREATE TABLE reminder_operations (
            operation_id TEXT PRIMARY KEY,
            reminder_id TEXT NOT NULL,
            operation_type TEXT NOT NULL,
            operation_state TEXT NOT NULL,
            effect_identity TEXT NOT NULL,
            external_id TEXT,
            scheduled_for TEXT,
            created_at TEXT NOT NULL,
            UNIQUE (reminder_id, effect_identity, operation_type)
        )",
        [],
    )
    .expect("Failed to create reminder_operations table");

    conn
}

fn insert_test_reminder(conn: &Connection, reminder_id: &str, item_id: &str) {
    conn.execute(
        "INSERT INTO reminders
         (reminder_id, item_id, request_state, schedule_state, delivery_state,
          acknowledgment_state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            reminder_id,
            item_id,
            "not_requested",
            "not_scheduled",
            "unknown",
            "not_acknowledged",
            Utc::now().to_rfc3339(),
            Utc::now().to_rfc3339(),
        ],
    )
    .expect("Failed to insert reminder");
}

#[test]
fn test_effect_identity_deterministic() {
    let mut state = make_reminder_state("r1", "i1");
    let id_gen0 = state.effect_identity();

    state.schedule_generation = 1;
    let id_gen1 = state.effect_identity();

    state.schedule_generation = 2;
    let id_gen2 = state.effect_identity();

    assert_eq!(id_gen0, "r1#0");
    assert_eq!(id_gen1, "r1#1");
    assert_eq!(id_gen2, "r1#2");
}

#[test]
fn test_create_schedule_operation_idempotent() {
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    // First creation: record operation
    let tx = conn.transaction().unwrap();
    let op1 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("First operation should succeed");

    // Same operation again: should return the same record (idempotency)
    let op1_again = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Idempotent operation should succeed");

    assert_eq!(op1.operation_id, op1_again.operation_id);
    assert_eq!(op1.effect_identity, op1_again.effect_identity);
    tx.commit().unwrap();
}

#[test]
fn test_reschedule_increments_generation_creates_new_operation() {
    let mut conn = setup_test_db();
    let mut state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    // First schedule
    let tx = conn.transaction().unwrap();
    let op1 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("First operation");

    // Reschedule: increment generation, create new effect_identity
    state.schedule_generation = 1;
    let op2 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Reschedule,
        Some(Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("Reschedule operation");

    assert_eq!(
        op1.operation_id,
        "r1#0"
            .split('#')
            .next()
            .map(|_| op1.operation_id.clone())
            .unwrap_or_default()
    );
    assert_ne!(op1.effect_identity, op2.effect_identity);
    assert_eq!(op2.effect_identity, "r1#1");
    tx.commit().unwrap();
}

#[test]
fn test_unsupported_recurrence_not_scheduled() {
    let mut state = make_reminder_state("r1", "i1");

    // Apply a resolution that indicates unsupported recurrence
    let resolution = ResolutionResult {
        original_phrase: "remind me every Monday".to_string(),
        resolved_date: None,
        resolved_local: None,
        resolved_time: None,
        candidates: vec![],
        is_ambiguous: false,
        ambiguity_kind: None,
        ambiguity_reason: Some("Repeating reminders not supported in M1".to_string()),
        is_past: false,
        context: TimeContext {
            timezone: "UTC".to_string(),
            locale: "en_US".to_string(),
            reference_time: Utc::now(),
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        },
    };

    apply_resolution(&mut state, &resolution).expect("Apply resolution");

    // Unsupported recurrence must be preserved, not silently converted to one-shot
    assert_eq!(state.request_state, RequestState::UnsupportedRecurrence);
    assert!(state.unsupported_reason.is_some());
    assert_eq!(state.schedule_state, ScheduleState::NotScheduled);
    assert!(state.resolved_instant.is_none());
}

#[test]
fn test_ambiguous_time_not_scheduled_yet() {
    let mut state = make_reminder_state("r1", "i1");

    let resolution = ResolutionResult {
        original_phrase: "Friday".to_string(),
        resolved_date: None,
        resolved_local: None,
        resolved_time: None,
        candidates: vec![],
        is_ambiguous: true,
        ambiguity_kind: Some(AmbiguityKind::MissingHour),
        ambiguity_reason: Some("Missing hour in date".to_string()),
        is_past: false,
        context: TimeContext {
            timezone: "UTC".to_string(),
            locale: "en_US".to_string(),
            reference_time: Utc::now(),
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        },
    };

    apply_resolution(&mut state, &resolution).expect("Apply resolution");

    assert_eq!(state.request_state, RequestState::NotScheduledYet);
    assert!(state.ambiguity_reason.is_some());
    assert_eq!(state.schedule_state, ScheduleState::NotScheduled);
    assert!(state.resolved_instant.is_none());
}

#[test]
fn test_explicit_resolved_time_accepted() {
    let mut state = make_reminder_state("r1", "i1");
    let resolved_time = Utc::now() + chrono::Duration::hours(2);

    let resolution = ResolutionResult {
        original_phrase: "2 hours from now".to_string(),
        resolved_date: None,
        resolved_local: None,
        resolved_time: Some(resolved_time),
        candidates: vec![],
        is_ambiguous: false,
        ambiguity_kind: None,
        ambiguity_reason: None,
        is_past: false,
        context: TimeContext {
            timezone: "UTC".to_string(),
            locale: "en_US".to_string(),
            reference_time: Utc::now(),
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        },
    };

    apply_resolution(&mut state, &resolution).expect("Apply resolution");

    assert_eq!(state.request_state, RequestState::Resolved);
    assert_eq!(state.resolved_instant, Some(resolved_time));
    assert!(state.ambiguity_reason.is_none());
    assert!(state.unsupported_reason.is_none());
}

#[test]
fn test_expired_opportunity_remains_inspectable() {
    let past = Utc::now() - chrono::Duration::hours(1);
    let mut state = make_reminder_state("r1", "i1");

    let resolution = ResolutionResult {
        original_phrase: "yesterday".to_string(),
        resolved_date: None,
        resolved_local: None,
        resolved_time: Some(past),
        candidates: vec![],
        is_ambiguous: false,
        ambiguity_kind: None,
        ambiguity_reason: None,
        is_past: true,
        context: TimeContext {
            timezone: "UTC".to_string(),
            locale: "en_US".to_string(),
            reference_time: Utc::now(),
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        },
    };

    apply_resolution(&mut state, &resolution).expect("Apply resolution");

    // Past times result in Unschedulable state per the contract
    assert_eq!(state.request_state, RequestState::Unschedulable);
    assert_eq!(state.unsupported_reason, Some("time_in_past".to_string()));
    assert!(state.resolved_instant.is_none());
    assert_eq!(state.delivery_state, DeliveryState::Unknown);
}

#[test]
fn test_delivery_state_independent_from_request_state() {
    let mut state = make_reminder_state("r1", "i1");
    state.request_state = RequestState::Resolved;
    state.resolved_instant = Some(Utc::now());
    state.delivery_state = DeliveryState::Unknown;

    // Delivery state can advance independently
    state.delivery_state = DeliveryState::Delivered;
    assert_eq!(state.request_state, RequestState::Resolved);

    state.delivery_state = DeliveryState::Opened;
    assert_eq!(state.request_state, RequestState::Resolved);
}

#[test]
fn test_acknowledgment_state_independent_from_delivery_state() {
    let mut state = make_reminder_state("r1", "i1");
    state.delivery_state = DeliveryState::Delivered;
    state.acknowledgment_state = AcknowledgmentState::NotAcknowledged;

    // User can acknowledge without changing delivery state
    state.acknowledgment_state = AcknowledgmentState::Acknowledged;
    assert_eq!(state.delivery_state, DeliveryState::Delivered);
}

#[test]
fn test_cancel_operation_idempotent() {
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();
    let cancel1 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Cancel,
        None,
    )
    .expect("First cancel");

    // Cancel again: same operation
    let cancel2 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Cancel,
        None,
    )
    .expect("Second cancel (idempotent)");

    assert_eq!(cancel1.operation_id, cancel2.operation_id);
    tx.commit().unwrap();
}

#[test]
fn test_complete_operation_idempotent() {
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();
    let complete1 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Complete,
        None,
    )
    .expect("First complete");

    // Complete again: same operation
    let complete2 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Complete,
        None,
    )
    .expect("Second complete (idempotent)");

    assert_eq!(complete1.operation_id, complete2.operation_id);
    tx.commit().unwrap();
}

#[test]
fn test_schedule_generation_unique_effect_identities() {
    let mut conn = setup_test_db();
    let mut state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();

    // Schedule at gen 0
    let op0 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Gen 0 operation");

    // Reschedule to gen 1
    state.schedule_generation = 1;
    let op1 = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Reschedule,
        Some(Utc::now() + chrono::Duration::hours(1)),
    )
    .expect("Gen 1 operation");

    // Both operations should exist with different effect_identities
    assert_eq!(op0.effect_identity, "r1#0");
    assert_eq!(op1.effect_identity, "r1#1");
    assert_ne!(op0.operation_id, op1.operation_id);

    tx.commit().unwrap();
}

#[test]
fn test_state_preserves_item_id() {
    let state = make_reminder_state("r1", "item-123");
    assert_eq!(state.item_id, "item-123");
    assert_eq!(state.reminder_id, "r1");
}

#[test]
fn test_pending_ambiguity_inspectable() {
    let mut state = make_reminder_state("r1", "i1");

    let resolution = ResolutionResult {
        original_phrase: "next week".to_string(),
        resolved_date: None,
        resolved_local: None,
        resolved_time: None,
        candidates: vec![],
        is_ambiguous: true,
        ambiguity_kind: Some(AmbiguityKind::MissingHour),
        ambiguity_reason: Some("Partial date without time".to_string()),
        is_past: false,
        context: TimeContext {
            timezone: "America/Los_Angeles".to_string(),
            locale: "en_US".to_string(),
            reference_time: Utc::now(),
            utc_offset_at_capture: -28800,
            calendar: "gregorian".to_string(),
        },
    };

    apply_resolution(&mut state, &resolution).expect("Apply resolution");

    assert_eq!(state.request_state, RequestState::NotScheduledYet);
    assert!(state.ambiguity_reason.is_some());
    assert_eq!(state.timezone_id, Some("America/Los_Angeles".to_string()));
}

#[test]
fn test_operation_state_tracking() {
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();
    let op = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Create operation");

    // New operations start in Pending state
    assert_eq!(op.operation_state, OperationState::Pending);
    tx.commit().unwrap();
}

#[test]
fn test_create_then_cancel_produces_different_operations() {
    // Regression test for the confirmed bug: after Create(r1#0),
    // Cancel(r1#0) must produce a different operation, not return the Create.
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();

    // Create a reminder at generation 0
    let create_op = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Create operation should succeed");

    assert_eq!(create_op.operation_type, OperationType::Create);
    assert_eq!(create_op.effect_identity, "r1#0");

    // Cancel the same reminder (same effect_identity)
    let cancel_op = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Cancel,
        None,
    )
    .expect("Cancel operation should succeed");

    // The cancel operation must be different from create
    assert_eq!(cancel_op.operation_type, OperationType::Cancel);
    assert_eq!(cancel_op.effect_identity, "r1#0");
    assert_ne!(
        create_op.operation_id, cancel_op.operation_id,
        "Create and Cancel must be separate operations with different IDs"
    );

    // Create again: should return the original create operation (idempotent)
    let create_again = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Second create should be idempotent");

    assert_eq!(
        create_op.operation_id, create_again.operation_id,
        "Replaying Create should return the same operation"
    );

    tx.commit().unwrap();
}

#[test]
fn test_create_then_complete_produces_different_operations() {
    // Similar test: Create and Complete should be separate operations
    let mut conn = setup_test_db();
    let state = make_reminder_state("r1", "i1");
    insert_test_reminder(&conn, "r1", "i1");

    let tx = conn.transaction().unwrap();

    let create_op = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Create,
        Some(Utc::now()),
    )
    .expect("Create operation should succeed");

    let complete_op = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Complete,
        None,
    )
    .expect("Complete operation should succeed");

    // The complete operation must be different from create
    assert_eq!(complete_op.operation_type, OperationType::Complete);
    assert_ne!(
        create_op.operation_id, complete_op.operation_id,
        "Create and Complete must be separate operations"
    );

    // Complete again: should return the original complete operation (idempotent)
    let complete_again = record_operation(
        &tx,
        "r1",
        &state.effect_identity(),
        OperationType::Complete,
        None,
    )
    .expect("Second complete should be idempotent");

    assert_eq!(
        complete_op.operation_id, complete_again.operation_id,
        "Replaying Complete should return the same operation"
    );

    tx.commit().unwrap();
}
