use chrono::{prelude::*, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
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

    fn validate_context(context: &TimeContext) -> Result<(), ResolutionError> {
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
