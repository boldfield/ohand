//! Behavior tests for reading, editing, cancelling and reviewing an item's reminder (A04) against
//! the real core store. State is re-read through a fresh connection to the file.

use chrono::{DateTime, Duration, Utc};
use ohand_core::ffi::core_handle::failure::AbiFailure;
use ohand_core::ffi::reminder_controls::{
    apply_reminder_cancel, apply_reminder_time_edit, read_item_reminder, ReminderCancel,
    ReminderRead, ReminderReadKind, ReminderTimeEdit, ReminderWriteOutcome,
};
use ohand_core::reminders::state::{
    desired_notifications, get_reminder, list_operations, OperationType,
};
use ohand_core::store::events::ItemScope;
use ohand_core::store::schema::{Clock, Database};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const NOW: &str = "2026-01-15T10:30:00Z";
const ZONE: &str = "Europe/Dublin";

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        now()
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .with_timezone(&Utc)
}

fn open(path: &str) -> Database {
    Database::open(path, Arc::new(FixedClock)).unwrap()
}

struct Fixture {
    db: Database,
    path: String,
    item_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Fixture {
    fn new(item_type: &str, scope: &str) -> Fixture {
        let path = format!(
            "{}/test_reminder_controls_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let db = open(&path);
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        db.conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, 'synthetic capture', ?, 'UTC', 0, 'en', 'gregorian', ?, 'route-1', 0, ?)",
                rusqlite::params![capture_id, NOW, scope, NOW],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type, \
                 save_state, sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', ?, 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, item_type, NOW, NOW],
            )
            .unwrap();
        Fixture { db, path, item_id }
    }

    fn action() -> Fixture {
        Fixture::new("action", "personal")
    }

    fn edit(
        &mut self,
        command_id: &str,
        version: i64,
        instant: DateTime<Utc>,
    ) -> ReminderWriteOutcome {
        apply_reminder_time_edit(
            &mut self.db,
            &FixedClock,
            &ReminderTimeEdit {
                command_id: command_id.to_string(),
                item_id: self.item_id.clone(),
                expected_revision: 0,
                expected_reminder_version: version,
                instant,
                timezone_id: ZONE.to_string(),
            },
        )
        .unwrap()
    }

    fn cancel(&mut self, command_id: &str, version: i64) -> ReminderWriteOutcome {
        apply_reminder_cancel(
            &mut self.db,
            &FixedClock,
            &ReminderCancel {
                command_id: command_id.to_string(),
                item_id: self.item_id.clone(),
                expected_revision: 0,
                expected_reminder_version: version,
            },
        )
        .unwrap()
    }

    fn read(&mut self, kind: ReminderReadKind, scopes: &[ItemScope]) -> Result<Value, AbiFailure> {
        read_item_reminder(
            &mut self.db,
            &FixedClock,
            &ReminderRead {
                kind,
                item_id: self.item_id.clone(),
                read_scopes: scopes.to_vec(),
            },
        )
    }

    fn stored_instant(&self) -> Option<DateTime<Utc>> {
        let mut fresh = open(&self.path);
        let tx = fresh.transaction().unwrap();
        get_reminder(&tx, &self.item_id)
            .unwrap()
            .and_then(|record| record.resolved_instant)
    }
}

fn applied(outcome: ReminderWriteOutcome) -> (ohand_core::reminders::state::ReminderRecord, bool) {
    match outcome {
        ReminderWriteOutcome::Applied { reminder, replayed } => (reminder, replayed),
        other => panic!("expected an applied command, got {other:?}"),
    }
}

#[test]
fn the_stored_instant_is_exactly_the_one_the_user_chose() {
    let mut fixture = Fixture::action();
    let chosen = now() + Duration::days(3) + Duration::seconds(17);
    let (record, replayed) = applied(fixture.edit("c-1", 0, chosen));
    assert!(!replayed);
    assert_eq!(record.resolved_instant, Some(chosen));
    assert_eq!(record.timezone_id.as_deref(), Some(ZONE));
    assert_eq!(fixture.stored_instant(), Some(chosen));
}

#[test]
fn only_an_instant_after_now_is_accepted() {
    let mut fixture = Fixture::action();
    assert_eq!(
        fixture.edit("c-1", 0, now()),
        ReminderWriteOutcome::TimeInPast
    );
    assert_eq!(
        fixture.edit("c-2", 0, now() - Duration::days(1)),
        ReminderWriteOutcome::TimeInPast
    );
    assert_eq!(
        fixture.stored_instant(),
        None,
        "nothing backdated was stored"
    );

    let soonest = now() + Duration::seconds(1);
    let (record, _) = applied(fixture.edit("c-3", 0, soonest));
    assert_eq!(record.resolved_instant, Some(soonest));
}

#[test]
fn an_edit_queues_the_new_generation_and_a_cancel_queues_its_removal_for_the_next_pass() {
    let mut fixture = Fixture::action();
    let first = now() + Duration::days(1);
    let (record, _) = applied(fixture.edit("c-1", 0, first));
    let second = now() + Duration::days(2);
    let (record, _) = applied(fixture.edit("c-2", record.state_version, second));
    assert_eq!(record.resolved_instant, Some(second));

    let mut fresh = open(&fixture.path);
    let tx = fresh.transaction().unwrap();
    let desired = desired_notifications(&tx).unwrap();
    assert_eq!(desired.len(), 1);
    assert_eq!(desired[0].fire_at, second);
    let operations = list_operations(&tx, &record.reminder_id).unwrap();
    assert!(operations.iter().any(
        |operation| operation.operation_type == OperationType::Schedule
            && operation.scheduled_for == Some(second)
    ));
    drop(tx);
    drop(fresh);

    let (cancelled, _) = applied(fixture.cancel("c-3", record.state_version));
    let mut fresh = open(&fixture.path);
    let tx = fresh.transaction().unwrap();
    assert!(desired_notifications(&tx).unwrap().is_empty());
    let operations = list_operations(&tx, &cancelled.reminder_id).unwrap();
    assert!(operations
        .iter()
        .any(|operation| operation.operation_type == OperationType::Cancel));
}

#[test]
fn stale_versions_are_refused_and_a_replay_is_not_applied_twice() {
    let mut fixture = Fixture::action();
    let (record, _) = applied(fixture.edit("c-1", 0, now() + Duration::days(1)));
    let later = now() + Duration::days(5);

    assert_eq!(
        fixture.edit("c-2", 0, later),
        ReminderWriteOutcome::StaleReminderVersion {
            current_reminder_version: record.state_version
        }
    );
    assert_eq!(
        fixture.cancel("c-3", record.state_version + 1),
        ReminderWriteOutcome::StaleReminderVersion {
            current_reminder_version: record.state_version
        }
    );
    assert_eq!(fixture.stored_instant(), record.resolved_instant);

    let (again, replayed) = applied(fixture.edit("c-1", 0, now() + Duration::days(1)));
    assert!(replayed);
    assert_eq!(again.state_version, record.state_version);
    assert_eq!(
        fixture.edit("c-1", 0, later),
        ReminderWriteOutcome::CommandConflict
    );
}

#[test]
fn reads_are_scoped_and_a_reminder_reports_its_states() {
    let mut fixture = Fixture::action();
    let chosen = now() + Duration::days(1);
    applied(fixture.edit("c-1", 0, chosen));

    assert_eq!(
        fixture.read(ReminderReadKind::Reminder, &[ItemScope::Work]),
        Err(AbiFailure::NOT_FOUND)
    );
    let body = fixture
        .read(ReminderReadKind::Reminder, &[ItemScope::Personal])
        .unwrap();
    assert_eq!(body["item_revision"], 0);
    assert_eq!(body["reminder"]["request_state"], "resolved");
    assert_eq!(body["reminder"]["resolved_instant"], "2026-01-16T10:30:00Z");

    let history = fixture
        .read(ReminderReadKind::History, &[ItemScope::Personal])
        .unwrap();
    assert_eq!(history["summary"]["due"], "upcoming");
    assert!(!history["entries"].as_array().unwrap().is_empty());
    assert_eq!(history["truncated"], false);
}
