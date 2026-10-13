// Integration tests for prompt activation and responses (S03). Every test drives the real
// store, eligibility, event and reminder-state code through `PromptCoordinator`.

use chrono::{DateTime, Duration, Utc};
use ohand_core::export::ReadAuthentication;
use ohand_core::lifecycle::delete_intent::mark_deletion_intent;
use ohand_core::reminders::state::{
    apply_user_time_correction, get_reminder, list_operations, ReminderOperation, ReminderRecord,
    UserTimeCorrection,
};
use ohand_core::store::captures::{self, Capture};
use ohand_core::store::events::{
    self, Correction, CorrectionKind, Event, EventPayload, EventType, ItemScope,
};
use ohand_core::store::schema::{Clock, Database};
use ohand_core::suggestions::coordinator::{
    PromptActivation, PromptCoordinator, PromptCoordinatorError, PromptOffer, PromptResponse,
    PromptResponseCommand, ResponseOutcome, PROMPT_RESPONSES,
};
use ohand_core::suggestions::eligibility::NOT_NOW_COOLDOWN;
use std::sync::{Arc, Mutex};

const START: &str = "2026-10-08T10:00:00Z";
const PROMPT_ID: &str = "prompt:2026-10-08";
const AUTHENTICATED: ReadAuthentication = ReadAuthentication::Authenticated;

fn instant(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    fn at(text: &str) -> Arc<TestClock> {
        Arc::new(TestClock {
            now: Mutex::new(instant(text)),
        })
    }

    fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}

struct Fixture {
    database: Database,
    clock: Arc<TestClock>,
}

impl Fixture {
    fn new() -> Fixture {
        let clock = TestClock::at(START);
        let database = Database::open(":memory:", clock.clone()).unwrap();
        Fixture { database, clock }
    }

    fn activate_with(
        &mut self,
        authentication: ReadAuthentication,
        notification_id: &str,
        scope_filter: Option<ItemScope>,
    ) -> Result<PromptActivation, PromptCoordinatorError> {
        PromptCoordinator::new(self.clock.as_ref()).activate(
            &mut self.database,
            authentication,
            notification_id,
            scope_filter,
        )
    }

    fn respond_with(
        &mut self,
        authentication: ReadAuthentication,
        command: &PromptResponseCommand,
    ) -> Result<ResponseOutcome, PromptCoordinatorError> {
        PromptCoordinator::new(self.clock.as_ref()).respond(
            &mut self.database,
            authentication,
            command,
        )
    }

    /// Insert an active item whose capture text is `text`, typed `item_type` when given.
    /// Returns the item's revision.
    fn add_item(&mut self, item_id: &str, text: &str, item_type: Option<&str>) -> i32 {
        self.add_item_with_scope(item_id, text, item_type, "personal")
    }

    fn add_item_with_scope(
        &mut self,
        item_id: &str,
        text: &str,
        item_type: Option<&str>,
        scope: &str,
    ) -> i32 {
        let capture_id = format!("capture-{item_id}");
        let capture = Capture::new(
            capture_id.clone(),
            Some(text.to_string()),
            None,
            START.to_string(),
            "America/New_York".to_string(),
            -240,
            "en_US".to_string(),
            "gregorian".to_string(),
            scope.to_string(),
            "route-default".to_string(),
            false,
            START.to_string(),
            None,
        )
        .unwrap();
        captures::save_capture(&mut self.database, &capture).unwrap();
        let transaction = self.database.immediate_transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO items (
                    item_id, capture_id, revision, lifecycle_state, save_state,
                    sync_state, processing_state, transcription_state, created_at, updated_at
                ) VALUES (?, ?, 0, 'active', 'saved', 'not_configured', 'not_configured',
                          'not_configured', ?, ?)",
                rusqlite::params![item_id, capture_id, START, START],
            )
            .unwrap();
        let mut revision = 0;
        if let Some(item_type) = item_type {
            let event = Event::new(
                format!("type-{item_id}"),
                item_id.to_string(),
                0,
                EventType::Correction,
                EventPayload::Correction(Correction {
                    kind: CorrectionKind::Type,
                    old_value: None,
                    new_value: item_type.to_string(),
                }),
                START.to_string(),
            )
            .unwrap();
            events::save_event_in_tx(&transaction, &event, 0).unwrap();
            revision = 1;
        }
        transaction.commit().unwrap();
        revision
    }

    fn revision(&mut self, item_id: &str) -> i32 {
        let transaction = self.database.transaction().unwrap();
        events::get_item_snapshot(&transaction, item_id)
            .unwrap()
            .unwrap()
            .revision
    }

    fn lifecycle(&mut self, item_id: &str) -> String {
        let transaction = self.database.transaction().unwrap();
        events::get_item_snapshot(&transaction, item_id)
            .unwrap()
            .unwrap()
            .lifecycle_state
    }

    fn activate(&mut self) -> PromptActivation {
        self.activate_with(AUTHENTICATED, PROMPT_ID, None).unwrap()
    }

    fn offered_item(&mut self) -> Option<String> {
        match self.activate() {
            PromptActivation::Offer(offer) => Some(offer.item_id),
            PromptActivation::NothingToOffer => None,
        }
    }

    fn respond(
        &mut self,
        command_id: &str,
        item_id: &str,
        expected_revision: i32,
        response: PromptResponse,
    ) -> Result<ResponseOutcome, PromptCoordinatorError> {
        let command = PromptResponseCommand {
            command_id: command_id.to_string(),
            item_id: item_id.to_string(),
            expected_revision,
            response,
        };
        self.respond_with(AUTHENTICATED, &command)
    }

    fn event_count(&mut self) -> i64 {
        self.database
            .transaction()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }

    fn selection_count(&mut self) -> i64 {
        self.database
            .transaction()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM suggestion_eligibility WHERE last_selected_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn expect_offer(activation: PromptActivation) -> PromptOffer {
    match activation {
        PromptActivation::Offer(offer) => offer,
        PromptActivation::NothingToOffer => panic!("expected an offer"),
    }
}

#[test]
fn offers_one_current_eligible_action_with_simple_responses() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Water the plants", Some("action"));
    fixture.add_item("note-b", "A passing thought", Some("note"));
    fixture.add_item("untyped-c", "Not interpreted yet", None);

    let offer = expect_offer(fixture.activate());

    assert_eq!(offer.item_id, "item-a");
    assert_eq!(offer.revision, revision);
    assert_eq!(offer.text.as_deref(), Some("Water the plants"));
    assert_eq!(offer.scope, ItemScope::Personal);
    assert_eq!(offer.selection_reason, "active_action");
    assert_eq!(offer.responses, PROMPT_RESPONSES);
    assert!(!format!("{offer:?}").contains("Water the plants"));
}

#[test]
fn empty_store_and_ineligible_items_give_a_calm_empty_result() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);

    fixture.add_item("note-a", "A note", Some("note"));
    fixture.add_item("idea-b", "An idea", Some("idea"));
    fixture.add_item("untyped-c", "Uninterpreted", None);

    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
    assert_eq!(fixture.selection_count(), 0);
}

#[test]
fn items_preselected_earlier_are_never_current_once_deleted_completed_or_snoozed() {
    let mut fixture = Fixture::new();
    let revision_a = fixture.add_item("item-a", "Alpha", Some("action"));
    let revision_b = fixture.add_item("item-b", "Beta", Some("action"));
    let revision_c = fixture.add_item("item-c", "Gamma", Some("action"));
    let revision_d = fixture.add_item("item-d", "Delta", Some("action"));

    // Rotation has now preselected every item once.
    let mut first_round = Vec::new();
    for _ in 0..4 {
        first_round.push(fixture.offered_item().unwrap());
    }
    first_round.sort();
    assert_eq!(first_round, ["item-a", "item-b", "item-c", "item-d"]);

    let now = fixture.clock.now();
    mark_deletion_intent(&mut fixture.database, "item-a", revision_a, now).unwrap();
    assert_eq!(fixture.lifecycle("item-a"), "deleted");

    assert_eq!(
        fixture
            .respond("done-b", "item-b", revision_b, PromptResponse::Done)
            .unwrap(),
        ResponseOutcome::Applied { replayed: false }
    );
    assert_eq!(
        fixture
            .respond("later-c", "item-c", revision_c, PromptResponse::NotNow)
            .unwrap(),
        ResponseOutcome::Applied { replayed: false }
    );

    // Only item-d remains current, however long ago the others were chosen.
    for _ in 0..3 {
        assert_eq!(fixture.offered_item().as_deref(), Some("item-d"));
    }

    assert_eq!(
        fixture
            .respond(
                "stop-d",
                "item-d",
                revision_d,
                PromptResponse::StopSuggesting
            )
            .unwrap(),
        ResponseOutcome::Applied { replayed: false }
    );
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);

    // The snoozed item comes back once its stated cooldown has passed.
    fixture
        .clock
        .advance(NOT_NOW_COOLDOWN + Duration::seconds(1));
    assert_eq!(fixture.offered_item().as_deref(), Some("item-c"));
}

#[test]
fn rotation_alternates_between_eligible_items() {
    let mut fixture = Fixture::new();
    fixture.add_item("item-a", "Alpha", Some("action"));
    fixture.add_item("item-b", "Beta", Some("action"));

    let sequence: Vec<_> = (0..4).map(|_| fixture.offered_item().unwrap()).collect();

    assert_eq!(sequence, ["item-a", "item-b", "item-a", "item-b"]);
}

#[test]
fn scope_filter_limits_the_offer() {
    let mut fixture = Fixture::new();
    fixture.add_item_with_scope("work-a", "Quarterly numbers", Some("action"), "work");

    let personal_only = fixture
        .activate_with(AUTHENTICATED, PROMPT_ID, Some(ItemScope::Personal))
        .unwrap();
    assert_eq!(personal_only, PromptActivation::NothingToOffer);

    let work = fixture
        .activate_with(AUTHENTICATED, PROMPT_ID, Some(ItemScope::Work))
        .unwrap();
    assert_eq!(expect_offer(work).item_id, "work-a");
}

#[test]
fn unauthenticated_sessions_read_and_write_nothing() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));
    let events_before = fixture.event_count();

    let activation = fixture.activate_with(ReadAuthentication::CaptureOnly, PROMPT_ID, None);
    assert!(matches!(
        activation,
        Err(PromptCoordinatorError::NotAuthenticated)
    ));

    let command = PromptResponseCommand {
        command_id: "locked".to_string(),
        item_id: "item-a".to_string(),
        expected_revision: revision,
        response: PromptResponse::Done,
    };
    let response = fixture.respond_with(ReadAuthentication::CaptureOnly, &command);
    assert!(matches!(
        response,
        Err(PromptCoordinatorError::NotAuthenticated)
    ));

    assert_eq!(fixture.selection_count(), 0);
    assert_eq!(fixture.event_count(), events_before);
    assert_eq!(fixture.lifecycle("item-a"), "active");
}

#[test]
fn only_prompt_notifications_activate_an_offer() {
    let mut fixture = Fixture::new();
    fixture.add_item("item-a", "Alpha", Some("action"));

    for notification_id in ["reminder:abc#1", "prompt:2026-10-08#2", "", "other"] {
        let result = fixture.activate_with(AUTHENTICATED, notification_id, None);
        assert!(
            matches!(result, Err(PromptCoordinatorError::NotAPrompt(_))),
            "{notification_id}"
        );
    }
    assert_eq!(fixture.selection_count(), 0);
}

#[test]
fn not_now_applies_the_cooldown_through_the_revisioned_operation() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));

    let outcome = fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::NotNow)
        .unwrap();

    assert_eq!(outcome, ResponseOutcome::Applied { replayed: false });
    assert_eq!(fixture.revision("item-a"), revision + 1);
    assert_eq!(fixture.lifecycle("item-a"), "active");
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);

    fixture
        .clock
        .advance(NOT_NOW_COOLDOWN - Duration::seconds(1));
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
    fixture.clock.advance(Duration::seconds(2));
    assert_eq!(fixture.offered_item().as_deref(), Some("item-a"));
}

#[test]
fn stop_suggesting_is_durable_pull_only_and_keeps_the_item() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));

    let outcome = fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::StopSuggesting)
        .unwrap();

    assert_eq!(outcome, ResponseOutcome::Applied { replayed: false });
    assert_eq!(fixture.revision("item-a"), revision + 1);
    assert_eq!(fixture.lifecycle("item-a"), "active");
    fixture.clock.advance(Duration::days(400));
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
}

#[test]
fn done_completes_the_item_and_stops_offering_it() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));

    let outcome = fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::Done)
        .unwrap();

    assert_eq!(outcome, ResponseOutcome::Applied { replayed: false });
    assert_eq!(fixture.lifecycle("item-a"), "completed");
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
}

#[test]
fn a_retried_response_has_one_effect() {
    for response in PROMPT_RESPONSES {
        let mut fixture = Fixture::new();
        let revision = fixture.add_item("item-a", "Alpha", Some("action"));

        let first = fixture
            .respond("cmd-1", "item-a", revision, response)
            .unwrap();
        let events_after_first = fixture.event_count();
        let revision_after_first = fixture.revision("item-a");
        fixture.clock.advance(Duration::minutes(5));
        let retry = fixture
            .respond("cmd-1", "item-a", revision, response)
            .unwrap();

        assert_eq!(first, ResponseOutcome::Applied { replayed: false });
        assert_eq!(retry, ResponseOutcome::Applied { replayed: true });
        assert_eq!(fixture.event_count(), events_after_first, "{response:?}");
        assert_eq!(fixture.revision("item-a"), revision_after_first);
    }
}

#[test]
fn reusing_a_command_id_for_another_response_is_a_conflict() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));
    fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::NotNow)
        .unwrap();

    let conflict = fixture.respond("cmd-1", "item-a", revision, PromptResponse::Done);

    assert!(matches!(
        conflict,
        Err(PromptCoordinatorError::CommandConflict { .. })
    ));
    assert_eq!(fixture.lifecycle("item-a"), "active");
}

#[test]
fn a_response_to_a_changed_item_is_stale_and_writes_nothing() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Alpha", Some("action"));
    let transaction = fixture.database.immediate_transaction().unwrap();
    let correction = Event::new(
        "edit-1".to_string(),
        "item-a".to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some("Alpha".to_string()),
            new_value: "Alpha, but better".to_string(),
        }),
        START.to_string(),
    )
    .unwrap();
    events::save_event_in_tx(&transaction, &correction, revision).unwrap();
    transaction.commit().unwrap();
    let events_before = fixture.event_count();

    for response in PROMPT_RESPONSES {
        let outcome = fixture
            .respond("cmd-1", "item-a", revision, response)
            .unwrap();
        assert_eq!(
            outcome,
            ResponseOutcome::Stale {
                current_revision: revision + 1
            }
        );
    }

    assert_eq!(fixture.event_count(), events_before);
    assert_eq!(fixture.lifecycle("item-a"), "active");
    let offer = expect_offer(fixture.activate());
    assert_eq!(offer.revision, revision + 1);
    assert_eq!(offer.text.as_deref(), Some("Alpha, but better"));
}

#[test]
fn responding_to_an_item_that_ended_meanwhile_is_no_longer_current() {
    let mut fixture = Fixture::new();
    let revision_a = fixture.add_item("item-a", "Alpha", Some("action"));
    let revision_b = fixture.add_item("item-b", "Beta", Some("action"));
    fixture
        .respond("done-a", "item-a", revision_a, PromptResponse::Done)
        .unwrap();
    let now = fixture.clock.now();
    mark_deletion_intent(&mut fixture.database, "item-b", revision_b, now).unwrap();

    for (command, item_id, revision) in [
        ("late-a", "item-a", revision_a + 1),
        ("late-b", "item-b", revision_b + 1),
        ("late-gone", "missing", 0),
    ] {
        for response in PROMPT_RESPONSES {
            let outcome = fixture
                .respond(
                    &format!("{command}-{response:?}"),
                    item_id,
                    revision,
                    response,
                )
                .unwrap();
            assert_eq!(outcome, ResponseOutcome::NoLongerCurrent, "{item_id}");
        }
    }
    assert_eq!(fixture.lifecycle("item-a"), "completed");
}

fn reminder_snapshot(
    fixture: &mut Fixture,
    item_id: &str,
) -> (Option<ReminderRecord>, Vec<ReminderOperation>) {
    let transaction = fixture.database.transaction().unwrap();
    let reminder = get_reminder(&transaction, item_id).unwrap();
    let operations = reminder
        .as_ref()
        .map(|record| list_operations(&transaction, &record.reminder_id).unwrap())
        .unwrap_or_default();
    (reminder, operations)
}

fn set_reminder(fixture: &mut Fixture, item_id: &str, expected_revision: i32) {
    let correction = UserTimeCorrection {
        command_id: format!("remind-{item_id}"),
        item_id: item_id.to_string(),
        expected_revision,
        expected_reminder_version: 0,
        instant: instant("2026-10-20T15:00:00Z"),
        timezone_id: "America/New_York".to_string(),
    };
    let transaction = fixture.database.immediate_transaction().unwrap();
    apply_user_time_correction(&transaction, fixture.clock.as_ref(), &correction).unwrap();
    transaction.commit().unwrap();
}

#[test]
fn muting_optional_help_leaves_explicit_reminders_unchanged() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Call the plumber", Some("action"));
    set_reminder(&mut fixture, "item-a", revision);
    let revision = fixture.revision("item-a");
    let before = reminder_snapshot(&mut fixture, "item-a");
    assert!(before.0.is_some());

    // A dated item is never offered, but a stale prompt may still be answered.
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
    fixture.clock.advance(Duration::minutes(1));
    fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::NotNow)
        .unwrap();
    let revision = fixture.revision("item-a");
    fixture
        .respond("cmd-2", "item-a", revision, PromptResponse::StopSuggesting)
        .unwrap();

    assert_eq!(reminder_snapshot(&mut fixture, "item-a"), before);
    assert_eq!(fixture.lifecycle("item-a"), "active");
}

#[test]
fn an_explicit_reminder_can_still_be_set_after_stop_suggesting() {
    let mut fixture = Fixture::new();
    let revision = fixture.add_item("item-a", "Call the plumber", Some("action"));
    fixture
        .respond("cmd-1", "item-a", revision, PromptResponse::StopSuggesting)
        .unwrap();
    let revision = fixture.revision("item-a");

    set_reminder(&mut fixture, "item-a", revision);

    let (reminder, _) = reminder_snapshot(&mut fixture, "item-a");
    assert!(reminder.is_some());
    assert_eq!(fixture.activate(), PromptActivation::NothingToOffer);
}

#[test]
fn done_cancels_only_the_completed_items_own_reminder() {
    let mut fixture = Fixture::new();
    let revision_a = fixture.add_item("item-a", "Call the plumber", Some("action"));
    let revision_b = fixture.add_item("item-b", "Book the dentist", Some("action"));
    set_reminder(&mut fixture, "item-a", revision_a);
    set_reminder(&mut fixture, "item-b", revision_b);
    let other_before = reminder_snapshot(&mut fixture, "item-b");
    let revision_a = fixture.revision("item-a");

    fixture
        .respond("cmd-1", "item-a", revision_a, PromptResponse::Done)
        .unwrap();

    assert_eq!(reminder_snapshot(&mut fixture, "item-b"), other_before);
    assert_eq!(fixture.lifecycle("item-a"), "completed");
}
