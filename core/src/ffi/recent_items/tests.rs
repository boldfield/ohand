//! Drives the recent-items export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, Event, Outcome, WAIT};
use crate::ingress::import::import_foreground_ingress;
use crate::lifecycle::delete_intent::mark_deletion_intent;
use crate::store::captures::Capture;
use crate::store::schema::SystemClock;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tempfile::TempDir;

const CURSOR_SENTINEL: &str = "sentinelcursor";

fn capture(
    capture_id: &str,
    text: Option<&str>,
    audio_reference: Option<&str>,
    scope: &str,
    capture_instant: &str,
) -> Capture {
    Capture::new(
        capture_id.to_string(),
        text.map(str::to_string),
        audio_reference.map(str::to_string),
        capture_instant.to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        format!("route-{scope}"),
        false,
        capture_instant.to_string(),
        None,
    )
    .unwrap()
}

struct Seeded {
    oldest: String,
    typed: String,
    corrected: String,
    voice: String,
    work: String,
    deleted: String,
    mixed_offset: String,
}

struct Fixture {
    _directory: TempDir,
    store_path: String,
    seeded: Seeded,
}

impl Fixture {
    fn new() -> Fixture {
        let directory = TempDir::new().unwrap();
        let store_path = directory.path().join("core.sqlite").display().to_string();
        let mut database = Database::open(&store_path, Arc::new(SystemClock)).unwrap();
        for scope in ["personal", "work"] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                         created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-01T00:00:00Z')",
                    rusqlite::params![format!("route-{scope}"), format!("route-{scope}"), scope],
                )
                .unwrap();
        }
        let mut import = |record: Capture| {
            import_foreground_ingress(&mut database, &record)
                .unwrap()
                .item_id
        };
        let oldest = import(capture(
            "c-oldest",
            Some("Buy oat milk"),
            None,
            "personal",
            "2026-10-01T09:00:00+00:00",
        ));
        let work = import(capture(
            "c-work",
            Some("Quarterly budget review"),
            None,
            "work",
            "2026-10-03T09:00:00+00:00",
        ));
        let deleted = import(capture(
            "c-deleted",
            Some("Secret deleted note"),
            None,
            "personal",
            "2026-10-04T09:00:00+00:00",
        ));
        let corrected = import(capture(
            "c-corrected",
            Some("teh plan"),
            None,
            "personal",
            "2026-10-05T09:00:00+00:00",
        ));
        let voice = import(capture(
            "c-voice",
            None,
            Some("staging/2026-10-06/clip.m4a"),
            "personal",
            "2026-10-06T09:00:00+00:00",
        ));
        // 11:00 at +03:00 is 08:00 UTC, which is before 09:00 UTC on the same day even though
        // its text sorts after it.
        let mixed_offset = import(capture(
            "c-mixed",
            Some("Mixed offset note"),
            None,
            "personal",
            "2026-10-07T11:00:00+03:00",
        ));
        let typed = import(capture(
            "c-typed",
            Some("Water the ferns"),
            None,
            "personal",
            "2026-10-07T09:00:00+00:00",
        ));

        database
            .conn()
            .execute(
                "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value,
                                          new_value, created_at)
                 VALUES ('correction-1', ?, 1, 'text', 'teh plan', 'the launch plan',
                         '2026-10-08T09:00:00Z')",
                [&corrected],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "UPDATE items SET item_type = 'note' WHERE item_id = ?",
                [&typed],
            )
            .unwrap();
        let deleted_revision: i32 = database
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [&deleted],
                |row| row.get(0),
            )
            .unwrap();
        mark_deletion_intent(
            &mut database,
            &deleted,
            deleted_revision,
            DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();
        drop(database);
        Fixture {
            _directory: directory,
            store_path,
            seeded: Seeded {
                oldest,
                typed,
                corrected,
                voice,
                work,
                deleted,
                mixed_offset,
            },
        }
    }
}

struct Session {
    handle: OhandCoreHandle,
    events: std::sync::mpsc::Receiver<Event>,
    _recorder: Box<crate::ffi::core_handle::tests::Recorder>,
}

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_recent_items(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

impl Session {
    fn over(path: &str) -> Session {
        let mut handle = 0;
        let opened = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(opened.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
        }
    }

    fn succeed(&self, request: &Value) -> Value {
        let outcome = start(self.handle, 7, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, 7);
        assert_ne!(event.thread, std::thread::current().id());
        assert_eq!(
            event.status,
            OHAND_CORE_STATUS_OK,
            "{:?}",
            String::from_utf8_lossy(&event.payload)
        );
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(body["operation_id"], 7);
        body
    }

    fn refuse(&self, request: &Value) {
        let outcome = start(self.handle, 8, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
        let text = String::from_utf8_lossy(&outcome.payload);
        assert!(!text.contains(CURSOR_SENTINEL), "failure echoed the cursor");
        assert!(
            self.events
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "a refused request must not queue work"
        );
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn item_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["item_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn lists_newest_capture_first_with_the_documented_fields() {
    let _guard = serial();
    let fixture = Fixture::new();
    let seeded = &fixture.seeded;
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({"read_scopes": ["personal", "work"]}));
    assert_eq!(body["next_cursor"], Value::Null);
    assert_eq!(
        item_ids(&body),
        vec![
            seeded.typed.clone(),
            seeded.mixed_offset.clone(),
            seeded.voice.clone(),
            seeded.corrected.clone(),
            seeded.work.clone(),
            seeded.oldest.clone(),
        ],
        "ordered by instant, not by text; the deleted item is absent"
    );

    let items = body["items"].as_array().unwrap();
    assert_eq!(
        items[0],
        json!({
            "item_id": seeded.typed,
            "revision": items[0]["revision"],
            "item_type": "note",
            "display_text": "Water the ferns",
            "captured_at": "2026-10-07T09:00:00+00:00",
            "source_kind": "text",
        })
    );
    assert!(items[0]["revision"].is_i64());
    assert_eq!(items[1]["captured_at"], "2026-10-07T11:00:00+03:00");
    session.close();
}

#[test]
fn unclassified_voice_and_corrected_items_present_their_current_text() {
    let _guard = serial();
    let fixture = Fixture::new();
    let seeded = &fixture.seeded;
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({"read_scopes": ["personal"]}));
    let by_id = |item_id: &str| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["item_id"] == item_id)
            .unwrap()
            .clone()
    };

    let unclassified = by_id(&seeded.oldest);
    assert_eq!(unclassified["item_type"], Value::Null);
    assert_eq!(unclassified["display_text"], "Buy oat milk");

    let voice = by_id(&seeded.voice);
    assert_eq!(voice["source_kind"], "voice");
    assert_eq!(voice["display_text"], Value::Null, "no transcript yet");

    let corrected = by_id(&seeded.corrected);
    assert_eq!(corrected["display_text"], "the launch plan");
    assert_eq!(corrected["source_kind"], "text");
    session.close();
}

#[test]
fn a_work_only_read_never_returns_personal_items_and_the_reverse() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let work = session.succeed(&json!({"read_scopes": ["work"]}));
    assert_eq!(item_ids(&work), vec![fixture.seeded.work.clone()]);
    let personal = session.succeed(&json!({"read_scopes": ["personal"]}));
    assert!(!item_ids(&personal).contains(&fixture.seeded.work));
    assert_eq!(item_ids(&personal).len(), 5);
    session.close();
}

#[test]
fn deleted_items_never_appear() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({"read_scopes": ["personal", "work"], "limit": 100}));
    assert!(!item_ids(&body).contains(&fixture.seeded.deleted));
    assert!(!body.to_string().contains("Secret deleted note"));
    session.close();
}

#[test]
fn pages_follow_the_cursor_without_gaps_or_repeats() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let everything = item_ids(&session.succeed(&json!({"read_scopes": ["personal", "work"]})));

    let mut collected: Vec<String> = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let mut request = json!({"read_scopes": ["personal", "work"], "limit": 4});
        if !cursor.is_null() {
            request["cursor"] = cursor.clone();
        }
        let body = session.succeed(&request);
        pages += 1;
        let ids = item_ids(&body);
        assert!(ids.len() <= 4);
        collected.extend(ids);
        cursor = body["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert_eq!(body["items"].as_array().unwrap().len(), 4);
    }
    assert_eq!(pages, 2);
    assert_eq!(collected, everything);
    session.close();
}

#[test]
fn a_page_that_ends_exactly_at_the_last_item_has_no_cursor() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({"read_scopes": ["personal", "work"], "limit": 6}));
    assert_eq!(body["items"].as_array().unwrap().len(), 6);
    assert_eq!(body["next_cursor"], Value::Null);
    session.close();
}

#[test]
fn a_capture_saved_between_pages_does_not_shift_the_next_page() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let first = session.succeed(&json!({"read_scopes": ["personal", "work"], "limit": 2}));
    let cursor = first["next_cursor"].clone();
    assert!(cursor.is_string());

    {
        let mut database = Database::open(&fixture.store_path, Arc::new(SystemClock)).unwrap();
        import_foreground_ingress(
            &mut database,
            &capture(
                "c-new",
                Some("Brand new"),
                None,
                "personal",
                "2026-10-10T09:00:00+00:00",
            ),
        )
        .unwrap();
    }

    let second = session.succeed(&json!({
        "read_scopes": ["personal", "work"], "limit": 2, "cursor": cursor
    }));
    let seeded = &fixture.seeded;
    assert_eq!(
        item_ids(&second),
        vec![seeded.voice.clone(), seeded.corrected.clone()]
    );
    session.close();
}

#[test]
fn the_default_limit_is_thirty_and_the_maximum_is_one_hundred() {
    let _guard = serial();
    let fixture = Fixture::new();
    {
        let mut database = Database::open(&fixture.store_path, Arc::new(SystemClock)).unwrap();
        for index in 0..110 {
            import_foreground_ingress(
                &mut database,
                &capture(
                    &format!("c-bulk-{index}"),
                    Some("bulk"),
                    None,
                    "personal",
                    &format!("2026-11-01T00:{:02}:{:02}+00:00", index / 60, index % 60),
                ),
            )
            .unwrap();
        }
    }
    let session = Session::over(&fixture.store_path);
    let default = session.succeed(&json!({"read_scopes": ["personal"]}));
    assert_eq!(default["items"].as_array().unwrap().len(), 30);
    assert!(default["next_cursor"].is_string());
    let maximum = session.succeed(&json!({"read_scopes": ["personal"], "limit": 100}));
    assert_eq!(maximum["items"].as_array().unwrap().len(), 100);
    session.close();
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    session.refuse(&json!({}));
    session.refuse(&json!({"read_scopes": []}));
    session.refuse(&json!({"read_scopes": ["shared"]}));
    session.refuse(&json!({"read_scopes": ["personal"], "limit": 0}));
    session.refuse(&json!({"read_scopes": ["personal"], "limit": 101}));
    session.refuse(&json!({"read_scopes": ["personal"], "cursor": CURSOR_SENTINEL}));
    session.refuse(&json!({"read_scopes": ["personal"], "cursor": ""}));
    session.refuse(&json!({"read_scopes": ["personal"], "cursor": "|"}));
    session.refuse(&json!({"read_scopes": ["personal"], "query": "milk"}));
    session.close();
}

#[test]
fn sub_millisecond_captures_order_and_page_at_full_precision() {
    let _guard = serial();
    let fixture = Fixture::new();
    let (older, newer) = {
        let mut database = Database::open(&fixture.store_path, Arc::new(SystemClock)).unwrap();
        let older = import_foreground_ingress(
            &mut database,
            &capture(
                "c-micro-1",
                Some("Older microsecond"),
                None,
                "personal",
                "2026-11-01T09:00:00.000001Z",
            ),
        )
        .unwrap()
        .item_id;
        let newer = import_foreground_ingress(
            &mut database,
            &capture(
                "c-micro-2",
                Some("Newer microsecond"),
                None,
                "personal",
                "2026-11-01T09:00:00.000002Z",
            ),
        )
        .unwrap()
        .item_id;
        (older, newer)
    };
    let session = Session::over(&fixture.store_path);
    let first = session.succeed(&json!({"read_scopes": ["personal", "work"], "limit": 1}));
    assert_eq!(item_ids(&first), vec![newer.clone()]);
    let second = session.succeed(&json!({
        "read_scopes": ["personal", "work"], "limit": 1, "cursor": first["next_cursor"]
    }));
    assert_eq!(item_ids(&second), vec![older]);
    session.close();
}
