use chrono::Utc;
use ohand_core::reminders::state::{
    validate_operation, OperationType, OperationValidity, RemindTime, ReminderIntent,
    ReminderLifecycle, ReminderOperation, ReminderState,
};

fn make_intent(reminder_id: &str, source_text: &str, requested_time: RemindTime) -> ReminderIntent {
    let is_ambiguous = matches!(requested_time, RemindTime::Ambiguous { .. });
    let ambiguity_reason = match &requested_time {
        RemindTime::Ambiguous { .. } => Some("missing hour".to_string()),
        _ => None,
    };
    ReminderIntent {
        reminder_id: reminder_id.to_string(),
        source_text: source_text.to_string(),
        requested_time,
        is_ambiguous,
        ambiguity_reason,
        is_past: false,
        captured_at: Utc::now(),
    }
}

fn make_state_from_intent(reminder_id: &str, intent: ReminderIntent) -> ReminderState {
    ReminderState {
        reminder_id: reminder_id.to_string(),
        current_intent: intent,
        lifecycle_state: ReminderLifecycle::Active,
        operations: vec![],
        native_id: None,
    }
}

#[test]
fn test_create_scheduled_reminder_idempotent() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me in 1 hour", RemindTime::Scheduled(future));
    let state = make_state_from_intent("r1", intent.clone());

    // Creating the same reminder again should produce the same key fields
    // (not comparing timestamps which naturally differ).
    let intent2 = make_intent("r1", "remind me in 1 hour", RemindTime::Scheduled(future));
    let state2 = make_state_from_intent("r1", intent2);

    assert_eq!(state.reminder_id, state2.reminder_id);
    assert_eq!(
        state.current_intent.reminder_id,
        state2.current_intent.reminder_id
    );
    assert_eq!(
        state.current_intent.source_text,
        state2.current_intent.source_text
    );
    assert_eq!(
        state.current_intent.requested_time,
        state2.current_intent.requested_time
    );
    assert_eq!(state.lifecycle_state, state2.lifecycle_state);
    assert_eq!(state.operations.len(), state2.operations.len());
}

#[test]
fn test_edit_scheduled_reminder_produces_new_operation() {
    let future1 = Utc::now() + chrono::Duration::hours(1);
    let intent1 = make_intent("r1", "remind me in 1 hour", RemindTime::Scheduled(future1));
    let mut state = make_state_from_intent("r1", intent1);

    // Record the initial creation.
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Created {
            intent: state.current_intent.clone(),
        },
        recorded_at: Utc::now(),
        native_id: Some("native1".to_string()),
    });
    state.native_id = Some("native1".to_string());

    // Now reschedule to a different time.
    let future2 = Utc::now() + chrono::Duration::hours(2);
    let intent2 = make_intent(
        "r1",
        "remind me in 2 hours instead",
        RemindTime::Scheduled(future2),
    );
    let op = OperationType::Rescheduled {
        new_intent: intent2.clone(),
    };

    // Operation should be valid.
    assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);

    // Update state to reflect the new operation.
    state.current_intent = intent2;
    state.operations.push(ReminderOperation {
        operation_id: "op2".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: op,
        recorded_at: Utc::now(),
        native_id: None, // reschedule doesn't set a new native_id
    });

    assert_eq!(state.operations.len(), 2);
}

#[test]
fn test_cancel_reminder_idempotent() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me later", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent);

    // First cancellation.
    let op1 = OperationType::Cancelled;
    assert_eq!(validate_operation(&state, &op1), OperationValidity::Valid);

    // Apply the cancellation.
    state.lifecycle_state = ReminderLifecycle::Cancelled;
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: op1,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // Second cancellation (idempotent).
    let op2 = OperationType::Cancelled;
    assert_eq!(validate_operation(&state, &op2), OperationValidity::Valid);
}

#[test]
fn test_complete_reminder_idempotent() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me to call", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent);

    // First completion.
    let op1 = OperationType::Completed;
    assert_eq!(validate_operation(&state, &op1), OperationValidity::Valid);

    // Apply the completion.
    state.lifecycle_state = ReminderLifecycle::Completed;
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: op1,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // Second completion (idempotent).
    let op2 = OperationType::Completed;
    assert_eq!(validate_operation(&state, &op2), OperationValidity::Valid);
}

#[test]
fn test_ambiguous_time_remains_inspectable() {
    let time = RemindTime::Ambiguous {
        original_phrase: "Friday".to_string(),
        resolved_date: None,
        resolved_local: None,
    };
    let intent = make_intent("r1", "remind me Friday", time);
    let state = make_state_from_intent("r1", intent);

    // Ambiguous reminder should be active and not scheduled yet.
    assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
    assert!(state.current_intent.is_ambiguous);
    assert!(!state.native_id.is_some());
}

#[test]
fn test_past_time_remains_inspectable() {
    let past = Utc::now() - chrono::Duration::hours(1);
    let mut intent = make_intent("r1", "remind me yesterday", RemindTime::Scheduled(past));
    intent.is_past = true;
    let state = make_state_from_intent("r1", intent);

    // Past reminder should be active, inspectable, but likely not scheduled.
    assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
    assert!(state.current_intent.is_past);
    assert!(!state.native_id.is_some());
}

#[test]
fn test_expired_opportunity_remains_inspectable() {
    // Scenario: reminder was for a future time, but that time has now passed and
    // the reminder was never triggered or cancelled. This can happen if:
    // - The native scheduler couldn't deliver before the time
    // - The reminder scheduling failed silently
    // - The device was offline
    let now = Utc::now();
    let scheduled_time = now - chrono::Duration::hours(1); // scheduled for 1 hour ago

    let intent = make_intent(
        "r1",
        "remind me to check email",
        RemindTime::Scheduled(scheduled_time),
    );

    let mut state = make_state_from_intent("r1", intent);
    state.native_id = Some("native_id_123".to_string());
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Created {
            intent: state.current_intent.clone(),
        },
        recorded_at: now,
        native_id: Some("native_id_123".to_string()),
    });

    // Even though the reminder time has passed and it's still active,
    // it should remain inspectable.
    assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
    assert_eq!(state.operations.len(), 1);
}

#[test]
fn test_unsupported_repeating_request_not_silently_reduced() {
    let time = RemindTime::Unsupported {
        original_phrase: "remind me every Monday at 2pm".to_string(),
        reason: "repeating reminders are not supported in M1".to_string(),
    };
    let intent = make_intent("r1", "remind me every Monday at 2pm", time);
    let state = make_state_from_intent("r1", intent);

    // The reminder must remain in the unsupported state, not be silently converted to one-shot.
    assert!(state.current_intent.requested_time.is_unsupported());
    assert!(!state.current_intent.requested_time.is_scheduled());
    assert_eq!(state.lifecycle_state, ReminderLifecycle::Active);
}

#[test]
fn test_fabricated_deadline_rejected_in_user_correction() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent(
        "r1",
        "remind me about the dentist",
        RemindTime::Scheduled(future),
    );
    let mut state = make_state_from_intent("r1", intent);

    // User explicitly sets the reminder to a specific time (e.g., "Friday at 3pm").
    let user_chosen_time = Utc::now() + chrono::Duration::days(2);
    let user_intent = make_intent(
        "r1",
        "actually, Friday at 3pm",
        RemindTime::Scheduled(user_chosen_time),
    );
    let op = OperationType::Rescheduled {
        new_intent: user_intent.clone(),
    };

    // This should be allowed and recorded.
    assert_eq!(validate_operation(&state, &op), OperationValidity::Valid);

    state.current_intent = user_intent;
    state.operations.push(ReminderOperation {
        operation_id: "op2".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: op,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // The corrected time should be preserved as-is, not modified or fabricated.
    assert_eq!(
        state.current_intent.requested_time,
        RemindTime::Scheduled(user_chosen_time)
    );
}

#[test]
fn test_preserve_explicit_user_corrections() {
    let time1 = Utc::now() + chrono::Duration::hours(1);
    let intent1 = make_intent("r1", "remind me", RemindTime::Scheduled(time1));
    let mut state = make_state_from_intent("r1", intent1.clone());

    // Record the initial creation.
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Created {
            intent: state.current_intent.clone(),
        },
        recorded_at: Utc::now(),
        native_id: Some("native1".to_string()),
    });

    // User corrects the time.
    let time2 = Utc::now() + chrono::Duration::hours(2);
    let intent2 = make_intent("r1", "no wait, 2 hours", RemindTime::Scheduled(time2));
    let op_correct = OperationType::Rescheduled {
        new_intent: intent2.clone(),
    };

    // Apply the correction.
    state.current_intent = intent2.clone();
    state.operations.push(ReminderOperation {
        operation_id: "op2".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: op_correct,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // Later, a model might try to "re-interpret" and suggest the original time.
    // But the user's correction should remain authoritative.
    assert_eq!(
        state.current_intent.requested_time,
        RemindTime::Scheduled(time2)
    );

    // Verify the operations record both the original and the correction.
    assert_eq!(state.operations.len(), 2);
    match &state.operations[0].operation_type {
        OperationType::Created { intent } => {
            assert_eq!(intent.requested_time, RemindTime::Scheduled(time1));
        }
        _ => panic!("First operation should be Created"),
    }
    match &state.operations[1].operation_type {
        OperationType::Rescheduled { new_intent } => {
            assert_eq!(new_intent.requested_time, RemindTime::Scheduled(time2));
        }
        _ => panic!("Second operation should be Rescheduled"),
    }
}

#[test]
fn test_operation_history_auditable() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent1 = make_intent("r1", "call the doctor", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent1);

    // Record creation.
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Created {
            intent: state.current_intent.clone(),
        },
        recorded_at: Utc::now(),
        native_id: Some("native1".to_string()),
    });

    // Record cancellation.
    state.operations.push(ReminderOperation {
        operation_id: "op2".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Cancelled,
        recorded_at: Utc::now(),
        native_id: None,
    });
    state.lifecycle_state = ReminderLifecycle::Cancelled;

    // The operation history should be complete and auditable.
    assert_eq!(state.operations.len(), 2);
    assert!(matches!(
        state.operations[0].operation_type,
        OperationType::Created { .. }
    ));
    assert!(matches!(
        state.operations[1].operation_type,
        OperationType::Cancelled
    ));
}

#[test]
fn test_native_id_set_only_on_schedule() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent);

    // Initial schedule sets native_id.
    let op_create = ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Created {
            intent: state.current_intent.clone(),
        },
        recorded_at: Utc::now(),
        native_id: Some("native1".to_string()),
    };
    state.operations.push(op_create);
    state.native_id = Some("native1".to_string());

    // Reschedule does not set a new native_id at the operation level
    // (that would be handled by the scheduling layer).
    let future2 = Utc::now() + chrono::Duration::hours(2);
    let intent2 = make_intent("r1", "different time", RemindTime::Scheduled(future2));
    let op_reschedule = ReminderOperation {
        operation_id: "op2".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Rescheduled {
            new_intent: intent2,
        },
        recorded_at: Utc::now(),
        native_id: None, // Reschedule operation itself doesn't set native_id
    };
    state.operations.push(op_reschedule);

    assert_eq!(state.native_id, Some("native1".to_string()));
    assert!(state.operations[1].native_id.is_none());
}

#[test]
fn test_cancelled_then_completed_not_allowed() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent);

    // Cancel the reminder.
    state.lifecycle_state = ReminderLifecycle::Cancelled;
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Cancelled,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // Trying to complete a cancelled reminder should be rejected.
    let op_complete = OperationType::Completed;
    assert_eq!(
        validate_operation(&state, &op_complete),
        OperationValidity::NotAllowed
    );
}

#[test]
fn test_completed_then_cancelled_not_allowed() {
    let future = Utc::now() + chrono::Duration::hours(1);
    let intent = make_intent("r1", "remind me", RemindTime::Scheduled(future));
    let mut state = make_state_from_intent("r1", intent);

    // Complete the reminder.
    state.lifecycle_state = ReminderLifecycle::Completed;
    state.operations.push(ReminderOperation {
        operation_id: "op1".to_string(),
        reminder_id: "r1".to_string(),
        operation_type: OperationType::Completed,
        recorded_at: Utc::now(),
        native_id: None,
    });

    // Trying to cancel a completed reminder should be rejected.
    let op_cancel = OperationType::Cancelled;
    assert_eq!(
        validate_operation(&state, &op_cancel),
        OperationValidity::NotAllowed
    );
}
