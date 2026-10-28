//! Drives scoped retrieval the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, Event, Outcome, WAIT};
use crate::ffi::item_lifecycle_controls::{
    apply_item_lifecycle_control, ItemLifecycleControl, LifecycleControl, LifecycleControlOutcome,
};
use crate::ingress::import::import_foreground_ingress;
use crate::retrieval::index::sync_item_in_tx;
use crate::store::captures::Capture;
use crate::store::schema::SystemClock;
use std::sync::Arc;
use tempfile::TempDir;

/// Ends an item through the same path the lifecycle control export uses.
fn end_item(store_path: &str, item_id: &str, control: LifecycleControl) {
    let mut database = Database::open(store_path, Arc::new(SystemClock)).unwrap();
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )
        .unwrap();
    let outcome = apply_item_lifecycle_control(
        &mut database,
        &ItemLifecycleControl {
            command_id: format!("end-{item_id}"),
            item_id: item_id.to_string(),
            expected_revision: revision,
            control,
        },
    )
    .unwrap();
    assert!(matches!(outcome, LifecycleControlOutcome::Applied { .. }));
}

const WORK_SECRET: &str = "zebrafinch";
const QUERY_SENTINEL: &str = "sentinelquery";

struct Seeded {
    personal_groceries: String,
    personal_dentist: String,
    work_budget: String,
    corrected_plan: String,
    untopiced_note: String,
}

fn capture(
    capture_id: &str,
    text: &str,
    scope: &str,
    capture_instant: &str,
    session_topic: Option<&str>,
) -> Capture {
    Capture::new(
        capture_id.to_string(),
        Some(text.to_string()),
        None,
        capture_instant.to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        format!("route-{scope}"),
        false,
        capture_instant.to_string(),
        session_topic.map(str::to_string),
    )
    .unwrap()
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
        let mut import = |capture: &Capture| {
            import_foreground_ingress(&mut database, capture)
                .unwrap()
                .item_id
        };
        let personal_groceries = import(&capture(
            "groceries-1",
            "Buy oat milk and apples",
            "personal",
            "2026-10-01T09:00:00+00:00",
            Some("Groceries"),
        ));
        let personal_dentist = import(&capture(
            "dentist-1",
            "Call the dentist about the milk allergy",
            "personal",
            "2026-10-05T09:00:00+00:00",
            Some("  GROCERIES "),
        ));
        let work_budget = import(&capture(
            "budget-1",
            &format!("Quarterly milk budget review {WORK_SECRET}"),
            "work",
            "2026-10-03T09:00:00+00:00",
            Some("Planning"),
        ));
        let corrected_plan = import(&capture(
            "plan-1",
            "teh plan",
            "personal",
            "2026-10-07T09:00:00+00:00",
            None,
        ));
        let untopiced_note = import(&capture(
            "note-1",
            "Water the ferns",
            "personal",
            "2026-10-09T09:00:00+00:00",
            None,
        ));
        {
            let transaction = database.immediate_transaction().unwrap();
            transaction
                .execute(
                    "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value,
                                              new_value, created_at)
                     VALUES ('correction-1', ?, 1, 'text', 'teh plan', 'the launch plan',
                             '2026-10-08T09:00:00Z')",
                    [&corrected_plan],
                )
                .unwrap();
            sync_item_in_tx(&transaction, &corrected_plan).unwrap();
            transaction
                .execute(
                    "UPDATE items SET item_type = 'note' WHERE item_id IN (?, ?)",
                    [&personal_groceries, &untopiced_note],
                )
                .unwrap();
            transaction
                .execute(
                    "UPDATE items SET item_type = 'action' WHERE item_id = ?",
                    [&personal_dentist],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
        drop(database);
        Fixture {
            _directory: directory,
            store_path,
            seeded: Seeded {
                personal_groceries,
                personal_dentist,
                work_budget,
                corrected_plan,
                untopiced_note,
            },
        }
    }
}

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_retrieval(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

struct Session {
    handle: OhandCoreHandle,
    events: std::sync::mpsc::Receiver<Event>,
    _recorder: Box<crate::ffi::core_handle::tests::Recorder>,
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
        assert!(!text.contains(QUERY_SENTINEL), "failure echoed the query");
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
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["item_id"].as_str().unwrap().to_string())
        .collect()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

#[test]
fn text_search_returns_the_documented_fields_and_the_accessible_total() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({
        "op": "search", "query": "OAT mil", "read_scopes": ["personal"]
    }));
    assert_eq!(body["total_accessible"], 1);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit["item_id"], fixture.seeded.personal_groceries);
    assert_eq!(hit["capture_id"], "groceries-1");
    assert_eq!(hit["scope"], "personal");
    assert_eq!(hit["item_type"], "note");
    assert_eq!(hit["text"], "Buy oat milk and apples");
    assert_eq!(hit["text_basis"], "original");
    assert_eq!(hit["original_text"], Value::Null);
    assert_eq!(hit["matched"], "current");
    assert_eq!(hit["match_span"], json!({"start": 4, "end": 7}));
    assert_eq!(hit["captured_at"], "2026-10-01T09:00:00+00:00");
    session.close();
}

#[test]
fn a_corrected_item_shows_current_text_and_keeps_the_original_where_corrected() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);

    let by_current = session.succeed(&json!({
        "op": "search", "query": "launch", "read_scopes": ["personal"]
    }));
    let hit = &by_current["hits"][0];
    assert_eq!(hit["item_id"], fixture.seeded.corrected_plan);
    assert_eq!(hit["text"], "the launch plan");
    assert_eq!(hit["text_basis"], "corrected");
    assert_eq!(hit["original_text"], "teh plan");
    assert_eq!(hit["matched"], "current");
    assert_eq!(hit["match_span"], json!({"start": 4, "end": 10}));

    let by_original = session.succeed(&json!({
        "op": "search", "query": "teh", "read_scopes": ["personal"]
    }));
    let hit = &by_original["hits"][0];
    assert_eq!(
        hit["text"], "the launch plan",
        "the original is never presented as current"
    );
    assert_eq!(hit["matched"], "superseded_original");
    assert_eq!(hit["match_span"], Value::Null);
    session.close();
}

#[test]
fn list_returns_every_accessible_item_newest_capture_first_without_spans() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({"op": "list", "read_scopes": ["personal", "work"]}));
    assert_eq!(body["total_accessible"], 5);
    assert_eq!(
        item_ids(&body),
        vec![
            fixture.seeded.untopiced_note.clone(),
            fixture.seeded.corrected_plan.clone(),
            fixture.seeded.personal_dentist.clone(),
            fixture.seeded.work_budget.clone(),
            fixture.seeded.personal_groceries.clone(),
        ]
    );
    assert!(body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["match_span"].is_null()));
    session.close();
}

#[test]
fn punctuation_only_search_matches_nothing() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let body = session.succeed(&json!({
        "op": "search", "query": "?!", "read_scopes": ["personal", "work"]
    }));
    assert_eq!(body["total_accessible"], 0);
    assert_eq!(body["hits"], json!([]));
    session.close();
}

#[test]
fn date_bounds_are_inclusive_and_apply_to_search_and_list() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let window = session.succeed(&json!({
        "op": "search", "query": "milk", "read_scopes": both,
        "captured_after": "2026-10-03T09:00:00Z", "captured_before": "2026-10-05T09:00:00Z"
    }));
    assert_eq!(
        sorted(item_ids(&window)),
        sorted(vec![
            fixture.seeded.work_budget.clone(),
            fixture.seeded.personal_dentist.clone()
        ])
    );

    let after_only = session.succeed(&json!({
        "op": "list", "read_scopes": both, "captured_after": "2026-10-07T09:00:00+00:00"
    }));
    assert_eq!(after_only["total_accessible"], 2);

    let before_only = session.succeed(&json!({
        "op": "list", "read_scopes": both, "captured_before": "2026-10-03T08:59:59Z"
    }));
    assert_eq!(
        item_ids(&before_only),
        vec![fixture.seeded.personal_groceries.clone()]
    );

    let empty_window = session.succeed(&json!({
        "op": "list", "read_scopes": both,
        "captured_after": "2026-10-10T00:00:00Z", "captured_before": "2026-10-11T00:00:00Z"
    }));
    assert_eq!(empty_window["total_accessible"], 0);
    session.close();
}

#[test]
fn session_topics_match_case_and_whitespace_insensitively() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let groceries = session.succeed(&json!({
        "op": "list", "read_scopes": both, "session_topics": ["groceries"]
    }));
    assert_eq!(
        sorted(item_ids(&groceries)),
        sorted(vec![
            fixture.seeded.personal_groceries.clone(),
            fixture.seeded.personal_dentist.clone()
        ])
    );

    let planning_search = session.succeed(&json!({
        "op": "search", "query": "milk", "read_scopes": both, "session_topics": ["PLANNING"]
    }));
    assert_eq!(
        item_ids(&planning_search),
        vec![fixture.seeded.work_budget.clone()]
    );

    let none_set = session.succeed(&json!({
        "op": "list", "read_scopes": both,
        "session_topics": ["planning"], "include_no_session_topic": true
    }));
    assert_eq!(
        sorted(item_ids(&none_set)),
        sorted(vec![
            fixture.seeded.work_budget.clone(),
            fixture.seeded.corrected_plan.clone(),
            fixture.seeded.untopiced_note.clone()
        ])
    );

    let any_topic = session.succeed(&json!({
        "op": "list", "read_scopes": both, "require_session_topic": true
    }));
    assert_eq!(any_topic["total_accessible"], 3);
    session.close();
}

#[test]
fn item_type_lifecycle_and_route_filters_apply() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let actions = session.succeed(&json!({
        "op": "list", "read_scopes": both, "item_types": ["action"]
    }));
    assert_eq!(
        item_ids(&actions),
        vec![fixture.seeded.personal_dentist.clone()]
    );

    let completed = session.succeed(&json!({
        "op": "list", "read_scopes": both, "lifecycle_states": ["completed"]
    }));
    assert_eq!(completed["total_accessible"], 0);

    let everything = session.succeed(&json!({"op": "list", "read_scopes": both}));
    assert!(!everything["hits"].as_array().unwrap().is_empty());
    for hit in everything["hits"].as_array().unwrap() {
        assert_eq!(hit["lifecycle_state"], "active");
    }

    let work_route = session.succeed(&json!({
        "op": "list", "read_scopes": both, "route_ids": ["route-work"]
    }));
    assert_eq!(
        item_ids(&work_route),
        vec![fixture.seeded.work_budget.clone()]
    );
    session.close();
}

fn hit_for<'a>(body: &'a Value, item_id: &str) -> &'a Value {
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["item_id"] == item_id)
        .expect("the item is among the hits")
}

#[test]
fn a_hit_carries_its_item_type_or_null_when_unclassified() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let searched = session.succeed(&json!({
        "op": "search", "query": "milk", "read_scopes": both
    }));
    assert_eq!(
        hit_for(&searched, &fixture.seeded.personal_groceries)["item_type"],
        "note"
    );
    assert_eq!(
        hit_for(&searched, &fixture.seeded.personal_dentist)["item_type"],
        "action"
    );
    assert_eq!(
        hit_for(&searched, &fixture.seeded.work_budget)["item_type"],
        Value::Null,
        "an unclassified item reports no type rather than a guess"
    );

    let listed = session.succeed(&json!({"op": "list", "read_scopes": both}));
    assert_eq!(
        hit_for(&listed, &fixture.seeded.untopiced_note)["item_type"],
        "note"
    );
    assert_eq!(
        hit_for(&listed, &fixture.seeded.corrected_plan)["item_type"],
        Value::Null
    );

    let personal_only = session.succeed(&json!({
        "op": "search", "query": "milk", "read_scopes": ["personal"]
    }));
    assert!(!personal_only.to_string().contains("budget"));
    assert!(!item_ids(&personal_only).contains(&fixture.seeded.work_budget));
    session.close();
}

#[test]
fn a_type_correction_is_reflected_on_the_next_search_and_list() {
    use crate::ffi::item_corrections::{apply_item_correction, CorrectionChange, ItemCorrection};
    use crate::store::events::ItemType;

    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let request = json!({"op": "search", "query": "oat", "read_scopes": ["personal"]});
    let before = session.succeed(&request);
    assert_eq!(before["hits"][0]["item_type"], "note");

    {
        let mut database = Database::open(&fixture.store_path, Arc::new(SystemClock)).unwrap();
        let revision: i32 = database
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [&fixture.seeded.personal_groceries],
                |row| row.get(0),
            )
            .unwrap();
        apply_item_correction(
            &mut database,
            &ItemCorrection {
                command_id: "type-correction-1".to_string(),
                item_id: fixture.seeded.personal_groceries.clone(),
                expected_revision: revision,
                change: CorrectionChange::Type(ItemType::Idea),
            },
        )
        .unwrap();
    }

    let after = session.succeed(&request);
    assert_eq!(after["hits"][0]["item_type"], "idea");
    let listed = session.succeed(&json!({"op": "list", "read_scopes": ["personal"]}));
    assert_eq!(
        hit_for(&listed, &fixture.seeded.personal_groceries)["item_type"],
        "idea"
    );
    session.close();
}

#[test]
fn an_excluded_scope_contributes_no_hit_count_or_text() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);

    let personal_only = session.succeed(&json!({
        "op": "search", "query": "milk", "read_scopes": ["personal"]
    }));
    assert_eq!(personal_only["total_accessible"], 2);
    let ids = item_ids(&personal_only);
    assert!(!ids.contains(&fixture.seeded.work_budget));
    assert!(!personal_only.to_string().contains(WORK_SECRET));

    let work_term_in_personal = session.succeed(&json!({
        "op": "search", "query": WORK_SECRET, "read_scopes": ["personal"]
    }));
    assert_eq!(work_term_in_personal["total_accessible"], 0);
    assert_eq!(work_term_in_personal["hits"], json!([]));

    let listed = session.succeed(&json!({"op": "list", "read_scopes": ["personal"]}));
    assert_eq!(listed["total_accessible"], 4);
    assert!(!listed.to_string().contains(WORK_SECRET));

    let work_only = session.succeed(&json!({
        "op": "list", "read_scopes": ["work", "work"]
    }));
    assert_eq!(
        item_ids(&work_only),
        vec![fixture.seeded.work_budget.clone()]
    );
    assert_eq!(work_only["hits"][0]["scope"], "work");
    session.close();
}

#[test]
fn pagination_pages_a_stable_order_and_reports_the_unpaged_total() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let all = item_ids(&session.succeed(&json!({"op": "list", "read_scopes": both})));
    let mut paged = Vec::new();
    for offset in [0, 2, 4] {
        let page = session.succeed(&json!({
            "op": "list", "read_scopes": both, "limit": 2, "offset": offset
        }));
        assert_eq!(page["total_accessible"], 5);
        assert!(page["hits"].as_array().unwrap().len() <= 2);
        paged.extend(item_ids(&page));
    }
    assert_eq!(paged, all);

    let past_the_end = session.succeed(&json!({
        "op": "list", "read_scopes": both, "limit": 2, "offset": 50
    }));
    assert_eq!(past_the_end["total_accessible"], 5);
    assert_eq!(past_the_end["hits"], json!([]));

    let at_the_maximum = session.succeed(&json!({
        "op": "list", "read_scopes": both, "limit": OHAND_CORE_MAX_RETRIEVAL_LIMIT
    }));
    assert_eq!(at_the_maximum["hits"].as_array().unwrap().len(), 5);
    session.close();
}

#[test]
fn invalid_requests_are_refused_before_anything_is_queued_and_never_echo_the_query() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let scopes = json!(["personal"]);
    let oversized = "a".repeat(OHAND_CORE_MAX_RETRIEVAL_QUERY_BYTES + 1);
    let invalid = [
        json!({"op": "search", "query": QUERY_SENTINEL}),
        json!({"op": "search", "query": QUERY_SENTINEL, "read_scopes": []}),
        json!({"op": "list"}),
        json!({"op": "list", "read_scopes": ["private"]}),
        json!({"op": "search", "query": QUERY_SENTINEL, "read_scopes": ["Personal"]}),
        json!({"op": "search", "read_scopes": scopes}),
        json!({"op": "search", "query": "   ", "read_scopes": scopes}),
        json!({"op": "search", "query": oversized, "read_scopes": scopes}),
        json!({"op": "list", "query": QUERY_SENTINEL, "read_scopes": scopes}),
        json!({"op": "fetch", "read_scopes": scopes}),
        json!({"op": "list", "read_scopes": scopes, "unknown": QUERY_SENTINEL}),
        json!({"op": "list", "read_scopes": scopes, "captured_after": QUERY_SENTINEL}),
        json!({"op": "list", "read_scopes": scopes, "captured_before": "2026-10-03"}),
        json!({
            "op": "list", "read_scopes": scopes,
            "captured_after": "2026-10-05T00:00:00Z", "captured_before": "2026-10-04T00:00:00Z"
        }),
        json!({"op": "list", "read_scopes": scopes, "item_types": [QUERY_SENTINEL]}),
        json!({"op": "list", "read_scopes": scopes, "lifecycle_states": [QUERY_SENTINEL]}),
        json!({"op": "list", "read_scopes": scopes, "route_ids": [""]}),
        json!({"op": "list", "read_scopes": scopes, "session_topics": vec!["t"; 33]}),
        json!({"op": "list", "read_scopes": scopes, "limit": 0}),
        json!({"op": "list", "read_scopes": scopes, "limit": OHAND_CORE_MAX_RETRIEVAL_LIMIT + 1}),
        json!({"op": "list", "read_scopes": scopes, "offset": OHAND_CORE_MAX_RETRIEVAL_OFFSET + 1}),
        json!({"op": "list", "read_scopes": scopes, "limit": -1}),
        json!({"op": "list", "read_scopes": scopes, "include_no_session_topic": "yes"}),
    ];
    for request in &invalid {
        session.refuse(request);
    }

    for (bytes, expected) in [
        (&b""[..], "invalid_request"),
        (&b"not json"[..], "invalid_request"),
    ] {
        let outcome = consume(unsafe {
            ohand_core_start_retrieval(session.handle, 9, bytes.as_ptr(), bytes.len())
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT);
        assert_eq!(outcome.code(), expected);
    }
    let null_request =
        consume(unsafe { ohand_core_start_retrieval(session.handle, 9, std::ptr::null(), 4) });
    assert_eq!(null_request.code(), "null_argument");
    let too_large = consume(unsafe {
        ohand_core_start_retrieval(
            session.handle,
            9,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");
    let unknown_handle = consume(unsafe { ohand_core_start_retrieval(0, 9, b"{}".as_ptr(), 2) });
    assert_eq!(unknown_handle.code(), "invalid_handle");
    session.close();
}

#[test]
fn match_span_counts_unicode_scalars_and_ignores_case_diacritics_and_punctuation() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("span.sqlite").display().to_string();
    let database = Database::open(&path, Arc::new(SystemClock)).unwrap();
    let connection = database.conn();
    let span = |text: &str, query: &str| match_span(connection, text, query).unwrap();
    assert_eq!(
        span("Café, ÉCLAIR time", "éclair"),
        Some(json!({"start": 6, "end": 12}))
    );
    assert_eq!(
        span("Café, ÉCLAIR time", "cafe"),
        Some(json!({"start": 0, "end": 4}))
    );
    assert_eq!(
        span("Café, ÉCLAIR time", "ECLAIR"),
        Some(json!({"start": 6, "end": 12}))
    );
    assert_eq!(span("one two", "tw"), Some(json!({"start": 4, "end": 7})));
    assert_eq!(span("one two", "three"), None);
    assert_eq!(span("one two", "?!"), None);
    assert_eq!(span("", "one"), None);
    let leftovers: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM temp.sqlite_master WHERE name LIKE 'ohand_span_fold%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0, "the folding scratch tables are dropped");
}

#[test]
fn an_unaccented_query_gets_a_span_in_accented_current_text() {
    let _guard = serial();
    let directory = TempDir::new().unwrap();
    let store_path = directory.path().join("core.sqlite").display().to_string();
    {
        let mut database = Database::open(&store_path, Arc::new(SystemClock)).unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                     created_at)
                 VALUES ('route-personal', 'route-personal', 'personal', '[]',
                         '2026-10-01T00:00:00Z')",
                [],
            )
            .unwrap();
        import_foreground_ingress(
            &mut database,
            &capture(
                "cafe-1",
                "Meet at the Café, then ÉCLAIR time",
                "personal",
                "2026-10-01T09:00:00+00:00",
                None,
            ),
        )
        .unwrap();
    }
    let session = Session::over(&store_path);
    for (query, expected) in [
        ("cafe", json!({"start": 12, "end": 16})),
        ("CAFÉ", json!({"start": 12, "end": 16})),
        ("eclair", json!({"start": 23, "end": 29})),
    ] {
        let body = session.succeed(&json!({
            "op": "search", "query": query, "read_scopes": ["personal"]
        }));
        assert_eq!(body["total_accessible"], 1, "{query}");
        let hit = &body["hits"][0];
        assert_eq!(hit["matched"], "current", "{query}");
        assert_eq!(hit["match_span"], expected, "{query}");
    }
    session.close();
}

#[test]
fn closed_hits_carry_their_lifecycle_and_the_default_filter_still_returns_them() {
    let _guard = serial();
    let fixture = Fixture::new();
    end_item(
        &fixture.store_path,
        &fixture.seeded.personal_dentist,
        LifecycleControl::Complete,
    );
    end_item(
        &fixture.store_path,
        &fixture.seeded.personal_groceries,
        LifecycleControl::Cancel,
    );
    let session = Session::over(&fixture.store_path);
    let both = ["personal", "work"];

    let everything = session.succeed(&json!({"op": "list", "read_scopes": both}));
    let state_of = |item_id: &str| {
        everything["hits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["item_id"] == item_id)
            .map(|hit| hit["lifecycle_state"].as_str().unwrap().to_string())
    };
    assert_eq!(
        state_of(&fixture.seeded.personal_dentist).as_deref(),
        Some("completed")
    );
    assert_eq!(
        state_of(&fixture.seeded.personal_groceries).as_deref(),
        Some("cancelled")
    );
    assert_eq!(
        state_of(&fixture.seeded.work_budget).as_deref(),
        Some("active")
    );

    let open_only = session.succeed(&json!({
        "op": "list", "read_scopes": both, "lifecycle_states": ["active"]
    }));
    let open_ids = item_ids(&open_only);
    assert!(!open_ids.is_empty());
    assert!(!open_ids.contains(&fixture.seeded.personal_dentist));
    assert!(!open_ids.contains(&fixture.seeded.personal_groceries));
    session.close();
}
