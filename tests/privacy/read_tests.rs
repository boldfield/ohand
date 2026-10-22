//! Local authenticated reads (T04b, criterion 1): queries, counts, snippets and private source
//! detail through the real exported functions over a mixed personal and work store. The scopes a
//! caller passes are the session's authorization; content outside them must not appear in any
//! hit, total, error or log-bound payload.

use crate::core_boundary::{
    MixedStore, OpenCore, Outcome, PERSONAL_CANARY, PERSONAL_CORRECTED_CANARY,
    PERSONAL_ORIGINAL_CANARY, PERSONAL_TOPIC_CANARY, WORK_CANARY,
};
use chrono::{TimeZone, Utc};
use ohand_core::ffi::item_detail::ohand_core_start_item_detail;
use ohand_core::ffi::recent_items::ohand_core_start_recent_items;
use ohand_core::ffi::retrieval::ohand_core_start_retrieval;
use ohand_core::lifecycle::delete_intent::mark_deletion_intent;
use ohand_core::retrieval::query::{
    scoped_list, scoped_query, scoped_query_direct, QueryFilter, QueryPagination,
};
use ohand_core::store::events::ItemScope;
use serde_json::{json, Value};

const ALL_CANARIES: [&str; 5] = [
    PERSONAL_CANARY,
    WORK_CANARY,
    PERSONAL_ORIGINAL_CANARY,
    PERSONAL_CORRECTED_CANARY,
    PERSONAL_TOPIC_CANARY,
];

fn retrieve(core: &mut OpenCore, request: Value) -> Outcome {
    core.run(ohand_core_start_retrieval, &request)
}

fn detail(core: &mut OpenCore, item_id: &str, scopes: &[&str]) -> Outcome {
    core.run(
        ohand_core_start_item_detail,
        &json!({ "item_id": item_id, "read_scopes": scopes }),
    )
}

fn search(term: &str, scopes: &[&str]) -> Value {
    json!({ "op": "search", "query": term, "read_scopes": scopes })
}

fn list(scopes: &[&str]) -> Value {
    json!({ "op": "list", "read_scopes": scopes })
}

fn assert_no_content_beyond(outcome: &Outcome, allowed: &[&str]) {
    let text = outcome.text();
    for canary in ALL_CANARIES {
        if !allowed.contains(&canary) {
            assert!(!text.contains(canary), "{canary} leaked into {text}");
        }
    }
}

#[test]
fn a_work_only_search_never_finds_personal_text_in_hits_snippets_or_counts() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);

    for term in [
        PERSONAL_CANARY,
        PERSONAL_ORIGINAL_CANARY,
        PERSONAL_CORRECTED_CANARY,
    ] {
        let outcome = retrieve(&mut core, search(term, &["work"]));
        assert!(outcome.is_ok(), "{}", outcome.text());
        let body = outcome.json();
        assert_eq!(body["total_accessible"], 0, "{term}");
        assert_eq!(body["hits"], json!([]), "{term}");
        assert_no_content_beyond(&outcome, &[]);
    }

    let own = retrieve(&mut core, search(WORK_CANARY, &["work"]));
    assert_eq!(own.json()["total_accessible"], 1);
    assert_no_content_beyond(&own, &[WORK_CANARY]);
}

#[test]
fn a_personal_only_search_never_finds_work_text_and_a_listing_counts_only_its_scope() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);

    let search_outcome = retrieve(&mut core, search(WORK_CANARY, &["personal"]));
    assert_eq!(search_outcome.json()["total_accessible"], 0);
    assert_no_content_beyond(&search_outcome, &[]);

    let personal_list = retrieve(&mut core, list(&["personal"]));
    assert_eq!(personal_list.json()["total_accessible"], 2);
    assert_no_content_beyond(
        &personal_list,
        &[
            PERSONAL_CANARY,
            PERSONAL_CORRECTED_CANARY,
            PERSONAL_ORIGINAL_CANARY,
        ],
    );

    let work_list = retrieve(&mut core, list(&["work"]));
    assert_eq!(work_list.json()["total_accessible"], 1);
    assert_no_content_beyond(&work_list, &[WORK_CANARY]);
}

#[test]
fn a_session_topic_filter_cannot_reach_across_scopes() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);

    let mut request = list(&["work"]);
    request["session_topics"] = json!([PERSONAL_TOPIC_CANARY]);
    let outcome = retrieve(&mut core, request);
    assert!(outcome.is_ok(), "{}", outcome.text());
    assert_eq!(outcome.json()["total_accessible"], 0);
    assert_no_content_beyond(&outcome, &[]);
}

#[test]
fn a_query_without_an_authorized_scope_is_refused_without_echoing_anything() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);

    // A locked session passes no scopes: there is no default that could read history.
    for request in [
        search(PERSONAL_CANARY, &[]),
        list(&[]),
        search(PERSONAL_CANARY, &["everything"]),
    ] {
        let outcome = retrieve(&mut core, request);
        assert!(!outcome.is_ok());
        assert_eq!(outcome.code(), "invalid_request");
        assert_no_content_beyond(&outcome, &[]);
    }
}

#[test]
fn the_library_queries_enforce_the_same_scope_before_search_and_counting() {
    let store = MixedStore::new();
    let database = store.database();
    let work_only = QueryFilter {
        read_scopes: vec![ItemScope::Work],
        ..Default::default()
    };
    let everything = QueryPagination::default();

    for term in [PERSONAL_CANARY, PERSONAL_ORIGINAL_CANARY] {
        let indexed = scoped_query(database.conn(), term, &work_only, &everything).unwrap();
        let direct = scoped_query_direct(database.conn(), term, &work_only, &everything).unwrap();
        for result in [indexed, direct] {
            assert!(result.hits.is_empty(), "{term}");
            assert_eq!(result.total_accessible, 0, "{term}");
        }
    }
    let listing = scoped_list(database.conn(), &work_only, &everything).unwrap();
    assert_eq!(listing.total_accessible, 1);
    assert!(listing
        .hits
        .iter()
        .all(|hit| !format!("{hit:?}").contains(PERSONAL_CANARY)));
}

#[test]
fn private_source_detail_is_not_found_outside_the_authorized_scope() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);
    let personal = &store.seeded.personal_item;

    let outside = detail(&mut core, personal, &["work"]);
    let unknown = detail(&mut core, "no-such-item", &["work"]);
    assert!(!outside.is_ok());
    assert_eq!(outside.code(), "not_found");
    assert_eq!(
        outside.payload, unknown.payload,
        "an out-of-scope item must look like an unknown one"
    );
    assert_no_content_beyond(&outside, &[]);
    assert!(!outside.text().contains(personal.as_str()));

    let inside = detail(&mut core, personal, &["personal"]);
    assert!(inside.is_ok(), "{}", inside.text());
    assert!(inside.text().contains(PERSONAL_CANARY));

    // The same holds the other way round: a personal-only session cannot open a work item.
    let work = &store.seeded.work_item;
    let reverse = detail(&mut core, work, &["personal"]);
    assert_eq!(reverse.code(), "not_found");
    assert_no_content_beyond(&reverse, &[]);
    let own = detail(&mut core, work, &["work"]);
    assert!(own.text().contains(WORK_CANARY));

    // The replaced original of a corrected item is private source detail too.
    let corrected = &store.seeded.corrected_item;
    let hidden = detail(&mut core, corrected, &["work"]);
    assert_eq!(hidden.code(), "not_found");
    assert_no_content_beyond(&hidden, &[]);
}

#[test]
fn a_deleted_item_disappears_from_search_listing_and_detail() {
    let store = MixedStore::new();
    let personal = store.seeded.personal_item.clone();
    let mut database = store.database();
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [&personal],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_intent(
        &mut database,
        &personal,
        revision,
        Utc.with_ymd_and_hms(2026, 10, 9, 9, 0, 0).unwrap(),
    )
    .unwrap();

    let mut core = OpenCore::open(&store.store_path);
    let found = retrieve(&mut core, search(PERSONAL_CANARY, &["personal", "work"]));
    assert_eq!(found.json()["total_accessible"], 0);
    assert_no_content_beyond(&found, &[]);

    let listing = retrieve(&mut core, list(&["personal", "work"]));
    assert_eq!(listing.json()["total_accessible"], 2);
    assert!(!listing.text().contains(&personal));

    let after = detail(&mut core, &personal, &["personal"]);
    assert_eq!(after.code(), "not_found");
    assert_no_content_beyond(&after, &[]);

    // The erased content is gone from durable state, not merely hidden from queries.
    assert!(
        !store.dump_state().contains(PERSONAL_CANARY),
        "deleted text remains readable in the store"
    );
}

fn recent(core: &mut OpenCore, scopes: &[&str]) -> Outcome {
    core.run(
        ohand_core_start_recent_items,
        &json!({ "read_scopes": scopes }),
    )
}

#[test]
fn recent_items_show_only_authorized_scopes_current_text_and_no_deleted_items() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);

    let work = recent(&mut core, &["work"]);
    assert!(work.is_ok(), "{}", work.text());
    assert_eq!(work.json()["items"].as_array().unwrap().len(), 1);
    assert_no_content_beyond(&work, &[WORK_CANARY]);

    // A corrected item shows its current text, never the text it replaced.
    let personal = recent(&mut core, &["personal"]);
    assert_eq!(personal.json()["items"].as_array().unwrap().len(), 2);
    assert_no_content_beyond(&personal, &[PERSONAL_CANARY, PERSONAL_CORRECTED_CANARY]);
    assert!(!personal.text().contains(WORK_CANARY));

    let mut database = store.database();
    let personal_item = store.seeded.personal_item.clone();
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [&personal_item],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_intent(
        &mut database,
        &personal_item,
        revision,
        Utc.with_ymd_and_hms(2026, 10, 9, 9, 0, 0).unwrap(),
    )
    .unwrap();
    let after = recent(&mut core, &["personal", "work"]);
    assert_eq!(after.json()["items"].as_array().unwrap().len(), 2);
    assert!(!after.text().contains(PERSONAL_CANARY));
    assert!(!after.text().contains(&personal_item));
}

#[test]
fn a_relocked_session_reads_nothing_through_any_read_path() {
    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);
    let personal = store.seeded.personal_item.clone();

    // While authenticated, native passes the scopes and reads succeed.
    assert!(retrieve(&mut core, list(&["personal", "work"])).is_ok());
    assert!(detail(&mut core, &personal, &["personal"]).is_ok());
    assert!(recent(&mut core, &["personal", "work"]).is_ok());

    // Relocking returns to capture-only: native has no scope to pass, so nothing may come back,
    // and the refusals are the same as a session that was never unlocked.
    let fresh_store = MixedStore::new();
    let mut fresh = OpenCore::open(&fresh_store.store_path);
    let relocked_reads: [(&str, Outcome, Outcome); 4] = [
        (
            "search",
            retrieve(&mut core, search(PERSONAL_CANARY, &[])),
            retrieve(&mut fresh, search(PERSONAL_CANARY, &[])),
        ),
        (
            "list",
            retrieve(&mut core, list(&[])),
            retrieve(&mut fresh, list(&[])),
        ),
        (
            "detail",
            detail(&mut core, &personal, &[]),
            detail(&mut fresh, &personal, &[]),
        ),
        ("recent", recent(&mut core, &[]), recent(&mut fresh, &[])),
    ];
    for (path, relocked, never_unlocked) in relocked_reads {
        assert!(!relocked.is_ok(), "{path} answered a relocked session");
        assert_eq!(relocked.code(), "invalid_request", "{path}");
        assert_eq!(relocked.payload, never_unlocked.payload, "{path}");
        assert_no_content_beyond(&relocked, &[]);
        assert!(!relocked.text().contains(personal.as_str()), "{path}");
    }
}

#[test]
fn log_bound_failures_name_no_stored_content() {
    // The read, export and notification paths emit no logs of their own (no tracing, println or
    // eprintln in the core library); the log-bound surfaces are the Debug and Display of their
    // errors and the normalized ABI failures. Assert both stay content-free, and that none of
    // the library sources acquired a logging call that would need its own redaction test.
    use ohand_core::export::ExportError;
    use ohand_core::ffi::core_handle::failure::AbiFailure;

    let store = MixedStore::new();
    let mut core = OpenCore::open(&store.store_path);
    let rendered = [
        format!("{:?}", ExportError::NotAuthenticated),
        ExportError::NoScopeSelected.to_string(),
        ExportError::Cancelled.to_string(),
        format!("{:?}", AbiFailure::INVALID_REQUEST),
        retrieve(&mut core, search(PERSONAL_CANARY, &[])).text(),
        detail(&mut core, "no-such-item", &["work"]).text(),
        recent(&mut core, &[]).text(),
    ];
    for text in &rendered {
        for canary in ALL_CANARIES {
            assert!(!text.contains(canary), "{canary} in {text}");
        }
    }

    let sources = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![sources];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs")
                || name.contains("test")
                || path.to_string_lossy().contains("/tests/")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for logging in [
                "println!(",
                "eprintln!(",
                "tracing::",
                "log::info",
                "log::debug",
                "dbg!(",
            ] {
                assert!(
                    !source.contains(logging),
                    "{} gained a logging call ({logging}); add a redaction test for it",
                    path.display()
                );
            }
        }
    }
}
