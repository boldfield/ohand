//! Behavior tests for the user's item lifecycle controls (A03) against the real core store.
//! Every test asserts on durable state re-read through a fresh connection to the file.

use chrono::{DateTime, SecondsFormat, Utc};
use ohand_core::ffi::core_handle::failure::AbiFailure;
use ohand_core::ffi::item_lifecycle_controls::{
    apply_item_lifecycle_control, ItemLifecycleControl, LifecycleControl, LifecycleControlOutcome,
};
use ohand_core::reminders::state::{
    desired_notifications, end_item_with_event, get_reminder, list_operations, OperationType,
    RequestState,
};
use ohand_core::store::events::{Event, EventPayload, EventType};
use ohand_core::store::schema::{Clock, Database, SystemClock};
use std::sync::Arc;
use uuid::Uuid;

const CAPTURE_INSTANT: &str = "2026-01-15T10:30:00Z";

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(CAPTURE_INSTANT)
            .unwrap()
            .with_timezone(&Utc)
    }
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
    /// An active action item with a live, scheduled reminder.
    fn with_reminder() -> Fixture {
        let fixture = Fixture::new();
        let reminder_id = Uuid::new_v4().to_string();
        let notification_id = format!("{reminder_id}#1");
        fixture
            .db
            .conn()
            .execute(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, \
                 delivery_state, acknowledgment_state, resolved_instant, timezone_id, \
                 schedule_generation, created_at, updated_at) \
                 VALUES (?, ?, 'resolved', 'pending_schedule', 'unknown', 'not_acknowledged', \
                 '2090-01-01T09:00:00Z', 'UTC', 1, ?, ?)",
                rusqlite::params![
                    reminder_id,
                    fixture.item_id,
                    CAPTURE_INSTANT,
                    CAPTURE_INSTANT
                ],
            )
            .unwrap();
        fixture
            .db
            .conn()
            .execute(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, \
                 operation_state, effect_identity, external_id, scheduled_for, created_at) \
                 VALUES (?, ?, 'schedule', 'pending', ?, ?, '2090-01-01T09:00:00Z', ?)",
                rusqlite::params![
                    format!("schedule:{notification_id}"),
                    reminder_id,
                    format!("schedule:{notification_id}"),
                    notification_id,
                    CAPTURE_INSTANT
                ],
            )
            .unwrap();
        fixture
    }

    fn new() -> Fixture {
        let path = format!(
            "{}/test_item_lifecycle_controls_{}.db",
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
                 VALUES (?, 'synthetic capture about parsnips', ?, 'UTC', 0, 'en', 'gregorian', \
                 'personal', 'route-1', 0, ?)",
                rusqlite::params![capture_id, CAPTURE_INSTANT, CAPTURE_INSTANT],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type, \
                 save_state, sync_state, processing_state, transcription_state, created_at, \
                 updated_at) \
                 VALUES (?, ?, 0, 'active', 'action', 'saved_local', 'not_configured', \
                 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, CAPTURE_INSTANT, CAPTURE_INSTANT],
            )
            .unwrap();
        Fixture { db, path, item_id }
    }

    fn command(
        &self,
        command_id: &str,
        expected_revision: i32,
        control: LifecycleControl,
    ) -> ItemLifecycleControl {
        ItemLifecycleControl {
            command_id: command_id.to_string(),
            item_id: self.item_id.clone(),
            expected_revision,
            control,
        }
    }

    fn run(
        &mut self,
        command_id: &str,
        expected_revision: i32,
        control: LifecycleControl,
    ) -> LifecycleControlOutcome {
        let command = self.command(command_id, expected_revision, control);
        apply_item_lifecycle_control(&mut self.db, &command).unwrap()
    }

    fn applied(
        &mut self,
        command_id: &str,
        expected_revision: i32,
        control: LifecycleControl,
    ) -> i32 {
        match self.run(command_id, expected_revision, control) {
            LifecycleControlOutcome::Applied {
                revision,
                replayed: false,
            } => revision,
            other => panic!("expected a fresh application, got {other:?}"),
        }
    }

    fn mark_deleted(&self) {
        self.db
            .conn()
            .execute("UPDATE items SET lifecycle_state = 'deleted'", [])
            .unwrap();
    }

    fn durable(&self) -> Durable {
        let db = open(&self.path);
        let conn = db.conn();
        let scalar = |sql: &str| -> i64 {
            conn.query_row(sql, [&self.item_id], |row| row.get(0))
                .unwrap()
        };
        let (lifecycle_state, revision): (String, i32) = conn
            .query_row(
                "SELECT lifecycle_state, revision FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let eligibility: Option<(bool, bool, bool)> = conn
            .query_row(
                "SELECT eligible, snoozed, pull_only FROM suggestion_eligibility WHERE item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();
        Durable {
            lifecycle_state,
            revision,
            events: scalar("SELECT COUNT(*) FROM events WHERE item_id = ?"),
            eligibility,
        }
    }

    fn event_types(&self) -> Vec<String> {
        let db = open(&self.path);
        let mut statement = db
            .conn()
            .prepare("SELECT event_type FROM events WHERE item_id = ? ORDER BY revision")
            .unwrap();
        statement
            .query_map([&self.item_id], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn reminder_snapshot(&self) -> (RequestState, Vec<OperationType>, usize) {
        let mut db = open(&self.path);
        let tx = db.immediate_transaction().unwrap();
        let record = get_reminder(&tx, &self.item_id).unwrap().unwrap();
        let operations = list_operations(&tx, &record.reminder_id)
            .unwrap()
            .iter()
            .map(|operation| operation.operation_type)
            .collect();
        let desired = desired_notifications(&tx).unwrap().len();
        (record.request_state, operations, desired)
    }
}

#[derive(Debug, PartialEq)]
struct Durable {
    lifecycle_state: String,
    revision: i32,
    events: i64,
    /// `(eligible, snoozed, pull_only)` of the item's suggestion eligibility row, if any.
    eligibility: Option<(bool, bool, bool)>,
}

const ALL_CONTROLS: [LifecycleControl; 4] = [
    LifecycleControl::Complete,
    LifecycleControl::Cancel,
    LifecycleControl::NotNow,
    LifecycleControl::StopSuggesting,
];

#[test]
fn completing_an_item_ends_it_and_cancels_its_live_reminder_in_one_step() {
    let mut fixture = Fixture::with_reminder();
    assert_eq!(
        fixture.reminder_snapshot(),
        (RequestState::Resolved, vec![OperationType::Schedule], 1)
    );

    let revision = fixture.applied("c-1", 0, LifecycleControl::Complete);

    assert_eq!(revision, 1);
    let durable = fixture.durable();
    assert_eq!(durable.lifecycle_state, "completed");
    assert_eq!(durable.revision, 1);
    assert_eq!(fixture.event_types(), vec!["completion"]);
    let (state, operations, desired) = fixture.reminder_snapshot();
    assert_eq!(state, RequestState::Cancelled);
    assert_eq!(
        operations,
        vec![OperationType::Schedule, OperationType::Cancel]
    );
    assert_eq!(desired, 0, "nothing is left for the OS to hold");
}

#[test]
fn cancelling_an_item_ends_it_and_cancels_its_live_reminder_in_one_step() {
    let mut fixture = Fixture::with_reminder();

    let revision = fixture.applied("c-1", 0, LifecycleControl::Cancel);

    assert_eq!(revision, 1);
    assert_eq!(fixture.durable().lifecycle_state, "cancelled");
    assert_eq!(fixture.event_types(), vec!["cancellation"]);
    let (state, operations, desired) = fixture.reminder_snapshot();
    assert_eq!(state, RequestState::Cancelled);
    assert_eq!(
        operations,
        vec![OperationType::Schedule, OperationType::Cancel]
    );
    assert_eq!(desired, 0);
}

#[test]
fn completion_leaves_the_same_reminder_state_as_the_notification_action_path() {
    let mut by_item = Fixture::with_reminder();
    by_item.applied("c-1", 0, LifecycleControl::Complete);

    let mut by_notification = Fixture::with_reminder();
    let event = Event::new(
        "notification-complete:synthetic".to_string(),
        by_notification.item_id.clone(),
        0,
        EventType::Completion,
        EventPayload::Completion,
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    )
    .unwrap();
    let tx = by_notification.db.immediate_transaction().unwrap();
    end_item_with_event(&tx, &SystemClock, &event, 0).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        by_item.reminder_snapshot(),
        by_notification.reminder_snapshot()
    );
    assert_eq!(by_item.durable(), by_notification.durable());
}

#[test]
fn completing_an_item_without_a_reminder_just_ends_it() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.applied("c-1", 0, LifecycleControl::Complete), 1);
    assert_eq!(fixture.durable().lifecycle_state, "completed");
}

#[test]
fn not_now_snoozes_suggestions_and_leaves_the_item_and_its_reminder_alone() {
    let mut fixture = Fixture::with_reminder();

    let revision = fixture.applied("c-1", 0, LifecycleControl::NotNow);

    assert_eq!(revision, 1);
    let durable = fixture.durable();
    assert_eq!(durable.lifecycle_state, "active");
    assert_eq!(durable.revision, 1);
    assert_eq!(durable.eligibility, Some((false, true, false)));
    assert_eq!(fixture.event_types(), vec!["suggestion_control"]);
    assert_eq!(
        fixture.reminder_snapshot(),
        (RequestState::Resolved, vec![OperationType::Schedule], 1),
        "muting optional help never touches an explicit reminder"
    );
}

#[test]
fn stop_suggesting_marks_the_item_pull_only_and_leaves_the_item_and_its_reminder_alone() {
    let mut fixture = Fixture::with_reminder();

    let revision = fixture.applied("c-1", 0, LifecycleControl::StopSuggesting);

    assert_eq!(revision, 1);
    let durable = fixture.durable();
    assert_eq!(durable.lifecycle_state, "active");
    assert_eq!(durable.eligibility, Some((false, false, true)));
    assert_eq!(fixture.event_types(), vec!["suggestion_control"]);
    assert_eq!(
        fixture.reminder_snapshot(),
        (RequestState::Resolved, vec![OperationType::Schedule], 1)
    );
}

#[test]
fn every_command_refuses_a_stale_revision_with_the_current_one_and_writes_nothing() {
    for control in ALL_CONTROLS {
        let mut fixture = Fixture::with_reminder();
        fixture.applied("c-0", 0, LifecycleControl::NotNow);
        let before = fixture.durable();

        let outcome = fixture.run("c-1", 0, control);

        assert_eq!(
            outcome,
            LifecycleControlOutcome::StaleRevision {
                current_revision: 1
            },
            "{control:?}"
        );
        assert_eq!(fixture.durable(), before, "{control:?}");
        assert_eq!(fixture.reminder_snapshot().0, RequestState::Resolved);
    }
}

#[test]
fn every_command_is_idempotent_by_command_id_and_ignores_the_spent_revision_on_replay() {
    for control in ALL_CONTROLS {
        let mut fixture = Fixture::with_reminder();
        let first = fixture.applied("c-1", 0, control);
        let after_first = fixture.durable();
        let reminder_after_first = fixture.reminder_snapshot();

        let repeated = fixture.run("c-1", 0, control);
        let stale_repeat = fixture.run("c-1", 99, control);

        let expected = LifecycleControlOutcome::Applied {
            revision: first,
            replayed: true,
        };
        assert_eq!(repeated, expected, "{control:?}");
        assert_eq!(stale_repeat, expected, "{control:?}");
        assert_eq!(fixture.durable(), after_first, "{control:?}");
        assert_eq!(fixture.reminder_snapshot(), reminder_after_first);
    }
}

#[test]
fn a_command_id_reused_for_another_control_or_item_conflicts_and_writes_nothing() {
    let mut fixture = Fixture::new();
    fixture.applied("c-1", 0, LifecycleControl::NotNow);
    let before = fixture.durable();

    assert_eq!(
        fixture.run("c-1", 0, LifecycleControl::StopSuggesting),
        LifecycleControlOutcome::CommandConflict
    );
    assert_eq!(
        fixture.run("c-1", 0, LifecycleControl::Complete),
        LifecycleControlOutcome::CommandConflict
    );
    let other_item = ItemLifecycleControl {
        item_id: Uuid::new_v4().to_string(),
        ..fixture.command("c-1", 0, LifecycleControl::NotNow)
    };
    assert_eq!(
        apply_item_lifecycle_control(&mut fixture.db, &other_item).unwrap(),
        LifecycleControlOutcome::CommandConflict
    );
    assert_eq!(fixture.durable(), before);
}

#[test]
fn every_command_on_a_deleted_item_is_refused_and_writes_nothing() {
    for control in ALL_CONTROLS {
        let mut fixture = Fixture::with_reminder();
        fixture.mark_deleted();
        let before = fixture.durable();

        let outcome = fixture.run("c-1", 0, control);

        assert_eq!(outcome, LifecycleControlOutcome::ItemDeleted, "{control:?}");
        assert_eq!(fixture.durable(), before, "{control:?}");
        assert_eq!(fixture.event_types(), Vec::<String>::new());
    }
}

#[test]
fn an_item_that_already_ended_takes_no_further_command() {
    for (ended_by, lifecycle) in [
        (LifecycleControl::Complete, "completed"),
        (LifecycleControl::Cancel, "cancelled"),
    ] {
        for control in ALL_CONTROLS {
            let mut fixture = Fixture::with_reminder();
            fixture.applied("c-0", 0, ended_by);
            let before = fixture.durable();

            let outcome = fixture.run("c-1", 1, control);

            assert_eq!(
                outcome,
                LifecycleControlOutcome::ItemEnded {
                    lifecycle: lifecycle.to_string()
                },
                "{ended_by:?} then {control:?}"
            );
            assert_eq!(fixture.durable(), before);
        }
    }
}

#[test]
fn an_unknown_item_is_not_found_for_every_command() {
    for control in ALL_CONTROLS {
        let mut fixture = Fixture::new();
        let command = ItemLifecycleControl {
            item_id: Uuid::new_v4().to_string(),
            ..fixture.command("c-1", 0, control)
        };
        assert_eq!(
            apply_item_lifecycle_control(&mut fixture.db, &command).unwrap_err(),
            AbiFailure::NOT_FOUND,
            "{control:?}"
        );
    }
}

fn reopened(outcome: LifecycleControlOutcome) -> (i32, bool, Option<String>) {
    match outcome {
        LifecycleControlOutcome::Reopened {
            revision,
            replayed,
            former_reminder_instant,
        } => (revision, replayed, former_reminder_instant),
        other => panic!("expected a reopen, got {other:?}"),
    }
}

#[test]
fn reopening_a_completed_or_cancelled_item_makes_it_active_and_keeps_its_reminder_cancelled() {
    for ending in [LifecycleControl::Complete, LifecycleControl::Cancel] {
        let mut fixture = Fixture::with_reminder();
        fixture.applied("c-1", 0, ending);
        assert_eq!(fixture.reminder_snapshot().0, RequestState::Cancelled);
        let operations_before = fixture.reminder_snapshot().1;

        let (revision, replayed, instant) =
            reopened(fixture.run("c-2", 1, LifecycleControl::Reopen));

        assert_eq!((revision, replayed), (2, false), "{ending:?}");
        assert_eq!(instant.as_deref(), Some("2090-01-01T09:00:00Z"));
        let durable = fixture.durable();
        assert_eq!(durable.lifecycle_state, "active");
        assert_eq!(durable.revision, 2);
        assert_eq!(fixture.event_types().last().unwrap(), "reopen");
        let (request_state, operations, desired) = fixture.reminder_snapshot();
        assert_eq!(request_state, RequestState::Cancelled);
        assert_eq!(operations, operations_before);
        assert_eq!(desired, 0);
    }
}

#[test]
fn reopening_an_item_that_never_had_a_reminder_returns_no_instant() {
    let mut fixture = Fixture::new();
    fixture.applied("c-1", 0, LifecycleControl::Complete);

    let (_, _, instant) = reopened(fixture.run("c-2", 1, LifecycleControl::Reopen));

    assert_eq!(instant, None);
}

#[test]
fn reopening_keeps_a_stop_suggesting_item_pull_only() {
    let mut fixture = Fixture::new();
    fixture.applied("c-1", 0, LifecycleControl::StopSuggesting);
    fixture.applied("c-2", 1, LifecycleControl::Complete);
    let ended = fixture.durable().eligibility;

    reopened(fixture.run("c-3", 2, LifecycleControl::Reopen));

    assert_eq!(fixture.durable().eligibility, ended);
    assert_eq!(fixture.durable().eligibility, Some((false, false, true)));
}

#[test]
fn reopening_an_active_deleted_or_stale_item_changes_nothing() {
    let mut active = Fixture::with_reminder();
    let before = active.durable();
    assert_eq!(
        active.run("c-1", 0, LifecycleControl::Reopen),
        LifecycleControlOutcome::AlreadyOpen
    );
    assert_eq!(active.durable(), before);

    let mut deleted = Fixture::new();
    deleted.mark_deleted();
    assert_eq!(
        deleted.run("c-1", 0, LifecycleControl::Reopen),
        LifecycleControlOutcome::ItemDeleted
    );
    assert_eq!(deleted.durable().lifecycle_state, "deleted");

    let mut stale = Fixture::new();
    stale.applied("c-1", 0, LifecycleControl::Cancel);
    let before = stale.durable();
    assert_eq!(
        stale.run("c-2", 0, LifecycleControl::Reopen),
        LifecycleControlOutcome::StaleRevision {
            current_revision: 1
        }
    );
    assert_eq!(stale.durable(), before);
}

#[test]
fn reopening_is_idempotent_by_command_id_and_a_reused_id_conflicts() {
    let mut fixture = Fixture::with_reminder();
    fixture.applied("c-1", 0, LifecycleControl::Complete);
    let first = reopened(fixture.run("c-2", 1, LifecycleControl::Reopen));
    let after_first = fixture.durable();

    let (revision, replayed, instant) = reopened(fixture.run("c-2", 1, LifecycleControl::Reopen));

    assert_eq!((revision, replayed), (first.0, true));
    assert_eq!(instant, first.2);
    assert_eq!(fixture.durable(), after_first);
    assert_eq!(
        fixture.run("c-2", 2, LifecycleControl::Complete),
        LifecycleControlOutcome::CommandConflict
    );
    assert_eq!(
        fixture.run("c-1", 0, LifecycleControl::Reopen),
        LifecycleControlOutcome::CommandConflict
    );
}

#[test]
fn a_reopened_item_can_be_completed_again() {
    let mut fixture = Fixture::new();
    fixture.applied("c-1", 0, LifecycleControl::Complete);
    reopened(fixture.run("c-2", 1, LifecycleControl::Reopen));

    fixture.applied("c-3", 2, LifecycleControl::Cancel);

    assert_eq!(fixture.durable().lifecycle_state, "cancelled");
}
