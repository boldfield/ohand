use chrono::{prelude::*, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayOfWeek {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl DayOfWeek {
    pub fn from_chrono(wd: chrono::Weekday) -> Self {
        match wd {
            chrono::Weekday::Mon => DayOfWeek::Monday,
            chrono::Weekday::Tue => DayOfWeek::Tuesday,
            chrono::Weekday::Wed => DayOfWeek::Wednesday,
            chrono::Weekday::Thu => DayOfWeek::Thursday,
            chrono::Weekday::Fri => DayOfWeek::Friday,
            chrono::Weekday::Sat => DayOfWeek::Saturday,
            chrono::Weekday::Sun => DayOfWeek::Sunday,
        }
    }

    pub fn to_chrono(&self) -> chrono::Weekday {
        match self {
            DayOfWeek::Monday => chrono::Weekday::Mon,
            DayOfWeek::Tuesday => chrono::Weekday::Tue,
            DayOfWeek::Wednesday => chrono::Weekday::Wed,
            DayOfWeek::Thursday => chrono::Weekday::Thu,
            DayOfWeek::Friday => chrono::Weekday::Fri,
            DayOfWeek::Saturday => chrono::Weekday::Sat,
            DayOfWeek::Sunday => chrono::Weekday::Sun,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeContext {
    pub timezone: String,
    pub locale: String,
    pub reference_time: DateTime<Utc>,
    pub utc_offset_at_capture: i32, // seconds (required, from F01 contract)
    pub calendar: String,           // e.g., "gregorian" (required, validated)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbiguityKind {
    MissingHour,
    DstGap,
    DstFold,
    Past,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionResult {
    pub original_phrase: String,
    pub resolved_date: Option<NaiveDate>,
    pub resolved_local: Option<NaiveDateTime>, // wall-clock date and time as parsed, in the captured zone
    pub resolved_time: Option<DateTime<Utc>>,
    pub candidates: Vec<DateTime<Utc>>, // for ambiguous times (DST folds)
    pub is_ambiguous: bool,
    pub ambiguity_kind: Option<AmbiguityKind>,
    pub ambiguity_reason: Option<String>,
    pub is_past: bool,
    pub context: TimeContext,
}

#[derive(Debug, Error)]
pub enum ResolutionError {
    #[error("Invalid date format: {0}")]
    InvalidDateFormat(String),

    #[error("Unsupported repeat pattern: {0}")]
    UnsupportedRepeat(String),

    #[error("Invalid timezone: {0}")]
    InvalidTimezone(String),

    #[error("Invalid calendar: {0}")]
    InvalidCalendar(String),

    #[error("Inconsistent time context: {0}")]
    InconsistentContext(String),
}

#[derive(Debug, Clone, Copy)]
enum Meridiem {
    Am,
    Pm,
}

#[derive(Debug, Clone, Copy)]
enum ClockPart {
    Time { hour: u32, minute: u32 },
    MissingMeridiem,
}

pub struct TimeResolver;

impl TimeResolver {
    pub fn resolve(
        phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let phrase_normalized = phrase.trim().to_lowercase();

        Self::validate_context(context)?;

        // Parse unsupported repeats early
        if Self::is_unsupported_repeat(&phrase_normalized) {
            return Err(ResolutionError::UnsupportedRepeat(
                "Repeating reminders are not supported in M1".to_string(),
            ));
        }

        // Try to parse as explicit date formats first
        if let Ok(result) = Self::parse_explicit_date(&phrase_normalized, phrase, context) {
            return Ok(result);
        }

        // Try natural language phrases
        if let Ok(result) = Self::parse_natural_language(&phrase_normalized, phrase, context) {
            return Ok(result);
        }

        // Try a day part combined with a clock time
        if let Some(result) = Self::parse_clock_phrase(&phrase_normalized, phrase, context) {
            return Ok(result);
        }

        Err(ResolutionError::InvalidDateFormat(phrase.to_string()))
    }

    fn is_unsupported_repeat(phrase: &str) -> bool {
        let tokens: Vec<&str> = phrase.split_whitespace().collect();
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

    fn get_tz(context: &TimeContext) -> Result<Tz, ResolutionError> {
        Tz::from_str(&context.timezone)
            .map_err(|_| ResolutionError::InvalidTimezone(context.timezone.clone()))
    }

    pub fn validate_context(context: &TimeContext) -> Result<(), ResolutionError> {
        Self::validate_calendar(&context.calendar)?;
        let tz = Self::get_tz(context)?;
        let expected_offset = tz
            .offset_from_utc_datetime(&context.reference_time.naive_utc())
            .fix()
            .local_minus_utc();
        if context.utc_offset_at_capture != expected_offset {
            return Err(ResolutionError::InconsistentContext(format!(
                "utc_offset_at_capture {} does not match {} offset {} at reference time",
                context.utc_offset_at_capture, context.timezone, expected_offset
            )));
        }
        Ok(())
    }

    fn validate_calendar(calendar: &str) -> Result<(), ResolutionError> {
        if calendar == "gregorian" {
            Ok(())
        } else {
            Err(ResolutionError::InvalidCalendar(format!(
                "Only gregorian calendar is supported in M1, got: {}",
                calendar
            )))
        }
    }

    fn parse_explicit_date(
        phrase_normalized: &str,
        original_phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let tz = Self::get_tz(context)?;

        // Try ISO 8601 date format (YYYY-MM-DD)
        if let Ok(date) = NaiveDate::parse_from_str(phrase_normalized, "%Y-%m-%d") {
            return Self::handle_date_only_result(date, original_phrase, context);
        }

        // Try with time component (YYYY-MM-DD HH:MM:SS)
        if let Ok(dt) = NaiveDateTime::parse_from_str(phrase_normalized, "%Y-%m-%d %H:%M:%S") {
            return Self::handle_complete_datetime_result(dt, original_phrase, context, tz);
        }

        Err(ResolutionError::InvalidDateFormat(
            original_phrase.to_string(),
        ))
    }

    fn handle_date_only_result(
        date: NaiveDate,
        original_phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let tz = Self::get_tz(context)?;
        let ref_date = context.reference_time.with_timezone(&tz).date_naive();
        let is_past = date < ref_date;

        Ok(ResolutionResult {
            original_phrase: original_phrase.to_string(),
            resolved_date: Some(date),
            resolved_local: None,
            resolved_time: None,
            candidates: vec![],
            is_ambiguous: true,
            ambiguity_kind: Some(AmbiguityKind::MissingHour),
            ambiguity_reason: Some("Missing hour: date-only input requires a time".to_string()),
            is_past,
            context: context.clone(),
        })
    }

    fn handle_complete_datetime_result(
        naive_dt: NaiveDateTime,
        original_phrase: &str,
        context: &TimeContext,
        tz: Tz,
    ) -> Result<ResolutionResult, ResolutionError> {
        let ref_utc = context.reference_time;
        let ref_local = ref_utc.with_timezone(&tz).naive_local();

        let result = match tz.from_local_datetime(&naive_dt) {
            LocalResult::None => ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_date: Some(naive_dt.date()),
                resolved_local: Some(naive_dt),
                resolved_time: None,
                candidates: vec![],
                is_ambiguous: true,
                ambiguity_kind: Some(AmbiguityKind::DstGap),
                ambiguity_reason: Some("Nonexistent time in DST gap".to_string()),
                is_past: naive_dt < ref_local,
                context: context.clone(),
            },
            LocalResult::Ambiguous(dt1, dt2) => {
                let utc1 = dt1.with_timezone(&Utc);
                let utc2 = dt2.with_timezone(&Utc);
                ResolutionResult {
                    original_phrase: original_phrase.to_string(),
                    resolved_date: Some(naive_dt.date()),
                    resolved_local: Some(naive_dt),
                    resolved_time: None,
                    candidates: vec![utc1, utc2],
                    is_ambiguous: true,
                    ambiguity_kind: Some(AmbiguityKind::DstFold),
                    ambiguity_reason: Some(format!(
                        "Ambiguous time in DST fold: could be {} or {}",
                        utc1, utc2
                    )),
                    is_past: utc1.max(utc2) < ref_utc,
                    context: context.clone(),
                }
            }
            LocalResult::Single(dt) => {
                let utc_dt = dt.with_timezone(&Utc);
                let is_past = utc_dt < ref_utc;

                ResolutionResult {
                    original_phrase: original_phrase.to_string(),
                    resolved_date: Some(naive_dt.date()),
                    resolved_local: Some(naive_dt),
                    resolved_time: Some(utc_dt),
                    candidates: vec![],
                    is_ambiguous: is_past,
                    ambiguity_kind: if is_past {
                        Some(AmbiguityKind::Past)
                    } else {
                        None
                    },
                    ambiguity_reason: if is_past {
                        Some("Time is in the past".to_string())
                    } else {
                        None
                    },
                    is_past,
                    context: context.clone(),
                }
            }
        };
        Ok(result)
    }

    fn parse_natural_language(
        phrase_normalized: &str,
        original_phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let tz = Self::get_tz(context)?;

        // Get local date/time from reference_time
        let ref_dt = context.reference_time.with_timezone(&tz);

        if phrase_normalized == "tomorrow" {
            let tomorrow = ref_dt.date_naive() + Duration::days(1);
            return Ok(Self::make_date_only_result(
                tomorrow,
                original_phrase,
                context,
                tz,
            ));
        }

        // Handle weekday names
        let weekdays = [
            ("monday", chrono::Weekday::Mon),
            ("tuesday", chrono::Weekday::Tue),
            ("wednesday", chrono::Weekday::Wed),
            ("thursday", chrono::Weekday::Thu),
            ("friday", chrono::Weekday::Fri),
            ("saturday", chrono::Weekday::Sat),
            ("sunday", chrono::Weekday::Sun),
        ];

        for (name, target_weekday) in &weekdays {
            if phrase_normalized == *name {
                let target_date = Self::get_next_weekday_date(&ref_dt, *target_weekday);
                return Ok(Self::make_date_only_result(
                    target_date,
                    original_phrase,
                    context,
                    tz,
                ));
            }
        }

        // Handle "next <weekday>" phrases
        if let Some(day_str) = phrase_normalized.strip_prefix("next ") {
            for (name, target_weekday) in &weekdays {
                if day_str == *name {
                    let target_date = Self::get_next_weekday_date(&ref_dt, *target_weekday);
                    return Ok(Self::make_date_only_result(
                        target_date,
                        original_phrase,
                        context,
                        tz,
                    ));
                }
            }
        }

        // Handle "since" phrases - means most recent occurrence (past)
        if let Some(day_str) = phrase_normalized.strip_prefix("since ") {
            for (name, target_weekday) in &weekdays {
                if day_str == *name {
                    let target_date = Self::get_past_weekday_date(&ref_dt, *target_weekday);
                    return Ok(Self::make_date_only_result(
                        target_date,
                        original_phrase,
                        context,
                        tz,
                    ));
                }
            }
        }

        Err(ResolutionError::InvalidDateFormat(
            original_phrase.to_string(),
        ))
    }

    /// Resolve a day part and/or a clock part ("Friday at 3 p.m.", "5 pm tomorrow", "at 5 p.m.").
    /// Returns `None` when the phrase is not a clock phrase at all.
    fn parse_clock_phrase(
        phrase_normalized: &str,
        original_phrase: &str,
        context: &TimeContext,
    ) -> Option<ResolutionResult> {
        let tz = Self::get_tz(context).ok()?;
        let tokens: Vec<&str> = phrase_normalized.split_whitespace().collect();
        let (day, clock, rest) = Self::split_day_and_clock(&tokens, context, tz)?;

        let ambiguous = |date: Option<NaiveDate>, reason: &str| {
            let ref_date = context.reference_time.with_timezone(&tz).date_naive();
            ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_date: date,
                resolved_local: None,
                resolved_time: None,
                candidates: vec![],
                is_ambiguous: true,
                ambiguity_kind: Some(AmbiguityKind::MissingHour),
                ambiguity_reason: Some(reason.to_string()),
                is_past: date.is_some_and(|date| date < ref_date),
                context: context.clone(),
            }
        };

        if !rest.is_empty() {
            return Some(ambiguous(
                day,
                "Time zone or unrecognized qualifier after the time is not supported yet",
            ));
        }
        let (hour, minute) = match clock {
            ClockPart::Time { hour, minute } => (hour, minute),
            ClockPart::MissingMeridiem => {
                return Some(ambiguous(
                    day,
                    "Missing meridiem: say a.m. or p.m. for the hour",
                ));
            }
        };
        let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
        let ref_local_date = context.reference_time.with_timezone(&tz).date_naive();
        let date = day.unwrap_or(ref_local_date);
        let result = Self::handle_complete_datetime_result(
            date.and_time(time),
            original_phrase,
            context,
            tz,
        )
        .ok()?;
        if day.is_none() {
            let later_today = result
                .resolved_time
                .is_some_and(|instant| instant > context.reference_time)
                && !result.is_ambiguous;
            if !later_today {
                return Some(ambiguous(
                    None,
                    "Missing day: that time is not later today, so the day is unclear",
                ));
            }
        }
        Some(result)
    }

    /// Split tokens into an optional day and a clock, in either order, plus any unconsumed
    /// trailing tokens (a zone qualifier or other text this grammar does not understand).
    fn split_day_and_clock<'a>(
        tokens: &[&'a str],
        context: &TimeContext,
        tz: Tz,
    ) -> Option<(Option<NaiveDate>, ClockPart, Vec<&'a str>)> {
        // Day first: <day> [at] <clock> [rest]
        for split in 1..tokens.len() {
            if let Some(date) = Self::parse_day_part(&tokens[..split], context, tz) {
                if let Some((clock, used)) = Self::parse_clock_tokens(&tokens[split..]) {
                    return Some((Some(date), clock, tokens[split + used..].to_vec()));
                }
            }
        }
        // Clock first: [at] <clock> [day] [rest]
        let (clock, used) = Self::parse_clock_tokens(tokens)?;
        let after = &tokens[used..];
        for take in (1..=after.len()).rev() {
            if let Some(date) = Self::parse_day_part(&after[..take], context, tz) {
                return Some((Some(date), clock, after[take..].to_vec()));
            }
        }
        Some((None, clock, after.to_vec()))
    }

    fn parse_day_part(tokens: &[&str], context: &TimeContext, tz: Tz) -> Option<NaiveDate> {
        let tokens = match tokens.first() {
            Some(&"on") => &tokens[1..],
            _ => tokens,
        };
        let day = tokens.join(" ");
        if day.is_empty() {
            return None;
        }
        if day == "today" {
            return Some(context.reference_time.with_timezone(&tz).date_naive());
        }
        if let Ok(date) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") {
            return Some(date);
        }
        match Self::parse_natural_language(&day, &day, context) {
            Ok(result) if result.ambiguity_kind == Some(AmbiguityKind::MissingHour) => {
                result.resolved_date
            }
            _ => None,
        }
    }

    /// Parse a clock at the start of `tokens`, with an optional leading `at`. Returns the clock
    /// and the number of tokens consumed.
    fn parse_clock_tokens(tokens: &[&str]) -> Option<(ClockPart, usize)> {
        let (has_at, tokens) = match tokens.first() {
            Some(&"at") => (true, &tokens[1..]),
            _ => (false, tokens),
        };
        let at_len = usize::from(has_at);
        let first = *tokens.first()?;
        match first {
            "noon" => {
                return Some((
                    ClockPart::Time {
                        hour: 12,
                        minute: 0,
                    },
                    at_len + 1,
                ));
            }
            "midnight" => {
                return Some((ClockPart::Time { hour: 0, minute: 0 }, at_len + 1));
            }
            _ => {}
        }
        let numeric_end = first
            .find(|character: char| !(character.is_ascii_digit() || character == ':'))
            .unwrap_or(first.len());
        let (numeric, suffix) = first.split_at(numeric_end);
        let (hour_text, minute_text) = match numeric.split_once(':') {
            Some((hour_text, minute_text)) => (hour_text, Some(minute_text)),
            None => (numeric, None),
        };
        if hour_text.is_empty()
            || !hour_text
                .chars()
                .all(|character| character.is_ascii_digit())
            || hour_text.len() > 2
        {
            return None;
        }
        let minute = match minute_text {
            Some(text) => {
                if text.len() != 2 || !text.chars().all(|character| character.is_ascii_digit()) {
                    return None;
                }
                text.parse::<u32>().ok()?
            }
            None => 0,
        };
        let hour: u32 = hour_text.parse().ok()?;
        if minute > 59 {
            return None;
        }
        let (meridiem, meridiem_tokens) = if !suffix.is_empty() {
            (Self::parse_meridiem(suffix)?, 0)
        } else if let Some(meridiem) = tokens.get(1).and_then(|next| Self::parse_meridiem(next)) {
            (meridiem, 1)
        } else {
            // No meridiem. A two-digit HH:MM is 24-hour; anything else is ambiguous. A bare
            // number only counts as a clock after `at`.
            if minute_text.is_some() && hour_text.len() == 2 {
                if hour > 23 {
                    return None;
                }
                return Some((ClockPart::Time { hour, minute }, at_len + 1));
            }
            if minute_text.is_none() && !has_at {
                return None;
            }
            return Some((ClockPart::MissingMeridiem, at_len + 1));
        };
        if !(1..=12).contains(&hour) {
            return None;
        }
        let hour_24 = match (hour, meridiem) {
            (12, Meridiem::Am) => 0,
            (12, Meridiem::Pm) => 12,
            (hour, Meridiem::Am) => hour,
            (hour, Meridiem::Pm) => hour + 12,
        };
        Some((
            ClockPart::Time {
                hour: hour_24,
                minute,
            },
            at_len + 1 + meridiem_tokens,
        ))
    }

    fn parse_meridiem(text: &str) -> Option<Meridiem> {
        match text {
            "a.m." | "a.m" | "am" => Some(Meridiem::Am),
            "p.m." | "p.m" | "pm" => Some(Meridiem::Pm),
            _ => None,
        }
    }

    fn make_date_only_result(
        date: NaiveDate,
        original_phrase: &str,
        context: &TimeContext,
        tz: Tz,
    ) -> ResolutionResult {
        let ref_date = context.reference_time.with_timezone(&tz).date_naive();
        let is_past = date < ref_date;

        ResolutionResult {
            original_phrase: original_phrase.to_string(),
            resolved_date: Some(date),
            resolved_local: None,
            resolved_time: None,
            candidates: vec![],
            is_ambiguous: true,
            ambiguity_kind: Some(AmbiguityKind::MissingHour),
            ambiguity_reason: Some("Missing hour: date-only input requires a time".to_string()),
            is_past,
            context: context.clone(),
        }
    }

    fn get_next_weekday_date(ref_dt: &DateTime<Tz>, target: chrono::Weekday) -> NaiveDate {
        let current_wd = ref_dt.weekday();
        let today = ref_dt.date_naive();

        let days_ahead = Self::days_until_weekday(current_wd, target);
        if days_ahead == 0 {
            today + Duration::days(7)
        } else {
            today + Duration::days(days_ahead as i64)
        }
    }

    fn get_past_weekday_date(ref_dt: &DateTime<Tz>, target: chrono::Weekday) -> NaiveDate {
        let current_wd = ref_dt.weekday();
        let today = ref_dt.date_naive();

        let current_num = current_wd.number_from_monday();
        let target_num = target.number_from_monday();

        let days_back = if target_num >= current_num {
            7 - (target_num - current_num)
        } else {
            current_num - target_num
        };

        today - Duration::days(days_back as i64)
    }

    fn days_until_weekday(from: chrono::Weekday, to: chrono::Weekday) -> u32 {
        let from_num = from.number_from_monday();
        let to_num = to.number_from_monday();
        if to_num > from_num {
            to_num - from_num
        } else {
            7 - (from_num - to_num)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> TimeContext {
        TimeContext {
            timezone: "UTC".to_string(),
            locale: "en-US".to_string(),
            reference_time: DateTime::parse_from_rfc3339("2024-10-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            utc_offset_at_capture: 0,
            calendar: "gregorian".to_string(),
        }
    }

    fn ny_context() -> TimeContext {
        TimeContext {
            timezone: "America/New_York".to_string(),
            locale: "en-US".to_string(),
            reference_time: DateTime::parse_from_rfc3339("2025-10-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            utc_offset_at_capture: -14400, // EDT: -4 hours
            calendar: "gregorian".to_string(),
        }
    }

    #[test]
    fn test_explicit_date_preserves_original_phrase() {
        let context = test_context();
        let result = TimeResolver::resolve("  2024-10-20  ", &context).unwrap();
        assert_eq!(result.original_phrase, "  2024-10-20  ");
    }

    #[test]
    fn test_date_without_time_is_ambiguous_no_resolved_time() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.resolved_time.is_none());
        assert!(result.resolved_date.is_some());
        assert!(result.ambiguity_reason.is_some());
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }

    #[test]
    fn test_explicit_datetime_not_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20 14:30:00", &context).unwrap();
        assert!(!result.is_ambiguous);
        assert!(result.resolved_time.is_some());
    }

    #[test]
    fn test_dst_gap_america_new_york() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-03-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        context.utc_offset_at_capture = -18000; // EST: -5 hours

        let result = TimeResolver::resolve("2025-03-09 02:30:00", &context).unwrap();
        assert!(result.is_ambiguous);
        assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstGap));
        assert_eq!(
            result.resolved_local,
            NaiveDate::from_ymd_opt(2025, 3, 9).and_then(|d| d.and_hms_opt(2, 30, 0))
        );
        assert!(!result.is_past);
        assert!(result.resolved_time.is_none());
        assert!(result
            .ambiguity_reason
            .as_ref()
            .unwrap()
            .contains("DST gap"));
    }

    #[test]
    fn test_dst_fold_america_new_york_has_both_candidates() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-11-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("2025-11-02 01:30:00", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.resolved_time.is_none());
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstFold));
        assert_eq!(
            result.resolved_local,
            NaiveDate::from_ymd_opt(2025, 11, 2).and_then(|d| d.and_hms_opt(1, 30, 0))
        );
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("fold"));
    }

    #[test]
    fn test_past_dst_gap_and_fold_are_flagged_past() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-12-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        context.utc_offset_at_capture = -18000; // EST: -5 hours

        let gap = TimeResolver::resolve("2025-03-09 02:30:00", &context).unwrap();
        assert_eq!(gap.ambiguity_kind, Some(AmbiguityKind::DstGap));
        assert!(gap.is_past);

        let fold = TimeResolver::resolve("2025-11-02 01:30:00", &context).unwrap();
        assert_eq!(fold.ambiguity_kind, Some(AmbiguityKind::DstFold));
        assert!(fold.is_past);
    }

    #[test]
    fn test_wrong_utc_offset_is_rejected() {
        let mut context = ny_context();
        context.utc_offset_at_capture = 0;
        let err = TimeResolver::resolve("2025-10-20 12:00:00", &context).unwrap_err();
        assert!(matches!(err, ResolutionError::InconsistentContext(_)));
    }

    #[test]
    fn test_non_gregorian_calendar_is_rejected() {
        let mut context = test_context();
        context.calendar = "hebrew".to_string();
        let err = TimeResolver::resolve("2024-10-20 12:00:00", &context).unwrap_err();
        assert!(matches!(err, ResolutionError::InvalidCalendar(_)));
    }

    #[test]
    fn test_tomorrow_returns_date_only() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-10-15T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("tomorrow", &context).unwrap();
        assert!(result.resolved_date.is_some());
        assert!(result.resolved_time.is_none());
        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }

    #[test]
    fn test_weekday_names_return_date_only() {
        let context = test_context();

        for day in &[
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ] {
            let result = TimeResolver::resolve(day, &context).unwrap();
            assert!(result.resolved_date.is_some());
            assert!(result.resolved_time.is_none());
        }
    }

    #[test]
    fn test_since_finds_past_occurrence_as_date_only() {
        let mut context = test_context();
        context.reference_time = DateTime::parse_from_rfc3339("2024-10-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("since friday", &context).unwrap();
        assert!(result.resolved_date.is_some());
        assert!(result.resolved_time.is_none());

        let resolved_date = result.resolved_date.unwrap();
        assert!(resolved_date < context.reference_time.date_naive());
    }

    #[test]
    fn test_unsupported_repeat_every_day() {
        let context = test_context();
        let result = TimeResolver::resolve("every day", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_repeat_every_week() {
        let context = test_context();
        let result = TimeResolver::resolve("every week", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_repeat_daily() {
        let context = test_context();
        let result = TimeResolver::resolve("daily", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_repeat_monthly() {
        let context = test_context();
        let result = TimeResolver::resolve("monthly", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_repeat_yearly() {
        let context = test_context();
        let result = TimeResolver::resolve("yearly", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_repeat_recurring() {
        let context = test_context();
        let result = TimeResolver::resolve("recurring", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_format() {
        let context = test_context();
        let result = TimeResolver::resolve("invalid date", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_timezone_context_preserved() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20 14:30:00", &context).unwrap();
        assert_eq!(result.context.timezone, "UTC");
        assert_eq!(result.context.locale, "en-US");
        assert_eq!(result.context.reference_time, context.reference_time);
    }

    #[test]
    fn test_multiple_timezones() {
        let utc_context = test_context();
        let ny_ctx = ny_context();

        let utc_result = TimeResolver::resolve("2025-06-20 12:00:00", &utc_context).unwrap();
        let ny_result = TimeResolver::resolve("2025-06-20 12:00:00", &ny_ctx).unwrap();

        assert_ne!(
            utc_result.resolved_time.unwrap(),
            ny_result.resolved_time.unwrap()
        );
    }

    #[test]
    fn test_yesterday_phrase_without_time_is_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("tomorrow", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }

    #[test]
    fn test_tomorrow_lowercase_preservation() {
        let context = test_context();
        let result = TimeResolver::resolve("Tomorrow", &context).unwrap();
        assert_eq!(result.original_phrase, "Tomorrow");
    }

    #[test]
    fn test_datetime_with_time_component() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20 14:30:00", &context).unwrap();
        assert!(result.resolved_time.is_some());
        let resolved = result.resolved_time.unwrap();
        assert_eq!(resolved.hour(), 14);
        assert_eq!(resolved.minute(), 30);
    }

    #[test]
    fn test_past_dates_still_resolve() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-10 14:30:00", &context).unwrap();
        assert!(result.resolved_time.is_some());
    }

    #[test]
    fn test_invalid_timezone() {
        let mut context = test_context();
        context.timezone = "Invalid/Timezone".to_string();
        let result = TimeResolver::resolve("2024-10-20", &context);
        assert!(result.is_err());
    }

    #[test]
    fn test_everyone_is_not_repeat_keyword() {
        let context = test_context();
        let result = TimeResolver::resolve("meet everyone tomorrow", &context);
        assert!(result.is_err()); // Invalid date format, not an unsupported repeat
    }

    #[test]
    fn test_past_datetime_is_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-10 14:30:00", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("past"));
    }

    #[test]
    fn test_past_date_only_is_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-10", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.resolved_date.is_some());
        assert!(result.resolved_time.is_none());
        assert!(result.is_past);
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }
}
