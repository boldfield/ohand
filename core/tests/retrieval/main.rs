use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use ohand_core::retrieval::index::sync_item_in_tx;
use ohand_core::retrieval::query::{
    scoped_query, scoped_query_direct, QueryFilter, QueryPagination,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock, Database};

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_retrieval_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn new_db(label: &str) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    Database::open(&temp_db_path(label), clock)
}

#[allow(clippy::too_many_arguments)]
fn add_item(
    db: &mut Database,
    item_id: &str,
    text: Option<&str>,
    scope: &str,
    route_id: &str,
    item_type: Option<&str>,
    session_topic: Option<&str>,
    capture_instant: &str,
) -> Result<()> {
    let capture_id = format!("cap-{item_id}");
    let tx = db.immediate_transaction()?;
    let capture = Capture::new(
        capture_id.clone(),
        text.map(str::to_string),
        None,
        capture_instant.to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        route_id.to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        session_topic.map(str::to_string),
    )?;
    ohand_core::store::captures::save_capture_in_tx(&tx, &capture)?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                           save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, ?, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        rusqlite::params![
            item_id,
            &capture_id,
            item_type,
            "2026-01-15T10:30:00Z",
            "2026-01-15T10:30:00Z"
        ],
    )?;
    sync_item_in_tx(&tx, item_id)?;
    tx.commit()?;
    Ok(())
}

fn mark_completed(db: &mut Database, item_id: &str) -> Result<()> {
    let tx = db.immediate_transaction()?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'completed' WHERE item_id = ?",
        [item_id],
    )?;
    sync_item_in_tx(&tx, item_id)?;
    tx.commit()?;
    Ok(())
}

#[test]
fn personal_scope_does_not_leak_work_items() -> Result<()> {
    let mut db = new_db("scope_isolation")?;

    add_item(
        &mut db,
        "personal-secret",
        Some("personal banking info"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "work-secret",
        Some("confidential project details"),
        "work",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    // Personal-only filter should not return work items.
    let filter = QueryFilter::personal_only();
    let pagination = QueryPagination::default();
    let result = scoped_query(db.conn(), "banking", &filter, &pagination)?;

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "personal-secret");

    // Should return no results for work-specific terms when restricted to personal scope.
    let result = scoped_query(db.conn(), "confidential", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 0);

    Ok(())
}

#[test]
fn work_only_filter_excludes_personal_items() -> Result<()> {
    let mut db = new_db("work_only_filter")?;

    add_item(
        &mut db,
        "personal-1",
        Some("personal thought"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "work-1",
        Some("work thought"),
        "work",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let mut filter = QueryFilter::both_scopes();
    filter.read_scopes = vec![ohand_core::store::events::ItemScope::Work];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "thought", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "work-1");

    Ok(())
}

#[test]
fn stable_pagination_order_is_deterministic() -> Result<()> {
    let mut db = new_db("pagination_order")?;

    for i in (1..=10).rev() {
        add_item(
            &mut db,
            &format!("item-{i:02}"),
            Some("query match"),
            "personal",
            "route-1",
            Some("note"),
            None,
            "2026-01-15T10:30:00Z",
        )?;
    }

    let filter = QueryFilter::personal_only();

    let mut all_ids = Vec::new();
    let mut offset = 0;
    loop {
        let pagination = QueryPagination { limit: 3, offset };
        let result = scoped_query(db.conn(), "match", &filter, &pagination)?;
        if result.hits.is_empty() {
            break;
        }
        for hit in &result.hits {
            all_ids.push(hit.item_id.clone());
        }
        offset += 3;
    }

    // Check that results are stable across multiple queries.
    let pagination = QueryPagination {
        limit: 0,
        offset: 0,
    };
    let full_result = scoped_query(db.conn(), "match", &filter, &pagination)?;
    let full_ids: Vec<String> = full_result.hits.iter().map(|h| h.item_id.clone()).collect();

    assert_eq!(all_ids, full_ids);
    assert_eq!(full_ids.len(), 10);
    Ok(())
}

#[test]
fn honest_no_match_returns_zero_count() -> Result<()> {
    let mut db = new_db("no_match_honest")?;

    add_item(
        &mut db,
        "item-1",
        Some("apple banana cherry"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let filter = QueryFilter::personal_only();
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "nonexistent", &filter, &pagination)?;

    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.total_accessible, 0);

    Ok(())
}

#[test]
fn session_topic_and_scope_filters_combined() -> Result<()> {
    let mut db = new_db("session_and_scope")?;

    add_item(
        &mut db,
        "therapy-personal",
        Some("personal therapy note"),
        "personal",
        "local",
        Some("note"),
        Some("therapy"),
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "therapy-work",
        Some("work therapy note"),
        "work",
        "local",
        Some("note"),
        Some("therapy"),
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "personal-no-topic",
        Some("personal without topic"),
        "personal",
        "local",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    // Personal scope + therapy topic should return only therapy-personal.
    let mut filter = QueryFilter::personal_only();
    filter.session_topics = vec!["therapy".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "therapy-personal");

    Ok(())
}

#[test]
fn local_only_session_note_retrievable_without_provider() -> Result<()> {
    let mut db = new_db("local_session_retrieval")?;

    add_item(
        &mut db,
        "private-session",
        Some("bring this up in therapy"),
        "personal",
        "local",
        Some("note"),
        Some("therapy"),
        "2026-01-15T10:30:00Z",
    )?;

    // Query with local route and therapy topic filter.
    let mut filter = QueryFilter::personal_only();
    filter.route_ids = vec!["local".to_string()];
    filter.session_topics = vec!["therapy".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "private-session");
    assert_eq!(result.hits[0].current_text, "bring this up in therapy");

    Ok(())
}

#[test]
fn route_filter_prevents_cloud_results_in_local_query() -> Result<()> {
    let mut db = new_db("route_exclusion")?;

    add_item(
        &mut db,
        "local-item",
        Some("local file"),
        "personal",
        "local",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "cloud-item",
        Some("cloud file"),
        "personal",
        "cloud",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let mut filter = QueryFilter::personal_only();
    filter.route_ids = vec!["local".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "file", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "local-item");

    Ok(())
}

#[test]
fn date_range_filter_respects_capture_instant() -> Result<()> {
    let mut db = new_db("date_range")?;

    add_item(
        &mut db,
        "early",
        Some("early note"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-10T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "middle",
        Some("middle note"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "late",
        Some("late note"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-20T10:30:00Z",
    )?;

    let mut filter = QueryFilter::personal_only();
    filter.captured_after = Some("2026-01-12T00:00:00Z".to_string());
    filter.captured_before = Some("2026-01-18T23:59:59Z".to_string());
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "note", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);

    let ids: std::collections::HashSet<_> =
        result.hits.iter().map(|h| h.item_id.as_str()).collect();
    assert!(ids.contains("middle")); // 2026-01-15, within range
    assert!(!ids.contains("late")); // late is 2026-01-20, outside range
    assert!(!ids.contains("early")); // early is 2026-01-10, outside range

    Ok(())
}

#[test]
fn item_type_filter_distinguishes_actions_from_notes() -> Result<()> {
    let mut db = new_db("item_types")?;

    add_item(
        &mut db,
        "action-1",
        Some("call the roofer"),
        "personal",
        "route-1",
        Some("action"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "idea-1",
        Some("roof garden would be nice"),
        "personal",
        "route-1",
        Some("idea"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let mut filter = QueryFilter::personal_only();
    filter.item_types = vec!["action".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "roof", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "action-1");

    Ok(())
}

#[test]
fn lifecycle_state_filter_excludes_completed_items() -> Result<()> {
    let mut db = new_db("lifecycle_filter")?;

    add_item(
        &mut db,
        "active-1",
        Some("call roofer"),
        "personal",
        "route-1",
        Some("action"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "completed-1",
        Some("call roofer"),
        "personal",
        "route-1",
        Some("action"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    mark_completed(&mut db, "completed-1")?;

    let mut filter = QueryFilter::personal_only();
    filter.lifecycle_states = vec!["active".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "roofer", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "active-1");

    Ok(())
}

#[test]
fn direct_source_query_fallback_agrees_with_index() -> Result<()> {
    let mut db = new_db("direct_fallback")?;

    add_item(
        &mut db,
        "item-1",
        Some("search this content"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "item-2",
        Some("more content"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let filter = QueryFilter::personal_only();
    let pagination = QueryPagination::default();

    let indexed_result = scoped_query(db.conn(), "content", &filter, &pagination)?;
    let direct_result = scoped_query_direct(db.conn(), "content", &filter, &pagination)?;

    assert_eq!(indexed_result.hits.len(), direct_result.hits.len());
    let indexed_ids: std::collections::HashSet<_> = indexed_result
        .hits
        .iter()
        .map(|h| h.item_id.as_str())
        .collect();
    let direct_ids: std::collections::HashSet<_> = direct_result
        .hits
        .iter()
        .map(|h| h.item_id.as_str())
        .collect();
    assert_eq!(indexed_ids, direct_ids);

    Ok(())
}

#[test]
fn mixed_domain_records_filter_correctly() -> Result<()> {
    let mut db = new_db("mixed_domains")?;

    // Mixed personal and work, local and cloud routes, various item types and topics.
    add_item(
        &mut db,
        "personal-local-action",
        Some("call roofer"),
        "personal",
        "local",
        Some("action"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "personal-cloud-note",
        Some("cloud sync test"),
        "personal",
        "cloud",
        Some("note"),
        Some("testing"),
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "work-local-note",
        Some("work local"),
        "work",
        "local",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "work-cloud-action",
        Some("send report"),
        "work",
        "cloud",
        Some("action"),
        Some("project"),
        "2026-01-15T10:30:00Z",
    )?;

    // Query: personal scope, local route, action type.
    let mut filter = QueryFilter::personal_only();
    filter.route_ids = vec!["local".to_string()];
    filter.item_types = vec!["action".to_string()];
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "roofer", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "personal-local-action");

    Ok(())
}

#[test]
fn total_accessible_count_includes_all_filtered_not_paginated() -> Result<()> {
    let mut db = new_db("total_count")?;

    for i in 1..=15 {
        add_item(
            &mut db,
            &format!("item-{i:02}"),
            Some("matching content"),
            "personal",
            "route-1",
            Some("note"),
            None,
            "2026-01-15T10:30:00Z",
        )?;
    }

    let filter = QueryFilter::personal_only();

    // Request page of 5, but total_accessible should be all 15.
    let pagination = QueryPagination {
        limit: 5,
        offset: 0,
    };
    let result = scoped_query(db.conn(), "matching", &filter, &pagination)?;

    assert_eq!(result.hits.len(), 5);
    assert_eq!(result.total_accessible, 15);

    Ok(())
}

#[test]
fn include_no_session_topic_filter() -> Result<()> {
    let mut db = new_db("no_session_topic")?;

    add_item(
        &mut db,
        "with-topic",
        Some("has a topic"),
        "personal",
        "route-1",
        Some("note"),
        Some("work"),
        "2026-01-15T10:30:00Z",
    )?;
    add_item(
        &mut db,
        "without-topic",
        Some("no topic"),
        "personal",
        "route-1",
        Some("note"),
        None,
        "2026-01-15T10:30:00Z",
    )?;

    let mut filter = QueryFilter::personal_only();
    filter.include_no_session_topic = true;
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "topic", &filter, &pagination)?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item_id, "without-topic");

    Ok(())
}
