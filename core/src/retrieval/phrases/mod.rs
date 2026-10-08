// Deterministic, local translation of documented query phrases into `QueryFilter`s.
//
// Supported grammar (case-insensitive, no model or network involved):
//
//   [private] [TOPIC...] [session] [TYPE] NOUN [since DATE]
//   since DATE
//
// NOUN is `notes` or `items`, or a plural type (`actions`, `ideas`, `broad intentions`).
// TYPE is `action`, `note`, `idea` or `broad intention` (also `broad_intention`/`broad-intention`). TOPIC words are only read when the
// phrase says `private` or `session`; `session` without a topic means "any session topic".
// Retrieval is always personal-scope only; `private` never widens it.
//
// DATE is `today`, `yesterday`, a weekday name, `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]`, or
// `<month> <day> [<year>]`. A date-only bound is the start of that day in `TimeContext.timezone`.
//
// A phrase is either resolved completely or not at all: if any clause cannot be resolved
// without guessing (future bound, ambiguous weekday, DST gap/fold time, unknown date words),
// no filter is returned, a clarification describes the problem, and the original words remain
// available for literal search. Phrases outside the grammar fall back to literal search with
// no clarification. Invalid time contexts are errors, never silently ignored.

use crate::retrieval::query::{
    normalize_session_topic, scoped_list, scoped_query, QueryFilter, QueryPagination, QueryResult,
};
use crate::time::{TimeContext, TimeResolver};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime};
use chrono::{TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use rusqlite::Connection;
use std::str::FromStr;

/// Parse result from a natural-language query phrase.
#[derive(Clone, Debug)]
pub struct PhraseResolution {
    /// The original input phrase (unchanged).
    pub original_phrase: String,
    /// Complete filter when the whole phrase resolved; `None` means use literal search.
    pub filter: Option<QueryFilter>,
    /// Set when a clause is uncertain and no filter was committed for it.
    pub clarification_needed: Option<ClarificationKind>,
    /// Text for literal full-text search: always the original words.
    pub fallback_search_text: String,
}

/// Kinds of optional clarification a caller can offer instead of guessing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClarificationKind {
    /// An explicit local time is ambiguous or nonexistent (DST fold or gap).
    AmbiguousTime { phrase: String, reason: String },
    /// A date phrase has several plausible dates (local dates, `YYYY-MM-DD`).
    AmbiguousDate {
        phrase: String,
        candidate_dates: Vec<String>,
    },
    /// The "since" bound would be after the reference time, so it could match nothing.
    FutureSinceBound { phrase: String },
    /// The words after "since" are not a supported date expression.
    UnrecognizedDate { phrase: String },
    /// Repeating patterns are not supported (e.g. "every day").
    UnsupportedRepeat { phrase: String },
}

const NOUNS: [&str; 2] = ["notes", "items"];
const REPEAT_WORDS: [&str; 6] = ["every", "recurring", "daily", "weekly", "monthly", "yearly"];
const ITEM_TYPES: [&str; 4] = ["action", "note", "idea", "broad_intention"];

#[derive(Default)]
struct HeadFilter {
    item_type: Option<String>,
    topic_tokens: Vec<String>,
    any_session_topic: bool,
}

impl HeadFilter {
    fn restricts_anything(&self) -> bool {
        self.item_type.is_some() || !self.topic_tokens.is_empty() || self.any_session_topic
    }

    fn apply_to(&self, filter: &mut QueryFilter) {
        if let Some(item_type) = &self.item_type {
            filter.item_types = vec![item_type.clone()];
        }
        if !self.topic_tokens.is_empty() {
            filter.session_topics = vec![normalize_session_topic(&self.topic_tokens.join(" "))];
        }
        filter.require_session_topic = self.any_session_topic || !self.topic_tokens.is_empty();
    }
}

enum SinceOutcome {
    Bound(DateTime<Utc>),
    Clarify(ClarificationKind),
}

/// Parse a query phrase into a complete filter, a clarification, or a literal fallback.
/// Errors only for an invalid `TimeContext` when the phrase contains a "since" clause.
pub fn parse_phrase(phrase: &str, context: &TimeContext) -> Result<PhraseResolution> {
    let original = phrase.to_string();
    let literal = |clarification: Option<ClarificationKind>| PhraseResolution {
        original_phrase: original.clone(),
        filter: None,
        clarification_needed: clarification,
        fallback_search_text: original.clone(),
    };

    let trimmed = phrase.trim().trim_end_matches(['?', '.', '!']).trim_end();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let lowered: Vec<String> = tokens.iter().map(|token| token.to_lowercase()).collect();

    if lowered
        .iter()
        .any(|token| REPEAT_WORDS.contains(&token.as_str()))
    {
        return Ok(literal(Some(ClarificationKind::UnsupportedRepeat {
            phrase: original.clone(),
        })));
    }

    let since_position = lowered.iter().rposition(|token| token == "since");
    let head_end = since_position.unwrap_or(tokens.len());
    let Some(head) = parse_head(&tokens[..head_end], &lowered[..head_end]) else {
        return Ok(literal(None));
    };

    let mut filter = QueryFilter::personal_only();
    head.apply_to(&mut filter);

    let Some(since_position) = since_position else {
        if !head.restricts_anything() {
            return Ok(literal(None));
        }
        return Ok(PhraseResolution {
            original_phrase: original.clone(),
            filter: Some(filter),
            clarification_needed: None,
            fallback_search_text: original,
        });
    };

    let date_tokens = &lowered[since_position + 1..];
    if date_tokens.is_empty() {
        return Ok(literal(None));
    }

    TimeResolver::validate_context(context)?;
    match resolve_since(&date_tokens.join(" "), context)? {
        SinceOutcome::Bound(bound) => {
            filter.captured_after = Some(bound.to_rfc3339());
            Ok(PhraseResolution {
                original_phrase: original.clone(),
                filter: Some(filter),
                clarification_needed: None,
                fallback_search_text: original,
            })
        }
        SinceOutcome::Clarify(clarification) => Ok(literal(Some(clarification))),
    }
}

/// Run a resolution: the filter alone when it resolved, else literal search over the original
/// words. Both paths are personal-scope only and never leave the local database.
pub fn retrieve_phrase(
    conn: &Connection,
    resolution: &PhraseResolution,
    pagination: &QueryPagination,
) -> Result<QueryResult> {
    match &resolution.filter {
        Some(filter) => scoped_list(conn, filter, pagination),
        None => scoped_query(
            conn,
            &resolution.fallback_search_text,
            &QueryFilter::personal_only(),
            pagination,
        ),
    }
}

/// Join the spoken "broad intention(s)" (and "broad-intention(s)") into the stored type name so
/// every spelling reaches the same single-token type match.
fn merge_broad_intention_tokens(tokens: &[&str], lowered: &[String]) -> (Vec<String>, Vec<String>) {
    let mut merged_tokens: Vec<String> = Vec::new();
    let mut merged_lowered: Vec<String> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let is_broad_pair = lowered[index] == "broad"
            && lowered
                .get(index + 1)
                .is_some_and(|next| next == "intention" || next == "intentions");
        if is_broad_pair {
            let joined = format!("broad_{}", lowered[index + 1]);
            merged_tokens.push(joined.clone());
            merged_lowered.push(joined);
            index += 2;
            continue;
        }
        let hyphen_form = lowered[index].replace("broad-intention", "broad_intention");
        merged_tokens.push(if hyphen_form != lowered[index] {
            hyphen_form.clone()
        } else {
            tokens[index].to_string()
        });
        merged_lowered.push(hyphen_form);
        index += 1;
    }
    (merged_tokens, merged_lowered)
}

fn parse_head(raw_tokens: &[&str], raw_lowered: &[String]) -> Option<HeadFilter> {
    let (owned_tokens, lowered) = merge_broad_intention_tokens(raw_tokens, raw_lowered);
    let tokens: Vec<&str> = owned_tokens.iter().map(String::as_str).collect();
    let tokens = tokens.as_slice();
    let lowered = lowered.as_slice();
    let mut head = HeadFilter::default();
    if tokens.is_empty() {
        return Some(head);
    }

    let private = lowered[0] == "private";
    let start = usize::from(private);
    let mut end = tokens.len();
    if end <= start {
        return None;
    }

    let last = lowered[end - 1].as_str();
    if NOUNS.contains(&last) {
        end -= 1;
    } else {
        let item_type = last.strip_suffix('s').filter(|t| ITEM_TYPES.contains(t))?;
        head.item_type = Some(item_type.to_string());
        end -= 1;
    }

    if head.item_type.is_none() && end > start && ITEM_TYPES.contains(&lowered[end - 1].as_str()) {
        head.item_type = Some(lowered[end - 1].clone());
        end -= 1;
    }

    let mut session = false;
    if end > start && lowered[end - 1] == "session" {
        session = true;
        end -= 1;
    }

    let topic_tokens = &tokens[start..end];
    if !topic_tokens.is_empty() {
        if !(private || session) {
            return None;
        }
        let is_topic_word = |token: &&str| {
            token
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '\'')
                && !matches!(token.to_lowercase().as_str(), "private" | "session")
        };
        if !topic_tokens.iter().all(is_topic_word) {
            return None;
        }
        head.topic_tokens = topic_tokens.iter().map(|t| t.to_string()).collect();
    }
    head.any_session_topic = session;
    Some(head)
}

fn resolve_since(date_text: &str, context: &TimeContext) -> Result<SinceOutcome> {
    let tz = Tz::from_str(&context.timezone)
        .map_err(|_| anyhow!("invalid timezone: {}", context.timezone))?;
    let today = context.reference_time.with_timezone(&tz).date_naive();
    let phrase = date_text.to_string();
    let unrecognized = || {
        Ok(SinceOutcome::Clarify(ClarificationKind::UnrecognizedDate {
            phrase: phrase.clone(),
        }))
    };

    if date_text == "tomorrow" || date_text.starts_with("next ") {
        return Ok(SinceOutcome::Clarify(ClarificationKind::FutureSinceBound {
            phrase,
        }));
    }

    let date = if date_text == "today" {
        today
    } else if date_text == "yesterday" {
        today - Duration::days(1)
    } else if let Some(weekday) = parse_weekday(date_text) {
        let days_back =
            (7 + today.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
        if days_back == 0 {
            return Ok(SinceOutcome::Clarify(ClarificationKind::AmbiguousDate {
                phrase,
                candidate_dates: vec![
                    today.format("%Y-%m-%d").to_string(),
                    (today - Duration::days(7)).format("%Y-%m-%d").to_string(),
                ],
            }));
        }
        today - Duration::days(i64::from(days_back))
    } else if let Some(local) = parse_local_datetime(date_text) {
        return explicit_time_bound(local, tz, context.reference_time, phrase);
    } else if let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") {
        date
    } else if let Some(date) = parse_month_day(date_text, today) {
        date
    } else {
        return unrecognized();
    };

    let bound = start_of_local_day(tz, date)?;
    if bound > context.reference_time {
        return Ok(SinceOutcome::Clarify(ClarificationKind::FutureSinceBound {
            phrase,
        }));
    }
    Ok(SinceOutcome::Bound(bound))
}

fn explicit_time_bound(
    local: NaiveDateTime,
    tz: Tz,
    reference_time: DateTime<Utc>,
    phrase: String,
) -> Result<SinceOutcome> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(instant) => {
            let bound = instant.with_timezone(&Utc);
            if bound > reference_time {
                Ok(SinceOutcome::Clarify(ClarificationKind::FutureSinceBound {
                    phrase,
                }))
            } else {
                Ok(SinceOutcome::Bound(bound))
            }
        }
        LocalResult::None => Ok(SinceOutcome::Clarify(ClarificationKind::AmbiguousTime {
            phrase,
            reason: "Nonexistent time in DST gap".to_string(),
        })),
        LocalResult::Ambiguous(first, second) => {
            Ok(SinceOutcome::Clarify(ClarificationKind::AmbiguousTime {
                phrase,
                reason: format!(
                    "Ambiguous time in DST fold: could be {} or {}",
                    first.with_timezone(&Utc).to_rfc3339(),
                    second.with_timezone(&Utc).to_rfc3339()
                ),
            }))
        }
    }
}

/// Earliest instant of the local calendar day; when local midnight does not exist (DST gap),
/// the first instant that does.
fn start_of_local_day(tz: Tz, date: NaiveDate) -> Result<DateTime<Utc>> {
    let midnight = date.and_time(NaiveTime::MIN);
    for minutes_after_midnight in 0..=(24 * 60) {
        let candidate = midnight + Duration::minutes(minutes_after_midnight);
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(instant) | LocalResult::Ambiguous(instant, _) => {
                return Ok(instant.with_timezone(&Utc));
            }
            LocalResult::None => {}
        }
    }
    Err(anyhow!("no valid local time on {date} in {tz}"))
}

fn parse_weekday(text: &str) -> Option<Weekday> {
    match text {
        "monday" => Some(Weekday::Mon),
        "tuesday" => Some(Weekday::Tue),
        "wednesday" => Some(Weekday::Wed),
        "thursday" => Some(Weekday::Thu),
        "friday" => Some(Weekday::Fri),
        "saturday" => Some(Weekday::Sat),
        "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_local_datetime(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M"))
        .ok()
}

/// `<month> <day> [<year>]`. Without a year, the most recent such date that is not after today.
fn parse_month_day(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let cleaned = text.replace(',', " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.len() != 2 && words.len() != 3 {
        return None;
    }
    let month = parse_month(words[0])?;
    let day_digits = words[1].trim_end_matches(|c: char| c.is_alphabetic());
    let day: u32 = day_digits.parse().ok()?;
    match words.get(2) {
        Some(year_text) => {
            if year_text.len() != 4 {
                return None;
            }
            NaiveDate::from_ymd_opt(year_text.parse().ok()?, month, day)
        }
        None => {
            let this_year = NaiveDate::from_ymd_opt(today.year(), month, day)?;
            if this_year <= today {
                Some(this_year)
            } else {
                NaiveDate::from_ymd_opt(today.year() - 1, month, day)
            }
        }
    }
}

fn parse_month(word: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    if word.len() < 3 {
        return None;
    }
    let word = if word == "sept" { "sep" } else { word };
    MONTHS
        .iter()
        .position(|month| *month == word || (word.len() == 3 && month.starts_with(word)))
        .map(|index| index as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Offset;

    fn context_in(timezone: &str, reference_rfc3339: &str) -> TimeContext {
        let reference_time = DateTime::parse_from_rfc3339(reference_rfc3339)
            .unwrap()
            .with_timezone(&Utc);
        let tz = Tz::from_str(timezone).unwrap();
        TimeContext {
            timezone: timezone.to_string(),
            locale: "en".to_string(),
            reference_time,
            utc_offset_at_capture: tz
                .offset_from_utc_datetime(&reference_time.naive_utc())
                .fix()
                .local_minus_utc(),
            calendar: "gregorian".to_string(),
        }
    }

    // Thursday 2026-01-15
    fn utc_context() -> TimeContext {
        context_in("UTC", "2026-01-15T10:30:00Z")
    }

    fn captured_after(phrase: &str, context: &TimeContext) -> Option<String> {
        parse_phrase(phrase, context)
            .unwrap()
            .filter
            .and_then(|filter| filter.captured_after)
    }

    #[test]
    fn private_notes_without_topic_does_not_panic() {
        let context = utc_context();
        for phrase in ["private notes", "private", "private notes since monday", ""] {
            parse_phrase(phrase, &context).unwrap();
        }
    }

    #[test]
    fn headline_phrase_requires_any_session_topic_not_a_topic_named_session() {
        let resolution =
            parse_phrase("private session notes since monday", &utc_context()).unwrap();
        let filter = resolution.filter.unwrap();
        assert!(filter.session_topics.is_empty());
        assert!(filter.require_session_topic);
        assert_eq!(
            filter.captured_after.as_deref(),
            Some("2026-01-12T00:00:00+00:00")
        );
        assert!(resolution.clarification_needed.is_none());
    }

    #[test]
    fn topic_words_are_normalized_for_matching() {
        let filter = parse_phrase("Private Therapy notes", &utc_context())
            .unwrap()
            .filter
            .unwrap();
        assert_eq!(filter.session_topics, vec!["therapy"]);
    }

    #[test]
    fn broad_intention_spellings_all_become_the_broad_intention_type() {
        for phrase in [
            "broad intentions since 2026-01-10",
            "Broad Intention notes since 2026-01-10",
            "broad_intentions since 2026-01-10",
            "broad-intentions since 2026-01-10",
        ] {
            let filter = parse_phrase(phrase, &utc_context())
                .unwrap()
                .filter
                .unwrap_or_else(|| panic!("{phrase} should resolve"));
            assert_eq!(filter.item_types, vec!["broad_intention"], "{phrase}");
        }
        assert!(parse_phrase("broad notes", &utc_context())
            .unwrap()
            .filter
            .is_none());
    }

    #[test]
    fn weekday_looks_backward_and_today_is_ambiguous() {
        let context = utc_context();
        assert_eq!(
            captured_after("notes since friday", &context).as_deref(),
            Some("2026-01-09T00:00:00+00:00")
        );
        let thursday = parse_phrase("notes since thursday", &context).unwrap();
        assert!(thursday.filter.is_none());
        assert_eq!(
            thursday.clarification_needed,
            Some(ClarificationKind::AmbiguousDate {
                phrase: "thursday".to_string(),
                candidate_dates: vec!["2026-01-15".to_string(), "2026-01-08".to_string()],
            })
        );
    }

    #[test]
    fn today_and_yesterday_use_the_local_date() {
        let context = context_in("America/Los_Angeles", "2026-01-15T03:30:00Z");
        assert_eq!(
            captured_after("notes since today", &context).as_deref(),
            Some("2026-01-14T08:00:00+00:00")
        );
        assert_eq!(
            captured_after("notes since yesterday", &context).as_deref(),
            Some("2026-01-13T08:00:00+00:00")
        );
    }

    #[test]
    fn dst_gap_at_local_midnight_uses_first_valid_instant() {
        let context = context_in("America/Santiago", "2026-09-06T12:00:00Z");
        assert_eq!(
            captured_after("notes since today", &context).as_deref(),
            Some("2026-09-06T04:00:00+00:00")
        );
    }

    #[test]
    fn month_day_without_year_is_most_recent_past_occurrence() {
        let context = utc_context();
        assert_eq!(
            captured_after("notes since jan 10", &context).as_deref(),
            Some("2026-01-10T00:00:00+00:00")
        );
        assert_eq!(
            captured_after("notes since december 25", &context).as_deref(),
            Some("2025-12-25T00:00:00+00:00")
        );
    }

    #[test]
    fn future_and_unknown_dates_withhold_the_whole_filter() {
        let context = utc_context();
        for phrase in [
            "action notes since tomorrow",
            "action notes since 2026-02-01",
            "action notes since next friday",
        ] {
            let resolution = parse_phrase(phrase, &context).unwrap();
            assert!(resolution.filter.is_none(), "{phrase}");
            assert!(
                matches!(
                    resolution.clarification_needed,
                    Some(ClarificationKind::FutureSinceBound { .. })
                ),
                "{phrase}"
            );
        }
        let vague = parse_phrase("action notes since last week", &context).unwrap();
        assert!(vague.filter.is_none());
        assert_eq!(
            vague.clarification_needed,
            Some(ClarificationKind::UnrecognizedDate {
                phrase: "last week".to_string()
            })
        );
        assert_eq!(vague.fallback_search_text, "action notes since last week");
    }

    #[test]
    fn dst_fold_time_is_withheld_with_ambiguous_time() {
        let context = context_in("America/New_York", "2025-11-10T12:00:00Z");
        let resolution = parse_phrase("notes since 2025-11-02 01:30:00", &context).unwrap();
        assert!(resolution.filter.is_none());
        assert!(matches!(
            resolution.clarification_needed,
            Some(ClarificationKind::AmbiguousTime { .. })
        ));
    }

    #[test]
    fn invalid_context_is_an_error_for_every_date_form() {
        let mut bad_timezone = utc_context();
        bad_timezone.timezone = "Not/A_Zone".to_string();
        let mut bad_calendar = utc_context();
        bad_calendar.calendar = "hebrew".to_string();
        let mut bad_offset = context_in("America/Los_Angeles", "2026-01-15T10:30:00Z");
        bad_offset.utc_offset_at_capture = 0;
        for context in [&bad_timezone, &bad_calendar, &bad_offset] {
            for phrase in [
                "action notes since today",
                "action notes since monday",
                "notes since 2026-01-10",
                "notes since last week",
            ] {
                assert!(parse_phrase(phrase, context).is_err(), "{phrase}");
            }
            assert!(parse_phrase("something about the roof", context).is_ok());
        }
    }

    #[test]
    fn type_words_match_on_word_boundaries() {
        let context = utc_context();
        let resolution = parse_phrase("inaction notes", &context).unwrap();
        assert!(resolution.filter.is_none());
        assert!(resolution.clarification_needed.is_none());
        let resolution = parse_phrase("private keynotes", &context).unwrap();
        assert!(resolution.filter.is_none());
    }

    #[test]
    fn repeat_patterns_ask_for_clarification() {
        let resolution = parse_phrase("notes since every monday", &utc_context()).unwrap();
        assert!(resolution.filter.is_none());
        assert!(matches!(
            resolution.clarification_needed,
            Some(ClarificationKind::UnsupportedRepeat { .. })
        ));
    }
}
