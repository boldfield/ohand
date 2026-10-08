use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::retrieval::index::{list_index, search_index, search_source_direct, SearchHit};
use crate::store::events::ItemScope;

/// Query filters for scoped text retrieval. All filters are optional (None = no filter).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryFilter {
    /// Item types to include (e.g., "action", "note", "idea", "broad_intention").
    /// Empty vec means no filter.
    pub item_types: Vec<String>,
    /// Lifecycle states to include (e.g., "active", "completed", "cancelled", "deleted").
    /// Empty vec means no filter (but deleted is always excluded by default).
    pub lifecycle_states: Vec<String>,
    /// Route IDs to include (processing/privacy-route filters).
    /// Empty vec means no filter.
    pub route_ids: Vec<String>,
    /// Session topics to include (exact match, case-sensitive).
    /// Empty vec means no filter. If specified, includes items with matching session_topic.
    pub session_topics: Vec<String>,
    /// Include items with no session topic set.
    pub include_no_session_topic: bool,
    /// Only include items that have some session topic (any value).
    pub require_session_topic: bool,
    /// Date range filter: RFC3339 datetime strings.
    /// If specified, includes items captured within this range (inclusive).
    pub captured_after: Option<String>,
    pub captured_before: Option<String>,
    /// Explicitly allowed read scopes: items must match one of these scopes.
    /// If empty, defaults to personal scope only.
    pub read_scopes: Vec<ItemScope>,
}

impl QueryFilter {
    /// Create a filter that allows only personal items (default minimal read scope).
    pub fn personal_only() -> Self {
        Self {
            read_scopes: vec![ItemScope::Personal],
            ..Default::default()
        }
    }

    /// Create a filter that allows both personal and work items.
    pub fn both_scopes() -> Self {
        Self {
            read_scopes: vec![ItemScope::Personal, ItemScope::Work],
            ..Default::default()
        }
    }
}

/// Pagination parameters for stable result ordering.
#[derive(Clone, Debug, Default)]
pub struct QueryPagination {
    /// Maximum number of results to return. If 0, returns all results.
    pub limit: usize,
    /// Offset into results (for cursor-based pagination, use an empty offset for the first page).
    pub offset: usize,
}

/// Result of a scoped query with metadata for pagination.
#[derive(Clone, Debug)]
pub struct QueryResult {
    /// Matched hits satisfying all filters and scope restrictions.
    pub hits: Vec<SearchHit>,
    /// Total count of accessible items matching filters (before pagination).
    /// Used to determine if there are more results beyond the current page.
    pub total_accessible: usize,
}

/// Query the full-text search index with filters and scope enforcement.
/// Scope filtering is enforced before searching: only hits readable under allowed_read_scopes
/// are returned. This prevents leaking private text through snippets, counts, or errors.
pub fn scoped_query(
    conn: &Connection,
    search_text: &str,
    filter: &QueryFilter,
    pagination: &QueryPagination,
) -> Result<QueryResult> {
    let date_bounds = parse_date_bounds(filter)?;
    let allowed_scopes = allowed_read_scopes(filter);
    let candidate_hits = search_index(conn, search_text, &allowed_scopes)?;
    finish_query(
        conn,
        candidate_hits,
        filter,
        &date_bounds,
        pagination,
        CANDIDATE_ID_BATCH_SIZE,
    )
}

/// Filter-only retrieval: every accessible item satisfying the filter, with no search terms.
/// Scope, lifecycle, type, topic and date rules are identical to `scoped_query`.
pub fn scoped_list(
    conn: &Connection,
    filter: &QueryFilter,
    pagination: &QueryPagination,
) -> Result<QueryResult> {
    let date_bounds = parse_date_bounds(filter)?;
    let allowed_scopes = allowed_read_scopes(filter);
    let candidate_hits = list_index(conn, &allowed_scopes)?;
    finish_query(
        conn,
        candidate_hits,
        filter,
        &date_bounds,
        pagination,
        CANDIDATE_ID_BATCH_SIZE,
    )
}

/// Alternative query using direct source table (fallback when index is out of sync).
pub fn scoped_query_direct(
    conn: &Connection,
    search_text: &str,
    filter: &QueryFilter,
    pagination: &QueryPagination,
) -> Result<QueryResult> {
    let date_bounds = parse_date_bounds(filter)?;
    let allowed_scopes = allowed_read_scopes(filter);
    let candidate_hits = search_source_direct(conn, search_text, &allowed_scopes)?;
    finish_query(
        conn,
        candidate_hits,
        filter,
        &date_bounds,
        pagination,
        CANDIDATE_ID_BATCH_SIZE,
    )
}

/// Read scopes from the filter, defaulting to personal only when none are given.
fn allowed_read_scopes(filter: &QueryFilter) -> Vec<ItemScope> {
    if filter.read_scopes.is_empty() {
        vec![ItemScope::Personal]
    } else {
        filter.read_scopes.clone()
    }
}

/// Shared post-search filtering and pagination so the indexed and direct paths cannot drift.
fn finish_query(
    conn: &Connection,
    mut hits: Vec<SearchHit>,
    filter: &QueryFilter,
    date_bounds: &DateBounds,
    pagination: &QueryPagination,
    batch_size: usize,
) -> Result<QueryResult> {
    hits.retain(|hit| {
        // Deleted items are never returned, regardless of the lifecycle filter.
        if hit.lifecycle_state == "deleted" {
            return false;
        }
        if !filter.lifecycle_states.is_empty()
            && !filter.lifecycle_states.contains(&hit.lifecycle_state)
        {
            return false;
        }
        if !filter.route_ids.is_empty() && !filter.route_ids.contains(&hit.route_id) {
            return false;
        }
        true
    });

    let total_accessible = filter_hits_in_db(conn, &mut hits, filter, date_bounds, batch_size)?;

    let limit = if pagination.limit == 0 {
        hits.len()
    } else {
        pagination.limit
    };
    let paginated_hits = hits
        .into_iter()
        .skip(pagination.offset)
        .take(limit)
        .collect();

    Ok(QueryResult {
        hits: paginated_hits,
        total_accessible,
    })
}

/// Parsed UTC instants for the optional capture-date bounds (both inclusive).
struct DateBounds {
    captured_after: Option<DateTime<Utc>>,
    captured_before: Option<DateTime<Utc>>,
}

impl DateBounds {
    fn is_set(&self) -> bool {
        self.captured_after.is_some() || self.captured_before.is_some()
    }
}

/// Parse and validate date bounds before searching; invalid RFC3339 is an error.
fn parse_date_bounds(filter: &QueryFilter) -> Result<DateBounds> {
    Ok(DateBounds {
        captured_after: filter
            .captured_after
            .as_deref()
            .map(parse_rfc3339_utc)
            .transpose()?,
        captured_before: filter
            .captured_before
            .as_deref()
            .map(parse_rfc3339_utc)
            .transpose()?,
    })
}

/// Candidate ids bound per SQL statement; far below SQLite's bound-parameter limit (32,766 in
/// the bundled build), leaving room for the item-type and session-topic parameters.
const CANDIDATE_ID_BATCH_SIZE: usize = 500;

/// Apply database-level filters to reduce hit set by item_type, dates, and session_topic.
/// Modifies the hits vector in place and returns the total accessible count (after all filters, before pagination).
///
/// Candidate ids are looked up in batches so that broad queries matching more items than SQLite
/// allows bound parameters still succeed.
///
/// Date bounds are compared as parsed UTC instants in Rust to keep full sub-second precision.
/// When a bound is set, items whose stored capture instant is not valid RFC3339 are excluded
/// (they cannot be placed in the range); without a bound they are returned normally.
fn filter_hits_in_db(
    conn: &Connection,
    hits: &mut Vec<SearchHit>,
    filter: &QueryFilter,
    date_bounds: &DateBounds,
    batch_size: usize,
) -> Result<usize> {
    if hits.is_empty() {
        return Ok(0);
    }

    let mut allowed_item_ids = std::collections::HashSet::new();
    for batch in hits.chunks(batch_size.max(1)) {
        let item_ids: Vec<&str> = batch.iter().map(|h| h.item_id.as_str()).collect();
        let matching_rows = query_candidate_batch(conn, &item_ids, filter)?;
        for (item_id, capture_instant) in matching_rows {
            if date_bounds.is_set() {
                let Ok(stored_instant) = parse_rfc3339_utc(&capture_instant) else {
                    continue;
                };
                if date_bounds
                    .captured_after
                    .is_some_and(|after| stored_instant < after)
                    || date_bounds
                        .captured_before
                        .is_some_and(|before| stored_instant > before)
                {
                    continue;
                }
            }
            allowed_item_ids.insert(item_id);
        }
    }

    hits.retain(|hit| allowed_item_ids.contains(&hit.item_id));
    Ok(hits.len())
}

/// Return (item_id, capture_instant) for the candidate ids that satisfy the item-type and
/// session-topic filters.
fn query_candidate_batch(
    conn: &Connection,
    item_ids: &[&str],
    filter: &QueryFilter,
) -> Result<Vec<(String, String)>> {
    let placeholders = vec!["?"; item_ids.len()].join(", ");
    let mut where_clauses = vec![format!("i.item_id IN ({})", placeholders)];

    if !filter.item_types.is_empty() {
        let type_placeholders = vec!["?"; filter.item_types.len()].join(", ");
        where_clauses.push(format!("i.item_type IN ({})", type_placeholders));
    }

    let sql = format!(
        "SELECT i.item_id, c.capture_instant,
                COALESCE(i.current_session_topic, c.session_topic)
         FROM items i
         JOIN captures c ON c.capture_id = i.capture_id
         WHERE {}",
        where_clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;

    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    for id in item_ids {
        params.push(id);
    }
    for type_str in &filter.item_types {
        params.push(type_str);
    }

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let wanted_topics: Vec<String> = filter
        .session_topics
        .iter()
        .map(|topic| normalize_session_topic(topic))
        .collect();
    Ok(rows
        .into_iter()
        .filter(|(_, _, topic)| session_topic_allowed(topic.as_deref(), filter, &wanted_topics))
        .map(|(item_id, captured_at, _)| (item_id, captured_at))
        .collect())
}

fn session_topic_allowed(topic: Option<&str>, filter: &QueryFilter, wanted: &[String]) -> bool {
    let topic = topic.filter(|topic| !topic.trim().is_empty());
    if filter.require_session_topic && topic.is_none() {
        return false;
    }
    match topic {
        None => wanted.is_empty() || filter.include_no_session_topic,
        Some(_) if wanted.is_empty() => !filter.include_no_session_topic,
        Some(topic) => wanted.contains(&normalize_session_topic(topic)),
    }
}

/// Session topics are free text that is stored as typed, so they are compared trimmed,
/// whitespace-collapsed and case-insensitively.
pub fn normalize_session_topic(topic: &str) -> String {
    topic
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Parse an RFC3339 datetime into a UTC instant, preserving full sub-second precision.
fn parse_rfc3339_utc(rfc3339_str: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(rfc3339_str)
        .map_err(|e| anyhow!("Invalid RFC3339 datetime '{}': {}", rfc3339_str, e))?
        .with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::index::sync_item_in_tx;
    use crate::store::captures::Capture;
    use crate::store::schema::{Clock, Database};
    use chrono::{DateTime, Utc};
    use std::sync::Arc;

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
            "{}/test_query_{}_{}.db",
            std::env::temp_dir().display(),
            label,
            uuid::Uuid::new_v4()
        )
    }

    fn new_db(label: &str) -> Result<Database> {
        let clock: Arc<dyn Clock> = Arc::new(FixedClock);
        Database::open(&temp_db_path(label), clock)
    }

    fn add_item(
        db: &mut Database,
        item_id: &str,
        text: Option<&str>,
        scope: &str,
        route_id: &str,
        item_type: Option<&str>,
        session_topic: Option<&str>,
    ) -> Result<String> {
        let capture_id = format!("cap-{item_id}");
        let tx = db.immediate_transaction()?;
        let capture = Capture::new(
            capture_id.clone(),
            text.map(str::to_string),
            None,
            "2026-01-15T10:30:00Z".to_string(),
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
        crate::store::captures::save_capture_in_tx(&tx, &capture)?;
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
        Ok(capture_id)
    }

    #[test]
    fn scope_filter_enforces_personal_read_only() -> Result<()> {
        let mut db = new_db("scope_personal")?;
        add_item(
            &mut db,
            "personal-1",
            Some("personal note"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;
        add_item(
            &mut db,
            "work-1",
            Some("work note"),
            "work",
            "route-1",
            Some("note"),
            None,
        )?;

        let filter = QueryFilter::personal_only();
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].item_id, "personal-1");
        Ok(())
    }

    #[test]
    fn scope_filter_allows_both_scopes() -> Result<()> {
        let mut db = new_db("scope_both")?;
        add_item(
            &mut db,
            "personal-1",
            Some("personal note"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;
        add_item(
            &mut db,
            "work-1",
            Some("work note"),
            "work",
            "route-1",
            Some("note"),
            None,
        )?;

        let filter = QueryFilter::both_scopes();
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 2);
        Ok(())
    }

    #[test]
    fn route_filter_restricts_by_processing_route() -> Result<()> {
        let mut db = new_db("route_filter")?;
        add_item(
            &mut db,
            "item-1",
            Some("local item"),
            "personal",
            "local",
            Some("note"),
            None,
        )?;
        add_item(
            &mut db,
            "item-2",
            Some("cloud item"),
            "personal",
            "cloud",
            Some("note"),
            None,
        )?;

        let mut filter = QueryFilter::personal_only();
        filter.route_ids = vec!["local".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "item", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].item_id, "item-1");
        Ok(())
    }

    #[test]
    fn session_topic_filter_matches_exact_topic() -> Result<()> {
        let mut db = new_db("session_topic_filter")?;
        add_item(
            &mut db,
            "item-1",
            Some("therapy note"),
            "personal",
            "route-1",
            Some("note"),
            Some("therapy"),
        )?;
        add_item(
            &mut db,
            "item-2",
            Some("work note"),
            "personal",
            "route-1",
            Some("note"),
            Some("project"),
        )?;
        add_item(
            &mut db,
            "item-3",
            Some("general note"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;

        let mut filter = QueryFilter::personal_only();
        filter.session_topics = vec!["therapy".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].item_id, "item-1");
        Ok(())
    }

    #[test]
    fn item_type_filter_restricts_by_type() -> Result<()> {
        let mut db = new_db("item_type_filter")?;
        add_item(
            &mut db,
            "action-1",
            Some("call the roofer"),
            "personal",
            "route-1",
            Some("action"),
            None,
        )?;
        add_item(
            &mut db,
            "idea-1",
            Some("roof garden idea"),
            "personal",
            "route-1",
            Some("idea"),
            None,
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
    fn pagination_respects_limit_and_offset() -> Result<()> {
        let mut db = new_db("pagination")?;
        for i in 1..=5 {
            add_item(
                &mut db,
                &format!("item-{i}"),
                Some("test item"),
                "personal",
                "route-1",
                Some("note"),
                None,
            )?;
        }

        let filter = QueryFilter::personal_only();

        // First page: limit 2, offset 0
        let pagination = QueryPagination {
            limit: 2,
            offset: 0,
        };
        let result = scoped_query(db.conn(), "item", &filter, &pagination)?;
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.total_accessible, 5);

        // Second page: limit 2, offset 2
        let pagination = QueryPagination {
            limit: 2,
            offset: 2,
        };
        let result = scoped_query(db.conn(), "item", &filter, &pagination)?;
        assert_eq!(result.hits.len(), 2);

        // Beyond available results
        let pagination = QueryPagination {
            limit: 2,
            offset: 10,
        };
        let result = scoped_query(db.conn(), "item", &filter, &pagination)?;
        assert_eq!(result.hits.len(), 0);
        Ok(())
    }

    #[test]
    fn no_matches_returns_empty_honest_result() -> Result<()> {
        let mut db = new_db("no_matches")?;
        add_item(
            &mut db,
            "item-1",
            Some("foo bar"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;

        let filter = QueryFilter::personal_only();
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "nonexistent", &filter, &pagination)?;

        assert!(result.hits.is_empty());
        assert_eq!(result.total_accessible, 0);
        Ok(())
    }

    #[test]
    fn local_only_session_note_retrievable_without_provider() -> Result<()> {
        let mut db = new_db("local_session_topic")?;
        add_item(
            &mut db,
            "session-1",
            Some("private therapy note"),
            "personal",
            "local",
            Some("note"),
            Some("therapy"),
        )?;

        let mut filter = QueryFilter::personal_only();
        filter.route_ids = vec!["local".to_string()];
        filter.session_topics = vec!["therapy".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].item_id, "session-1");
        Ok(())
    }

    #[test]
    fn multiple_filters_combined_correctly() -> Result<()> {
        let mut db = new_db("multiple_filters")?;
        add_item(
            &mut db,
            "item-1",
            Some("action for work"),
            "work",
            "cloud",
            Some("action"),
            Some("project"),
        )?;
        add_item(
            &mut db,
            "item-2",
            Some("action personal local"),
            "personal",
            "local",
            Some("action"),
            None,
        )?;
        add_item(
            &mut db,
            "item-3",
            Some("note personal local"),
            "personal",
            "local",
            Some("note"),
            None,
        )?;

        let mut filter = QueryFilter::personal_only();
        filter.item_types = vec!["action".to_string()];
        filter.route_ids = vec!["local".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "action", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].item_id, "item-2");
        Ok(())
    }

    #[test]
    fn total_accessible_count_reflects_filters() -> Result<()> {
        let mut db = new_db("total_accessible_with_filters")?;
        // Create 3 items matching "note" but with different types.
        add_item(
            &mut db,
            "action-1",
            Some("action note"),
            "personal",
            "route-1",
            Some("action"),
            None,
        )?;
        add_item(
            &mut db,
            "note-1",
            Some("plain note"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;
        add_item(
            &mut db,
            "idea-1",
            Some("note idea"),
            "personal",
            "route-1",
            Some("idea"),
            None,
        )?;

        // Without type filter: should see all 3.
        let filter = QueryFilter::personal_only();
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;
        assert_eq!(result.hits.len(), 3);
        assert_eq!(result.total_accessible, 3);

        // With type filter for "note" only: should see 1, not 3.
        let mut filter = QueryFilter::personal_only();
        filter.item_types = vec!["note".to_string()];
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;
        assert_eq!(result.hits.len(), 1);
        assert_eq!(
            result.total_accessible, 1,
            "total_accessible should be 1 after type filter, not unfiltered count"
        );
        Ok(())
    }

    #[test]
    fn total_accessible_count_reflects_topic_filter() -> Result<()> {
        let mut db = new_db("total_accessible_with_topics")?;
        // Create 3 items matching "note" with different topics.
        add_item(
            &mut db,
            "therapy-1",
            Some("therapy note"),
            "personal",
            "route-1",
            Some("note"),
            Some("therapy"),
        )?;
        add_item(
            &mut db,
            "groceries-1",
            Some("groceries note"),
            "personal",
            "route-1",
            Some("note"),
            Some("groceries"),
        )?;
        add_item(
            &mut db,
            "general-1",
            Some("general note"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;

        // Query with nonexistent topic filter: should get 0 hits and 0 total_accessible.
        let mut filter = QueryFilter::personal_only();
        filter.session_topics = vec!["nonexistent-topic".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "note", &filter, &pagination)?;
        assert_eq!(
            result.hits.len(),
            0,
            "Should have no hits for nonexistent topic"
        );
        assert_eq!(
            result.total_accessible, 0,
            "Should not leak count of matches outside requested topic"
        );
        Ok(())
    }

    #[test]
    fn date_filter_with_mixed_timezone_offsets() -> Result<()> {
        let mut db = new_db("date_filter_mixed_tz")?;

        // Create helper to add item with custom capture instant.
        let add_item_with_capture = |db: &mut Database, id: &str, instant: &str| -> Result<()> {
            let capture_id = format!("cap-{id}");
            let tx = db.immediate_transaction()?;
            let capture = Capture::new(
                capture_id.clone(),
                Some("test".to_string()),
                None,
                instant.to_string(), // Custom instant
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                instant.to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![id, &capture_id, instant, instant],
            )?;
            sync_item_in_tx(&tx, id)?;
            tx.commit()?;
            Ok(())
        };

        // Add item at 2026-01-15T10:30:00+05:00 (which is 2026-01-15T05:30:00Z).
        add_item_with_capture(&mut db, "item-plus5", "2026-01-15T10:30:00+05:00")?;
        // Add item at 2026-01-15T12:00:00-03:00 (which is 2026-01-15T15:00:00Z).
        add_item_with_capture(&mut db, "item-minus3", "2026-01-15T12:00:00-03:00")?;
        // Add item at 2026-01-15T06:00:00Z (UTC).
        add_item_with_capture(&mut db, "item-utc", "2026-01-15T06:00:00Z")?;

        // Query with captured_after = 2026-01-15T06:00:00Z.
        // Should include item-minus3 (15:00Z) and item-utc (06:00Z), but NOT item-plus5 (05:30Z).
        let mut filter = QueryFilter::personal_only();
        filter.captured_after = Some("2026-01-15T06:00:00Z".to_string());
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;

        assert_eq!(
            result.hits.len(),
            2,
            "Should match items at/after 06:00Z (UTC)"
        );
        let ids: Vec<String> = result.hits.iter().map(|h| h.item_id.clone()).collect();
        assert!(ids.contains(&"item-utc".to_string()));
        assert!(ids.contains(&"item-minus3".to_string()));
        assert!(
            !ids.contains(&"item-plus5".to_string()),
            "item-plus5 at 05:30Z should be excluded"
        );
        Ok(())
    }

    #[test]
    fn invalid_date_bounds_return_error() -> Result<()> {
        let db = new_db("invalid_date")?;
        let mut filter = QueryFilter::personal_only();
        filter.captured_after = Some("not-a-valid-date".to_string());
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination);
        assert!(
            result.is_err(),
            "Invalid RFC3339 date should return an error"
        );
        Ok(())
    }

    #[test]
    fn filter_removing_all_results_honest_count() -> Result<()> {
        let mut db = new_db("filter_all_removed")?;
        add_item(
            &mut db,
            "item-1",
            Some("test note"),
            "personal",
            "route-1",
            Some("note"),
            Some("topic-a"),
        )?;
        add_item(
            &mut db,
            "item-2",
            Some("test idea"),
            "personal",
            "route-1",
            Some("idea"),
            Some("topic-b"),
        )?;

        // Filter by topic that doesn't match anything.
        let mut filter = QueryFilter::personal_only();
        filter.session_topics = vec!["nonexistent".to_string()];
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 0);
        assert_eq!(
            result.total_accessible, 0,
            "Should report 0 accessible items, not total before filter"
        );
        Ok(())
    }

    #[test]
    fn date_filter_with_before_bound() -> Result<()> {
        let mut db = new_db("date_filter_before")?;
        add_item(
            &mut db,
            "early",
            Some("test"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;
        add_item(
            &mut db,
            "late",
            Some("test"),
            "personal",
            "route-1",
            Some("note"),
            None,
        )?;

        // Filter before 2026-01-15T10:30:00Z should exclude all items created at exactly that time.
        let mut filter = QueryFilter::personal_only();
        filter.captured_before = Some("2026-01-15T10:29:00Z".to_string());
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;

        assert_eq!(result.hits.len(), 0);
        assert_eq!(result.total_accessible, 0);
        Ok(())
    }

    #[test]
    fn date_filter_fractional_after_both_paths() -> Result<()> {
        // Test both scoped_query and scoped_query_direct with fractional second boundaries
        let mut db = new_db("date_filter_fractional_after")?;

        let add_item_with_capture = |db: &mut Database, id: &str, instant: &str| -> Result<()> {
            let capture_id = format!("cap-{id}");
            let tx = db.immediate_transaction()?;
            let capture = Capture::new(
                capture_id.clone(),
                Some("test".to_string()),
                None,
                instant.to_string(),
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                instant.to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![id, &capture_id, instant, instant],
            )?;
            sync_item_in_tx(&tx, id)?;
            tx.commit()?;
            Ok(())
        };

        // Add items at 2026-01-15T10:30:00.200Z and 2026-01-15T10:30:00.900Z
        add_item_with_capture(&mut db, "item-200ms", "2026-01-15T10:30:00.200Z")?;
        add_item_with_capture(&mut db, "item-900ms", "2026-01-15T10:30:00.900Z")?;

        // Test scoped_query with captured_after at 500ms
        let mut filter = QueryFilter::personal_only();
        filter.captured_after = Some("2026-01-15T10:30:00.500Z".to_string());
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;

        assert_eq!(
            result.hits.len(),
            1,
            "scoped_query: only item-900ms should match after 500ms"
        );
        assert_eq!(result.hits[0].item_id, "item-900ms");
        assert_eq!(result.total_accessible, 1);

        // Test scoped_query_direct with the same filter
        let result_direct = scoped_query_direct(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result_direct.hits.len(),
            1,
            "scoped_query_direct: only item-900ms should match after 500ms"
        );
        assert_eq!(result_direct.hits[0].item_id, "item-900ms");
        assert_eq!(result_direct.total_accessible, 1);

        Ok(())
    }

    #[test]
    fn date_filter_fractional_before_both_paths() -> Result<()> {
        let mut db = new_db("date_filter_fractional_before")?;

        let add_item_with_capture = |db: &mut Database, id: &str, instant: &str| -> Result<()> {
            let capture_id = format!("cap-{id}");
            let tx = db.immediate_transaction()?;
            let capture = Capture::new(
                capture_id.clone(),
                Some("test".to_string()),
                None,
                instant.to_string(),
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                instant.to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![id, &capture_id, instant, instant],
            )?;
            sync_item_in_tx(&tx, id)?;
            tx.commit()?;
            Ok(())
        };

        add_item_with_capture(&mut db, "item-200ms", "2026-01-15T10:30:00.200Z")?;
        add_item_with_capture(&mut db, "item-900ms", "2026-01-15T10:30:00.900Z")?;

        // Test scoped_query with captured_before at 500ms
        let mut filter = QueryFilter::personal_only();
        filter.captured_before = Some("2026-01-15T10:30:00.500Z".to_string());
        let pagination = QueryPagination::default();
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;

        assert_eq!(
            result.hits.len(),
            1,
            "scoped_query: only item-200ms should match before 500ms"
        );
        assert_eq!(result.hits[0].item_id, "item-200ms");
        assert_eq!(result.total_accessible, 1);

        // Test scoped_query_direct with the same filter
        let result_direct = scoped_query_direct(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result_direct.hits.len(),
            1,
            "scoped_query_direct: only item-200ms should match before 500ms"
        );
        assert_eq!(result_direct.hits[0].item_id, "item-200ms");
        assert_eq!(result_direct.total_accessible, 1);

        Ok(())
    }

    #[test]
    fn date_filter_sub_millisecond_precision_before_both_paths() -> Result<()> {
        // Test that sub-millisecond precision is preserved in date filtering.
        // This reproduces the issue: capture at .500400Z should be excluded by captured_before=.500100Z.
        let mut db = new_db("date_filter_sub_millisecond_before")?;

        let add_item_with_capture = |db: &mut Database, id: &str, instant: &str| -> Result<()> {
            let capture_id = format!("cap-{id}");
            let tx = db.immediate_transaction()?;
            let capture = Capture::new(
                capture_id.clone(),
                Some("test".to_string()),
                None,
                instant.to_string(),
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                instant.to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![id, &capture_id, instant, instant],
            )?;
            sync_item_in_tx(&tx, id)?;
            tx.commit()?;
            Ok(())
        };

        // Capture at 500400 microseconds (which is 300 microseconds after the query bound)
        add_item_with_capture(&mut db, "item-sub-ms", "2026-01-15T10:30:00.500400Z")?;

        // Query with bound at 500100 microseconds
        let mut filter = QueryFilter::personal_only();
        filter.captured_before = Some("2026-01-15T10:30:00.500100Z".to_string());
        let pagination = QueryPagination::default();

        // Test scoped_query: capture is after the bound, should be excluded
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result.hits.len(),
            0,
            "scoped_query: capture at .500400Z should be excluded by before=.500100Z"
        );
        assert_eq!(result.total_accessible, 0);

        // Test scoped_query_direct: same expectation
        let result_direct = scoped_query_direct(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result_direct.hits.len(),
            0,
            "scoped_query_direct: capture at .500400Z should be excluded by before=.500100Z"
        );
        assert_eq!(result_direct.total_accessible, 0);

        Ok(())
    }

    #[test]
    fn date_filter_sub_millisecond_precision_after_both_paths() -> Result<()> {
        // Test that sub-millisecond precision is preserved in date filtering.
        // Capture at .500100Z should be excluded by captured_after=.500400Z.
        let mut db = new_db("date_filter_sub_millisecond_after")?;

        let add_item_with_capture = |db: &mut Database, id: &str, instant: &str| -> Result<()> {
            let capture_id = format!("cap-{id}");
            let tx = db.immediate_transaction()?;
            let capture = Capture::new(
                capture_id.clone(),
                Some("test".to_string()),
                None,
                instant.to_string(),
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                instant.to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![id, &capture_id, instant, instant],
            )?;
            sync_item_in_tx(&tx, id)?;
            tx.commit()?;
            Ok(())
        };

        // Capture at 500100 microseconds
        add_item_with_capture(&mut db, "item-sub-ms", "2026-01-15T10:30:00.500100Z")?;

        // Query with bound at 500400 microseconds
        let mut filter = QueryFilter::personal_only();
        filter.captured_after = Some("2026-01-15T10:30:00.500400Z".to_string());
        let pagination = QueryPagination::default();

        // Test scoped_query: capture is before the bound, should be excluded
        let result = scoped_query(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result.hits.len(),
            0,
            "scoped_query: capture at .500100Z should be excluded by after=.500400Z"
        );
        assert_eq!(result.total_accessible, 0);

        // Test scoped_query_direct: same expectation
        let result_direct = scoped_query_direct(db.conn(), "test", &filter, &pagination)?;
        assert_eq!(
            result_direct.hits.len(),
            0,
            "scoped_query_direct: capture at .500100Z should be excluded by after=.500400Z"
        );
        assert_eq!(result_direct.total_accessible, 0);

        Ok(())
    }

    #[test]
    fn non_rfc3339_capture_instant_still_retrievable_without_date_bounds() -> Result<()> {
        let mut db = new_db("non_rfc3339_instant")?;
        let instant = "2026-01-15 10:30:00";
        let capture_id = "cap-odd-instant";
        let tx = db.immediate_transaction()?;
        let capture = Capture::new(
            capture_id.to_string(),
            Some("test".to_string()),
            None,
            instant.to_string(),
            "UTC".to_string(),
            0,
            "en".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            "route-1".to_string(),
            false,
            instant.to_string(),
            None,
        )?;
        crate::store::captures::save_capture_in_tx(&tx, &capture)?;
        tx.execute(
            "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                               save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
            rusqlite::params!["item-odd", capture_id, instant, instant],
        )?;
        sync_item_in_tx(&tx, "item-odd")?;
        tx.commit()?;

        let pagination = QueryPagination::default();

        // No date bounds: the item is still returned on both paths.
        let filter = QueryFilter::personal_only();
        let indexed = scoped_query(db.conn(), "test", &filter, &pagination)?;
        let direct = scoped_query_direct(db.conn(), "test", &filter, &pagination)?;
        assert_eq!((indexed.hits.len(), indexed.total_accessible), (1, 1));
        assert_eq!((direct.hits.len(), direct.total_accessible), (1, 1));

        // With a date bound the unparseable instant cannot be placed in range and is excluded.
        let mut bounded = QueryFilter::personal_only();
        bounded.captured_after = Some("2000-01-01T00:00:00Z".to_string());
        let indexed = scoped_query(db.conn(), "test", &bounded, &pagination)?;
        let direct = scoped_query_direct(db.conn(), "test", &bounded, &pagination)?;
        assert_eq!((indexed.hits.len(), indexed.total_accessible), (0, 0));
        assert_eq!((direct.hits.len(), direct.total_accessible), (0, 0));

        Ok(())
    }

    #[test]
    fn small_candidate_batches_match_unbatched_results_on_both_paths() -> Result<()> {
        let mut db = new_db("batched")?;
        for i in 0..7 {
            let topic = if i % 2 == 0 { Some("therapy") } else { None };
            let item_type = if i % 3 == 0 { "idea" } else { "note" };
            add_item(
                &mut db,
                &format!("item-{i}"),
                Some("shared batch term"),
                "personal",
                "route-1",
                Some(item_type),
                topic,
            )?;
        }

        let mut filter = QueryFilter::personal_only();
        filter.item_types = vec!["note".to_string()];
        filter.session_topics = vec!["therapy".to_string()];
        let date_bounds = parse_date_bounds(&filter)?;
        let allowed = allowed_read_scopes(&filter);
        let pagination = QueryPagination {
            limit: 2,
            offset: 1,
        };

        let expected = scoped_query(db.conn(), "batch", &filter, &pagination)?;
        assert_eq!(expected.total_accessible, 2);
        for batch_size in [1, 2, 3, 100] {
            let indexed = finish_query(
                db.conn(),
                search_index(db.conn(), "batch", &allowed)?,
                &filter,
                &date_bounds,
                &pagination,
                batch_size,
            )?;
            let direct = finish_query(
                db.conn(),
                search_source_direct(db.conn(), "batch", &allowed)?,
                &filter,
                &date_bounds,
                &pagination,
                batch_size,
            )?;
            for result in [&indexed, &direct] {
                assert_eq!(result.total_accessible, expected.total_accessible);
                let ids: Vec<_> = result.hits.iter().map(|h| &h.item_id).collect();
                let expected_ids: Vec<_> = expected.hits.iter().map(|h| &h.item_id).collect();
                assert_eq!(ids, expected_ids, "batch_size {batch_size}");
            }
        }
        Ok(())
    }

    #[test]
    fn broad_query_exceeding_sqlite_parameter_limit_succeeds_on_both_paths() -> Result<()> {
        let mut db = new_db("broad_query")?;
        let total_items = 33_000;
        let tx = db.immediate_transaction()?;
        for i in 0..total_items {
            let capture_id = format!("cap-{i:05}");
            let item_id = format!("item-{i:05}");
            let capture = Capture::new(
                capture_id.clone(),
                Some("common word".to_string()),
                None,
                "2026-01-15T10:30:00Z".to_string(),
                "UTC".to_string(),
                0,
                "en".to_string(),
                "gregorian".to_string(),
                "personal".to_string(),
                "route-1".to_string(),
                false,
                "2026-01-15T10:30:00Z".to_string(),
                None,
            )?;
            crate::store::captures::save_capture_in_tx(&tx, &capture)?;
            tx.execute(
                "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                                   save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'note', 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
                rusqlite::params![item_id, capture_id, "2026-01-15T10:30:00Z", "2026-01-15T10:30:00Z"],
            )?;
        }
        crate::retrieval::index::rebuild_index(&tx)?;
        tx.commit()?;

        let filter = QueryFilter::personal_only();
        let pagination = QueryPagination {
            limit: 10,
            offset: 0,
        };
        let indexed = scoped_query(db.conn(), "common", &filter, &pagination)?;
        let direct = scoped_query_direct(db.conn(), "common", &filter, &pagination)?;
        for result in [&indexed, &direct] {
            assert_eq!(result.hits.len(), 10);
            assert_eq!(result.total_accessible, total_items);
            assert_eq!(result.hits[0].item_id, "item-00000");
        }
        Ok(())
    }
}
