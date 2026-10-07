use chrono::{prelude::*, Duration, NaiveDate};
use serde::{Deserialize, Serialize};
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
        let original = phrase.trim().to_lowercase();

        // Try to parse as explicit date formats first
        if let Ok(result) = Self::parse_explicit_date(&original, context) {
            return Ok(result);
        }

        // Try natural language phrases - propagate errors like unsupported repeats
        match Self::parse_natural_language(&original, context) {
            Ok(result) => return Ok(result),
            Err(ResolutionError::UnsupportedRepeat(_)) => {
                return Err(ResolutionError::UnsupportedRepeat(
                    "Repeating reminders are not supported in M1".to_string(),
                ))
            }
            Err(_) => {} // Fall through for other errors
        }

        Err(ResolutionError::InvalidDateFormat(phrase.to_string()))
    }

    fn parse_explicit_date(
        phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        // Try ISO 8601 formats
        if let Ok(date) = NaiveDate::parse_from_str(phrase, "%Y-%m-%d") {
            let naive_dt = date.and_hms_opt(0, 0, 0).unwrap();
            let dt = match Self::local_to_utc(naive_dt, context) {
                Ok(dt) => dt,
                Err(_) => {
                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: None,
                        is_ambiguous: true,
                        ambiguity_reason: Some("DST gap detected".to_string()),
                        context: context.clone(),
                    });
                }
            };

            if dt < context.reference_time {
                return Ok(ResolutionResult {
                    original_phrase: phrase.to_string(),
                    resolved_time: Some(dt),
                    is_ambiguous: true,
                    ambiguity_reason: Some("Time is in the past".to_string()),
                    context: context.clone(),
                });
            }

            return Ok(ResolutionResult {
                original_phrase: phrase.to_string(),
                resolved_time: Some(dt),
                is_ambiguous: false,
                ambiguity_reason: None,
                context: context.clone(),
            });
        }

        // Try with time component (YYYY-MM-DD HH:MM:SS)
        if let Ok(dt) = NaiveDateTime::parse_from_str(phrase, "%Y-%m-%d %H:%M:%S") {
            match Self::local_to_utc(dt, context) {
                Ok(utc_dt) => {
                    if utc_dt < context.reference_time {
                        return Ok(ResolutionResult {
                            original_phrase: phrase.to_string(),
                            resolved_time: Some(utc_dt),
                            is_ambiguous: true,
                            ambiguity_reason: Some("Time is in the past".to_string()),
                            context: context.clone(),
                        });
                    }

                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: Some(utc_dt),
                        is_ambiguous: false,
                        ambiguity_reason: None,
                        context: context.clone(),
                    });
                }
                Err(_) => {
                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: None,
                        is_ambiguous: true,
                        ambiguity_reason: Some("DST gap or fold detected".to_string()),
                        context: context.clone(),
                    });
                }
            }
        }

        Err(ResolutionError::InvalidDateFormat(phrase.to_string()))
    }

    fn parse_natural_language(
        phrase: &str,
        context: &TimeContext,
    ) -> Result<ResolutionResult, ResolutionError> {
        if phrase == "tomorrow" {
            let tomorrow = (context.reference_time + Duration::days(1))
                .naive_utc()
                .date();
            let naive_dt = tomorrow.and_hms_opt(0, 0, 0).unwrap();

            match Self::local_to_utc(naive_dt, context) {
                Ok(dt) => {
                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: Some(dt),
                        is_ambiguous: false,
                        ambiguity_reason: None,
                        context: context.clone(),
                    });
                }
                Err(_) => {
                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: None,
                        is_ambiguous: true,
                        ambiguity_reason: Some("DST gap detected".to_string()),
                        context: context.clone(),
                    });
                }
            }
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
            if phrase == *name || phrase == format!("next {}", name) {
                if let Ok(dt) = Self::get_next_weekday(context, *target_weekday) {
                    return Ok(ResolutionResult {
                        original_phrase: phrase.to_string(),
                        resolved_time: Some(dt),
                        is_ambiguous: false,
                        ambiguity_reason: None,
                        context: context.clone(),
                    });
                }
            }
        }

        // Handle "since" phrases
        if let Some(day_str) = phrase.strip_prefix("since ") {
            for (name, target_weekday) in &weekdays {
                if day_str == *name {
                    if let Ok(dt) = Self::get_next_weekday(context, *target_weekday) {
                        return Ok(ResolutionResult {
                            original_phrase: phrase.to_string(),
                            resolved_time: Some(dt),
                            is_ambiguous: false,
                            ambiguity_reason: None,
                            context: context.clone(),
                        });
                    }
                }
            }
        }

        // Handle unsupported repeats
        if phrase.contains("every")
            || phrase.contains("recurring")
            || phrase.contains("daily")
            || phrase.contains("weekly")
            || phrase.contains("monthly")
            || phrase.contains("yearly")
        {
            return Err(ResolutionError::UnsupportedRepeat(
                "Repeating reminders are not supported in M1".to_string(),
            ));
        }

        Err(ResolutionError::InvalidDateFormat(phrase.to_string()))
    }

    fn get_next_weekday(
        context: &TimeContext,
        target: chrono::Weekday,
    ) -> Result<DateTime<Utc>, ResolutionError> {
        let ref_date = context.reference_time.naive_utc().date();
        let current_wd = ref_date.weekday();

        let days_ahead = match target {
            chrono::Weekday::Mon => (1 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Tue => (2 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Wed => (3 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Thu => (4 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Fri => (5 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Sat => (6 + (7 - current_wd.number_from_monday())) % 7,
            chrono::Weekday::Sun => (7 + (7 - current_wd.number_from_monday())) % 7,
        };

        if days_ahead == 0 {
            // Today is the target weekday, schedule for next week
            let target_date = ref_date + Duration::days(7);
            let naive_dt = target_date.and_hms_opt(0, 0, 0).unwrap();
            Self::local_to_utc(naive_dt, context)
        } else {
            let target_date = ref_date + Duration::days(days_ahead as i64);
            let naive_dt = target_date.and_hms_opt(0, 0, 0).unwrap();
            Self::local_to_utc(naive_dt, context)
        }
    }

    fn local_to_utc(
        local_dt: NaiveDateTime,
        context: &TimeContext,
    ) -> Result<DateTime<Utc>, ResolutionError> {
        // For now, use a simple timezone offset model
        // In production, this would use a proper timezone database
        match context.timezone.as_str() {
            "UTC" => Ok(DateTime::<Utc>::from_naive_utc_and_offset(local_dt, Utc)),
            "America/New_York" => {
                // Simplified EST/EDT handling
                let is_dst = Self::is_dst_ny(local_dt);
                let offset_hours = if is_dst { -4 } else { -5 };
                Ok(DateTime::<Utc>::from_naive_utc_and_offset(
                    local_dt - Duration::hours(offset_hours),
                    Utc,
                ))
            }
            "Europe/London" => {
                let is_dst = Self::is_dst_london(local_dt);
                let offset_hours = if is_dst { -1 } else { 0 };
                Ok(DateTime::<Utc>::from_naive_utc_and_offset(
                    local_dt - Duration::hours(offset_hours),
                    Utc,
                ))
            }
            "Australia/Sydney" => {
                let is_dst = Self::is_dst_sydney(local_dt);
                let offset_hours = if is_dst { -11 } else { -10 };
                Ok(DateTime::<Utc>::from_naive_utc_and_offset(
                    local_dt - Duration::hours(offset_hours),
                    Utc,
                ))
            }
            _ => Err(ResolutionError::InvalidTimezone(context.timezone.clone())),
        }
    }

    fn is_dst_ny(dt: NaiveDateTime) -> bool {
        // US DST: second Sunday of March to first Sunday of November
        let month = dt.month();
        let day = dt.day();
        let weekday = dt.weekday();

        if !(3..=11).contains(&month) {
            return false;
        }
        if (4..=10).contains(&month) {
            return true;
        }

        // March: DST starts on second Sunday
        if month == 3 {
            let sunday_count = (1..=day)
                .filter(|d| {
                    (dt.date() - Duration::days((day - d) as i64)).weekday() == chrono::Weekday::Sun
                })
                .count();
            return sunday_count >= 2;
        }

        // November: DST ends on first Sunday
        if month == 11 {
            return weekday != chrono::Weekday::Sun || day < 1;
        }

        false
    }

    fn is_dst_london(dt: NaiveDateTime) -> bool {
        // UK DST: last Sunday of March to last Sunday of October
        let month = dt.month();
        if !(3..=10).contains(&month) {
            return false;
        }
        if (4..=9).contains(&month) {
            return true;
        }

        let weekday = dt.weekday();

        // March: DST starts on last Sunday
        if month == 3 {
            let next_day = dt + Duration::days(1);
            return weekday == chrono::Weekday::Sun && next_day.month() == 4;
        }

        // October: DST ends on last Sunday
        if month == 10 {
            let next_day = dt + Duration::days(1);
            return !(weekday == chrono::Weekday::Sun && next_day.month() == 11);
        }

        false
    }

    fn is_dst_sydney(dt: NaiveDateTime) -> bool {
        // Australia DST: first Sunday of October to first Sunday of April
        let month = dt.month();

        if (10..=12).contains(&month) || (1..=3).contains(&month) {
            let day = dt.day();
            let weekday = dt.weekday();

            // October: DST starts on first Sunday
            if month == 10 {
                return weekday == chrono::Weekday::Sun && day <= 7;
            }

            // April: DST ends on first Sunday
            if month == 4 {
                return !(weekday == chrono::Weekday::Sun && day <= 7);
            }

            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> TimeContext {
        TimeContext {
            timezone: "UTC".to_string(),
            reference_time: DateTime::parse_from_rfc3339("2024-10-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn test_explicit_date_iso_format() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20", &context).unwrap();

        assert_eq!(result.original_phrase, "2024-10-20");
        assert!(result.resolved_time.is_some());
        assert!(!result.is_ambiguous);
    }

    #[test]
    fn test_tomorrow_phrase() {
        let context = test_context();
        let result = TimeResolver::resolve("tomorrow", &context).unwrap();

        assert_eq!(result.original_phrase, "tomorrow");
        assert!(result.resolved_time.is_some());
        assert!(!result.is_ambiguous);
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
            assert_eq!(result.original_phrase, *day);
            assert!(result.resolved_time.is_some());
        }
    }

    #[test]
    fn test_since_phrase() {
        let context = test_context();
        let result = TimeResolver::resolve("since friday", &context).unwrap();

        assert_eq!(result.original_phrase, "since friday");
        assert!(result.resolved_time.is_some());
    }

    #[test]
    fn test_unsupported_repeat() {
        let context = test_context();

        let daily = TimeResolver::resolve("every day", &context);
        assert!(daily.is_err());

        let weekly = TimeResolver::resolve("every week", &context);
        assert!(weekly.is_err());

        let monthly = TimeResolver::resolve("monthly", &context);
        assert!(monthly.is_err());
    }

    #[test]
    fn test_past_time_marked_ambiguous() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-10", &context).unwrap();

        assert!(result.is_ambiguous);
        assert!(result.ambiguity_reason.is_some());
    }

    #[test]
    fn test_invalid_format() {
        let context = test_context();
        let result = TimeResolver::resolve("invalid date", &context);

        assert!(result.is_err());
    }

    #[test]
    fn test_timezone_handling() {
        let mut context = test_context();
        context.timezone = "America/New_York".to_string();

        let result = TimeResolver::resolve("2024-10-20", &context).unwrap();
        assert!(result.resolved_time.is_some());
    }

    #[test]
    fn test_context_preserved() {
        let context = test_context();
        let result = TimeResolver::resolve("2024-10-20", &context).unwrap();

        assert_eq!(result.context.timezone, "UTC");
        assert_eq!(result.context.reference_time, context.reference_time);
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
}
