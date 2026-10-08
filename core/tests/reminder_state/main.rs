use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::Transaction;
use std::sync::Arc;

use ohand_core::reminders::state::{
    apply_derived_request, apply_user_time_correction, cancel_for_inactive_item, cancel_reminder,
    desired_notifications, get_reminder, list_operations, notification_identifier,
    AcknowledgmentState, DeliveryState, DerivedReminderRequest, OperationState, OperationType,
    ReminderRecord, ReminderStateError, RequestState, ScheduleState, UnschedulableReason,
    UserTimeCorrection,
};
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use ohand_core::store::schema::{Clock, Database};

struct FixedClock {
    instant: DateTime<Utc>,
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

fn instant(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn capture_instant() -> DateTime<Utc> {
    instant("2026-01-15T10:30:00Z")
}

fn clock_at(moment: DateTime<Utc>) -> FixedClock {
    FixedClock { instant: moment }
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_reminder_state_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn open_db(path: &str) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(clock_at(capture_instant()));
    Database::open(path, clock)
}

/// Insert a capture and its item; when `as_action`, type it as an action through the real
/// correction path so the item ends at revision 1.
fn insert_item(db: &mut Database, item_id: &str, text: &str, as_action: bool) -> Result<i32> {
    let tx = db.transaction()?;
    let capture_id = format!("cap-{item_id}");
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, NULL, '2026-01-15T10:30:00Z', 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, '2026-01-15T10:30:00Z', NULL)",
        rusqlite::params![&capture_id, text],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', '2026-01-15T10:30:00Z', '2026-01-15T10:30:00Z')",
        rusqlite::params![item_id, &capture_id],
    )?;
    tx.commit()?;
    if !as_action {
        return Ok(0);
    }
    let event = Event::new(
        format!("evt-type-{item_id}"),
        item_id.to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(db, &event, 0)?;
    Ok(1)
}

fn complete_item(db: &mut Database, item_id: &str, revision: i32) -> Result<()> {
    let event = Event::new(
        format!("evt-complete-{item_id}"),
        item_id.to_string(),
        revision,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T11:00:00Z".to_string(),
    )?;
    save_event(db, &event, revision)?;
    Ok(())
}

fn item_revision(db: &mut Database, item_id: &str) -> Result<i32> {
    Ok(db.conn().query_row(
        "SELECT revision FROM items WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )?)
}

/// Run one command in an immediate transaction; commit on success, roll back on error.
fn run<T>(
    db: &mut Database,
    command: impl FnOnce(&Transaction<'_>) -> Result<T, ReminderStateError>,
) -> Result<T, ReminderStateError> {
    let tx = db.immediate_transaction()?;
    let value = command(&tx)?;
    tx.commit()?;
    Ok(value)
}

fn derived(item_id: &str, source_revision: i32, phrase: &str) -> DerivedReminderRequest {
    DerivedReminderRequest {
        item_id: item_id.to_string(),
        source_revision,
        phrase: phrase.to_string(),
    }
}

fn user_time(item_id: &str, expected_revision: i32, moment: &str) -> UserTimeCorrection {
    UserTimeCorrection {
        item_id: item_id.to_string(),
        expected_revision,
        instant: instant(moment),
        timezone_id: "UTC".to_string(),
    }
}

fn reminder(db: &mut Database, item_id: &str) -> Result<Option<ReminderRecord>> {
    Ok(run(db, |tx| get_reminder(tx, item_id))?)
}

fn operations(db: &mut Database, item_id: &str) -> Result<Vec<(OperationType, OperationState)>> {
    let Some(record) = reminder(db, item_id)? else {
        return Ok(vec![]);
    };
    let operations = run(db, |tx| list_operations(tx, &record.reminder_id))?;
    Ok(operations
        .into_iter()
        .map(|operation| (operation.operation_type, operation.operation_state))
        .collect())
}

const ROOFER_TEXT: &str = "call the roofer 2026-01-16 09:00:00";

#[test]
fn derived_request_records_desired_state_and_one_schedule_operation() -> Result<()> {
    let mut db = open_db(&temp_db_path("derived_create"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());

    let record = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;

    assert_eq!(record.request_state, RequestState::Resolved);
    assert_eq!(record.schedule_state, ScheduleState::PendingSchedule);
    assert_eq!(
        record.resolved_instant,
        Some(instant("2026-01-16T09:00:00Z"))
    );
    assert_eq!(record.schedule_generation, 1);
    assert_eq!(
        record.notification_id(),
        Some(notification_identifier(&record.reminder_id, 1))
    );

    let operations = run(&mut db, |tx| list_operations(tx, &record.reminder_id))?;
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].operation_type, OperationType::Schedule);
    assert_eq!(operations[0].operation_state, OperationState::Pending);
    assert_eq!(
        operations[0].notification_id,
        record.notification_id().unwrap()
    );
    assert_eq!(operations[0].scheduled_for, record.resolved_instant);

    let desired = run(&mut db, desired_notifications)?;
    assert_eq!(desired.len(), 1);
    assert_eq!(
        desired[0].notification_id,
        record.notification_id().unwrap()
    );
    assert_eq!(desired[0].fire_at, instant("2026-01-16T09:00:00Z"));
    Ok(())
}

#[test]
fn reminder_commands_do_not_change_the_item_revision() -> Result<()> {
    let mut db = open_db(&temp_db_path("revision_unchanged"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-17T09:00:00Z"),
        )
    })?;
    run(&mut db, |tx| {
        cancel_reminder(tx, &clock, "item-1", revision)
    })?;
    assert_eq!(item_revision(&mut db, "item-1")?, revision);
    Ok(())
}

#[test]
fn replayed_derived_request_is_idempotent_even_after_the_due_time_passes() -> Result<()> {
    let mut db = open_db(&temp_db_path("derived_replay"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let request = derived("item-1", revision, "2026-01-16 09:00:00");

    let first = run(&mut db, |tx| {
        apply_derived_request(tx, &clock_at(capture_instant()), &request)
    })?;
    let replay = run(&mut db, |tx| {
        apply_derived_request(tx, &clock_at(capture_instant()), &request)
    })?;
    assert_eq!(first, replay);

    let much_later = clock_at(instant("2026-02-01T00:00:00Z"));
    let late_replay = run(&mut db, |tx| {
        apply_derived_request(tx, &much_later, &request)
    })?;
    assert_eq!(late_replay.request_state, RequestState::Resolved);
    assert_eq!(late_replay, first);
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    Ok(())
}

#[test]
fn rescheduling_starts_a_new_generation_and_retires_the_old_identifier() -> Result<()> {
    let mut db = open_db(&temp_db_path("reschedule"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let first = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;

    let correction = user_time("item-1", revision, "2026-01-17T15:00:00Z");
    let second = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock, &correction)
    })?;
    assert_eq!(second.reminder_id, first.reminder_id);
    assert_eq!(second.schedule_generation, 2);
    assert_ne!(second.notification_id(), first.notification_id());
    assert_eq!(
        second.resolved_instant,
        Some(instant("2026-01-17T15:00:00Z"))
    );

    assert_eq!(
        operations(&mut db, "item-1")?,
        vec![
            (OperationType::Schedule, OperationState::Superseded),
            (OperationType::Cancel, OperationState::Pending),
            (OperationType::Schedule, OperationState::Pending),
        ]
    );
    let desired = run(&mut db, desired_notifications)?;
    assert_eq!(desired.len(), 1);
    assert_eq!(
        desired[0].notification_id,
        second.notification_id().unwrap()
    );

    let replay = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock, &correction)
    })?;
    assert_eq!(replay, second);
    assert_eq!(operations(&mut db, "item-1")?.len(), 3);
    Ok(())
}

#[test]
fn cancelling_a_scheduled_reminder_records_one_cancel_for_its_identifier() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancel"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let scheduled = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;

    let cancelled = run(&mut db, |tx| {
        cancel_reminder(tx, &clock, "item-1", revision)
    })?
    .expect("reminder exists");
    assert_eq!(cancelled.request_state, RequestState::Cancelled);
    let replay = run(&mut db, |tx| {
        cancel_reminder(tx, &clock, "item-1", revision)
    })?;
    assert_eq!(replay, Some(cancelled));

    let operations = run(&mut db, |tx| list_operations(tx, &scheduled.reminder_id))?;
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[1].operation_type, OperationType::Cancel);
    assert_eq!(
        operations[1].notification_id,
        scheduled.notification_id().unwrap()
    );
    assert!(run(&mut db, desired_notifications)?.is_empty());
    Ok(())
}

#[test]
fn cancelling_without_a_reminder_is_a_no_op() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancel_none"))?;
    let revision = insert_item(&mut db, "item-1", "call the roofer", true)?;
    let clock = clock_at(capture_instant());
    assert_eq!(
        run(&mut db, |tx| cancel_reminder(
            tx, &clock, "item-1", revision
        ))?,
        None
    );
    Ok(())
}

#[test]
fn completing_the_item_cancels_the_reminder_once() -> Result<()> {
    let mut db = open_db(&temp_db_path("complete"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let scheduled = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;

    let still_active = run(&mut db, |tx| cancel_for_inactive_item(tx, &clock, "item-1"));
    assert!(matches!(
        still_active,
        Err(ReminderStateError::ItemStillActive { .. })
    ));
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);

    complete_item(&mut db, "item-1", revision)?;
    let cancelled = run(&mut db, |tx| cancel_for_inactive_item(tx, &clock, "item-1"))?
        .expect("reminder exists");
    assert_eq!(cancelled.request_state, RequestState::Cancelled);
    let replay = run(&mut db, |tx| cancel_for_inactive_item(tx, &clock, "item-1"))?;
    assert_eq!(replay, Some(cancelled));

    let operations = run(&mut db, |tx| list_operations(tx, &scheduled.reminder_id))?;
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[1].operation_type, OperationType::Cancel);
    assert_eq!(
        operations[1].notification_id,
        scheduled.notification_id().unwrap()
    );
    Ok(())
}

#[test]
fn a_completed_item_accepts_no_new_reminder_time() -> Result<()> {
    let mut db = open_db(&temp_db_path("completed_rejects"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    complete_item(&mut db, "item-1", revision)?;
    let clock = clock_at(capture_instant());
    let completed_revision = item_revision(&mut db, "item-1")?;
    let result = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", completed_revision, "2026-01-17T09:00:00Z"),
        )
    });
    assert!(matches!(
        result,
        Err(ReminderStateError::ItemNotActive { .. })
    ));
    assert_eq!(reminder(&mut db, "item-1")?, None);
    Ok(())
}

#[test]
fn user_can_restore_a_cancelled_reminder_under_a_fresh_identifier() -> Result<()> {
    let mut db = open_db(&temp_db_path("restore"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let first = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    run(&mut db, |tx| {
        cancel_reminder(tx, &clock, "item-1", revision)
    })?;

    let restored = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-16T09:00:00Z"),
        )
    })?;
    assert_eq!(restored.request_state, RequestState::Resolved);
    assert_eq!(restored.schedule_generation, 2);
    assert_ne!(restored.notification_id(), first.notification_id());
    assert_eq!(
        operations(&mut db, "item-1")?,
        vec![
            (OperationType::Schedule, OperationState::Superseded),
            (OperationType::Cancel, OperationState::Pending),
            (OperationType::Schedule, OperationState::Pending),
        ]
    );
    Ok(())
}

#[test]
fn ambiguous_time_stays_pending_and_inspectable_with_no_operation() -> Result<()> {
    let mut db = open_db(&temp_db_path("ambiguous"))?;
    let revision = insert_item(&mut db, "item-1", "pay the invoice 2026-01-20", true)?;
    let clock = clock_at(capture_instant());

    let record = run(&mut db, |tx| {
        apply_derived_request(tx, &clock, &derived("item-1", revision, "2026-01-20"))
    })?;
    assert_eq!(record.request_state, RequestState::NotScheduledYet);
    assert_eq!(record.schedule_state, ScheduleState::NotScheduled);
    assert_eq!(record.resolved_instant, None);
    assert!(record.ambiguity_reason.as_deref().unwrap().contains("hour"));
    assert!(operations(&mut db, "item-1")?.is_empty());
    assert!(run(&mut db, desired_notifications)?.is_empty());

    let corrected = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-20T08:00:00Z"),
        )
    })?;
    assert_eq!(corrected.request_state, RequestState::Resolved);
    assert_eq!(corrected.ambiguity_reason, None);
    assert_eq!(corrected.schedule_generation, 1);
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    Ok(())
}

#[test]
fn unparseable_phrase_present_in_text_is_not_scheduled_yet() -> Result<()> {
    let mut db = open_db(&temp_db_path("unparseable"))?;
    let revision = insert_item(&mut db, "item-1", "call the roofer sometime soon", true)?;
    let clock = clock_at(capture_instant());
    let record = run(&mut db, |tx| {
        apply_derived_request(tx, &clock, &derived("item-1", revision, "sometime soon"))
    })?;
    assert_eq!(record.request_state, RequestState::NotScheduledYet);
    assert_eq!(record.unsupported_reason, None);
    assert!(operations(&mut db, "item-1")?.is_empty());
    Ok(())
}

#[test]
fn past_time_is_unschedulable_and_the_expired_opportunity_stays_inspectable() -> Result<()> {
    let mut db = open_db(&temp_db_path("past"))?;
    let revision = insert_item(
        &mut db,
        "item-1",
        "call the roofer 2026-01-14 09:00:00",
        true,
    )?;
    let clock = clock_at(capture_instant());

    let record = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-14 09:00:00"),
        )
    })?;
    assert_eq!(
        record.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(
        record.resolved_instant,
        Some(instant("2026-01-14T09:00:00Z"))
    );
    assert_eq!(record.schedule_generation, 0);
    assert!(operations(&mut db, "item-1")?.is_empty());
    assert!(run(&mut db, desired_notifications)?.is_empty());

    let reloaded = reminder(&mut db, "item-1")?.unwrap();
    assert_eq!(reloaded, record);
    Ok(())
}

#[test]
fn user_time_in_the_past_is_not_adjusted_or_scheduled() -> Result<()> {
    let mut db = open_db(&temp_db_path("user_past"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let record = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-15T09:00:00Z"),
        )
    })?;
    assert_eq!(
        record.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(
        record.resolved_instant,
        Some(instant("2026-01-15T09:00:00Z"))
    );
    assert!(operations(&mut db, "item-1")?.is_empty());
    Ok(())
}

#[test]
fn moving_a_scheduled_reminder_into_the_past_retires_the_old_identifier() -> Result<()> {
    let mut db = open_db(&temp_db_path("move_to_past"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    let record = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-15T09:00:00Z"),
        )
    })?;
    assert_eq!(
        record.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(
        operations(&mut db, "item-1")?,
        vec![
            (OperationType::Schedule, OperationState::Superseded),
            (OperationType::Cancel, OperationState::Pending),
        ]
    );
    assert!(run(&mut db, desired_notifications)?.is_empty());
    Ok(())
}

#[test]
fn passage_of_the_due_time_does_not_change_delivery_or_acknowledgment() -> Result<()> {
    let mut db = open_db(&temp_db_path("delivery"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let record = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock_at(capture_instant()),
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    assert!(!record.due_time_passed(capture_instant()));
    let later = instant("2026-01-20T00:00:00Z");
    assert!(record.due_time_passed(later));

    let after = reminder(&mut db, "item-1")?.unwrap();
    assert_eq!(after.delivery_state, DeliveryState::Unknown);
    assert_eq!(
        after.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );
    assert_eq!(after.request_state, RequestState::Resolved);
    Ok(())
}

#[test]
fn recurring_request_is_saved_unsupported_and_never_reduced_to_one_shot() -> Result<()> {
    let mut db = open_db(&temp_db_path("recurrence"))?;
    let revision = insert_item(&mut db, "item-1", "water the plants every day", true)?;
    let clock = clock_at(capture_instant());
    let request = derived("item-1", revision, "every day");

    let record = run(&mut db, |tx| apply_derived_request(tx, &clock, &request))?;
    assert_eq!(record.request_state, RequestState::UnsupportedRecurrence);
    assert_eq!(record.resolved_instant, None);
    assert_eq!(record.schedule_state, ScheduleState::NotScheduled);
    assert!(record.unsupported_reason.is_some());
    assert!(operations(&mut db, "item-1")?.is_empty());
    assert!(run(&mut db, desired_notifications)?.is_empty());

    let replay = run(&mut db, |tx| apply_derived_request(tx, &clock, &request))?;
    assert_eq!(replay, record);

    let manual = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-16T08:00:00Z"),
        )
    })?;
    assert_eq!(manual.request_state, RequestState::Resolved);
    assert_eq!(manual.unsupported_reason, None);
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    Ok(())
}

#[test]
fn fabricated_deadline_is_rejected_without_mutation() -> Result<()> {
    let mut db = open_db(&temp_db_path("fabricated"))?;
    let revision = insert_item(&mut db, "item-1", "call the roofer", true)?;
    let clock = clock_at(capture_instant());

    for phrase in ["2026-01-16 09:00:00", "", "   "] {
        let result = run(&mut db, |tx| {
            apply_derived_request(tx, &clock, &derived("item-1", revision, phrase))
        });
        assert!(
            matches!(result, Err(ReminderStateError::FabricatedDeadline(_))),
            "phrase {phrase:?} must be rejected, got {result:?}"
        );
    }
    assert_eq!(reminder(&mut db, "item-1")?, None);
    Ok(())
}

#[test]
fn phrase_matching_ignores_case_and_whitespace_but_not_content() -> Result<()> {
    let mut db = open_db(&temp_db_path("phrase_matching"))?;
    let revision = insert_item(&mut db, "item-1", "Water   plants EVERY\nDay", true)?;
    let clock = clock_at(capture_instant());
    let record = run(&mut db, |tx| {
        apply_derived_request(tx, &clock, &derived("item-1", revision, "every day"))
    })?;
    assert_eq!(record.request_state, RequestState::UnsupportedRecurrence);
    Ok(())
}

#[test]
fn derived_output_cannot_replace_an_explicit_user_correction() -> Result<()> {
    let mut db = open_db(&temp_db_path("precedence"))?;
    let text = "call the roofer 2026-01-16 09:00:00 or 2026-01-18 10:00:00";
    let revision = insert_item(&mut db, "item-1", text, true)?;
    let clock = clock_at(capture_instant());
    let corrected = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-17T12:00:00Z"),
        )
    })?;

    for phrase in ["2026-01-16 09:00:00", "2026-01-18 10:00:00"] {
        let result = run(&mut db, |tx| {
            apply_derived_request(tx, &clock, &derived("item-1", revision, phrase))
        });
        assert!(
            matches!(result, Err(ReminderStateError::CommittedTimeProtected)),
            "{phrase}: {result:?}"
        );
    }
    assert_eq!(reminder(&mut db, "item-1")?, Some(corrected));
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    Ok(())
}

#[test]
fn a_user_correction_over_a_derived_time_survives_reprocessing() -> Result<()> {
    let mut db = open_db(&temp_db_path("reprocess"))?;
    let text = "call the roofer 2026-01-16 09:00:00 or 2026-01-18 10:00:00";
    let revision = insert_item(&mut db, "item-1", text, true)?;
    let clock = clock_at(capture_instant());
    run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    let corrected = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-17T12:00:00Z"),
        )
    })?;

    let reprocessed = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-18 10:00:00"),
        )
    });
    assert!(matches!(
        reprocessed,
        Err(ReminderStateError::CommittedTimeProtected)
    ));
    assert_eq!(reminder(&mut db, "item-1")?, Some(corrected));
    Ok(())
}

#[test]
fn derived_output_cannot_revive_a_cancelled_reminder() -> Result<()> {
    let mut db = open_db(&temp_db_path("no_revive"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    })?;
    run(&mut db, |tx| {
        cancel_reminder(tx, &clock, "item-1", revision)
    })?;
    let result = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    });
    assert!(matches!(result, Err(ReminderStateError::ReminderCancelled)));
    assert_eq!(
        reminder(&mut db, "item-1")?.unwrap().request_state,
        RequestState::Cancelled
    );
    Ok(())
}

#[test]
fn stale_revisions_are_rejected_with_the_current_revision() -> Result<()> {
    let mut db = open_db(&temp_db_path("stale"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());

    let derived_result = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision - 1, "2026-01-16 09:00:00"),
        )
    });
    assert!(matches!(
        derived_result,
        Err(ReminderStateError::StaleRevision {
            expected: 0,
            current: 1,
            ..
        })
    ));
    let user_result = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock, &user_time("item-1", 7, "2026-01-17T09:00:00Z"))
    });
    assert!(matches!(
        user_result,
        Err(ReminderStateError::StaleRevision {
            expected: 7,
            current: 1,
            ..
        })
    ));
    let cancel_result = run(&mut db, |tx| cancel_reminder(tx, &clock, "item-1", 0));
    assert!(matches!(
        cancel_result,
        Err(ReminderStateError::StaleRevision { .. })
    ));
    assert_eq!(reminder(&mut db, "item-1")?, None);
    Ok(())
}

#[test]
fn only_actions_can_carry_reminders() -> Result<()> {
    let mut db = open_db(&temp_db_path("not_action"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, false)?;
    let clock = clock_at(capture_instant());
    let result = run(&mut db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &derived("item-1", revision, "2026-01-16 09:00:00"),
        )
    });
    assert!(matches!(
        result,
        Err(ReminderStateError::ItemCannotCarryReminder(_))
    ));
    let missing = run(&mut db, |tx| {
        apply_derived_request(tx, &clock, &derived("nope", 0, "2026-01-16 09:00:00"))
    });
    assert!(matches!(missing, Err(ReminderStateError::ItemNotFound(_))));
    Ok(())
}

#[test]
fn unknown_timezone_is_rejected() -> Result<()> {
    let mut db = open_db(&temp_db_path("bad_tz"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());
    let mut correction = user_time("item-1", revision, "2026-01-17T09:00:00Z");
    correction.timezone_id = "Mars/Olympus".to_string();
    let result = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock, &correction)
    });
    assert!(matches!(
        result,
        Err(ReminderStateError::InvalidTimezone(_))
    ));
    Ok(())
}

#[test]
fn failed_command_rolls_back_state_and_operations_together() -> Result<()> {
    let mut db = open_db(&temp_db_path("atomic"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let clock = clock_at(capture_instant());

    let result: Result<(), ReminderStateError> = run(&mut db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &user_time("item-1", revision, "2026-01-17T09:00:00Z"),
        )?;
        Err(ReminderStateError::Corrupt("simulated crash".to_string()))
    });
    assert!(result.is_err());
    assert_eq!(reminder(&mut db, "item-1")?, None);
    let operation_rows: i64 =
        db.conn()
            .query_row("SELECT COUNT(*) FROM reminder_operations", [], |row| {
                row.get(0)
            })?;
    assert_eq!(operation_rows, 0);
    Ok(())
}

#[test]
fn state_operations_and_idempotency_survive_reopening_the_database_file() -> Result<()> {
    let path = temp_db_path("reopen");
    let clock = clock_at(capture_instant());
    let (before, request) = {
        let mut db = open_db(&path)?;
        let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
        let request = derived("item-1", revision, "2026-01-16 09:00:00");
        let record = run(&mut db, |tx| apply_derived_request(tx, &clock, &request))?;
        run(&mut db, |tx| {
            apply_user_time_correction(
                tx,
                &clock,
                &user_time("item-1", revision, "2026-01-17T09:00:00Z"),
            )
        })?;
        (record, request)
    };

    let mut db = open_db(&path)?;
    let after = reminder(&mut db, "item-1")?.unwrap();
    assert_eq!(after.reminder_id, before.reminder_id);
    assert_eq!(after.schedule_generation, 2);
    assert_eq!(
        after.resolved_instant,
        Some(instant("2026-01-17T09:00:00Z"))
    );
    assert_eq!(operations(&mut db, "item-1")?.len(), 3);

    let result = run(&mut db, |tx| apply_derived_request(tx, &clock, &request));
    assert!(matches!(
        result,
        Err(ReminderStateError::CommittedTimeProtected)
    ));
    let replay = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock, &user_time("item-1", 1, "2026-01-17T09:00:00Z"))
    })?;
    assert_eq!(replay, after);
    assert_eq!(operations(&mut db, "item-1")?.len(), 3);
    Ok(())
}

#[test]
fn concurrent_retries_of_one_command_produce_one_reminder_and_one_operation() -> Result<()> {
    let path = temp_db_path("concurrent");
    let revision = {
        let mut db = open_db(&path)?;
        insert_item(&mut db, "item-1", ROOFER_TEXT, true)?
    };

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || -> Result<ReminderRecord> {
                let mut db = open_db(&path)?;
                let clock = clock_at(capture_instant());
                let correction = user_time("item-1", revision, "2026-01-16T09:00:00Z");
                Ok(run(&mut db, |tx| {
                    apply_user_time_correction(tx, &clock, &correction)
                })?)
            })
        })
        .collect();
    let records: Vec<ReminderRecord> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Result<_>>()?;
    assert!(records.windows(2).all(|pair| pair[0] == pair[1]));

    let mut db = open_db(&path)?;
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    let reminder_rows: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM reminders", [], |row| row.get(0))?;
    assert_eq!(reminder_rows, 1);
    assert_eq!(records[0].schedule_generation, 1);
    Ok(())
}

#[test]
fn late_replay_does_not_flip_a_scheduled_reminder_to_past() -> Result<()> {
    let mut db = open_db(&temp_db_path("late_user_replay"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT, true)?;
    let correction = user_time("item-1", revision, "2026-01-16T09:00:00Z");
    let first = run(&mut db, |tx| {
        apply_user_time_correction(tx, &clock_at(capture_instant()), &correction)
    })?;
    let later = clock_at(capture_instant() + Duration::days(30));
    let replay = run(&mut db, |tx| {
        apply_user_time_correction(tx, &later, &correction)
    })?;
    assert_eq!(replay, first);
    assert_eq!(operations(&mut db, "item-1")?.len(), 1);
    Ok(())
}
