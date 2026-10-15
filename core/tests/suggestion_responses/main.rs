//! Behavior tests for prompt activation and responses over the core's command document (A06),
//! against the real store. Every assertion re-reads durable state.

use chrono::{DateTime, Utc};
use ohand_core::ffi::suggestion_responses::apply_suggestion_command;
use ohand_core::store::schema::{Clock, Database};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

const CAPTURE_INSTANT: &str = "2026-10-08T10:00:00Z";
const PROMPT_ID: &str = "prompt:2026-10-08";

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        CAPTURE_INSTANT.parse().unwrap()
    }
}

struct Fixture {
    database: Database,
    path: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Fixture {
    fn new() -> Fixture {
        let path = format!(
            "{}/test_suggestion_responses_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let database = Database::open(&path, Arc::new(FixedClock)).unwrap();
        Fixture { database, path }
    }

    /// An active item of `item_type` whose capture has `text` (a voice capture when `None`).
    fn add_item(&self, item_id: &str, item_type: &str, text: Option<&str>) {
        let capture_id = format!("capture-{item_id}");
        self.database
            .conn()
            .execute(
                "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, \
                 timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, \
                 entry_locked, created_at) VALUES (?, ?, ?, ?, 'UTC', 0, 'en', 'gregorian', \
                 'personal', 'route-1', 0, ?)",
                rusqlite::params![
                    capture_id,
                    text,
                    text.is_none().then_some("audio/synthetic.m4a"),
                    CAPTURE_INSTANT,
                    CAPTURE_INSTANT
                ],
            )
            .unwrap();
        self.database
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type, \
                 save_state, sync_state, processing_state, transcription_state, created_at, \
                 updated_at) VALUES (?, ?, 0, 'active', ?, 'saved_local', 'not_configured', \
                 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![
                    item_id,
                    capture_id,
                    item_type,
                    CAPTURE_INSTANT,
                    CAPTURE_INSTANT
                ],
            )
            .unwrap();
    }

    fn run(&mut self, command: &Value) -> Value {
        let bytes = serde_json::to_vec(command).unwrap();
        apply_suggestion_command(&mut self.database, &bytes).expect("the command is accepted")
    }

    fn activate(&mut self) -> Value {
        self.run(&json!({"command": "activate_prompt",
                         "read_authentication": "authenticated",
                         "notification_id": PROMPT_ID}))
    }

    fn respond(&mut self, command_id: &str, item_id: &str, revision: i64, response: &str) -> Value {
        self.run(
            &json!({"command": "respond", "read_authentication": "authenticated",
                         "command_id": command_id, "item_id": item_id,
                         "expected_revision": revision, "response": response}),
        )
    }

    fn scalar(&self, sql: &str) -> String {
        use rusqlite::types::Value as Cell;
        match self
            .database
            .conn()
            .query_row(sql, [], |row| row.get::<_, Cell>(0))
            .unwrap()
        {
            Cell::Text(text) => text,
            Cell::Integer(number) => number.to_string(),
            Cell::Null => "NULL".to_string(),
            other => panic!("unexpected cell {other:?}"),
        }
    }
}

#[test]
fn an_item_without_text_is_offered_with_null_text() {
    let mut fixture = Fixture::new();
    fixture.add_item("voice-1", "action", None);
    let offer = fixture.activate();
    assert_eq!(offer["outcome"], "offer");
    assert_eq!(offer["item_id"], "voice-1");
    assert!(offer["text"].is_null(), "{offer}");
}

#[test]
fn each_response_is_stored_once_through_the_event_path() {
    for (response, event_type, lifecycle) in [
        ("not_now", "suggestion_control", "active"),
        ("stop_suggesting", "suggestion_control", "active"),
        ("done", "completion", "completed"),
    ] {
        let mut fixture = Fixture::new();
        fixture.add_item("item-1", "action", Some("Water the plants"));
        let offer = fixture.activate();
        assert_eq!(offer["item_id"], "item-1");

        let first = fixture.respond("command-1", "item-1", 0, response);
        assert_eq!(
            first,
            json!({"outcome": "applied", "replayed": false}),
            "{response}"
        );
        let second = fixture.respond("command-1", "item-1", 0, response);
        assert_eq!(
            second,
            json!({"outcome": "applied", "replayed": true}),
            "{response}"
        );

        assert_eq!(
            fixture.scalar("SELECT COUNT(*) FROM events"),
            "1",
            "{response}"
        );
        assert_eq!(
            fixture.scalar("SELECT event_id FROM events"),
            "prompt-response:command-1"
        );
        assert_eq!(fixture.scalar("SELECT event_type FROM events"), event_type);
        assert_eq!(
            fixture.scalar("SELECT lifecycle_state FROM items"),
            lifecycle,
            "{response}"
        );
        assert_eq!(
            fixture.scalar("SELECT text FROM captures"),
            "Water the plants",
            "the captured source is never altered"
        );
    }
}

#[test]
fn a_prompt_whose_item_changed_or_was_replaced_reports_it_honestly() {
    let mut fixture = Fixture::new();
    fixture.add_item("item-1", "action", Some("First"));
    fixture.add_item("item-2", "action", Some("Second"));
    assert_eq!(fixture.activate()["item_id"], "item-1");

    // The user stops suggesting item-1 elsewhere; the revision moves on.
    fixture.respond("elsewhere", "item-1", 0, "stop_suggesting");
    let stale = fixture.respond("late", "item-1", 0, "not_now");
    assert_eq!(
        stale,
        json!({"outcome": "stale_revision", "current_revision": 1})
    );

    // The user completes item-2 elsewhere; the late answer finds nothing current.
    fixture.respond("finished", "item-2", 0, "done");
    let gone = fixture.respond("later", "item-2", 0, "not_now");
    assert_eq!(gone, json!({"outcome": "no_longer_current"}));

    // Nothing is left to offer, and the answer carries no backlog.
    assert_eq!(fixture.activate(), json!({"outcome": "nothing_to_offer"}));
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), "2");
}

#[test]
fn a_command_id_reused_for_another_response_conflicts_and_writes_nothing() {
    let mut fixture = Fixture::new();
    fixture.add_item("item-1", "action", Some("First"));
    fixture.respond("command-1", "item-1", 0, "not_now");
    let conflict = fixture.respond("command-1", "item-1", 0, "done");
    assert_eq!(conflict, json!({"outcome": "command_conflict"}));
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), "1");
    assert_eq!(
        fixture.scalar("SELECT lifecycle_state FROM items"),
        "active"
    );
}

#[test]
fn a_capture_only_session_gets_a_fixed_refusal_and_nothing_changes() {
    let mut fixture = Fixture::new();
    fixture.add_item("item-1", "action", Some("Secret text"));
    for command in [
        json!({"command": "activate_prompt", "read_authentication": "capture_only",
               "notification_id": PROMPT_ID}),
        json!({"command": "respond", "read_authentication": "capture_only",
               "command_id": "c", "item_id": "item-1", "expected_revision": 0,
               "response": "done"}),
    ] {
        let bytes = serde_json::to_vec(&command).unwrap();
        let failure = apply_suggestion_command(&mut fixture.database, &bytes).unwrap_err();
        assert_eq!(failure.code, "suggestion_not_authorized");
        let shown = String::from_utf8(failure.to_json()).unwrap();
        assert!(
            !shown.contains("Secret text") && !shown.contains("item-1"),
            "{shown}"
        );
    }
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), "0");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM suggestion_eligibility"),
        "0"
    );
    assert_eq!(
        fixture.scalar("SELECT lifecycle_state FROM items"),
        "active"
    );
}
