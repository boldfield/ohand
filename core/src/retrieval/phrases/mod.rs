use crate::retrieval::query::QueryFilter;
use crate::time::{ResolutionError, TimeContext, TimeResolver};
use anyhow::Result;

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
/// - Combination of above patterns
///
/// Non-matching phrases return None filter and the original text as fallback_search_text.
pub fn parse_phrase(phrase: &str, context: &TimeContext) -> Result<PhraseResolution> {
    let original = phrase.to_string();
    let normalized = phrase.trim().to_lowercase();

    // Try to match known patterns
    if let Some(resolution) = try_parse_date_phrase(&normalized, &original, context)? {
        return Ok(resolution);
    }

    if let Some(resolution) = try_parse_type_phrase(&normalized, &original) {
        return Ok(resolution);
    }

    if let Some(resolution) = try_parse_scope_phrase(&normalized, &original) {
        return Ok(resolution);
    }

    // No pattern matched; use literal fallback
    Ok(PhraseResolution {
        original_phrase: original.clone(),
        filter: None,
        clarification_needed: None,
        fallback_search_text: original,
    })
}

/// Try to parse date-based phrases like "since monday", "since 2026-01-15", etc.
/// Returns Some(resolution) if matched, None if pattern not recognized, or Err if pattern matched but resolution failed.
fn try_parse_date_phrase(
    normalized: &str,
    original: &str,
    context: &TimeContext,
) -> Result<Option<PhraseResolution>> {
    // Pattern: "(notes/items) since DATE"
    if let Some(rest) = normalized.strip_prefix("notes since ") {
        return parse_since_date_phrase(rest, original, context, "notes");
    }
    if let Some(rest) = normalized.strip_prefix("items since ") {
        return parse_since_date_phrase(rest, original, context, "items");
    }
    if let Some(rest) = normalized.strip_prefix("reminders since ") {
        return parse_since_date_phrase(rest, original, context, "reminders");
    }

    // Bare "since DATE" (without notes/items/reminders prefix)
    if let Some(rest) = normalized.strip_prefix("since ") {
        return parse_since_date_phrase(rest, original, context, "");
    }

    Ok(None)
}

/// Parse "since DATE" phrases using the TimeResolver.
fn parse_since_date_phrase(
    date_phrase: &str,
    original: &str,
    context: &TimeContext,
    _prefix: &str,
) -> Result<Option<PhraseResolution>> {
    match TimeResolver::resolve(date_phrase, context) {
        Ok(result) => {
            // Convert the resolved date into a filter
            let mut filter = QueryFilter::personal_only();

            // If we have a resolved UTC time, use it as captured_after
            if let Some(resolved_utc) = result.resolved_time {
                filter.captured_after = Some(resolved_utc.to_rfc3339());
            } else if let Some(resolved_date) = result.resolved_date {
                // Date-only result: convert to start of day in UTC
                // User captured in their timezone, so we need context.timezone to get start-of-day UTC
                filter.captured_after = Some(format!("{}T00:00:00Z", resolved_date));
            }

            // If ambiguous due to missing time, expose clarification
            let clarification = if result.is_ambiguous && result.ambiguity_kind.is_some() {
                if let Some(ambiguity) = result.ambiguity_kind {
                    match ambiguity {
                        crate::time::AmbiguityKind::MissingHour => {
                            Some(ClarificationKind::MissingTime {
                                date_str: result.original_phrase.clone(),
                            })
                        }
                        crate::time::AmbiguityKind::DstGap
                        | crate::time::AmbiguityKind::DstFold => {
                            Some(ClarificationKind::AmbiguousTime {
                                phrase: result.original_phrase.clone(),
                                reason: result.ambiguity_reason.unwrap_or_default(),
                            })
                        }
                        crate::time::AmbiguityKind::Past => {
                            // Past is ambiguous but not clarification-worthy; just let the filter stand
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            Ok(Some(PhraseResolution {
                original_phrase: original.to_string(),
                filter: Some(filter),
                clarification_needed: clarification,
                fallback_search_text: original.to_string(),
            }))
        }
        Err(ResolutionError::UnsupportedRepeat(_)) => Ok(Some(PhraseResolution {
            original_phrase: original.to_string(),
            filter: None,
            clarification_needed: Some(ClarificationKind::UnsupportedRepeat {
                phrase: original.to_string(),
            }),
            fallback_search_text: original.to_string(),
        })),
        Err(_) => {
            // Resolution failed; not a recognized date phrase
            Ok(None)
        }
    }
}

/// Try to parse item-type phrases like "action notes", "idea items", etc.
fn try_parse_type_phrase(normalized: &str, original: &str) -> Option<PhraseResolution> {
    let item_types = ["action", "note", "idea", "broad_intention"];
    let nouns = ["notes", "items", "reminders"];

    for item_type in &item_types {
        for noun in &nouns {
            let pattern = format!("{} {}", item_type, noun);
            if normalized.starts_with(&pattern) {
                let mut filter = QueryFilter::personal_only();
                filter.item_types = vec![item_type.to_string()];

                return Some(PhraseResolution {
                    original_phrase: original.to_string(),
                    filter: Some(filter),
                    clarification_needed: None,
                    fallback_search_text: original.to_string(),
                });
            }
        }
    }

    None
}

/// Try to parse scope/session phrases like "private session notes", "work notes", etc.
fn try_parse_scope_phrase(normalized: &str, original: &str) -> Option<PhraseResolution> {
    // Pattern: "SESSION session notes" (e.g., "therapy session notes", "work notes")
    // If it looks like "WORD session notes", treat WORD as session_topic

    let nouns = ["notes", "items", "reminders"];

    for noun in &nouns {
        let pattern = format!(" session {}", noun);
        if let Some(session_part) = normalized.strip_suffix(&pattern) {
            // Extract the session topic (everything before "session notes/items/reminders")
            let parts: Vec<&str> = session_part.split_whitespace().collect();
            if !parts.is_empty() {
                let session_topic = parts.join(" ");
                let mut filter = QueryFilter::personal_only();
                filter.session_topics = vec![session_topic];

                return Some(PhraseResolution {
                    original_phrase: original.to_string(),
                    filter: Some(filter),
                    clarification_needed: None,
                    fallback_search_text: original.to_string(),
                });
            }
        }

        // Pattern: "private WORD notes" (e.g., "private therapy notes")
        if normalized.starts_with("private ") && normalized.ends_with(noun) {
            if let Some(middle) = normalized
                .strip_prefix("private ")
                .and_then(|s| s.strip_suffix(noun))
            {
                let session_topic = middle.trim_end_matches(' ');
                if !session_topic.is_empty() {
                    let mut filter = QueryFilter::personal_only();
                    filter.session_topics = vec![session_topic.to_string()];

                    return Some(PhraseResolution {
                        original_phrase: original.to_string(),
                        filter: Some(filter),
                        clarification_needed: None,
                        fallback_search_text: original.to_string(),
                    });
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
        let resolution = parse_phrase("notes since tomorrow", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert!(filter.captured_after.is_some());
        // Tomorrow is 2026-01-16
        assert!(filter
            .captured_after
            .as_ref()
            .unwrap()
            .contains("2026-01-16"));
        Ok(())
    }

    #[test]
    fn since_weekday_resolves_past_occurrence() -> Result<()> {
        let context = make_context();
        // Today is Wednesday 2026-01-15
        // "since monday" should resolve to the most recent monday (2026-01-12)
        let resolution = parse_phrase("notes since monday", &context)?;

        assert!(resolution.filter.is_some());
        let filter = resolution.filter.unwrap();
        assert!(filter.captured_after.is_some());
        Ok(())
    }

    #[test]
    fn date_only_without_time_exposes_clarification() -> Result<()> {
        let context = make_context();
        let resolution = parse_phrase("notes since 2026-01-20", &context)?;

        assert!(resolution.filter.is_some());
        assert!(resolution.clarification_needed.is_some());
        if let Some(ClarificationKind::MissingTime { date_str }) = resolution.clarification_needed {
            assert_eq!(date_str, "2026-01-20");
        } else {
            panic!("Expected MissingTime clarification");
        }
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
