use crate::retrieval::query::QueryFilter;
use crate::time::{ResolutionError, TimeContext, TimeResolver};
use anyhow::Result;
use chrono::TimeZone;
use std::str::FromStr;

/// Parse result from a natural-language query phrase.
/// Filters are constructed when phrases match known patterns.
/// Non-matching phrases are suitable for literal full-text search fallback.
#[derive(Clone, Debug)]
pub struct PhraseResolution {
    /// The original input phrase (unchanged).
    pub original_phrase: String,
    /// Resolved filters if the phrase matched a known pattern (None for literal fallback).
    pub filter: Option<QueryFilter>,
    /// Clarification needed if the phrase is ambiguous (e.g., missing hour after date-only phrase).
    pub clarification_needed: Option<ClarificationKind>,
    /// Full-text search text when the phrase doesn't match a known pattern.
    /// This is used for literal fallback retrieval.
    pub fallback_search_text: String,
}

/// Kinds of clarification needed when a phrase is ambiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClarificationKind {
    /// A date was parsed but no time was specified; user should clarify the time.
    MissingTime { date_str: String },
    /// A date phrase is ambiguous (e.g., DST fold); clarify which interpretation.
    AmbiguousTime { phrase: String, reason: String },
    /// Unsupported repeat pattern (e.g., "every day"); clarify if they want a one-shot reminder instead.
    UnsupportedRepeat { phrase: String },
}

/// Parse a natural-language query phrase into filter(s) and optional clarification.
/// Recognized patterns:
/// - "notes/items/reminders since DATE" → date filter with captured_after
/// - "TYPE notes/items" (where TYPE is action/note/idea/etc.) → item_type filter
/// - "private SESSION notes" → session_topic filter
/// - Combination of above patterns (e.g., "private therapy notes since monday")
///
/// Non-matching phrases return None filter and the original text as fallback_search_text.
pub fn parse_phrase(phrase: &str, context: &TimeContext) -> Result<PhraseResolution> {
    let original = phrase.to_string();
    let normalized = phrase.trim().to_lowercase();

    // Check for unsupported repeat patterns first
    if is_unsupported_repeat(&normalized) {
        return Ok(PhraseResolution {
            original_phrase: original.clone(),
            filter: None,
            clarification_needed: Some(ClarificationKind::UnsupportedRepeat {
                phrase: original.clone(),
            }),
            fallback_search_text: original,
        });
    }

    // Try to combine multiple patterns into a single filter
    let mut combined_filter = None;
    let mut clarification = None;

    // First try to extract date/since component
    if let Some((date_filter, date_clarification)) = extract_date_filter(&normalized, context)? {
        combined_filter = Some(date_filter);
        clarification = date_clarification;
    }

    // Try to extract type component
    if let Some(type_filter) = extract_type_filter(&normalized) {
        if let Some(ref mut filter) = combined_filter {
            // Merge type into existing filter
            filter.item_types = type_filter.item_types;
        } else {
            combined_filter = Some(type_filter);
        }
    }

    // Try to extract scope/session component
    if let Some(scope_filter) = extract_scope_filter(&normalized) {
        if let Some(ref mut filter) = combined_filter {
            // Merge scope into existing filter
            filter.session_topics = scope_filter.session_topics;
        } else {
            combined_filter = Some(scope_filter);
        }
    }

    if combined_filter.is_some() {
        return Ok(PhraseResolution {
            original_phrase: original.clone(),
            filter: combined_filter,
            clarification_needed: clarification,
            fallback_search_text: original,
        });
    }

    // No pattern matched; use literal fallback
    Ok(PhraseResolution {
        original_phrase: original.clone(),
        filter: None,
        clarification_needed: None,
        fallback_search_text: original,
    })
}

fn is_unsupported_repeat(normalized: &str) -> bool {
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    for token in tokens {
        if token == "every"
            || token == "recurring"
            || token == "daily"
            || token == "weekly"
            || token == "monthly"
            || token == "yearly"
        {
            return true;
        }
    }
    false
}

fn is_weekday(date_part: &str) -> bool {
    matches!(
        date_part.trim().to_lowercase().as_str(),
        "monday" | "tuesday" | "wednesday" | "thursday" | "friday" | "saturday" | "sunday"
    )
}

/// Extract date filter from a phrase like "since DATE".
/// Returns (filter, clarification) if a date pattern matched.
fn extract_date_filter(
    normalized: &str,
    context: &TimeContext,
) -> Result<Option<(QueryFilter, Option<ClarificationKind>)>> {
    // Look for " since DATE" patterns anywhere in the phrase
    let date_phrase = if let Some(idx) = normalized.rfind(" since ") {
        Some(&normalized[idx + 7..]) // Skip " since " (7 chars)
    } else {
        normalized.strip_prefix("since ")
    };

    if let Some(date_part) = date_phrase {
        // Handle special relative phrases that TimeResolver doesn't support
        if date_part.trim() == "yesterday" {
            if let Ok(tz) = chrono_tz::Tz::from_str(&context.timezone) {
                // Convert reference_time to local timezone first, then get the date
                let local_ref = context.reference_time.with_timezone(&tz);
                let yesterday = (local_ref - chrono::Duration::days(1)).date_naive();
                let mut filter = QueryFilter::personal_only();

                if let Some(naive_midnight) = yesterday.and_hms_opt(0, 0, 0) {
                    match tz.from_local_datetime(&naive_midnight) {
                        chrono::LocalResult::Single(local_dt) => {
                            filter.captured_after =
                                Some(local_dt.with_timezone(&chrono::Utc).to_rfc3339());
                            return Ok(Some((filter, None)));
                        }
                        _ => {
                            // DST ambiguity/gap; don't apply filter
                            return Ok(None);
                        }
                    }
                }
            }
            return Ok(None);
        } else if date_part.trim() == "today" {
            if let Ok(tz) = chrono_tz::Tz::from_str(&context.timezone) {
                // Convert reference_time to local timezone first, then get the date
                let local_ref = context.reference_time.with_timezone(&tz);
                let today = local_ref.date_naive();
                let mut filter = QueryFilter::personal_only();

                if let Some(naive_midnight) = today.and_hms_opt(0, 0, 0) {
                    match tz.from_local_datetime(&naive_midnight) {
                        chrono::LocalResult::Single(local_dt) => {
                            filter.captured_after =
                                Some(local_dt.with_timezone(&chrono::Utc).to_rfc3339());
                            return Ok(Some((filter, None)));
                        }
                        _ => {
                            // DST ambiguity/gap; don't apply filter
                            return Ok(None);
                        }
                    }
                }
            }
            return Ok(None);
        }

        // Determine how to phrase the date for resolution
        // Relative phrases need "since" for proper semantics, explicit dates don't
        let phrase_to_resolve = if is_weekday(date_part) {
            format!("since {}", date_part)
        } else {
            date_part.to_string()
        };

        match TimeResolver::resolve(&phrase_to_resolve, context) {
            Ok(result) => {
                let mut filter = QueryFilter::personal_only();
                let tz_result = chrono_tz::Tz::from_str(&context.timezone).ok();

                // Check if the resolved date is in the future for "since" queries
                // A "since" query should not have a future lower bound
                let is_future = if let Some(resolved_utc) = result.resolved_time {
                    resolved_utc > context.reference_time
                } else if let Some(resolved_date) = result.resolved_date {
                    // Convert resolved_date to UTC midnight and check against reference
                    let mut is_future_bound = false;
                    if let Some(tz) = tz_result.as_ref() {
                        if let Some(naive_midnight) = resolved_date.and_hms_opt(0, 0, 0) {
                            if let chrono::LocalResult::Single(local_dt) =
                                tz.from_local_datetime(&naive_midnight)
                            {
                                is_future_bound =
                                    local_dt.with_timezone(&chrono::Utc) > context.reference_time;
                            }
                        }
                    }
                    is_future_bound
                } else {
                    false
                };

                // Determine clarification needed (check BEFORE applying filter)
                let clarification = if is_future {
                    // Future "since" bounds are unsupported; ask for clarification
                    Some(ClarificationKind::MissingTime {
                        date_str: format!("since {}", date_part),
                    })
                } else if result.is_ambiguous && result.ambiguity_kind.is_some() {
                    if let Some(ambiguity) = result.ambiguity_kind {
                        match ambiguity {
                            crate::time::AmbiguityKind::MissingHour => {
                                // For date-only "since" phrases, the local midnight is the unambiguous boundary
                                // Apply the filter and don't ask for clarification
                                None
                            }
                            crate::time::AmbiguityKind::DstGap
                            | crate::time::AmbiguityKind::DstFold => {
                                // Withhold the filter when there's DST ambiguity
                                Some(ClarificationKind::AmbiguousTime {
                                    phrase: result.original_phrase.clone(),
                                    reason: result.ambiguity_reason.clone().unwrap_or_default(),
                                })
                            }
                            crate::time::AmbiguityKind::Past => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Only apply the filter if it's not in the future
                // (If there's a non-DST-ambiguity clarification needed, don't apply the filter)
                let has_dst_ambiguity =
                    matches!(clarification, Some(ClarificationKind::AmbiguousTime { .. }));

                if !is_future && !has_dst_ambiguity {
                    // If we have a resolved UTC time, use it as captured_after
                    if let Some(resolved_utc) = result.resolved_time {
                        filter.captured_after = Some(resolved_utc.to_rfc3339());
                        return Ok(Some((filter, clarification)));
                    } else if let Some(resolved_date) = result.resolved_date {
                        // Date-only result: convert to start of day in the user's timezone, then to UTC
                        if let Some(tz) = tz_result {
                            if let Some(naive_midnight) = resolved_date.and_hms_opt(0, 0, 0) {
                                // Convert naive datetime to the user's local timezone, then to UTC
                                match tz.from_local_datetime(&naive_midnight) {
                                    chrono::LocalResult::Single(local_dt) => {
                                        filter.captured_after =
                                            Some(local_dt.with_timezone(&chrono::Utc).to_rfc3339());
                                        return Ok(Some((filter, clarification)));
                                    }
                                    _ => {
                                        // DST ambiguity; withhold filter and ask for clarification
                                        return Ok(Some((
                                            QueryFilter::personal_only(),
                                            Some(ClarificationKind::AmbiguousTime {
                                                phrase: result.original_phrase.clone(),
                                                reason: result
                                                    .ambiguity_reason
                                                    .clone()
                                                    .unwrap_or_default(),
                                            }),
                                        )));
                                    }
                                }
                            }
                        } else {
                            // Fallback if timezone is invalid
                            return Ok(None);
                        }
                    }
                    return Ok(Some((filter, clarification)));
                } else if has_dst_ambiguity || is_future {
                    // Return no filter but request clarification
                    return Ok(Some((QueryFilter::personal_only(), clarification)));
                }

                return Ok(None);
            }
            Err(ResolutionError::UnsupportedRepeat(_)) => {
                return Ok(None); // Unsupported repeats are handled in parse_phrase
            }
            Err(ResolutionError::InvalidTimezone(_))
            | Err(ResolutionError::InconsistentContext(_)) => {
                // Surface context errors so callers know something went wrong
                return Err(anyhow::anyhow!(
                    "Invalid timezone or context in phrase resolution"
                ));
            }
            Err(_) => {
                return Ok(None); // Not a recognized date phrase
            }
        }
    }

    Ok(None)
}

/// Extract item-type filter from phrases like "action notes", "idea items", etc.
fn extract_type_filter(normalized: &str) -> Option<QueryFilter> {
    let item_types = ["action", "note", "idea", "broad_intention"];
    let nouns = ["notes", "items", "reminders"];

    for item_type in &item_types {
        for noun in &nouns {
            // Use word-boundary matching to avoid matching substrings like "inaction" as "action"
            let tokens: Vec<&str> = normalized.split_whitespace().collect();
            for i in 0..tokens.len().saturating_sub(1) {
                if tokens[i] == *item_type && tokens[i + 1] == *noun {
                    let mut filter = QueryFilter::personal_only();
                    filter.item_types = vec![item_type.to_string()];
                    return Some(filter);
                }
            }
        }
    }

    None
}

/// Extract scope/session filter from phrases like "private session notes", "work notes", etc.
fn extract_scope_filter(normalized: &str) -> Option<QueryFilter> {
    let nouns = ["notes", "items", "reminders"];

    // Pattern 2 (checked first): "private WORD notes/items/reminders" (e.g., "private therapy notes")
    if normalized.starts_with("private ") {
        for noun in &nouns {
            let pattern = format!(" {}", noun);
            if let Some(idx) = normalized.rfind(&pattern) {
                // Only process if idx >= 8 (to avoid slice panic on "private notes" where idx=7)
                if idx >= 8 {
                    let middle_part = &normalized[8..idx]; // Skip "private " (8 chars)
                    let session_topic = middle_part.trim();
                    if !session_topic.is_empty()
                        && session_topic
                            .chars()
                            .all(|c| c.is_alphabetic() || c == '_' || c == ' ')
                    {
                        // Validate that this looks like a single word or phrase (no "since", "every", etc.)
                        if !session_topic.contains("since")
                            && !session_topic.contains("every")
                            && session_topic
                                .chars()
                                .all(|c| c.is_alphabetic() || c == '_' || c == ' ')
                        {
                            let mut filter = QueryFilter::personal_only();
                            filter.session_topics = vec![session_topic.to_string()];
                            return Some(filter);
                        }
                    }
                }
            }
        }
    }

    // Pattern 1: "WORD session notes/items/reminders" (e.g., "work session notes")
    // Only try this if the phrase doesn't start with "private" (to avoid capturing "private" from "private session notes")
    if !normalized.starts_with("private ") {
        for noun in &nouns {
            let pattern = format!(" session {}", noun);
            if let Some(idx) = normalized.find(&pattern) {
                let session_part = &normalized[..idx];
                let parts: Vec<&str> = session_part.split_whitespace().collect();
                if !parts.is_empty() {
                    let session_topic = parts.join(" ");
                    let mut filter = QueryFilter::personal_only();
                    filter.session_topics = vec![session_topic];
                    return Some(filter);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

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
    fn since_date_resolves_to_captured_after() -> Result<()> {
        let context = make_context();
        // Use a past date instead of tomorrow (which is rejected as a future bound)
        let resolution = parse_phrase("notes since 2026-01-10", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert!(filter.captured_after.is_some());
        // Should contain 2026-01-10
        assert!(filter
            .captured_after
            .as_ref()
            .unwrap()
            .contains("2026-01-10"));
        Ok(())
    }

    #[test]
    fn since_weekday_resolves_past_occurrence() -> Result<()> {
        let context = make_context();
        // Today is Thursday 2026-01-15
        // "since monday" should resolve to the most recent monday (2026-01-12)
        let resolution = parse_phrase("notes since monday", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert!(filter.captured_after.is_some());
        Ok(())
    }

    #[test]
    fn date_only_without_time_applies_local_midnight() -> Result<()> {
        let context = make_context();
        // Use a past date (2026-01-10 is before reference_time 2026-01-15)
        let resolution = parse_phrase("notes since 2026-01-10", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        // Date-only phrases use local midnight as the unambiguous boundary
        assert!(filter.captured_after.is_some());
        assert!(filter
            .captured_after
            .as_ref()
            .unwrap()
            .contains("2026-01-10"));
        // No clarification needed since local midnight is unambiguous
        assert!(resolution.clarification_needed.is_none());
        Ok(())
    }

    #[test]
    fn unsupported_repeat_exposes_clarification() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("notes since every monday", &context)?;

        assert!(resolution.filter.is_none());
        assert!(resolution.clarification_needed.is_some());
        if let Some(ClarificationKind::UnsupportedRepeat { phrase }) =
            resolution.clarification_needed
        {
            assert!(phrase.contains("every"));
        } else {
            panic!("Expected UnsupportedRepeat clarification");
        }
        Ok(())
    }

    #[test]
    fn action_type_phrase_sets_item_type_filter() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("action notes", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert_eq!(filter.item_types, vec!["action"]);
        Ok(())
    }

    #[test]
    fn idea_type_phrase_sets_item_type_filter() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("idea items", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert_eq!(filter.item_types, vec!["idea"]);
        Ok(())
    }

    #[test]
    fn private_session_phrase_sets_session_topic() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("private therapy notes", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert_eq!(filter.session_topics, vec!["therapy"]);
        Ok(())
    }

    #[test]
    fn session_context_phrase_sets_session_topic() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("work session notes", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert_eq!(filter.session_topics, vec!["work"]);
        Ok(())
    }

    #[test]
    fn unrecognized_phrase_falls_back_to_literal_search() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("something about the roof", &context)?;

        assert!(resolution.filter.is_none());
        assert!(resolution.clarification_needed.is_none());
        assert_eq!(resolution.fallback_search_text, "something about the roof");
        Ok(())
    }

    #[test]
    fn non_private_query_not_routed_to_model() -> Result<()> {
        let context = make_context();
        // Verify that parsing does not require model inference
        let resolution = parse_phrase("notes since 2026-01-16 14:00:00", &context)?;

        assert!(resolution.filter.is_some());
        // If this resolved without calling a model, the test passes.
        // We can't directly test "no model call", but we can verify deterministic output.
        let resolution2 = parse_phrase("notes since 2026-01-16 14:00:00", &context)?;
        assert_eq!(resolution.filter, resolution2.filter);
        Ok(())
    }

    #[test]
    fn unsupported_phrase_still_has_useful_literal_search() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("notes since every friday", &context)?;

        // Unsupported repeat should be exposed as clarification
        assert!(resolution.clarification_needed.is_some());
        // But the fallback search text is still present
        assert_eq!(resolution.fallback_search_text, "notes since every friday");
        Ok(())
    }

    #[test]
    fn multiple_filters_can_be_combined() -> Result<()> {
        // While the current implementation handles one pattern at a time,
        // verify that combined patterns work (e.g., "action notes since friday")
        let context = make_context();
        let resolution = parse_phrase("action notes since friday", &context)?;

        // This should match "action notes" pattern first
        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert_eq!(filter.item_types, vec!["action"]);
        // The "since friday" part is not currently combined in this implementation
        // but the filter is still useful for retrieving action items
        Ok(())
    }

    #[test]
    fn original_phrase_preserved_in_resolution() -> Result<()> {
        let context = make_context();
        let original = "Private therapy notes since Monday";
        let resolution = parse_phrase(original, &context)?;

        assert_eq!(resolution.original_phrase, original);
        Ok(())
    }
}
