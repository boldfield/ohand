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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionResult {
    pub original_phrase: String,
    pub resolved_time: Option<DateTime<Utc>>,
    pub is_ambiguous: bool,
    pub ambiguity_reason: Option<String>,
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

    #[error("Ambiguous time (possibly DST gap): {0}")]
    AmbiguousTime(String),

    #[error("Time is in the past: {0}")]
    PastTime(String),

    #[error("Other error: {0}")]
    Other(String),
}

pub struct TimeResolver;

impl TimeResolver {
    pub fn resolve(
        phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let phrase_normalized = phrase.trim().to_lowercase();

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
        phrase.contains("every")
            || phrase.contains("recurring")
            || phrase.contains("daily")
            || phrase.contains("weekly")
            || phrase.contains("monthly")
            || phrase.contains("yearly")
    }

    fn get_tz(context: &TimeContext) -> Result<Tz, ResolutionError> {
        Tz::from_str(&context.timezone)
            .map_err(|_| ResolutionError::InvalidTimezone(context.timezone.clone()))
    }

    fn parse_explicit_date(
        phrase_normalized: &str,
        original_phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        let tz = Self::get_tz(context)?;

        // Try ISO 8601 date format (YYYY-MM-DD)
        if let Ok(date) = NaiveDate::parse_from_str(phrase_normalized, "%Y-%m-%d") {
            let naive_dt = date.and_hms_opt(0, 0, 0).unwrap();
            return Self::handle_datetime_result(
                naive_dt,
                original_phrase,
                context,
                tz,
                Some("hour"),
            );
        }

        // Try with time component (YYYY-MM-DD HH:MM:SS)
        if let Ok(dt) = NaiveDateTime::parse_from_str(phrase_normalized, "%Y-%m-%d %H:%M:%S") {
            return Self::handle_datetime_result(dt, original_phrase, context, tz, None);
        }

        Err(ResolutionError::InvalidDateFormat(
            original_phrase.to_string(),
        ))
    }

    fn handle_datetime_result(
        naive_dt: NaiveDateTime,
        original_phrase: &str,
        context: &TimeContext,
        tz: Tz,
        missing_component: Option<&str>,
    ) -> Result<ResolutionResult, ResolutionError> {
        match tz.from_local_datetime(&naive_dt) {
            LocalResult::None => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: None,
                is_ambiguous: true,
                ambiguity_reason: Some("Nonexistent time in DST gap".to_string()),
                context: context.clone(),
            }),
            LocalResult::Ambiguous(dt1, dt2) => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: Some(dt1.with_timezone(&Utc)),
                is_ambiguous: true,
                ambiguity_reason: Some(format!(
                    "Ambiguous time in DST fold: could be {} or {}",
                    dt1.with_timezone(&Utc),
                    dt2.with_timezone(&Utc)
                )),
                context: context.clone(),
            }),
            LocalResult::Single(dt) => {
                let utc_dt = dt.with_timezone(&Utc);
                let result = ResolutionResult {
                    original_phrase: original_phrase.to_string(),
                    resolved_time: Some(utc_dt),
                    is_ambiguous: missing_component.is_some(),
                    ambiguity_reason: missing_component
                        .map(|c| format!("Missing {}: midnight assumed", c)),
                    context: context.clone(),
                };
                Ok(result)
            }
        }
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
            let naive_dt = tomorrow.and_hms_opt(0, 0, 0).unwrap();
            return Self::handle_datetime_result(
                naive_dt,
                original_phrase,
                context,
                tz,
                Some("hour"),
            );
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
                if let Ok(dt) =
                    Self::get_next_weekday(&ref_dt, *target_weekday, tz, context, original_phrase)
                {
                    return Ok(dt);
                }
            }
        }

        // Handle "next <weekday>" phrases
        if let Some(day_str) = phrase_normalized.strip_prefix("next ") {
            for (name, target_weekday) in &weekdays {
                if day_str == *name {
                    if let Ok(dt) = Self::get_next_weekday(
                        &ref_dt,
                        *target_weekday,
                        tz,
                        context,
                        original_phrase,
                    ) {
                        return Ok(dt);
                    }
                }
            }
        }

        // Handle "since" phrases - means most recent occurrence (past)
        if let Some(day_str) = phrase_normalized.strip_prefix("since ") {
            for (name, target_weekday) in &weekdays {
                if day_str == *name {
                    if let Ok(dt) = Self::get_past_weekday(
                        &ref_dt,
                        *target_weekday,
                        tz,
                        context,
                        original_phrase,
                    ) {
                        return Ok(dt);
                    }
                }
            }
        }

        Err(ResolutionError::InvalidDateFormat(
            original_phrase.to_string(),
        ))
    }

    fn get_next_weekday(
        ref_dt: &DateTime<Tz>,
        target: chrono::Weekday,
        tz: Tz,
        context: &TimeContext,
        original_phrase: &str,
    ) -> Result<ResolutionResult, ResolutionError> {
        let current_wd = ref_dt.weekday();
        let today = ref_dt.date_naive();

        let days_ahead = Self::days_until_weekday(current_wd, target);
        let target_date = if days_ahead == 0 {
            today + Duration::days(7)
        } else {
            today + Duration::days(days_ahead as i64)
        };

        let naive_dt = target_date.and_hms_opt(0, 0, 0).unwrap();
        match tz.from_local_datetime(&naive_dt) {
            LocalResult::None => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: None,
                is_ambiguous: true,
                ambiguity_reason: Some("Nonexistent time in DST gap".to_string()),
                context: context.clone(),
            }),
            LocalResult::Ambiguous(dt1, _) => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: Some(dt1.with_timezone(&Utc)),
                is_ambiguous: true,
                ambiguity_reason: Some("Ambiguous time in DST fold".to_string()),
                context: context.clone(),
            }),
            LocalResult::Single(dt) => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: Some(dt.with_timezone(&Utc)),
                is_ambiguous: true,
                ambiguity_reason: Some("Missing hour: midnight assumed".to_string()),
                context: context.clone(),
            }),
        }
    }

    fn get_past_weekday(
        ref_dt: &DateTime<Tz>,
        target: chrono::Weekday,
        tz: Tz,
        context: &TimeContext,
        original_phrase: &str,
    ) -> Result<ResolutionResult, ResolutionError> {
        let current_wd = ref_dt.weekday();
        let today = ref_dt.date_naive();

        // Find the most recent occurrence (on or before today)
        let days_back = match current_wd {
            chrono::Weekday::Mon => match target {
                chrono::Weekday::Mon => 7,
                chrono::Weekday::Tue => 6,
                chrono::Weekday::Wed => 5,
                chrono::Weekday::Thu => 4,
                chrono::Weekday::Fri => 3,
                chrono::Weekday::Sat => 2,
                chrono::Weekday::Sun => 1,
            },
            chrono::Weekday::Tue => match target {
                chrono::Weekday::Mon => 1,
                chrono::Weekday::Tue => 7,
                chrono::Weekday::Wed => 6,
                chrono::Weekday::Thu => 5,
                chrono::Weekday::Fri => 4,
                chrono::Weekday::Sat => 3,
                chrono::Weekday::Sun => 2,
            },
            chrono::Weekday::Wed => match target {
                chrono::Weekday::Mon => 2,
                chrono::Weekday::Tue => 1,
                chrono::Weekday::Wed => 7,
                chrono::Weekday::Thu => 6,
                chrono::Weekday::Fri => 5,
                chrono::Weekday::Sat => 4,
                chrono::Weekday::Sun => 3,
            },
            chrono::Weekday::Thu => match target {
                chrono::Weekday::Mon => 3,
                chrono::Weekday::Tue => 2,
                chrono::Weekday::Wed => 1,
                chrono::Weekday::Thu => 7,
                chrono::Weekday::Fri => 6,
                chrono::Weekday::Sat => 5,
                chrono::Weekday::Sun => 4,
            },
            chrono::Weekday::Fri => match target {
                chrono::Weekday::Mon => 4,
                chrono::Weekday::Tue => 3,
                chrono::Weekday::Wed => 2,
                chrono::Weekday::Thu => 1,
                chrono::Weekday::Fri => 7,
                chrono::Weekday::Sat => 6,
                chrono::Weekday::Sun => 5,
            },
            chrono::Weekday::Sat => match target {
                chrono::Weekday::Mon => 5,
                chrono::Weekday::Tue => 4,
                chrono::Weekday::Wed => 3,
                chrono::Weekday::Thu => 2,
                chrono::Weekday::Fri => 1,
                chrono::Weekday::Sat => 7,
                chrono::Weekday::Sun => 6,
            },
            chrono::Weekday::Sun => match target {
                chrono::Weekday::Mon => 6,
                chrono::Weekday::Tue => 5,
                chrono::Weekday::Wed => 4,
                chrono::Weekday::Thu => 3,
                chrono::Weekday::Fri => 2,
                chrono::Weekday::Sat => 1,
                chrono::Weekday::Sun => 7,
            },
        };

        let target_date = today - Duration::days(days_back as i64);
        let naive_dt = target_date.and_hms_opt(0, 0, 0).unwrap();

        match tz.from_local_datetime(&naive_dt) {
            LocalResult::None => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: None,
                is_ambiguous: true,
                ambiguity_reason: Some("Nonexistent time in DST gap".to_string()),
                context: context.clone(),
            }),
            LocalResult::Ambiguous(dt1, _) => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: Some(dt1.with_timezone(&Utc)),
                is_ambiguous: true,
                ambiguity_reason: Some("Ambiguous time in DST fold".to_string()),
                context: context.clone(),
            }),
            LocalResult::Single(dt) => Ok(ResolutionResult {
                original_phrase: original_phrase.to_string(),
                resolved_time: Some(dt.with_timezone(&Utc)),
                is_ambiguous: true,
                ambiguity_reason: Some("Missing hour: midnight assumed".to_string()),
                context: context.clone(),
            }),
        }
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
        }
    }

    fn ny_context() -> TimeContext {
        TimeContext {
            timezone: "America/New_York".to_string(),
            locale: "en-US".to_string(),
            reference_time: DateTime::parse_from_rfc3339("2025-10-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn test_explicit_date_preserves_original_phrase() {
        let context = test_context();
        let result = TimeResolver::resolve("  2024-10-20  ", &context).unwrap();
        assert_eq!(result.original_phrase, "  2024-10-20  ");
    }

    #[test]
    fn test_date_without_time_is_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.is_some());
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }

    #[test]
    fn test_explicit_datetime_not_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20 14:30:00", &context).unwrap();
        assert!(!result.is_ambiguous);
    }

    #[test]
    fn test_dst_gap_america_new_york() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-03-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("2025-03-09 02:30:00", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.resolved_time.is_none());
        assert!(result
            .ambiguity_reason
            .as_ref()
            .unwrap()
            .contains("DST gap"));
    }

    #[test]
    fn test_dst_fold_america_new_york() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-11-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("2025-11-02 01:30:00", &context).unwrap();
        assert!(result.is_ambiguous);
        assert!(result.resolved_time.is_some());
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("fold"));
    }

    #[test]
    fn test_tomorrow_uses_local_timezone() {
        let mut context = ny_context();
        context.reference_time = DateTime::parse_from_rfc3339("2025-10-15T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("tomorrow", &context).unwrap();
        assert!(result.resolved_time.is_some());
        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
    }

    #[test]
    fn test_weekday_names() {
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
            assert!(result.resolved_time.is_some());
        }
    }

    #[test]
    fn test_since_finds_past_occurrence() {
        let mut context = test_context();
        context.reference_time = DateTime::parse_from_rfc3339("2024-10-18T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let result = TimeResolver::resolve("since friday", &context).unwrap();
        assert!(result.resolved_time.is_some());

        let resolved = result.resolved_time.unwrap();
        assert!(resolved < context.reference_time);
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
}
