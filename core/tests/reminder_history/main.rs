use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;

use ohand_core::lifecycle::delete_intent::mark_deletion_intent;
use ohand_core::reminders::history::{
    acknowledge_reminder, record_delivery_evidence, reminder_history, DeliveryEvidence,
    DeliveryEvidenceKind, DueStatus, HistoryEntryKind, HistoryError, NativeEffectStatus,
    RecordOutcome, ReminderHistory, UserAcknowledgment,
};
use ohand_core::reminders::reconcile::{
    reconcile, NotificationEffectError, NotificationEffects, PendingRequest, ScheduleRequest,
};
use ohand_core::reminders::state::{
    apply_derived_request, apply_user_time_correction, cancel_reminder, end_item_with_event,
    get_reminder, AcknowledgmentState, DeliveryState, DerivedReminderRequest, ReminderCancellation,
    ReminderRecord, RequestState, UserTimeCorrection,
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

fn clock_at(text: &str) -> FixedClock {
    FixedClock {
        instant: instant(text),
    }
}

const CAPTURE_TIME: &str = "2026-01-15T10:30:00Z";
const DUE_TIME: &str = "2026-01-16T09:00:00Z";
const LATER_DUE_TIME: &str = "2026-01-18T09:00:00Z";

/// The OS pending set; installs whatever it is asked to.
#[derive(Default)]
struct FakeCenter {
    pending: BTreeMap<String, DateTime<Utc>>,
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.pending
            .insert(request.notification_id.clone(), request.fire_at);
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.pending.remove(notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        Ok(self
            .pending
            .iter()
            .map(|(id, due)| PendingRequest {
                notification_id: id.clone(),
                fire_at: Some(*due),
            })
            .collect())
    }
}

fn open_db(label: &str) -> Result<Database> {
    let path = format!(
        "{}/test_reminder_history_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    );
    let clock: Arc<dyn Clock> = Arc::new(clock_at(CAPTURE_TIME));
    Database::open(&path, clock)
}

/// Insert a capture and an action item through the real correction path (revision 1).
fn insert_action(db: &mut Database, item_id: &str, text: &str) -> Result<i32> {
    let tx = db.transaction()?;
    let capture_id = format!("cap-{item_id}");
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, NULL, ?, 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, ?, NULL)",
        rusqlite::params![&capture_id, text, CAPTURE_TIME, CAPTURE_TIME],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
        rusqlite::params![item_id, &capture_id, CAPTURE_TIME, CAPTURE_TIME],
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
        CAPTURE_TIME.to_string(),
    )?;
    save_event(db, &event, 0)?;
    Ok(1)
}

fn reminder_version(db: &Database, item_id: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COALESCE(MAX(state_version), 0) FROM reminders WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// Set (or move) the reminder time as the user would, at `now`.
fn set_time(
    db: &mut Database,
    item_id: &str,
    revision: i32,
    due: &str,
    now: &str,
) -> Result<ReminderRecord> {
    let correction = UserTimeCorrection {
        command_id: format!("cmd-{}", uuid::Uuid::new_v4()),
        item_id: item_id.to_string(),
        expected_revision: revision,
        expected_reminder_version: reminder_version(db, item_id),
        instant: instant(due),
        timezone_id: "UTC".to_string(),
    };
    let tx = db.immediate_transaction()?;
    let record = apply_user_time_correction(&tx, &clock_at(now), &correction)?;
    tx.commit()?;
    Ok(record)
}

fn settle(db: &mut Database, center: &mut FakeCenter, now: &str) -> Result<()> {
    reconcile(db, &clock_at(now), center)?;
    Ok(())
}

fn history(db: &mut Database, item_id: &str, now: &str) -> Result<ReminderHistory> {
    let tx = db.immediate_transaction()?;
    let history = reminder_history(&tx, &clock_at(now), item_id)?;
    tx.commit()?;
    Ok(history.expect("item has a reminder"))
}

fn stored(db: &mut Database, item_id: &str) -> Result<ReminderRecord> {
    let tx = db.immediate_transaction()?;
    let record = get_reminder(&tx, item_id)?;
    tx.commit()?;
    Ok(record.expect("item has a reminder"))
}

fn evidence(
    record: &ReminderRecord,
    generation: i64,
    kind: DeliveryEvidenceKind,
    observed_at: &str,
) -> DeliveryEvidence {
    DeliveryEvidence {
        notification_id: format!("{}#{generation}", record.reminder_id),
        kind,
        observed_at: instant(observed_at),
    }
}

fn record_evidence(
    db: &mut Database,
    evidence: &DeliveryEvidence,
    now: &str,
) -> Result<RecordOutcome, HistoryError> {
    let tx = db.immediate_transaction().unwrap();
    let outcome = record_delivery_evidence(&tx, &clock_at(now), evidence)?;
    tx.commit().map_err(HistoryError::Database)?;
    Ok(outcome)
}

fn acknowledge(
    db: &mut Database,
    item_id: &str,
    notification_id: &str,
    now: &str,
) -> Result<RecordOutcome, HistoryError> {
    let acknowledgment = UserAcknowledgment {
        item_id: item_id.to_string(),
        notification_id: notification_id.to_string(),
    };
    let tx = db.immediate_transaction().unwrap();
    let outcome = acknowledge_reminder(&tx, &clock_at(now), &acknowledgment)?;
    tx.commit().map_err(HistoryError::Database)?;
    Ok(outcome)
}

fn kinds(history: &ReminderHistory) -> Vec<HistoryEntryKind> {
    history
        .entries
        .iter()
        .map(|entry| entry.kind.clone())
        .collect()
}

fn lifecycle_state(db: &Database, item_id: &str) -> String {
    db.conn()
        .query_row(
            "SELECT lifecycle_state FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn history_shows_requested_time_installation_and_cancellation() -> Result<()> {
    let mut db = open_db("lifecycle")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();

    set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    let requested = history(&mut db, "item-1", "2026-01-15T11:01:00Z")?;
    assert_eq!(
        kinds(&requested),
        [HistoryEntryKind::TimeRequested {
            generation: 1,
            due_at: instant(DUE_TIME),
            installation: NativeEffectStatus::NotAttempted,
        }],
        "a request core has not yet sent to the OS is not reported as installed"
    );
    assert_eq!(
        requested.entries[0].occurred_at,
        instant("2026-01-15T11:00:00Z")
    );

    settle(&mut db, &mut center, "2026-01-15T11:02:00Z")?;
    let installed = history(&mut db, "item-1", "2026-01-15T11:03:00Z")?;
    assert_eq!(
        kinds(&installed),
        [HistoryEntryKind::TimeRequested {
            generation: 1,
            due_at: instant(DUE_TIME),
            installation: NativeEffectStatus::Confirmed,
        }]
    );
    assert_eq!(installed.summary.due, DueStatus::Upcoming);

    let cancellation = ReminderCancellation {
        command_id: "cmd-cancel".to_string(),
        item_id: "item-1".to_string(),
        expected_revision: revision,
        expected_reminder_version: reminder_version(&db, "item-1"),
    };
    let tx = db.immediate_transaction()?;
    cancel_reminder(&tx, &clock_at("2026-01-15T12:00:00Z"), &cancellation)?;
    tx.commit()?;
    let cancelling = history(&mut db, "item-1", "2026-01-15T12:01:00Z")?;
    assert_eq!(
        kinds(&cancelling)[1],
        HistoryEntryKind::Cancelled {
            generation: Some(1),
            native_cancel: Some(NativeEffectStatus::NotAttempted),
        }
    );

    settle(&mut db, &mut center, "2026-01-15T12:02:00Z")?;
    let cancelled = history(&mut db, "item-1", "2026-01-15T12:03:00Z")?;
    assert_eq!(
        kinds(&cancelled)[1],
        HistoryEntryKind::Cancelled {
            generation: Some(1),
            native_cancel: Some(NativeEffectStatus::Confirmed),
        }
    );
    assert_eq!(cancelled.summary.request_state, RequestState::Cancelled);
    assert_eq!(cancelled.summary.due, DueStatus::NotApplicable);
    assert!(center.pending.is_empty());
    Ok(())
}

#[test]
fn elapsed_due_time_without_os_evidence_stays_unknown_and_unacknowledged() -> Result<()> {
    let mut db = open_db("elapsed")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;

    let before = history(&mut db, "item-1", "2026-01-15T12:00:00Z")?;
    assert_eq!(before.summary.due, DueStatus::Upcoming);

    // Three weeks after the due time, and the app was not opened in between.
    let after = history(&mut db, "item-1", "2026-02-06T09:00:00Z")?;
    assert_eq!(after.summary.due, DueStatus::Elapsed);
    assert_eq!(after.summary.delivery, DeliveryState::Unknown);
    assert_eq!(
        after.summary.acknowledgment,
        AcknowledgmentState::NotAcknowledged
    );
    assert!(after.summary.elapsed_without_delivery_evidence());
    assert_eq!(
        after.entries, before.entries,
        "the clock adds no fired, missed or seen entry"
    );

    let record = stored(&mut db, "item-1")?;
    assert_eq!(record.delivery_state, DeliveryState::Unknown);
    assert_eq!(
        record.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );
    assert_eq!(
        lifecycle_state(&db, "item-1"),
        "active",
        "an elapsed reminder never completes its item"
    );
    Ok(())
}

#[test]
fn elapsed_reminder_that_was_never_installed_is_not_reported_as_fired() -> Result<()> {
    let mut db = open_db("never_installed")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    // Reconciliation first runs after the due time: the request expires uninstalled.
    let mut center = FakeCenter::default();
    settle(&mut db, &mut center, "2026-01-17T09:00:00Z")?;

    let view = history(&mut db, "item-1", "2026-01-17T09:01:00Z")?;
    assert_eq!(
        view.summary.request_state,
        RequestState::Unschedulable(ohand_core::reminders::state::UnschedulableReason::TimeInPast)
    );
    assert_eq!(view.summary.due, DueStatus::Elapsed);
    assert!(view.summary.elapsed_without_delivery_evidence());
    assert!(center.pending.is_empty());
    assert!(!kinds(&view).iter().any(|kind| matches!(
        kind,
        HistoryEntryKind::Delivered { .. }
            | HistoryEntryKind::Opened { .. }
            | HistoryEntryKind::Acknowledged { .. }
    )));
    Ok(())
}

#[test]
fn os_evidence_is_recorded_once_and_only_moves_delivery_forward() -> Result<()> {
    let mut db = open_db("evidence")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    let record = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;
    let version = reminder_version(&db, "item-1");

    let delivered = evidence(
        &record,
        1,
        DeliveryEvidenceKind::Delivered,
        "2026-01-16T09:00:05Z",
    );
    assert_eq!(
        record_evidence(&mut db, &delivered, "2026-01-16T09:30:00Z")?,
        RecordOutcome::Recorded {
            current_generation: true
        }
    );
    assert_eq!(
        stored(&mut db, "item-1")?.delivery_state,
        DeliveryState::Delivered
    );

    // A duplicate callback, even one carrying another timestamp, changes nothing.
    let duplicate = DeliveryEvidence {
        observed_at: instant("2026-01-16T09:10:00Z"),
        ..delivered.clone()
    };
    assert_eq!(
        record_evidence(&mut db, &duplicate, "2026-01-16T09:31:00Z")?,
        RecordOutcome::AlreadyRecorded
    );

    let opened = evidence(
        &record,
        1,
        DeliveryEvidenceKind::Opened,
        "2026-01-16T09:20:00Z",
    );
    record_evidence(&mut db, &opened, "2026-01-16T09:32:00Z")?;
    assert_eq!(
        stored(&mut db, "item-1")?.delivery_state,
        DeliveryState::Opened
    );

    // `opened` arriving before a late `delivered` is not undone by it.
    let mut other = open_db("evidence_order")?;
    let other_revision = insert_action(&mut other, "item-2", "call the plumber")?;
    let other_record = set_time(
        &mut other,
        "item-2",
        other_revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut other, &mut center, "2026-01-15T11:01:00Z")?;
    record_evidence(
        &mut other,
        &evidence(
            &other_record,
            1,
            DeliveryEvidenceKind::Opened,
            "2026-01-16T09:20:00Z",
        ),
        "2026-01-16T09:32:00Z",
    )?;
    record_evidence(
        &mut other,
        &evidence(
            &other_record,
            1,
            DeliveryEvidenceKind::Delivered,
            "2026-01-16T09:00:05Z",
        ),
        "2026-01-16T09:33:00Z",
    )?;
    assert_eq!(
        stored(&mut other, "item-2")?.delivery_state,
        DeliveryState::Opened
    );

    let view = history(&mut db, "item-1", "2026-01-16T10:00:00Z")?;
    assert_eq!(
        kinds(&view)[1..],
        [
            HistoryEntryKind::Delivered { generation: 1 },
            HistoryEntryKind::Opened { generation: 1 }
        ]
    );
    assert_eq!(view.entries[1].occurred_at, instant("2026-01-16T09:00:05Z"));
    assert_eq!(
        view.summary.acknowledgment,
        AcknowledgmentState::NotAcknowledged
    );
    assert_eq!(
        reminder_version(&db, "item-1"),
        version,
        "native evidence does not make a pending user edit stale"
    );
    Ok(())
}

#[test]
fn acknowledgment_is_explicit_and_independent_of_delivery_and_completion() -> Result<()> {
    let mut db = open_db("ack")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    let record = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;
    let notification_id = format!("{}#1", record.reminder_id);

    // Delivery and opening are evidence, not acknowledgment.
    record_evidence(
        &mut db,
        &evidence(
            &record,
            1,
            DeliveryEvidenceKind::Opened,
            "2026-01-16T09:01:00Z",
        ),
        "2026-01-16T09:02:00Z",
    )?;
    let opened = stored(&mut db, "item-1")?;
    assert_eq!(opened.delivery_state, DeliveryState::Opened);
    assert_eq!(
        opened.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );

    let version = reminder_version(&db, "item-1");
    assert_eq!(
        acknowledge(&mut db, "item-1", &notification_id, "2026-01-16T09:05:00Z")?,
        RecordOutcome::Recorded {
            current_generation: true
        }
    );
    let acknowledged = stored(&mut db, "item-1")?;
    assert_eq!(
        acknowledged.acknowledgment_state,
        AcknowledgmentState::Acknowledged
    );
    assert_eq!(acknowledged.delivery_state, DeliveryState::Opened);
    assert_eq!(acknowledged.request_state, RequestState::Resolved);
    assert_eq!(
        lifecycle_state(&db, "item-1"),
        "active",
        "acknowledge is not complete"
    );
    assert_eq!(reminder_version(&db, "item-1"), version);

    assert_eq!(
        acknowledge(&mut db, "item-1", &notification_id, "2026-01-17T09:05:00Z")?,
        RecordOutcome::AlreadyRecorded
    );
    let view = history(&mut db, "item-1", "2026-01-17T10:00:00Z")?;
    assert_eq!(
        view.entries.last().unwrap().kind,
        HistoryEntryKind::Acknowledged { generation: 1 }
    );
    assert_eq!(
        view.entries.last().unwrap().occurred_at,
        instant("2026-01-16T09:05:00Z"),
        "the replay did not restamp the acknowledgment"
    );
    Ok(())
}

#[test]
fn acknowledging_without_delivery_evidence_does_not_invent_delivery() -> Result<()> {
    let mut db = open_db("ack_no_delivery")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    let record = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;

    acknowledge(
        &mut db,
        "item-1",
        &format!("{}#1", record.reminder_id),
        "2026-01-20T08:00:00Z",
    )?;
    let view = history(&mut db, "item-1", "2026-01-20T08:01:00Z")?;
    assert_eq!(view.summary.delivery, DeliveryState::Unknown);
    assert_eq!(
        view.summary.acknowledgment,
        AcknowledgmentState::Acknowledged
    );
    assert!(!kinds(&view).iter().any(|kind| matches!(
        kind,
        HistoryEntryKind::Delivered { .. } | HistoryEntryKind::Opened { .. }
    )));
    Ok(())
}

#[test]
fn repeat_intent_is_its_own_entry_and_cannot_be_acknowledged() -> Result<()> {
    let mut db = open_db("repeat")?;
    let revision = insert_action(&mut db, "item-1", "water the plants every day")?;
    let tx = db.immediate_transaction()?;
    let record = apply_derived_request(
        &tx,
        &clock_at("2026-01-15T11:00:00Z"),
        &DerivedReminderRequest {
            item_id: "item-1".to_string(),
            source_revision: revision,
            phrase: "every day".to_string(),
        },
    )?;
    tx.commit()?;
    assert_eq!(record.request_state, RequestState::UnsupportedRecurrence);

    let view = history(&mut db, "item-1", "2026-01-15T11:01:00Z")?;
    assert!(
        matches!(
            kinds(&view).as_slice(),
            [HistoryEntryKind::RepeatRequested { reason: Some(_) }]
        ),
        "{:?}",
        view.entries
    );
    assert_eq!(view.summary.due, DueStatus::NotApplicable);
    assert_eq!(
        view.summary.acknowledgment,
        AcknowledgmentState::NotAcknowledged
    );

    // Nothing was scheduled, so there is no notification to acknowledge or observe.
    let notification_id = format!("{}#1", record.reminder_id);
    assert!(matches!(
        acknowledge(&mut db, "item-1", &notification_id, "2026-01-15T12:00:00Z"),
        Err(HistoryError::UnknownNotification(_))
    ));
    assert!(matches!(
        record_evidence(
            &mut db,
            &evidence(
                &record,
                1,
                DeliveryEvidenceKind::Delivered,
                "2026-01-15T12:00:00Z"
            ),
            "2026-01-15T12:00:00Z"
        ),
        Err(HistoryError::UnknownNotification(_))
    ));
    Ok(())
}

#[test]
fn a_new_generation_starts_unknown_and_old_evidence_stays_in_history() -> Result<()> {
    let mut db = open_db("generations")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    let first = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;
    record_evidence(
        &mut db,
        &evidence(
            &first,
            1,
            DeliveryEvidenceKind::Delivered,
            "2026-01-16T09:00:05Z",
        ),
        "2026-01-16T09:01:00Z",
    )?;
    acknowledge(
        &mut db,
        "item-1",
        &format!("{}#1", first.reminder_id),
        "2026-01-16T09:02:00Z",
    )?;

    let second = set_time(
        &mut db,
        "item-1",
        revision,
        LATER_DUE_TIME,
        "2026-01-16T10:00:00Z",
    )?;
    assert_eq!(second.schedule_generation, 2);
    assert_eq!(second.delivery_state, DeliveryState::Unknown);
    assert_eq!(
        second.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );
    settle(&mut db, &mut center, "2026-01-16T10:01:00Z")?;

    // A late callback for the old notification is stored but cannot touch the new one.
    let late = evidence(
        &first,
        1,
        DeliveryEvidenceKind::Opened,
        "2026-01-16T09:30:00Z",
    );
    assert_eq!(
        record_evidence(&mut db, &late, "2026-01-16T11:00:00Z")?,
        RecordOutcome::Recorded {
            current_generation: false
        }
    );
    let after_late = stored(&mut db, "item-1")?;
    assert_eq!(after_late.delivery_state, DeliveryState::Unknown);
    assert_eq!(
        after_late.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );

    let view = history(&mut db, "item-1", "2026-01-16T11:01:00Z")?;
    let entry_kinds = kinds(&view);
    for expected in [
        HistoryEntryKind::Delivered { generation: 1 },
        HistoryEntryKind::Opened { generation: 1 },
        HistoryEntryKind::Acknowledged { generation: 1 },
    ] {
        assert!(
            entry_kinds.contains(&expected),
            "{expected:?} in {entry_kinds:?}"
        );
    }
    assert!(entry_kinds.contains(&HistoryEntryKind::TimeRequested {
        generation: 1,
        due_at: instant(DUE_TIME),
        installation: NativeEffectStatus::Confirmed,
    }));
    assert!(entry_kinds.contains(&HistoryEntryKind::Cancelled {
        generation: Some(1),
        native_cancel: Some(NativeEffectStatus::Confirmed),
    }));
    assert!(entry_kinds.contains(&HistoryEntryKind::TimeRequested {
        generation: 2,
        due_at: instant(LATER_DUE_TIME),
        installation: NativeEffectStatus::Confirmed,
    }));
    assert_eq!(view.summary.delivery, DeliveryState::Unknown);

    let current = evidence(
        &second,
        2,
        DeliveryEvidenceKind::Delivered,
        "2026-01-18T09:00:03Z",
    );
    assert_eq!(
        record_evidence(&mut db, &current, "2026-01-18T09:01:00Z")?,
        RecordOutcome::Recorded {
            current_generation: true
        }
    );
    assert_eq!(
        stored(&mut db, "item-1")?.delivery_state,
        DeliveryState::Delivered
    );
    Ok(())
}

#[test]
fn returning_after_a_lapse_replays_nothing() -> Result<()> {
    let mut db = open_db("lapse")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let other_revision = insert_action(&mut db, "item-2", "book the dentist")?;
    let mut center = FakeCenter::default();
    let first = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    set_time(
        &mut db,
        "item-2",
        other_revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;
    record_evidence(
        &mut db,
        &evidence(
            &first,
            1,
            DeliveryEvidenceKind::Delivered,
            "2026-01-16T09:00:05Z",
        ),
        "2026-01-16T09:01:00Z",
    )?;

    let during = history(&mut db, "item-1", "2026-01-16T12:00:00Z")?;
    let untouched = history(&mut db, "item-2", "2026-01-16T12:00:00Z")?;

    // Two months later, with a reconcile on return: histories are exactly what was recorded.
    settle(&mut db, &mut center, "2026-03-20T08:00:00Z")?;
    let returned = history(&mut db, "item-1", "2026-03-20T08:01:00Z")?;
    let returned_other = history(&mut db, "item-2", "2026-03-20T08:01:00Z")?;
    assert_eq!(returned.entries, during.entries);
    assert_eq!(returned_other.entries, untouched.entries);
    assert_eq!(returned.summary.delivery, DeliveryState::Delivered);
    assert!(returned_other.summary.elapsed_without_delivery_evidence());
    assert_eq!(lifecycle_state(&db, "item-2"), "active");
    Ok(())
}

#[test]
fn facts_for_unknown_stale_mismatched_or_deleted_targets_are_refused() -> Result<()> {
    let mut db = open_db("refused")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    insert_action(&mut db, "item-2", "book the dentist")?;
    let mut center = FakeCenter::default();
    let record = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;
    let id = record.reminder_id.clone();

    for notification_id in [
        "not-a-notification".to_string(),
        "missing-reminder#1".to_string(),
        format!("{id}#0"),
        format!("{id}#2"),
        format!("{id}#01"),
        format!("{id}#-1"),
        format!("{id}#"),
    ] {
        let attempt = DeliveryEvidence {
            notification_id: notification_id.clone(),
            kind: DeliveryEvidenceKind::Delivered,
            observed_at: instant("2026-01-16T09:00:00Z"),
        };
        assert!(
            matches!(
                record_evidence(&mut db, &attempt, "2026-01-16T09:01:00Z"),
                Err(HistoryError::UnknownNotification(_))
            ),
            "{notification_id}"
        );
    }

    assert!(matches!(
        acknowledge(
            &mut db,
            "item-2",
            &format!("{id}#1"),
            "2026-01-16T09:01:00Z"
        ),
        Err(HistoryError::ItemMismatch { .. })
    ));

    let future = evidence(
        &record,
        1,
        DeliveryEvidenceKind::Delivered,
        "2026-01-20T00:00:00Z",
    );
    assert!(matches!(
        record_evidence(&mut db, &future, "2026-01-16T09:01:00Z"),
        Err(HistoryError::FutureObservation(_))
    ));

    let events: i64 =
        db.conn()
            .query_row("SELECT COUNT(*) FROM reminder_history_events", [], |row| {
                row.get(0)
            })?;
    assert_eq!(events, 0, "refused facts leave no trace");
    assert_eq!(
        stored(&mut db, "item-1")?.delivery_state,
        DeliveryState::Unknown
    );

    mark_deletion_intent(&mut db, "item-1", revision, instant("2026-01-16T10:00:00Z"))?;
    let after_delete = evidence(
        &record,
        1,
        DeliveryEvidenceKind::Delivered,
        "2026-01-16T09:00:05Z",
    );
    assert!(matches!(
        record_evidence(&mut db, &after_delete, "2026-01-16T10:01:00Z"),
        Err(HistoryError::ItemDeleted(_))
    ));
    assert!(matches!(
        acknowledge(
            &mut db,
            "item-1",
            &format!("{id}#1"),
            "2026-01-16T10:02:00Z"
        ),
        Err(HistoryError::ItemDeleted(_))
    ));
    Ok(())
}

#[test]
fn evidence_for_a_completed_item_is_still_recorded() -> Result<()> {
    let mut db = open_db("completed")?;
    let revision = insert_action(&mut db, "item-1", "call the roofer")?;
    let mut center = FakeCenter::default();
    let record = set_time(
        &mut db,
        "item-1",
        revision,
        DUE_TIME,
        "2026-01-15T11:00:00Z",
    )?;
    settle(&mut db, &mut center, "2026-01-15T11:01:00Z")?;

    let completion = Event::new(
        "evt-complete".to_string(),
        "item-1".to_string(),
        revision,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-16T08:00:00Z".to_string(),
    )?;
    let tx = db.immediate_transaction()?;
    end_item_with_event(
        &tx,
        &clock_at("2026-01-16T08:00:00Z"),
        &completion,
        revision,
    )?;
    tx.commit()?;

    let delivered = evidence(
        &record,
        1,
        DeliveryEvidenceKind::Delivered,
        "2026-01-16T09:00:05Z",
    );
    assert_eq!(
        record_evidence(&mut db, &delivered, "2026-01-16T09:01:00Z")?,
        RecordOutcome::Recorded {
            current_generation: true
        }
    );
    let view = history(&mut db, "item-1", "2026-01-16T09:02:00Z")?;
    assert_eq!(view.summary.request_state, RequestState::Cancelled);
    assert!(kinds(&view).contains(&HistoryEntryKind::Delivered { generation: 1 }));
    Ok(())
}

#[test]
fn item_without_a_reminder_has_no_history() -> Result<()> {
    let mut db = open_db("none")?;
    insert_action(&mut db, "item-1", "call the roofer")?;
    let tx = db.immediate_transaction()?;
    assert!(reminder_history(&tx, &clock_at(CAPTURE_TIME), "item-1")?.is_none());
    tx.commit()?;
    Ok(())
}
