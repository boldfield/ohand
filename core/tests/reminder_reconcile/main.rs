// N03: idempotent reconciliation of durable reminder state with the OS pending set.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::collections::BTreeMap;
use std::sync::Arc;

use ohand_core::lifecycle::delete_intent::{
    list_pending_deletion_work, mark_deletion_intent, mark_deletion_work_completed,
    DeletionWorkType,
};
use ohand_core::providers::contracts::ErrorClass;
use ohand_core::reminders::reconcile::{
    apply_completion, reconcile, CompletionOutcome, EffectCompletion, EffectOutcome,
    NotificationEffectError, NotificationEffects, PendingRequest, ReconcileError, ScheduleRequest,
};
use ohand_core::reminders::state::{
    apply_derived_request, apply_user_time_correction, cancel_reminder, get_reminder,
    list_operations, DeliveryState, DerivedReminderRequest, OperationState, OperationType,
    ReminderCancellation, ReminderRecord, ReminderStateError, RequestState, ScheduleState,
    UnschedulableReason, UserTimeCorrection,
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

fn clock_at(moment: &str) -> FixedClock {
    FixedClock {
        instant: instant(moment),
    }
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_reminder_reconcile_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn open_db(path: &str) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock {
        instant: capture_instant(),
    });
    Database::open(path, clock)
}

const ROOFER_TEXT: &str = "call the roofer 2026-01-16 09:00:00";
const DUE: &str = "2026-01-16T09:00:00Z";

fn insert_item(db: &mut Database, item_id: &str, text: &str) -> Result<i32> {
    let tx = db.transaction()?;
    let capture_id = format!("cap-{item_id}");
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, NULL, '2026-01-15T10:30:00Z', 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, '2026-01-15T10:30:00Z', NULL)",
        rusqlite::params![&capture_id, text],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', '2026-01-15T10:30:00Z', '2026-01-15T10:30:00Z')",
        rusqlite::params![item_id, &capture_id],
    )?;
    tx.commit()?;
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

fn run<T>(
    db: &mut Database,
    command: impl FnOnce(&Transaction<'_>) -> Result<T, ReminderStateError>,
) -> Result<T, ReminderStateError> {
    let tx = db.immediate_transaction()?;
    let value = command(&tx)?;
    tx.commit()?;
    Ok(value)
}

fn reminder_version(conn: &Connection, item_id: &str) -> i64 {
    conn.query_row(
        "SELECT state_version FROM reminders WHERE item_id = ?",
        [item_id],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
    .unwrap_or(0)
}

fn create_reminder(db: &mut Database, item_id: &str, revision: i32) -> Result<ReminderRecord> {
    let clock = clock_at("2026-01-15T10:30:00Z");
    Ok(run(db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &DerivedReminderRequest {
                item_id: item_id.to_string(),
                source_revision: revision,
                phrase: "2026-01-16 09:00:00".to_string(),
            },
        )
    })?)
}

fn move_reminder(
    db: &mut Database,
    item_id: &str,
    revision: i32,
    moment: &str,
) -> Result<ReminderRecord> {
    let clock = clock_at("2026-01-15T10:30:00Z");
    Ok(run(db, |tx| {
        let command = UserTimeCorrection {
            command_id: format!("cmd-{}", uuid::Uuid::new_v4()),
            item_id: item_id.to_string(),
            expected_revision: revision,
            expected_reminder_version: reminder_version(tx, item_id),
            instant: instant(moment),
            timezone_id: "UTC".to_string(),
        };
        apply_user_time_correction(tx, &clock, &command)
    })?)
}

fn cancel_user_reminder(db: &mut Database, item_id: &str, revision: i32) -> Result<()> {
    let clock = clock_at("2026-01-15T10:30:00Z");
    run(db, |tx| {
        let command = ReminderCancellation {
            command_id: format!("cmd-{}", uuid::Uuid::new_v4()),
            item_id: item_id.to_string(),
            expected_revision: revision,
            expected_reminder_version: reminder_version(tx, item_id),
        };
        cancel_reminder(tx, &clock, &command)
    })?;
    Ok(())
}

fn reminder(db: &mut Database, item_id: &str) -> Result<ReminderRecord> {
    Ok(run(db, |tx| get_reminder(tx, item_id))?.expect("reminder exists"))
}

fn operation_states(
    db: &mut Database,
    item_id: &str,
) -> Result<Vec<(OperationType, String, OperationState)>> {
    let record = reminder(db, item_id)?;
    Ok(run(db, |tx| list_operations(tx, &record.reminder_id))?
        .into_iter()
        .map(|operation| {
            (
                operation.operation_type,
                operation.notification_id,
                operation.operation_state,
            )
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    List,
    Schedule(String),
    Cancel(String),
}

/// An in-memory stand-in for the OS pending-request set that records every call.
#[derive(Default)]
struct FakeCenter {
    pending: BTreeMap<String, DateTime<Utc>>,
    calls: Vec<Call>,
    fail_list: Option<NotificationEffectError>,
    fail_schedule: Option<NotificationEffectError>,
    fail_cancel: Option<NotificationEffectError>,
    install_then_report_cancelled: bool,
    hide_due_instants: bool,
    targets: BTreeMap<String, String>,
    on_schedule: Option<Box<dyn FnMut()>>,
    on_cancel: Option<Box<dyn FnMut()>>,
}

impl FakeCenter {
    fn mutations(&self) -> Vec<&Call> {
        self.calls
            .iter()
            .filter(|call| !matches!(call, Call::List))
            .collect()
    }

    fn clear_calls(&mut self) {
        self.calls.clear();
    }

    fn pending_ids(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.calls
            .push(Call::Schedule(request.notification_id.clone()));
        if let Some(error) = &self.fail_schedule {
            return Err(error.clone());
        }
        if let Some(hook) = self.on_schedule.as_mut() {
            hook();
        }
        self.pending
            .insert(request.notification_id.clone(), request.fire_at);
        self.targets.insert(
            request.notification_id.clone(),
            request.opaque_target_id.clone(),
        );
        if self.install_then_report_cancelled {
            return Err(NotificationEffectError::new(
                ErrorClass::Cancelled,
                "cancelled",
            ));
        }
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.calls.push(Call::Cancel(notification_id.to_string()));
        if let Some(error) = &self.fail_cancel {
            return Err(error.clone());
        }
        if let Some(hook) = self.on_cancel.as_mut() {
            hook();
        }
        self.pending.remove(notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        self.calls.push(Call::List);
        if let Some(error) = &self.fail_list {
            return Err(error.clone());
        }
        Ok(self
            .pending
            .iter()
            .map(|(id, due)| PendingRequest {
                notification_id: id.clone(),
                fire_at: (!self.hide_due_instants).then_some(*due),
            })
            .collect())
    }
}

fn transient(code: &str) -> NotificationEffectError {
    NotificationEffectError::new(ErrorClass::Transient, code)
}

fn reconcile_at(
    db: &mut Database,
    center: &mut FakeCenter,
    now: &str,
) -> Result<ohand_core::reminders::reconcile::ReconcileReport> {
    Ok(reconcile(db, &clock_at(now), center)?)
}

fn reconcile_now(
    db: &mut Database,
    center: &mut FakeCenter,
) -> Result<ohand_core::reminders::reconcile::ReconcileReport> {
    reconcile_at(db, center, "2026-01-15T10:31:00Z")
}

fn completion(
    reminder_id: &str,
    operation_type: OperationType,
    notification_id: &str,
    outcome: EffectOutcome,
) -> EffectCompletion {
    EffectCompletion {
        reminder_id: reminder_id.to_string(),
        effect_identity: format!("{}:{}", operation_type.as_str(), notification_id),
        outcome,
    }
}

fn apply(db: &mut Database, completion: &EffectCompletion) -> Result<CompletionOutcome> {
    let clock = clock_at("2026-01-15T10:31:00Z");
    let tx = db.immediate_transaction()?;
    let outcome = apply_completion(&tx, &clock, completion)?;
    tx.commit()?;
    Ok(outcome)
}

// ---- Installation ----------------------------------------------------------------------

#[test]
fn a_desired_reminder_is_installed_once_and_acknowledged() -> Result<()> {
    let mut db = open_db(&temp_db_path("install"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let mut center = FakeCenter::default();

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.installed, vec![id.clone()]);
    assert!(report.converged());
    assert_eq!(center.pending.get(&id), Some(&instant(DUE)));
    assert_eq!(center.targets.get(&id), Some(&"item-1".to_string()));
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.schedule_state, ScheduleState::Scheduled);
    assert_eq!(after.state_version, record.state_version);
    assert_eq!(after.delivery_state, DeliveryState::Unknown);
    assert_eq!(
        operation_states(&mut db, "item-1")?,
        vec![(OperationType::Schedule, id, OperationState::Acknowledged)]
    );
    Ok(())
}

#[test]
fn a_second_pass_over_converged_state_makes_no_os_mutation() -> Result<()> {
    let mut db = open_db(&temp_db_path("idempotent"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    create_reminder(&mut db, "item-1", revision)?;
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    center.clear_calls();

    let report = reconcile_now(&mut db, &mut center)?;

    assert!(center.mutations().is_empty());
    assert!(report.installed.is_empty() && report.removed.is_empty());
    assert_eq!(report.already_installed.len(), 1);
    Ok(())
}

#[test]
fn a_crash_between_os_success_and_acknowledgment_does_not_duplicate_the_request() -> Result<()> {
    let mut db = open_db(&temp_db_path("crash_after_os"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    // The OS call succeeded and the process died before the acknowledgment was written.
    let mut center = FakeCenter::default();
    center.pending.insert(id.clone(), instant(DUE));
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::PendingSchedule
    );

    let report = reconcile_now(&mut db, &mut center)?;

    assert!(center.mutations().is_empty(), "no second schedule call");
    assert_eq!(report.already_installed, vec![id.clone()]);
    assert_eq!(center.pending_ids(), vec![id.clone()]);
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::Scheduled
    );
    assert_eq!(
        operation_states(&mut db, "item-1")?,
        vec![(OperationType::Schedule, id, OperationState::Acknowledged)]
    );
    Ok(())
}

#[test]
fn a_cancelled_caller_after_a_successful_install_is_confirmed_by_the_next_pass() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancelled_after_install"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let mut center = FakeCenter {
        install_then_report_cancelled: true,
        ..FakeCenter::default()
    };

    let first = reconcile_now(&mut db, &mut center)?;
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].class, ErrorClass::Cancelled);
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::PendingSchedule
    );

    center.install_then_report_cancelled = false;
    center.clear_calls();
    let second = reconcile_now(&mut db, &mut center)?;

    assert!(center.mutations().is_empty());
    assert_eq!(second.already_installed, vec![id]);
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::Scheduled
    );
    Ok(())
}

#[test]
fn a_pending_request_with_a_different_or_unreported_due_instant_is_replaced() -> Result<()> {
    let mut db = open_db(&temp_db_path("replace"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    center
        .pending
        .insert(id.clone(), instant("2026-01-16T10:00:00Z"));

    let report = reconcile_now(&mut db, &mut center)?;
    assert_eq!(report.installed, vec![id.clone()]);
    assert_eq!(center.pending.get(&id), Some(&instant(DUE)));

    let mut hidden = FakeCenter {
        hide_due_instants: true,
        ..FakeCenter::default()
    };
    hidden.pending.insert(id.clone(), instant(DUE));
    let report = reconcile_now(&mut db, &mut hidden)?;
    assert_eq!(report.installed, vec![id]);
    Ok(())
}

#[test]
fn relaunch_reconciles_the_same_durable_state_with_a_fresh_connection() -> Result<()> {
    let path = temp_db_path("relaunch");
    let id = {
        let mut db = open_db(&path)?;
        let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
        create_reminder(&mut db, "item-1", revision)?
            .notification_id()
            .unwrap()
    };
    let mut db = open_db(&path)?;
    let mut center = FakeCenter::default();

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.installed, vec![id]);
    Ok(())
}

// ---- Stale request removal -------------------------------------------------------------

#[test]
fn a_changed_time_removes_the_old_request_and_installs_the_new_generation() -> Result<()> {
    let mut db = open_db(&temp_db_path("changed"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let old_id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;

    let moved = move_reminder(&mut db, "item-1", revision, "2026-01-17T09:00:00Z")?;
    let new_id = moved.notification_id().unwrap();
    assert_ne!(old_id, new_id);
    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.removed, vec![old_id.clone()]);
    assert_eq!(report.installed, vec![new_id.clone()]);
    assert_eq!(center.pending_ids(), vec![new_id.clone()]);
    let states = operation_states(&mut db, "item-1")?;
    assert!(states.contains(&(
        OperationType::Schedule,
        old_id.clone(),
        OperationState::Acknowledged
    )));
    assert!(states.contains(&(OperationType::Cancel, old_id, OperationState::Acknowledged)));
    assert!(states.contains(&(
        OperationType::Schedule,
        new_id,
        OperationState::Acknowledged
    )));
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::Scheduled
    );
    Ok(())
}

#[test]
fn a_cancelled_reminder_loses_its_request_and_reads_as_not_scheduled() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancelled"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;

    cancel_user_reminder(&mut db, "item-1", revision)?;
    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.removed, vec![id.clone()]);
    assert!(center.pending.is_empty());
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.request_state, RequestState::Cancelled);
    assert_eq!(after.schedule_state, ScheduleState::NotScheduled);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id,
        OperationState::Acknowledged
    )));

    center.clear_calls();
    reconcile_now(&mut db, &mut center)?;
    assert!(center.mutations().is_empty());
    Ok(())
}

#[test]
fn a_completed_item_loses_its_request_even_when_the_reminder_was_never_cancelled() -> Result<()> {
    let mut db = open_db(&temp_db_path("completed"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;

    // Completion through the plain event path leaves the reminder live until reconciliation.
    complete_item(&mut db, "item-1", revision)?;
    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.removed, vec![id]);
    assert!(center.pending.is_empty());
    assert_eq!(
        reminder(&mut db, "item-1")?.request_state,
        RequestState::Cancelled
    );
    Ok(())
}

#[test]
fn a_deleted_item_loses_its_request_and_notification_cleanup_can_then_finish() -> Result<()> {
    let mut db = open_db(&temp_db_path("deleted"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;

    let intent = mark_deletion_intent(&mut db, "item-1", revision, capture_instant())?;
    let cleanup = intent
        .work
        .iter()
        .find(|work| work.work_type == DeletionWorkType::CancelNotifications)
        .unwrap()
        .deletion_work_id
        .clone();
    assert!(
        mark_deletion_work_completed(&mut db, &cleanup, capture_instant()).is_err(),
        "cleanup cannot finish while the cancel is unacknowledged"
    );

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.removed, vec![id]);
    assert!(center.pending.is_empty());
    mark_deletion_work_completed(&mut db, &cleanup, capture_instant())?;
    assert!(list_pending_deletion_work(&db, "item-1")?
        .iter()
        .all(|work| work.work_type != DeletionWorkType::CancelNotifications));
    Ok(())
}

#[test]
fn a_cancel_that_fails_stays_open_and_is_retried_by_the_next_pass() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancel_retry"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    cancel_user_reminder(&mut db, "item-1", revision)?;

    center.fail_cancel = Some(transient("cancel_not_confirmed"));
    let failed = reconcile_now(&mut db, &mut center)?;
    assert!(!failed.converged());
    assert!(failed.removed.is_empty());
    assert_eq!(failed.failures[0].operation_type, OperationType::Cancel);
    assert_eq!(center.pending_ids(), vec![id.clone()]);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id.clone(),
        OperationState::Failed
    )));

    center.fail_cancel = None;
    let retried = reconcile_now(&mut db, &mut center)?;
    assert_eq!(retried.removed, vec![id.clone()]);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id,
        OperationState::Acknowledged
    )));
    Ok(())
}

#[test]
fn an_open_cancel_whose_request_is_already_gone_is_acknowledged_without_an_os_call() -> Result<()> {
    let mut db = open_db(&temp_db_path("already_absent"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    cancel_user_reminder(&mut db, "item-1", revision)?;
    // The OS cancel happened, then the process died before the acknowledgment.
    center.pending.clear();
    center.clear_calls();

    let report = reconcile_now(&mut db, &mut center)?;

    assert!(center.mutations().is_empty());
    assert_eq!(report.already_absent, vec![id.clone()]);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id,
        OperationState::Acknowledged
    )));
    Ok(())
}

#[test]
fn requests_of_unknown_reminders_are_left_alone() -> Result<()> {
    let mut db = open_db(&temp_db_path("foreign"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    create_reminder(&mut db, "item-1", revision)?;
    let mut center = FakeCenter::default();
    center.pending.insert(
        "daily-prompt#7".to_string(),
        instant("2026-01-20T09:00:00Z"),
    );

    reconcile_now(&mut db, &mut center)?;

    assert!(center.pending.contains_key("daily-prompt#7"));
    assert!(!center
        .calls
        .contains(&Call::Cancel("daily-prompt#7".to_string())));
    Ok(())
}

#[test]
fn a_request_for_an_old_generation_that_survived_is_removed() -> Result<()> {
    let mut db = open_db(&temp_db_path("old_generation"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let old_id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    move_reminder(&mut db, "item-1", revision, "2026-01-17T09:00:00Z")?;
    // The old request is back in the OS even though its cancel is already acknowledged.
    reconcile_now(&mut db, &mut center)?;
    center.pending.insert(old_id.clone(), instant(DUE));

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.removed, vec![old_id]);
    assert_eq!(center.pending.len(), 1);
    Ok(())
}

// ---- Failures ---------------------------------------------------------------------------

#[test]
fn a_listing_failure_aborts_before_any_os_mutation() -> Result<()> {
    let mut db = open_db(&temp_db_path("list_failure"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    create_reminder(&mut db, "item-1", revision)?;
    let mut center = FakeCenter {
        fail_list: Some(transient("notification_center_failed")),
        ..FakeCenter::default()
    };

    let error = reconcile_now(&mut db, &mut center).unwrap_err();

    assert!(error.to_string().contains("listing pending"));
    assert!(center.mutations().is_empty());
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::PendingSchedule
    );
    Ok(())
}

#[test]
fn a_failed_install_is_surfaced_then_retried_to_success() -> Result<()> {
    let mut db = open_db(&temp_db_path("install_failure"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter {
        fail_schedule: Some(NotificationEffectError::new(
            ErrorClass::Permanent,
            "invalid_payload",
        )),
        ..FakeCenter::default()
    };

    let failed = reconcile_now(&mut db, &mut center)?;
    assert_eq!(failed.failures.len(), 1);
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.schedule_state, ScheduleState::ScheduleFailed);
    assert_eq!(after.request_state, RequestState::Resolved);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Schedule,
        id.clone(),
        OperationState::Failed
    )));

    center.fail_schedule = None;
    let retried = reconcile_now(&mut db, &mut center)?;
    assert_eq!(retried.installed, vec![id.clone()]);
    assert_eq!(
        reminder(&mut db, "item-1")?.schedule_state,
        ScheduleState::Scheduled
    );
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Schedule,
        id,
        OperationState::Acknowledged
    )));
    Ok(())
}

// ---- Elapsed due times ------------------------------------------------------------------

#[test]
fn a_never_installed_reminder_past_its_due_time_becomes_unschedulable_without_an_os_call(
) -> Result<()> {
    let mut db = open_db(&temp_db_path("expired"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();

    let report = reconcile_at(&mut db, &mut center, "2026-01-16T09:30:00Z")?;

    assert_eq!(report.expired, vec![id.clone()]);
    assert!(center.mutations().is_empty());
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(
        after.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(after.resolved_instant, Some(instant(DUE)));
    assert_eq!(after.delivery_state, DeliveryState::Unknown);
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Schedule,
        id,
        OperationState::Superseded
    )));
    Ok(())
}

/// Delegates to the fake center, then dies right after a successful schedule call, before the
/// caller can acknowledge it.
struct CrashAfterInstall<'a> {
    center: &'a mut FakeCenter,
}

impl NotificationEffects for CrashAfterInstall<'_> {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.center.schedule(request)?;
        panic!("simulated crash after the OS accepted the request");
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.center.cancel(notification_id)
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        self.center.list_pending()
    }
}

fn assert_resolved_and_unconfirmed(db: &mut Database, id: &str, version: i64) -> Result<()> {
    let after = reminder(db, "item-1")?;
    assert_eq!(after.request_state, RequestState::Resolved);
    assert_eq!(after.state_version, version);
    assert_ne!(after.schedule_state, ScheduleState::Scheduled);
    assert!(!operation_states(db, "item-1")?.contains(&(
        OperationType::Schedule,
        id.to_string(),
        OperationState::Superseded
    )));
    Ok(())
}

#[test]
fn a_crash_after_os_success_then_firing_then_relaunch_after_due_is_not_unschedulable() -> Result<()>
{
    let path = temp_db_path("crash_fired");
    let mut db = open_db(&path)?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let mut center = FakeCenter::default();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut crashing = CrashAfterInstall {
            center: &mut center,
        };
        let _ = reconcile(&mut db, &clock_at("2026-01-15T10:31:00Z"), &mut crashing);
    }));
    assert!(crashed.is_err());
    drop(db);
    assert!(center.pending.contains_key(&id));
    // The request fired while the app was not running.
    center.pending.clear();
    center.clear_calls();

    let mut relaunched = open_db(&path)?;
    let report = reconcile_at(&mut relaunched, &mut center, "2026-01-16T09:30:00Z")?;

    assert!(report.expired.is_empty());
    assert_eq!(report.unconfirmed, vec![id.clone()]);
    assert!(center.mutations().is_empty());
    assert_resolved_and_unconfirmed(&mut relaunched, &id, record.state_version)?;
    Ok(())
}

#[test]
fn a_cancelled_caller_install_that_then_fires_is_not_unschedulable_after_due() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancelled_fired"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let mut center = FakeCenter {
        install_then_report_cancelled: true,
        ..FakeCenter::default()
    };
    reconcile_now(&mut db, &mut center)?;
    center.pending.clear();
    center.clear_calls();

    let report = reconcile_at(&mut db, &mut center, "2026-01-16T09:30:00Z")?;

    assert!(report.expired.is_empty());
    assert_eq!(report.unconfirmed, vec![id.clone()]);
    assert!(center.mutations().is_empty());
    assert_resolved_and_unconfirmed(&mut db, &id, record.state_version)?;
    Ok(())
}

#[test]
fn an_ambiguous_schedule_failure_leaves_the_attempt_recorded() -> Result<()> {
    let mut db = open_db(&temp_db_path("ambiguous_failure"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let mut center = FakeCenter {
        fail_schedule: Some(transient("timeout")),
        ..FakeCenter::default()
    };
    reconcile_now(&mut db, &mut center)?;

    let report = reconcile_at(&mut db, &mut center, "2026-01-16T09:30:00Z")?;

    assert!(report.expired.is_empty());
    assert_eq!(report.unconfirmed, vec![id.clone()]);
    assert_resolved_and_unconfirmed(&mut db, &id, record.state_version)?;
    Ok(())
}

#[test]
fn the_attempt_is_durable_before_the_os_call_returns() -> Result<()> {
    let path = temp_db_path("attempt_durable");
    let mut db = open_db(&path)?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let observer_path = path.clone();
    let observed_in_hook = observed.clone();
    let mut center = FakeCenter {
        on_schedule: Some(Box::new(move || {
            let connection = Connection::open(&observer_path).unwrap();
            let state: String = connection
                .query_row(
                    "SELECT operation_state FROM reminder_operations WHERE operation_type = 'schedule'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            *observed_in_hook.borrow_mut() = Some(state);
        })),
        ..FakeCenter::default()
    };

    reconcile_now(&mut db, &mut center)?;

    assert_eq!(observed.borrow().as_deref(), Some("failed"));
    assert_eq!(
        operation_states(&mut db, "item-1")?,
        vec![(OperationType::Schedule, id, OperationState::Acknowledged)]
    );
    Ok(())
}

#[test]
fn an_installed_reminder_whose_request_has_fired_is_left_alone() -> Result<()> {
    let mut db = open_db(&temp_db_path("fired"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    create_reminder(&mut db, "item-1", revision)?;
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    center.pending.clear();
    center.clear_calls();

    let report = reconcile_at(&mut db, &mut center, "2026-01-16T09:30:00Z")?;

    assert!(center.mutations().is_empty());
    assert!(report.expired.is_empty());
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.request_state, RequestState::Resolved);
    assert_eq!(after.schedule_state, ScheduleState::Scheduled);
    assert_eq!(after.delivery_state, DeliveryState::Unknown);
    Ok(())
}

// ---- Late, duplicate and out-of-order completions ---------------------------------------

#[test]
fn a_late_install_cannot_resurrect_a_cancelled_reminder() -> Result<()> {
    let mut db = open_db(&temp_db_path("late_install"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    cancel_user_reminder(&mut db, "item-1", revision)?;
    let cancelled = reminder(&mut db, "item-1")?;

    let outcome = apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Succeeded,
        ),
    )?;

    assert_eq!(
        outcome,
        CompletionOutcome::Stale {
            cancel_required: true
        }
    );
    assert_eq!(reminder(&mut db, "item-1")?, cancelled);
    assert_eq!(cancelled.request_state, RequestState::Cancelled);
    assert_ne!(cancelled.schedule_state, ScheduleState::Scheduled);

    let mut center = FakeCenter::default();
    center.pending.insert(id.clone(), instant(DUE));
    let report = reconcile_now(&mut db, &mut center)?;
    assert_eq!(report.removed, vec![id]);
    assert!(center.pending.is_empty());
    Ok(())
}

#[test]
fn duplicate_completions_change_nothing_after_the_first() -> Result<()> {
    let mut db = open_db(&temp_db_path("duplicate"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    let success = completion(
        &record.reminder_id,
        OperationType::Schedule,
        &id,
        EffectOutcome::Succeeded,
    );

    assert_eq!(apply(&mut db, &success)?, CompletionOutcome::Applied);
    let after_first = reminder(&mut db, "item-1")?;
    assert_eq!(
        apply(&mut db, &success)?,
        CompletionOutcome::AlreadyRecorded
    );
    assert_eq!(
        apply(&mut db, &success)?,
        CompletionOutcome::AlreadyRecorded
    );
    assert_eq!(reminder(&mut db, "item-1")?, after_first);
    assert_eq!(after_first.schedule_state, ScheduleState::Scheduled);
    Ok(())
}

#[test]
fn a_failure_arriving_after_an_acknowledged_install_is_ignored() -> Result<()> {
    let mut db = open_db(&temp_db_path("failure_after_ack"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Succeeded,
        ),
    )?;
    let acknowledged = reminder(&mut db, "item-1")?;

    let outcome = apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Failed(ErrorClass::Transient),
        ),
    )?;

    assert_eq!(outcome, CompletionOutcome::AlreadyRecorded);
    assert_eq!(reminder(&mut db, "item-1")?, acknowledged);
    Ok(())
}

#[test]
fn a_cancel_completion_before_the_install_completion_still_leaves_the_reminder_cancelled(
) -> Result<()> {
    let mut db = open_db(&temp_db_path("out_of_order"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    cancel_user_reminder(&mut db, "item-1", revision)?;

    let cancel = completion(
        &record.reminder_id,
        OperationType::Cancel,
        &id,
        EffectOutcome::Succeeded,
    );
    assert_eq!(apply(&mut db, &cancel)?, CompletionOutcome::Applied);
    let late_install = completion(
        &record.reminder_id,
        OperationType::Schedule,
        &id,
        EffectOutcome::Succeeded,
    );
    assert_eq!(
        apply(&mut db, &late_install)?,
        CompletionOutcome::Stale {
            cancel_required: true
        }
    );
    // A delayed duplicate of the earlier cancel result must not hide the late install either.
    assert_eq!(apply(&mut db, &cancel)?, CompletionOutcome::AlreadyRecorded);

    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.request_state, RequestState::Cancelled);
    assert_eq!(after.schedule_state, ScheduleState::NotScheduled);

    let mut center = FakeCenter::default();
    center.pending.insert(id.clone(), instant(DUE));
    let report = reconcile_now(&mut db, &mut center)?;
    assert_eq!(report.removed, vec![id]);
    assert!(center.pending.is_empty());
    Ok(())
}

#[test]
fn a_late_install_of_an_old_generation_does_not_mark_the_new_generation_scheduled() -> Result<()> {
    let mut db = open_db(&temp_db_path("old_generation_completion"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let first = create_reminder(&mut db, "item-1", revision)?;
    let old_id = first.notification_id().unwrap();
    let moved = move_reminder(&mut db, "item-1", revision, "2026-01-17T09:00:00Z")?;
    assert_eq!(moved.schedule_state, ScheduleState::PendingSchedule);

    let outcome = apply(
        &mut db,
        &completion(
            &first.reminder_id,
            OperationType::Schedule,
            &old_id,
            EffectOutcome::Succeeded,
        ),
    )?;

    assert_eq!(
        outcome,
        CompletionOutcome::Stale {
            cancel_required: true
        }
    );
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.schedule_state, ScheduleState::PendingSchedule);
    assert_eq!(after.notification_id(), moved.notification_id());
    Ok(())
}

#[test]
fn a_late_failure_of_a_superseded_install_needs_no_cancel() -> Result<()> {
    let mut db = open_db(&temp_db_path("late_failure"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    cancel_user_reminder(&mut db, "item-1", revision)?;

    let outcome = apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Failed(ErrorClass::Transient),
        ),
    )?;

    assert_eq!(
        outcome,
        CompletionOutcome::Stale {
            cancel_required: false
        }
    );
    Ok(())
}

#[test]
fn a_cancelled_effect_proves_nothing_and_changes_no_state() -> Result<()> {
    let mut db = open_db(&temp_db_path("cancelled_effect"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();

    let outcome = apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Failed(ErrorClass::Cancelled),
        ),
    )?;

    assert_eq!(outcome, CompletionOutcome::NoChange);
    assert_eq!(reminder(&mut db, "item-1")?, record);
    Ok(())
}

#[test]
fn a_late_install_for_an_item_that_ended_elsewhere_is_stale() -> Result<()> {
    let mut db = open_db(&temp_db_path("ended_elsewhere"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let id = record.notification_id().unwrap();
    complete_item(&mut db, "item-1", revision)?;

    let outcome = apply(
        &mut db,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &id,
            EffectOutcome::Succeeded,
        ),
    )?;

    assert_eq!(
        outcome,
        CompletionOutcome::Stale {
            cancel_required: true
        }
    );
    let after = reminder(&mut db, "item-1")?;
    assert_eq!(after.request_state, RequestState::Cancelled);
    assert_ne!(after.schedule_state, ScheduleState::Scheduled);
    Ok(())
}

#[test]
fn a_completion_for_an_unknown_effect_is_rejected() -> Result<()> {
    let mut db = open_db(&temp_db_path("unknown_effect"))?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let record = create_reminder(&mut db, "item-1", revision)?;
    let clock = clock_at("2026-01-15T10:31:00Z");

    let tx = db.immediate_transaction()?;
    let unknown_effect = apply_completion(
        &tx,
        &clock,
        &completion(
            &record.reminder_id,
            OperationType::Schedule,
            &format!("{}#99", record.reminder_id),
            EffectOutcome::Succeeded,
        ),
    );
    assert!(matches!(
        unknown_effect,
        Err(ReconcileError::UnknownEffect { .. })
    ));
    let unknown_reminder = apply_completion(
        &tx,
        &clock,
        &completion(
            "missing",
            OperationType::Schedule,
            "missing#1",
            EffectOutcome::Succeeded,
        ),
    );
    assert!(matches!(
        unknown_reminder,
        Err(ReconcileError::UnknownReminder(_))
    ));
    Ok(())
}

#[test]
fn an_install_that_completes_after_the_reminder_was_cancelled_is_rolled_back_in_the_same_pass(
) -> Result<()> {
    let path = temp_db_path("rolled_back");
    let mut db = open_db(&path)?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    let other_path = path.clone();
    // While the OS call is in flight, the user cancels through another connection.
    center.on_schedule = Some(Box::new(move || {
        let mut other = open_db(&other_path).unwrap();
        cancel_user_reminder(&mut other, "item-1", 1).unwrap();
    }));

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.rolled_back, vec![id.clone()]);
    assert!(report.installed.is_empty());
    assert!(center.pending.is_empty());
    assert_eq!(
        reminder(&mut db, "item-1")?.request_state,
        RequestState::Cancelled
    );
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id,
        OperationState::Acknowledged
    )));
    Ok(())
}

#[test]
fn a_cancel_committed_after_the_plan_snapshot_removes_an_already_pending_request() -> Result<()> {
    let path = temp_db_path("snapshot_race");
    let mut db = open_db(&path)?;
    let revision_one = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let revision_two = insert_item(&mut db, "item-2", ROOFER_TEXT)?;
    let id_one = create_reminder(&mut db, "item-1", revision_one)?
        .notification_id()
        .unwrap();
    let id_two = create_reminder(&mut db, "item-2", revision_two)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    assert_eq!(center.pending_ids().len(), 2);

    // The user cancels item-2, so the next pass has a stale request to remove. While that
    // removal is in flight, after the plan snapshot was taken, item-1 is cancelled elsewhere.
    cancel_user_reminder(&mut db, "item-2", revision_two)?;
    let other_path = path.clone();
    let mut fired = false;
    center.on_cancel = Some(Box::new(move || {
        if !fired {
            fired = true;
            let mut other = open_db(&other_path).unwrap();
            cancel_user_reminder(&mut other, "item-1", 1).unwrap();
        }
    }));

    let report = reconcile_now(&mut db, &mut center)?;

    assert!(report.rolled_back.contains(&id_one));
    assert!(!report.already_installed.contains(&id_one));
    assert!(report.removed.contains(&id_two));
    assert!(center.pending.is_empty());
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id_one,
        OperationState::Acknowledged
    )));
    Ok(())
}

#[test]
fn a_cancel_during_the_reinstall_of_an_acknowledged_request_rolls_the_reinstall_back() -> Result<()>
{
    let path = temp_db_path("acknowledged_reinstall_race");
    let mut db = open_db(&path)?;
    let revision = insert_item(&mut db, "item-1", ROOFER_TEXT)?;
    let id = create_reminder(&mut db, "item-1", revision)?
        .notification_id()
        .unwrap();
    let mut center = FakeCenter::default();
    reconcile_now(&mut db, &mut center)?;
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Schedule,
        id.clone(),
        OperationState::Acknowledged
    )));

    // The acknowledged request disappears from the OS, so the next pass re-issues it. While
    // that reinstall is in flight, the user cancels through another connection.
    center.pending.clear();
    center.clear_calls();
    let other_path = path.clone();
    center.on_schedule = Some(Box::new(move || {
        let mut other = open_db(&other_path).unwrap();
        cancel_user_reminder(&mut other, "item-1", 1).unwrap();
    }));

    let report = reconcile_now(&mut db, &mut center)?;

    assert_eq!(report.rolled_back, vec![id.clone()]);
    assert!(report.installed.is_empty());
    assert!(center.pending.is_empty());
    assert_eq!(
        reminder(&mut db, "item-1")?.request_state,
        RequestState::Cancelled
    );
    assert!(operation_states(&mut db, "item-1")?.contains(&(
        OperationType::Cancel,
        id,
        OperationState::Acknowledged
    )));
    Ok(())
}
