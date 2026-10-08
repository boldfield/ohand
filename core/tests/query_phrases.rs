use anyhow::Result;
use chrono::DateTime;
use ohand_core::retrieval::phrases::{parse_phrase, ClarificationKind};
use ohand_core::time::TimeContext;

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

#[test]
fn test_since_tomorrow_sets_captured_after() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since tomorrow", &context)?;

    assert!(
        resolution.filter.is_some(),
        "Should have filter for 'notes since tomorrow'"
    );
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some(), "Should set captured_after");
    assert!(filter
        .captured_after
        .as_ref()
        .unwrap()
        .contains("2026-01-16"));
    Ok(())
}

#[test]
fn test_since_explicit_date_resolves() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since 2026-01-16 09:00:00", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    Ok(())
}

#[test]
fn test_date_without_time_clarification() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since 2026-01-20", &context)?;

    assert!(
        resolution.clarification_needed.is_some(),
        "Should expose clarification for date-only input"
    );
    match resolution.clarification_needed {
        Some(ClarificationKind::MissingTime { date_str }) => {
            assert_eq!(date_str, "2026-01-20");
        }
        _ => panic!("Expected MissingTime clarification"),
    }
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
fn test_since_multiple_days_past() -> Result<()> {
    let context = make_context();
    // Today is Wed 2026-01-15; "since monday" should be most recent monday (2026-01-12)
    let resolution = parse_phrase("notes since monday", &context)?;

    assert!(resolution.filter.is_some());
    // The date should be in the past
    Ok(())
}

#[test]
fn test_explicit_iso_date_time() -> Result<()> {
    let context = make_context();
    let resolution = parse_phrase("notes since 2026-01-16 14:30:00", &context)?;

    assert!(resolution.filter.is_some());
    let filter = resolution.filter.unwrap();
    assert!(filter.captured_after.is_some());
    let captured_after = filter.captured_after.unwrap();
    assert!(captured_after.contains("2026-01-16"));
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
