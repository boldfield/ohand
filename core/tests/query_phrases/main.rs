use anyhow::Result;
use chrono::DateTime;
use ohand_core::retrieval::phrases::{parse_phrase, ClarificationKind};
use ohand_core::retrieval::query::{scoped_query, QueryPagination};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock, Database};
use ohand_core::time::TimeContext;
use std::sync::Arc;

fn make_context() -> TimeContext {
    TimeContext {
        timezone: "UTC".to_string(),
        locale: "en".to_string(),
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: 0,
        calendar: "gregorian".to_string(),
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> DateTime<chrono::Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }
}

fn temp_db_path(label: &str) -> String {
    format!(
        "{}/test_phrase_query_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    )
}

fn new_db(label: &str) -> Result<Database> {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    Database::open(&temp_db_path(label), clock)
}

fn add_test_item(
    db: &mut Database,
    item_id: &str,
    text: &str,
    item_type: Option<&str>,
    session_topic: Option<&str>,
    capture_instant: &str,
) -> Result<()> {
    use ohand_core::retrieval::index::sync_item_in_tx;
    use rusqlite::params;

    let capture_id = format!("cap-{item_id}");
    let tx = db.immediate_transaction()?;
    let capture = Capture::new(
        capture_id.clone(),
        Some(text.to_string()),
        None,
        capture_instant.to_string(),
        "UTC".to_string(),
        0,
        "en".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "test-route".to_string(),
        false,
        "2026-01-15T10:30:00Z".to_string(),
        session_topic.map(str::to_string),
    )?;

    ohand_core::store::captures::save_capture_in_tx(&tx, &capture)?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, item_type, lifecycle_state,
                           save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, ?, 'active', 'saved', 'not_synced', 'unprocessed', 'unprocessed', ?, ?)",
        params![
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

#[test]
fn test_since_tomorrow_gives_clarification() -> Result<()> {
    let context = make_context();
    // "since tomorrow" is a future bound and should not be applied automatically
    let resolution = parse_phrase("notes since tomorrow", &context)?;

    // Should not have a filter (or have a filter with no captured_after)
    if let Some(filter) = resolution.filter {
        assert!(
            filter.captured_after.is_none(),
            "Should not apply future captured_after filter"
        );
    }
    // Should ask for clarification instead
    assert!(
        resolution.clarification_needed.is_some(),
        "Should ask for clarification for future 'since' bound"
    );
    Ok(())
}

#[test]
fn test_since_explicit_date_resolves() -> Result<()> {
    let context = make_context();
    // Use a past date: 2026-01-10 at 14:00 (before reference_time)
    let resolution = parse_phrase("notes since 2026-01-10 14:00:00", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_date_without_time_clarification() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since 2026-01-10", &context)?;

    // Date-only phrases use local midnight as the unambiguous boundary
    // No clarification needed
    assert!(
        resolution.clarification_needed.is_none(),
        "Date-only phrases should use local midnight as unambiguous boundary"
    );
    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_unsupported_repeat_pattern_clarification() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since every monday", &context)?;

    assert!(resolution.clarification_needed.is_some());
    match resolution.clarification_needed {
        Some(ClarificationKind::UnsupportedRepeat { .. }) => {}
        _ => panic!("Expected UnsupportedRepeat clarification"),
    }
    // Fallback search should still be available
    assert!(!resolution.fallback_search_text.is_empty());
    Ok(())
}

#[test]
fn test_action_items_type_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("action notes", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.item_types, vec!["action"]);
    // Should default to personal scope only
    assert!(filter
        .read_scopes
        .iter()
        .any(|s| matches!(s, ohand_core::store::events::ItemScope::Personal)));
    Ok(())
}

#[test]
fn test_idea_items_type_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("idea items", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.item_types, vec!["idea"]);
    Ok(())
}

#[test]
fn test_note_items_type_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("note reminders", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.item_types, vec!["note"]);
    Ok(())
}

#[test]
fn test_broad_intention_type_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("broad_intention notes", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.item_types, vec!["broad_intention"]);
    Ok(())
}

#[test]
fn test_private_session_topic_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("private therapy notes", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.session_topics, vec!["therapy"]);
    Ok(())
}

#[test]
fn test_session_context_topic_filter() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("work session notes", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.session_topics, vec!["work"]);
    Ok(())
}

#[test]
fn test_session_topic_with_multiple_words() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("client meeting session notes", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.session_topics, vec!["client meeting"]);
    Ok(())
}

#[test]
fn test_unrecognized_phrase_fallback_to_literal() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("something about the roof", &context)?;

    assert!(
        resolution.filter.is_none(),
        "Should not create filter for unrecognized phrase"
    );
    assert!(resolution.clarification_needed.is_none());
    assert_eq!(resolution.fallback_search_text, "something about the roof");
    Ok(())
}

#[test]
fn test_compound_type_and_date_filter() -> Result<()> {
    let context = make_context();
    // "action notes since friday" should combine both type and date filters
    let resolution = parse_phrase("action notes since friday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.item_types, vec!["action"]);
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_no_model_routing_for_simple_phrases() -> Result<()> {
    let context = make_context();

    // Parse the same phrase twice; should be deterministic (no model dependency)
    let res1 = parse_phrase("action notes since friday", &context)?;
    let res2 = parse_phrase("action notes since friday", &context)?;

    // Both should produce the same filter
    assert_eq!(res1.filter.is_some(), res2.filter.is_some());
    if let (Some(f1), Some(f2)) = (res1.filter, res2.filter) {
        assert_eq!(f1.item_types, f2.item_types);
        assert_eq!(f1.captured_after, f2.captured_after);
    }
    Ok(())
}

#[test]
fn test_private_session_remains_retrievable_without_model() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("private therapy notes", &context)?;

    // This should not require routing to a model; it's a deterministic parse
    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.session_topics, vec!["therapy"]);
    // Verify it's personal-scope only (not routing to providers)
    assert!(filter
        .read_scopes
        .iter()
        .any(|s| matches!(s, ohand_core::store::events::ItemScope::Personal)));
    Ok(())
}

#[test]
fn test_case_insensitive_parsing() -> Result<()> {
    let context = make_context();

    let lower = parse_phrase("action notes", &context)?;
    let upper = parse_phrase("ACTION NOTES", &context)?;
    let mixed = parse_phrase("Action Notes", &context)?;

    assert_eq!(lower.filter.is_some(), upper.filter.is_some());
    assert_eq!(lower.filter.is_some(), mixed.filter.is_some());
    Ok(())
}

#[test]
fn test_whitespace_trimmed() -> Result<()> {
    let context = make_context();

    let padded = parse_phrase("  action notes  ", &context)?;
    let normal = parse_phrase("action notes", &context)?;

    assert_eq!(padded.filter.is_some(), normal.filter.is_some());
    Ok(())
}

#[test]
fn test_since_bare_weekday() -> Result<()> {
    let context = make_context();
    // "since monday" without "notes" prefix should still work
    let resolution = parse_phrase("since monday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_items_suffix_variant() -> Result<()> {
    let context = make_context();

    let res1 = parse_phrase("action items since tomorrow", &context)?;
    let res2 = parse_phrase("action notes since tomorrow", &context)?;
    let res3 = parse_phrase("action reminders since tomorrow", &context)?;

    // All should parse successfully
    assert!(res1.filter.is_some());
    assert!(res2.filter.is_some());
    assert!(res3.filter.is_some());
    Ok(())
}

#[test]
fn test_original_phrase_preserved_unchanged() -> Result<()> {
    let context = make_context();
    let original = "Private Therapy Notes Since Friday";
    let resolution = parse_phrase(original, &context)?;

    assert_eq!(
        resolution.original_phrase, original,
        "Original phrase should be preserved exactly"
    );
    Ok(())
}

#[test]
fn test_fallback_search_text_when_no_match() -> Result<()> {
    let context = make_context();
    let phrase = "remind me about the roof repair estimate";
    let resolution = parse_phrase(phrase, &context)?;

    // Should have fallback search text for literal retrieval
    assert_eq!(resolution.fallback_search_text, phrase);
    Ok(())
}

#[test]
fn test_fallback_search_text_with_unsupported_repeat() -> Result<()> {
    let context = make_context();
    let phrase = "notes since every friday";
    let resolution = parse_phrase(phrase, &context)?;

    // Even though it's unsupported, fallback should be available
    assert_eq!(resolution.fallback_search_text, phrase);
    assert!(resolution.clarification_needed.is_some());
    Ok(())
}

#[test]
fn test_since_weekday_resolves_past_occurrence() -> Result<()> {
    let context = make_context();
    // Today is Thursday 2026-01-15; "since monday" should resolve to most recent Monday (2026-01-12)
    let resolution = parse_phrase("notes since monday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    // Should contain 2026-01-12 (the most recent Monday)
    assert!(
        captured_after.contains("2026-01-12"),
        "Expected 2026-01-12 but got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_explicit_iso_date_time() -> Result<()> {
    let context = make_context();
    // Use a past date/time: 2026-01-10 at 14:30
    let resolution = parse_phrase("notes since 2026-01-10 14:30:00", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    assert!(captured_after.contains("2026-01-10"));
    assert!(captured_after.contains("14:30"));
    Ok(())
}

#[test]
fn test_private_prefix_variations() -> Result<()> {
    let context = make_context();

    let res1 = parse_phrase("private therapy notes", &context)?;
    let res2 = parse_phrase("private work items", &context)?;
    let res3 = parse_phrase("private research reminders", &context)?;

    assert!(res1.filter.is_some());
    assert!(res2.filter.is_some());
    assert!(res3.filter.is_some());

    assert_eq!(res1.filter.unwrap().session_topics, vec!["therapy"]);
    assert_eq!(res2.filter.unwrap().session_topics, vec!["work"]);
    assert_eq!(res3.filter.unwrap().session_topics, vec!["research"]);
    Ok(())
}

#[test]
fn test_compound_private_therapy_notes_since_monday() -> Result<()> {
    let context = make_context();
    // "private therapy notes since monday" should combine session_topic + date filters
    let resolution = parse_phrase("private therapy notes since monday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert_eq!(filter.session_topics, vec!["therapy"]);
    assert!(filter.captured_after.is_some());
    assert!(
        filter.captured_after.unwrap().contains("2026-01-12"),
        "Should resolve to most recent Monday"
    );
    Ok(())
}

#[test]
fn test_retrieval_through_date_filter() -> Result<()> {
    let mut db = new_db("retrieval_date")?;
    let context = make_context();

    // Add an item from yesterday
    add_test_item(
        &mut db,
        "item-old",
        "old therapy notes from yesterday",
        None,
        Some("therapy"),
        "2026-01-14T10:00:00Z",
    )?;

    // Add an item from a week ago
    add_test_item(
        &mut db,
        "item-week-ago",
        "therapy notes from a week ago",
        None,
        Some("therapy"),
        "2026-01-08T10:00:00Z",
    )?;

    // Add an item from the future
    add_test_item(
        &mut db,
        "item-future",
        "therapy notes from tomorrow",
        None,
        Some("therapy"),
        "2026-01-16T10:00:00Z",
    )?;

    let resolution = parse_phrase("private therapy notes since monday", &context)?;
    assert!(resolution.filter.is_some());

    let filter = resolution.filter.unwrap();
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;

    // Should retrieve items from monday (2026-01-12) and later
    // This includes: old (2026-01-14), week-ago (2026-01-08 is before monday), and future (2026-01-16)
    // So we expect: old and future (2 items)
    assert!(
        !result.hits.is_empty(),
        "Should retrieve items matching the date filter"
    );

    // Verify we can find the expected original text
    let found_old = result.hits.iter().any(|h| h.current_text.contains("old"));
    assert!(found_old, "Should find the item with 'old' in text");

    Ok(())
}

#[test]
fn test_type_filter_distinguishes_items() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("action notes since monday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();

    // Verify the filter has both type and date components
    assert_eq!(filter.item_types, vec!["action"]);
    assert!(filter.captured_after.is_some());
    assert!(
        filter.captured_after.unwrap().contains("2026-01-12"),
        "Should resolve to most recent Monday"
    );

    Ok(())
}

#[test]
fn test_literal_fallback_for_unsupported_phrase() -> Result<()> {
    let mut db = new_db("retrieval_literal")?;
    let context = make_context();

    add_test_item(
        &mut db,
        "item-1",
        "therapy notes since last week",
        None,
        Some("therapy"),
        "2026-01-15T10:00:00Z",
    )?;

    let resolution = parse_phrase("notes since last week", &context)?;

    // "since last week" is not a recognized pattern, so should fall back to literal
    assert!(resolution.filter.is_none());
    assert_eq!(resolution.fallback_search_text, "notes since last week");

    // Verify literal search still works
    let filter = ohand_core::retrieval::query::QueryFilter::personal_only();
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "last week", &filter, &pagination)?;

    // Should find the item through literal search
    assert!(
        result.hits.iter().any(|h| h.item_id == "item-1"),
        "Should retrieve through literal fallback"
    );

    Ok(())
}

#[test]
fn test_private_notes_alone_does_not_panic() -> Result<()> {
    let context = make_context();
    // "private notes" by itself should not panic (regression test for slice bounds issue)
    let resolution = parse_phrase("private notes", &context)?;

    // It should fall back to literal search (no pattern match)
    assert!(resolution.filter.is_none());
    assert_eq!(resolution.fallback_search_text, "private notes");
    Ok(())
}

#[test]
fn test_private_notes_since_monday_parses_correctly() -> Result<()> {
    let context = make_context();
    // The headline phrase: "private session notes since monday"
    let resolution = parse_phrase("private session notes since monday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    // Should recognize "session" as the topic, not "private"
    assert_eq!(
        filter.session_topics,
        vec!["session"],
        "Should extract 'session' as the topic"
    );
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_since_tomorrow_gives_clarification_not_filter() -> Result<()> {
    let context = make_context();
    // Tomorrow is 2026-01-16, which is after reference_time (2026-01-15)
    let resolution = parse_phrase("notes since tomorrow", &context)?;

    // Should not apply a future filter; ask for clarification instead
    if let Some(filter) = resolution.filter {
        assert!(
            filter.captured_after.is_none(),
            "Should not apply future captured_after filter"
        );
    }
    assert!(
        resolution.clarification_needed.is_some(),
        "Should ask for clarification for future 'since' bound"
    );
    Ok(())
}

#[test]
fn test_since_yesterday_resolves() -> Result<()> {
    let context = make_context();
    // Yesterday is 2026-01-14, which is before reference_time
    let resolution = parse_phrase("notes since yesterday", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    assert!(
        captured_after.contains("2026-01-14"),
        "Expected 2026-01-14 but got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_since_today_resolves() -> Result<()> {
    let context = make_context();
    // Today is 2026-01-15
    let resolution = parse_phrase("notes since today", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    assert!(
        captured_after.contains("2026-01-15"),
        "Expected 2026-01-15 but got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_retrieval_through_date_filter_excludes_older_items() -> Result<()> {
    let mut db = new_db("retrieval_date_exclude")?;
    let context = make_context();

    // Add items at different dates
    add_test_item(
        &mut db,
        "item-before-monday",
        "therapy notes from wednesday of previous week",
        None,
        Some("therapy"),
        "2026-01-08T10:00:00Z", // Before Monday 2026-01-12
    )?;

    add_test_item(
        &mut db,
        "item-on-monday",
        "therapy notes from monday",
        None,
        Some("therapy"),
        "2026-01-12T10:00:00Z", // On Monday 2026-01-12
    )?;

    add_test_item(
        &mut db,
        "item-after-monday",
        "therapy notes from thursday",
        None,
        Some("therapy"),
        "2026-01-15T10:00:00Z", // After Monday 2026-01-12
    )?;

    let resolution = parse_phrase("private therapy notes since monday", &context)?;
    assert!(resolution.filter.is_some());

    let filter = resolution.filter.unwrap();
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;

    // Should include items from monday and after, but exclude before-monday
    let found_before = result
        .hits
        .iter()
        .any(|h| h.item_id == "item-before-monday");
    let found_on = result.hits.iter().any(|h| h.item_id == "item-on-monday");
    let found_after = result.hits.iter().any(|h| h.item_id == "item-after-monday");

    assert!(
        !found_before,
        "Should exclude item from before Monday (2026-01-08)"
    );
    assert!(
        found_on || found_after,
        "Should include items from Monday onwards"
    );
    Ok(())
}

#[test]
fn test_timezone_aware_date_conversion() -> Result<()> {
    let context = TimeContext {
        timezone: "America/Los_Angeles".to_string(),
        locale: "en".to_string(),
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T10:30:00-08:00")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: -8 * 3600,
        calendar: "gregorian".to_string(),
    };

    let resolution = parse_phrase("notes since 2026-01-10", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());

    let captured_after = filter.captured_after.unwrap();
    // 2026-01-10 00:00:00 LA time should be 2026-01-10T08:00:00Z (8 hours ahead for PST)
    // The RFC3339 format may include timezone offset, so just check the essential parts
    assert!(
        captured_after.contains("2026-01-10") && captured_after.contains("08:00:00"),
        "Expected 2026-01-10T08:00:00 for LA timezone but got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_fallback_search_uses_resolution_text() -> Result<()> {
    let mut db = new_db("fallback_search")?;
    let context = make_context();

    add_test_item(
        &mut db,
        "item-1",
        "notes since last week about the project",
        None,
        None,
        "2026-01-15T10:00:00Z",
    )?;

    let resolution = parse_phrase("notes since last week", &context)?;

    // Should have fallback_search_text
    assert_eq!(resolution.fallback_search_text, "notes since last week");

    // The fallback search should use the resolution's fallback_search_text
    let filter = ohand_core::retrieval::query::QueryFilter::personal_only();
    let pagination = QueryPagination::default();

    let result = scoped_query(
        db.conn(),
        &resolution.fallback_search_text,
        &filter,
        &pagination,
    )?;

    assert!(
        result.hits.iter().any(|h| h.item_id == "item-1"),
        "Should retrieve through fallback search text"
    );

    Ok(())
}

#[test]
fn test_today_uses_local_date_not_utc() -> Result<()> {
    let context = TimeContext {
        timezone: "America/Los_Angeles".to_string(),
        locale: "en".to_string(),
        // 2026-01-15T03:30Z (UTC) = 2026-01-14T19:30 (LA, UTC-8)
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T03:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: -8 * 3600,
        calendar: "gregorian".to_string(),
    };

    let resolution = parse_phrase("notes since today", &context)?;
    assert!(resolution.filter.is_some());

    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    // With LA timezone, "today" should be 2026-01-14 (not 2026-01-15)
    // Local midnight 2026-01-14T00:00:00-08:00 = 2026-01-14T08:00:00Z
    assert!(
        captured_after.contains("2026-01-14"),
        "Expected 2026-01-14 for LA local 'today', got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_yesterday_uses_local_date_not_utc() -> Result<()> {
    let context = TimeContext {
        timezone: "America/Los_Angeles".to_string(),
        locale: "en".to_string(),
        // 2026-01-15T03:30Z (UTC) = 2026-01-14T19:30 (LA, UTC-8)
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T03:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: -8 * 3600,
        calendar: "gregorian".to_string(),
    };

    let resolution = parse_phrase("notes since yesterday", &context)?;
    assert!(resolution.filter.is_some());

    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    // With LA timezone at 2026-01-14T19:30 local, yesterday is 2026-01-13
    // Local midnight 2026-01-13T00:00:00-08:00 = 2026-01-13T08:00:00Z
    assert!(
        captured_after.contains("2026-01-13"),
        "Expected 2026-01-13 for LA local 'yesterday', got {}",
        captured_after
    );
    Ok(())
}

#[test]
fn test_word_boundary_matching_for_item_types() -> Result<()> {
    let context = make_context();

    // "inaction notes" should NOT match "action notes"
    let resolution = parse_phrase("inaction notes", &context)?;
    assert!(
        resolution.filter.is_none(),
        "Should not match 'inaction' as 'action'"
    );

    // But "action notes" should still match
    let resolution2 = parse_phrase("action notes", &context)?;
    assert!(resolution2.filter.is_some());
    assert_eq!(resolution2.filter.unwrap().item_types, vec!["action"]);

    Ok(())
}

#[test]
fn test_dst_fold_explicit_time_withholds_filter() -> Result<()> {
    let context = TimeContext {
        timezone: "America/New_York".to_string(),
        locale: "en".to_string(),
        // Reference time after DST fold
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: -5 * 3600,
        calendar: "gregorian".to_string(),
    };

    // A time during DST fold (2025-11-02 01:30:00 exists twice in America/New_York)
    let resolution = parse_phrase("notes since 2025-11-02 01:30:00", &context)?;

    // Should ask for clarification, not apply a filter
    if let Some(filter) = resolution.filter {
        assert!(
            filter.captured_after.is_none(),
            "Should not apply filter for ambiguous DST time"
        );
    }
    assert!(
        resolution.clarification_needed.is_some(),
        "Should ask for clarification on DST ambiguity"
    );
    match resolution.clarification_needed {
        Some(ClarificationKind::AmbiguousTime { .. }) => {}
        _ => panic!("Expected AmbiguousTime clarification"),
    }
    Ok(())
}

#[test]
fn test_future_since_bound_asks_for_clarification() -> Result<()> {
    let context = make_context();
    // Tomorrow is 2026-01-16, which is after reference_time 2026-01-15T10:30Z
    let resolution = parse_phrase("notes since tomorrow", &context)?;

    // Should not apply a future filter
    if let Some(filter) = resolution.filter {
        assert!(
            filter.captured_after.is_none(),
            "Should not apply future captured_after"
        );
    }
    assert!(
        resolution.clarification_needed.is_some(),
        "Should ask for clarification on future 'since' bound"
    );
    Ok(())
}

#[test]
fn test_retrieval_with_non_utc_timezone() -> Result<()> {
    let mut db = new_db("retrieval_nonuts")?;
    let context = TimeContext {
        timezone: "America/Los_Angeles".to_string(),
        locale: "en".to_string(),
        reference_time: DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        utc_offset_at_capture: -8 * 3600,
        calendar: "gregorian".to_string(),
    };

    // Add items at specific UTC times
    add_test_item(
        &mut db,
        "item-before-boundary",
        "therapy notes from before local midnight",
        None,
        Some("therapy"),
        "2026-01-14T07:59:59Z", // Before 2026-01-14T08:00Z (local midnight)
    )?;

    add_test_item(
        &mut db,
        "item-after-boundary",
        "therapy notes from after local midnight",
        None,
        Some("therapy"),
        "2026-01-14T08:00:00Z", // At 2026-01-14T08:00Z (local midnight)
    )?;

    let resolution = parse_phrase("private therapy notes since 2026-01-14", &context)?;
    assert!(resolution.filter.is_some());

    let filter = resolution.filter.unwrap();
    let pagination = QueryPagination::default();

    let result = scoped_query(db.conn(), "therapy", &filter, &pagination)?;

    // Should retrieve item at/after the local midnight, not the one before
    let found_after = result
        .hits
        .iter()
        .any(|h| h.item_id == "item-after-boundary");
    let found_before = result
        .hits
        .iter()
        .any(|h| h.item_id == "item-before-boundary");

    assert!(
        found_after,
        "Should include item at or after local midnight"
    );
    assert!(!found_before, "Should exclude item before local midnight");

    Ok(())
}
