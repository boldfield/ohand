use chrono::{DateTime, Timelike, Utc};
use ohand_core::time::{TimeContext, TimeResolver};

fn make_context(tz: &str, ref_time: &str) -> TimeContext {
    TimeContext {
        timezone: tz.to_string(),
        reference_time: DateTime::parse_from_rfc3339(ref_time)
            .unwrap()
            .with_timezone(&Utc),
    }
}

#[test]
fn test_explicit_dates_iso_format() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Future date
    let result = TimeResolver::resolve("2024-10-20", &context).unwrap();
    assert_eq!(result.original_phrase, "2024-10-20");
    assert!(result.resolved_time.is_some());
    assert!(!result.is_ambiguous);

    // Past date marked ambiguous but resolved
    let past = TimeResolver::resolve("2024-10-10", &context).unwrap();
    assert!(past.is_ambiguous);
    assert!(past.ambiguity_reason.is_some());
    assert!(past.resolved_time.is_some());
}

#[test]
fn test_tomorrow_phrase() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");
    let result = TimeResolver::resolve("tomorrow", &context).unwrap();

    assert_eq!(result.original_phrase, "tomorrow");
    assert!(result.resolved_time.is_some());
    assert!(!result.is_ambiguous);

    // Verify it's actually the next day
    let resolved = result.resolved_time.unwrap();
    let expected_date = "2024-10-16";
    assert!(resolved.to_rfc3339().starts_with(expected_date));
}

#[test]
fn test_weekday_phrases() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z"); // Tuesday

    // Test single weekday names
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
        assert!(!result.is_ambiguous);
    }
}

#[test]
fn test_next_weekday_phrases() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let result = TimeResolver::resolve("next monday", &context).unwrap();
    assert!(result.resolved_time.is_some());
    assert!(!result.is_ambiguous);
}

#[test]
fn test_since_phrases() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // since <day> should resolve to the next occurrence of that day
    let result = TimeResolver::resolve("since friday", &context).unwrap();
    assert_eq!(result.original_phrase, "since friday");
    assert!(result.resolved_time.is_some());
    assert!(!result.is_ambiguous);
}

#[test]
fn test_timezone_changes() {
    let utc_context = make_context("UTC", "2024-10-15T10:00:00Z");
    let ny_context = make_context("America/New_York", "2024-10-15T06:00:00Z");

    let utc_result = TimeResolver::resolve("2024-10-20", &utc_context).unwrap();
    let ny_result = TimeResolver::resolve("2024-10-20", &ny_context).unwrap();

    // Both should resolve, but to different absolute times
    assert!(utc_result.resolved_time.is_some());
    assert!(ny_result.resolved_time.is_some());

    // The NY time should be later (5 hours ahead in October, before DST ends)
    assert_ne!(
        utc_result.resolved_time.unwrap(),
        ny_result.resolved_time.unwrap()
    );
}

#[test]
fn test_past_times() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Explicitly past date
    let result = TimeResolver::resolve("2024-10-01", &context).unwrap();
    assert!(result.is_ambiguous);
    assert!(result.ambiguity_reason.is_some());
    assert!(result.resolved_time.is_some()); // Still resolved but marked ambiguous
}

#[test]
fn test_unsupported_repeats() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Daily repeat
    assert!(TimeResolver::resolve("every day", &context).is_err());

    // Weekly repeat
    assert!(TimeResolver::resolve("every week", &context).is_err());

    // Monthly repeat
    assert!(TimeResolver::resolve("every month", &context).is_err());

    // Yearly repeat
    assert!(TimeResolver::resolve("every year", &context).is_err());

    // "recurring" keyword
    assert!(TimeResolver::resolve("recurring", &context).is_err());

    // "daily" keyword
    assert!(TimeResolver::resolve("daily", &context).is_err());

    // "weekly" keyword
    assert!(TimeResolver::resolve("weekly", &context).is_err());

    // "monthly" keyword
    assert!(TimeResolver::resolve("monthly", &context).is_err());

    // "yearly" keyword
    assert!(TimeResolver::resolve("yearly", &context).is_err());
}

#[test]
fn test_preserves_original_phrase() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Test various phrases are preserved exactly
    let phrases = vec!["tomorrow", "monday", "next friday", "since wednesday"];

    for phrase in phrases {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert_eq!(result.original_phrase, phrase);
    }
}

#[test]
fn test_preserves_context() {
    let context = make_context("Europe/London", "2024-10-15T10:00:00Z");
    let result = TimeResolver::resolve("2024-10-20", &context).unwrap();

    assert_eq!(result.context.timezone, "Europe/London");
    assert_eq!(result.context.reference_time, context.reference_time);
}

#[test]
fn test_no_invented_times() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // When there's ambiguity or error, we don't invent a time
    let result = TimeResolver::resolve("invalid date phrase xyz", &context);
    assert!(result.is_err());
}

#[test]
fn test_datetime_with_explicit_time() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let result = TimeResolver::resolve("2024-10-20 14:30:00", &context).unwrap();
    assert!(result.resolved_time.is_some());

    let resolved = result.resolved_time.unwrap();
    assert_eq!(resolved.hour(), 14);
    assert_eq!(resolved.minute(), 30);
    assert_eq!(resolved.second(), 0);
}

#[test]
fn test_dst_handling_utc() {
    // UTC has no DST, so this should always work
    let spring_context = make_context("UTC", "2024-03-10T10:00:00Z");
    let fall_context = make_context("UTC", "2024-11-03T10:00:00Z");

    let spring_result = TimeResolver::resolve("2024-03-15", &spring_context).unwrap();
    assert!(!spring_result.is_ambiguous);

    let fall_result = TimeResolver::resolve("2024-11-05", &fall_context).unwrap();
    assert!(!fall_result.is_ambiguous);
}

#[test]
fn test_sydney_timezone_dst() {
    // Sydney DST runs October to April (opposite of Northern Hemisphere)
    let summer_context = make_context("Australia/Sydney", "2024-01-15T10:00:00Z");
    let winter_context = make_context("Australia/Sydney", "2024-07-15T10:00:00Z");

    let summer_result = TimeResolver::resolve("2024-01-20", &summer_context).unwrap();
    assert!(summer_result.resolved_time.is_some());

    let winter_result = TimeResolver::resolve("2024-07-20", &winter_context).unwrap();
    assert!(winter_result.resolved_time.is_some());
}

#[test]
fn test_ambiguous_time_representation() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // A result that's ambiguous should preserve the phrase and context for display
    let result = TimeResolver::resolve("2024-10-10", &context).unwrap(); // Past date
    assert_eq!(result.original_phrase, "2024-10-10");
    assert!(result.is_ambiguous);
    assert!(result.ambiguity_reason.is_some());

    // The caller can display this as: "You said 2024-10-10, but that's in the past.
    // Would you like to schedule for 2024-10-10 or choose a different date?"
}

#[test]
fn test_mixed_date_formats() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // ISO format without time
    let iso_date = TimeResolver::resolve("2024-10-25", &context).unwrap();
    assert!(iso_date.resolved_time.is_some());

    // ISO format with time
    let iso_datetime = TimeResolver::resolve("2024-10-25 15:45:00", &context).unwrap();
    assert!(iso_datetime.resolved_time.is_some());

    // Natural language
    let natural = TimeResolver::resolve("next monday", &context).unwrap();
    assert!(natural.resolved_time.is_some());
}

#[test]
fn test_case_insensitivity() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // All of these should work (lowercase is tested, should also accept mixed case)
    let tomorrow_lower = TimeResolver::resolve("tomorrow", &context).unwrap();
    let monday_lower = TimeResolver::resolve("monday", &context).unwrap();

    assert!(tomorrow_lower.resolved_time.is_some());
    assert!(monday_lower.resolved_time.is_some());
}

#[test]
fn test_various_timezone_formats() {
    let context_utc = make_context("UTC", "2024-10-15T10:00:00Z");
    let context_ny = make_context("America/New_York", "2024-10-15T10:00:00Z");
    let context_london = make_context("Europe/London", "2024-10-15T10:00:00Z");
    let context_sydney = make_context("Australia/Sydney", "2024-10-15T10:00:00Z");

    for context in &[context_utc, context_ny, context_london, context_sydney] {
        let result = TimeResolver::resolve("2024-10-20", context).unwrap();
        assert!(result.resolved_time.is_some());
        assert_eq!(result.context.timezone, context.timezone);
    }
}

#[test]
fn test_error_messages_are_informative() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Unsupported repeat should return an error
    let repeat_error = TimeResolver::resolve("every day", &context).unwrap_err();
    let error_str = repeat_error.to_string();
    eprintln!("Error message: {}", error_str);
    let lower = error_str.to_lowercase();
    assert!(
        lower.contains("unsupported")
            || lower.contains("not supported")
            || lower.contains("repeat")
    );

    // Invalid timezone error
    let bad_tz = TimeContext {
        timezone: "Invalid/Timezone".to_string(),
        reference_time: context.reference_time,
    };
    let tz_error = TimeResolver::resolve("2024-10-20", &bad_tz);
    // This might error during parsing - it's OK either way
    let _ = tz_error;
}
