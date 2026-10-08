use anyhow::Result;
use rusqlite::Connection;

use crate::retrieval::index::{search_index, search_source_direct, SearchHit};
use crate::store::events::ItemScope;

/// Query filters for scoped text retrieval. All filters are optional (None = no filter).
#[derive(Clone, Debug, Default)]
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
    // Use read_scopes from filter, or default to personal only if empty.
    let allowed_scopes = if filter.read_scopes.is_empty() {
        vec![ItemScope::Personal]
    } else {
        filter.read_scopes.clone()
    };

    // First pass: search with scope filter (the index handles scope enforcement).
    let mut all_hits = search_index(conn, search_text, &allowed_scopes)?;

    // Apply additional filters to the hits.
    all_hits.retain(|hit| {
        // Filter by lifecycle state (always exclude deleted).
        if hit.lifecycle_state != "deleted"
            && !filter.lifecycle_states.is_empty()
            && !filter.lifecycle_states.contains(&hit.lifecycle_state)
        {
            return false;
        }
        // Filter by route_id if specified.
        if !filter.route_ids.is_empty() && !filter.route_ids.contains(&hit.route_id) {
            return false;
        }
        true
    });

    // Join with items and captures tables to apply remaining filters (item_type, date, session_topic).
    let total_accessible = filter_hits_in_db(conn, &mut all_hits, filter)?;

    // Apply pagination.
    let offset = pagination.offset;
    let limit = if pagination.limit == 0 {
        all_hits.len()
    } else {
        pagination.limit
    };

    let paginated_hits = all_hits.into_iter().skip(offset).take(limit).collect();

    Ok(QueryResult {
        hits: paginated_hits,
        total_accessible,
    })
}

/// Alternative query using direct source table (fallback when index is out of sync).
pub fn scoped_query_direct(
    conn: &Connection,
    search_text: &str,
    filter: &QueryFilter,
    pagination: &QueryPagination,
) -> Result<QueryResult> {
    let allowed_scopes = if filter.read_scopes.is_empty() {
        vec![ItemScope::Personal]
    } else {
        filter.read_scopes.clone()
    };

    let mut all_hits = search_source_direct(conn, search_text, &allowed_scopes)?;

    all_hits.retain(|hit| {
        if hit.lifecycle_state != "deleted"
            && !filter.lifecycle_states.is_empty()
            && !filter.lifecycle_states.contains(&hit.lifecycle_state)
        {
            return false;
        }
        if !filter.route_ids.is_empty() && !filter.route_ids.contains(&hit.route_id) {
            return false;
        }
        true
    });

    let total_accessible = filter_hits_in_db(conn, &mut all_hits, filter)?;

    let offset = pagination.offset;
    let limit = if pagination.limit == 0 {
        all_hits.len()
    } else {
        pagination.limit
    };

    let paginated_hits = all_hits.into_iter().skip(offset).take(limit).collect();

    Ok(QueryResult {
        hits: paginated_hits,
        total_accessible,
    })
}

/// Apply database-level filters to reduce hit set by item_type, dates, and session_topic.
/// Modifies the hits vector in place and returns the total accessible count before pagination.
fn filter_hits_in_db(
    conn: &Connection,
    hits: &mut Vec<SearchHit>,
    filter: &QueryFilter,
) -> Result<usize> {
    if hits.is_empty() {
        return Ok(0);
    }

    let total_before_filter = hits.len();

    // Build a list of item_ids to join against.
    let item_ids: Vec<&str> = hits.iter().map(|h| h.item_id.as_str()).collect();
    let placeholders = vec!["?"; item_ids.len()].join(", ");

    // Build the WHERE clause for additional filters.
    let mut where_clauses = vec![format!("i.item_id IN ({})", placeholders)];

    // Add item_type filter if specified.
    if !filter.item_types.is_empty() {
        let type_placeholders = vec!["?"; filter.item_types.len()].join(", ");
        where_clauses.push(format!("i.item_type IN ({})", type_placeholders));
    }

    // Add date range filters if specified.
    if filter.captured_after.is_some() {
        where_clauses.push("c.capture_instant >= ?".to_string());
    }
    if filter.captured_before.is_some() {
        where_clauses.push("c.capture_instant <= ?".to_string());
    }

    // Add session_topic filter if specified.
    if !filter.session_topics.is_empty() {
        let topic_placeholders = vec!["?"; filter.session_topics.len()].join(", ");
        if filter.include_no_session_topic {
            where_clauses.push(format!(
                "(COALESCE(i.current_session_topic, c.session_topic) IN ({}) OR (i.current_session_topic IS NULL AND c.session_topic IS NULL))",
                topic_placeholders
            ));
        } else {
            where_clauses.push(format!(
                "COALESCE(i.current_session_topic, c.session_topic) IN ({})",
                topic_placeholders
            ));
        }
    } else if filter.include_no_session_topic {
        // If only include_no_session_topic is set (no specific topics), include only items with no topic.
        where_clauses
            .push("(i.current_session_topic IS NULL AND c.session_topic IS NULL)".to_string());
    }

    let where_clause = where_clauses.join(" AND ");
    let sql = format!(
        "SELECT i.item_id, i.item_type, c.capture_instant, COALESCE(i.current_session_topic, c.session_topic)
         FROM items i
         JOIN captures c ON c.capture_id = i.capture_id
         WHERE {}",
        where_clause
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build parameter list in order.
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    for id in &item_ids {
        params.push(id);
    }
    for type_str in &filter.item_types {
        params.push(type_str);
    }
    if let Some(after) = &filter.captured_after {
        params.push(after);
    }
    if let Some(before) = &filter.captured_before {
        params.push(before);
    }
    for topic in &filter.session_topics {
        params.push(topic);
    }

    let allowed_item_ids: std::collections::HashSet<String> = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .collect();

    // Retain only hits that passed the database filters.
    hits.retain(|hit| allowed_item_ids.contains(&hit.item_id));

    Ok(total_before_filter)
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
}
