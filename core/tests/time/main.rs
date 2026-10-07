use chrono::{DateTime, NaiveDate, Timelike, Utc};
use ohand_core::time::{TimeContext, TimeResolver};

fn make_context(tz: &str, ref_time: &str) -> TimeContext {
    TimeContext {
        timezone: tz.to_string(),
        locale: "en-US".to_string(),
        reference_time: DateTime::parse_from_rfc3339(ref_time)
            .unwrap()
            .with_timezone(&Utc),
        utc_offset_at_capture: None,
        calendar: None,
    }
}

#[test]
fn test_explicit_dates_table() {
    // Table: (timezone, reference_time, phrase, expect_date_only, expect_past)
    let test_cases = vec![
        ("UTC", "2024-10-15T10:00:00Z", "2024-10-20", true, false), // Future date, no time
        ("UTC", "2024-10-15T10:00:00Z", "2024-10-10", true, true),  // Past date, no time
        ("UTC", "2024-10-15T10:00:00Z", "2024-10-15", true, false), // Today, no time
    ];

    for (tz, ref_time, phrase, expect_date_only, expect_past) in test_cases {
        let context = make_context(tz, ref_time);
        let result = TimeResolver::resolve(phrase, &context).unwrap();

        assert_eq!(result.original_phrase, phrase, "Failed for: {}", phrase);
        assert!(result.is_ambiguous);
        assert_eq!(
            result.resolved_time.is_none(),
            expect_date_only,
            "Mismatch for {}: expected date_only={}",
            phrase,
            expect_date_only
        );
        assert!(result.resolved_date.is_some());

        if expect_past {
            assert!(result.ambiguity_reason.as_ref().unwrap().contains("past"));
        } else {
            assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
        }
    }
}

#[test]
fn test_datetime_with_explicit_time() {
    let test_cases = vec![
        ("UTC", "2024-10-15T10:00:00Z", "2024-10-20 14:30:00", 14, 30),
        ("UTC", "2024-10-15T10:00:00Z", "2024-10-10 09:15:00", 9, 15),
    ];

    for (tz, ref_time, phrase, expected_hour, expected_minute) in test_cases {
        let context = make_context(tz, ref_time);
        let result = TimeResolver::resolve(phrase, &context).unwrap();

        assert!(result.resolved_time.is_some());
        let resolved = result.resolved_time.unwrap();
        assert_eq!(resolved.hour(), expected_hour);
        assert_eq!(resolved.minute(), expected_minute);
    }
}

#[test]
fn test_tomorrow_phrase() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");
    let result = TimeResolver::resolve("tomorrow", &context).unwrap();

    assert_eq!(result.original_phrase, "tomorrow");
    assert!(result.resolved_date.is_some());
    assert!(result.resolved_time.is_none());
    assert!(result.is_ambiguous);
    assert_eq!(
        result.resolved_date.unwrap(),
        NaiveDate::from_ymd_opt(2024, 10, 16).unwrap()
    );
}

#[test]
fn test_weekday_phrases_table() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z"); // Tuesday

    // Expected next occurrence dates for each weekday starting from 2024-10-15 (Tuesday)
    let test_cases = vec![
        ("monday", NaiveDate::from_ymd_opt(2024, 10, 21).unwrap()),
        ("tuesday", NaiveDate::from_ymd_opt(2024, 10, 22).unwrap()),
        ("wednesday", NaiveDate::from_ymd_opt(2024, 10, 16).unwrap()),
        ("thursday", NaiveDate::from_ymd_opt(2024, 10, 17).unwrap()),
        ("friday", NaiveDate::from_ymd_opt(2024, 10, 18).unwrap()),
        ("saturday", NaiveDate::from_ymd_opt(2024, 10, 19).unwrap()),
        ("sunday", NaiveDate::from_ymd_opt(2024, 10, 20).unwrap()),
    ];

    for (day_name, expected_date) in test_cases {
        let result = TimeResolver::resolve(day_name, &context).unwrap();
        assert_eq!(result.original_phrase, day_name);
        assert_eq!(
            result.resolved_date,
            Some(expected_date),
            "Failed for: {}",
            day_name
        );
        assert!(result.resolved_time.is_none());
        assert!(result.is_ambiguous);
    }
}

#[test]
fn test_next_weekday_phrases_table() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z"); // Tuesday

    let test_cases = vec![
        (
            "next monday",
            NaiveDate::from_ymd_opt(2024, 10, 21).unwrap(),
        ),
        (
            "next tuesday",
            NaiveDate::from_ymd_opt(2024, 10, 22).unwrap(),
        ),
        (
            "next wednesday",
            NaiveDate::from_ymd_opt(2024, 10, 16).unwrap(),
        ),
    ];

    for (phrase, expected_date) in test_cases {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert_eq!(result.original_phrase, phrase);
        assert_eq!(
            result.resolved_date,
            Some(expected_date),
            "Failed for: {}",
            phrase
        );
        assert!(result.resolved_time.is_none());
    }
}

#[test]
fn test_since_phrases_table() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z"); // Tuesday

    let test_cases = vec![
        (
            "since monday",
            NaiveDate::from_ymd_opt(2024, 10, 14).unwrap(),
        ),
        (
            "since tuesday",
            NaiveDate::from_ymd_opt(2024, 10, 8).unwrap(),
        ), // Previous Tuesday
        (
            "since wednesday",
            NaiveDate::from_ymd_opt(2024, 10, 9).unwrap(),
        ),
        (
            "since friday",
            NaiveDate::from_ymd_opt(2024, 10, 11).unwrap(),
        ),
    ];

    for (phrase, expected_date) in test_cases {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert_eq!(result.original_phrase, phrase);
        assert_eq!(
            result.resolved_date,
            Some(expected_date),
            "Failed for: {}",
            phrase
        );
        assert!(result.resolved_time.is_none());
    }
}

#[test]
fn test_dst_gap_america_new_york() {
    let mut context = make_context("America/New_York", "2025-03-08T10:00:00Z");
    context.utc_offset_at_capture = Some(-18000); // EST: -5 hours
    context.calendar = Some("gregorian".to_string());

    let result = TimeResolver::resolve("2025-03-09 02:30:00", &context).unwrap();
    assert!(result.is_ambiguous);
    assert!(result.resolved_time.is_none());
    assert!(result.candidates.is_empty());
    assert!(result
        .ambiguity_reason
        .as_ref()
        .unwrap()
        .contains("DST gap"));
}

#[test]
fn test_dst_fold_america_new_york() {
    let mut context = make_context("America/New_York", "2025-11-01T10:00:00Z");
    context.utc_offset_at_capture = Some(-14400); // EDT: -4 hours
    context.calendar = Some("gregorian".to_string());

    let result = TimeResolver::resolve("2025-11-02 01:30:00", &context).unwrap();
    assert!(result.is_ambiguous);
    assert!(result.resolved_time.is_none());
    assert_eq!(
        result.candidates.len(),
        2,
        "DST fold should have two candidates"
    );
    assert!(result.ambiguity_reason.as_ref().unwrap().contains("fold"));
}

#[test]
fn test_timezone_changes_with_exact_times() {
    let utc_context = make_context("UTC", "2024-10-15T10:00:00Z");
    let ny_context = make_context("America/New_York", "2024-10-15T06:00:00Z");

    let utc_result = TimeResolver::resolve("2024-10-20 12:00:00", &utc_context).unwrap();
    let ny_result = TimeResolver::resolve("2024-10-20 12:00:00", &ny_context).unwrap();

    // Both should resolve with times
    assert!(utc_result.resolved_time.is_some());
    assert!(ny_result.resolved_time.is_some());

    // Local 12:00 UTC is 12:00 UTC
    // Local 12:00 in NY (EDT) is 16:00 UTC (4 hours later)
    let utc_instant = utc_result.resolved_time.unwrap();
    let ny_instant = ny_result.resolved_time.unwrap();

    // Difference should be 4 hours (14400 seconds)
    assert_eq!(ny_instant.timestamp() - utc_instant.timestamp(), 14400);
}

#[test]
fn test_past_times_flagged() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let past_date_result = TimeResolver::resolve("2024-10-10", &context).unwrap();
    assert!(past_date_result.is_ambiguous);
    assert!(past_date_result
        .ambiguity_reason
        .as_ref()
        .unwrap()
        .contains("past"));

    let past_datetime_result = TimeResolver::resolve("2024-10-10 14:30:00", &context).unwrap();
    assert!(past_datetime_result.is_ambiguous);
    assert!(past_datetime_result
        .ambiguity_reason
        .as_ref()
        .unwrap()
        .contains("past"));
}

#[test]
fn test_unsupported_repeats_table() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let repeat_phrases = vec![
        "every day",
        "every week",
        "every month",
        "every year",
        "recurring",
        "daily",
        "weekly",
        "monthly",
        "yearly",
    ];

    for phrase in repeat_phrases {
        let result = TimeResolver::resolve(phrase, &context);
        assert!(result.is_err(), "Expected error for: {}", phrase);
        if let Err(e) = result {
            assert!(e.to_string().to_lowercase().contains("unsupported"));
        }
    }
}

#[test]
fn test_repeat_keyword_not_triggered_by_substring() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // "everyone" contains "every" but should not trigger repeat detection
    let result = TimeResolver::resolve("meet everyone tomorrow", &context);
    // This should fail as InvalidDateFormat, not UnsupportedRepeat
    assert!(result.is_err());
    if let Err(e) = result {
        let error_str = e.to_string().to_lowercase();
        assert!(error_str.contains("format") || error_str.contains("invalid"));
        assert!(!error_str.contains("repeat") || !error_str.contains("unsupported"));
    }
}

#[test]
fn test_preserves_original_phrase_exact() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let test_cases = vec![
        "tomorrow",
        "monday",
        "next friday",
        "since wednesday",
        "  2024-10-20  ",
        "2024-10-20 14:30:00",
    ];

    for phrase in test_cases {
        let result = TimeResolver::resolve(phrase, &context)
            .unwrap_or_else(|_| panic!("Failed to resolve: {}", phrase));
        assert_eq!(
            result.original_phrase, phrase,
            "Phrase not preserved exactly: {}",
            phrase
        );
    }
}

#[test]
fn test_context_preserved_in_result() {
    let context = make_context("Europe/London", "2024-10-15T10:00:00Z");
    let result = TimeResolver::resolve("2024-10-20", &context).unwrap();

    assert_eq!(result.context.timezone, "Europe/London");
    assert_eq!(result.context.locale, "en-US");
    assert_eq!(result.context.reference_time, context.reference_time);
}

#[test]
fn test_no_resolved_time_for_date_only_phrases() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let date_only_phrases = vec![
        "2024-10-20",
        "tomorrow",
        "monday",
        "next friday",
        "since wednesday",
    ];

    for phrase in date_only_phrases {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert!(
            result.resolved_time.is_none(),
            "Date-only phrase '{}' should not have resolved_time",
            phrase
        );
        assert!(
            result.resolved_date.is_some(),
            "Date-only phrase '{}' should have resolved_date",
            phrase
        );
    }
}

#[test]
fn test_complete_datetime_has_resolved_time() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let datetime_phrases = vec![
        "2024-10-20 14:30:00",
        "2024-10-10 09:15:00",
        "2025-06-01 23:59:59",
    ];

    for phrase in datetime_phrases {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert!(
            result.resolved_time.is_some(),
            "Complete datetime '{}' should have resolved_time",
            phrase
        );
    }
}

#[test]
fn test_invalid_timezone_error() {
    let mut context = make_context("UTC", "2024-10-15T10:00:00Z");
    context.timezone = "Invalid/Timezone".to_string();

    let result = TimeResolver::resolve("2024-10-20", &context);
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(e.to_string().to_lowercase().contains("timezone"));
    }
}

#[test]
fn test_case_insensitivity() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    // Test that mixed case is normalized
    let tomorrow_lower = TimeResolver::resolve("tomorrow", &context).unwrap();
    let tomorrow_mixed = TimeResolver::resolve("Tomorrow", &context).unwrap();
    let tomorrow_upper = TimeResolver::resolve("TOMORROW", &context).unwrap();

    assert_eq!(tomorrow_lower.resolved_date, tomorrow_mixed.resolved_date);
    assert_eq!(tomorrow_lower.resolved_date, tomorrow_upper.resolved_date);

    // But original phrases are preserved
    assert_eq!(tomorrow_lower.original_phrase, "tomorrow");
    assert_eq!(tomorrow_mixed.original_phrase, "Tomorrow");
    assert_eq!(tomorrow_upper.original_phrase, "TOMORROW");
}
