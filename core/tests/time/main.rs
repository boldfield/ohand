use chrono::{DateTime, NaiveDate, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use ohand_core::time::{
    span_covers_time_expression, AmbiguityKind, ResolutionError, TimeContext, TimeResolver,
};

fn make_context(tz: &str, ref_time: &str) -> TimeContext {
    let reference_time = DateTime::parse_from_rfc3339(ref_time)
        .unwrap()
        .with_timezone(&Utc);
    let zone: Tz = tz.parse().unwrap();
    let utc_offset_at_capture = zone
        .offset_from_utc_datetime(&reference_time.naive_utc())
        .fix()
        .local_minus_utc();
    TimeContext {
        timezone: tz.to_string(),
        locale: "en-US".to_string(),
        reference_time,
        utc_offset_at_capture,
        calendar: "gregorian".to_string(),
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
            assert!(result.is_past);
        }
        assert!(result.ambiguity_reason.as_ref().unwrap().contains("hour"));
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
    let context = make_context("America/New_York", "2025-03-08T10:00:00Z");

    let result = TimeResolver::resolve("2025-03-09 02:30:00", &context).unwrap();
    assert!(result.is_ambiguous);
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstGap));
    assert_eq!(
        result.resolved_local,
        NaiveDate::from_ymd_opt(2025, 3, 9).and_then(|d| d.and_hms_opt(2, 30, 0))
    );
    assert!(!result.is_past);
    assert!(result.resolved_time.is_none());
    assert!(result.candidates.is_empty());
    assert!(result.resolved_date.is_some());
    assert!(result
        .ambiguity_reason
        .as_ref()
        .unwrap()
        .contains("DST gap"));
}

#[test]
fn test_dst_fold_america_new_york() {
    let context = make_context("America/New_York", "2025-11-01T10:00:00Z");

    let result = TimeResolver::resolve("2025-11-02 01:30:00", &context).unwrap();
    assert!(result.is_ambiguous);
    assert!(result.resolved_time.is_none());
    assert_eq!(
        result.candidates.len(),
        2,
        "DST fold should have two candidates"
    );
    assert!(result.resolved_date.is_some());
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstFold));
    assert_eq!(
        result.resolved_local,
        NaiveDate::from_ymd_opt(2025, 11, 2).and_then(|d| d.and_hms_opt(1, 30, 0))
    );
    assert!(!result.is_past);
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
    assert!(past_date_result.is_past);
    assert!(past_date_result
        .ambiguity_reason
        .as_ref()
        .unwrap()
        .contains("hour"));

    let past_datetime_result = TimeResolver::resolve("2024-10-10 14:30:00", &context).unwrap();
    assert!(past_datetime_result.is_ambiguous);
    assert!(past_datetime_result.is_past);
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

#[test]
fn test_past_date_in_non_utc_zones() {
    // Test case from reviewer: America/New_York, reference 2024-10-16T02:00Z (local Oct 15 22:00)
    // Phrase "2024-10-15" is TODAY locally, not past
    let context = make_context("America/New_York", "2024-10-16T02:00:00Z");
    let result = TimeResolver::resolve("2024-10-15", &context).unwrap();
    assert!(result.is_ambiguous);
    assert!(result.resolved_date.is_some());
    assert!(!result.is_past, "2024-10-15 is today locally in New York");

    // Test case: Australia/Sydney, reference 2024-10-15T22:00Z (local Oct 16 09:00)
    // Phrase "2024-10-15" is YESTERDAY locally
    let context = make_context("Australia/Sydney", "2024-10-15T22:00:00Z");
    let result = TimeResolver::resolve("2024-10-15", &context).unwrap();
    assert!(result.is_past, "2024-10-15 is yesterday locally in Sydney");

    // Test case: UTC, reference 2024-10-15T10:00Z
    // Phrase "2024-10-15" is TODAY
    let context = make_context("UTC", "2024-10-15T10:00:00Z");
    let result = TimeResolver::resolve("2024-10-15", &context).unwrap();
    assert!(!result.is_past, "2024-10-15 is today in UTC");
}

#[test]
fn test_past_dst_gap_and_fold_table() {
    // (reference, phrase, expected kind, expected is_past)
    let test_cases = vec![
        (
            "2025-12-01T12:00:00Z",
            "2025-03-09 02:30:00",
            AmbiguityKind::DstGap,
            true,
        ),
        (
            "2025-12-01T12:00:00Z",
            "2025-11-02 01:30:00",
            AmbiguityKind::DstFold,
            true,
        ),
        (
            "2025-03-08T10:00:00Z",
            "2025-03-09 02:30:00",
            AmbiguityKind::DstGap,
            false,
        ),
        (
            "2025-11-01T10:00:00Z",
            "2025-11-02 01:30:00",
            AmbiguityKind::DstFold,
            false,
        ),
    ];

    for (ref_time, phrase, expected_kind, expected_past) in test_cases {
        let context = make_context("America/New_York", ref_time);
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(expected_kind),
            "{phrase} @ {ref_time}"
        );
        assert_eq!(result.is_past, expected_past, "{phrase} @ {ref_time}");
        assert!(result.resolved_time.is_none());
        assert!(result.resolved_local.is_some());
    }
}

#[test]
fn test_wrong_utc_offset_rejected() {
    let mut context = make_context("America/New_York", "2025-07-01T12:00:00Z");
    context.utc_offset_at_capture = 0;
    let err = TimeResolver::resolve("2025-07-02 09:00:00", &context).unwrap_err();
    assert!(matches!(err, ResolutionError::InconsistentContext(_)));

    // EST offset is wrong during EDT
    context.utc_offset_at_capture = -18000;
    let err = TimeResolver::resolve("2025-07-02 09:00:00", &context).unwrap_err();
    assert!(matches!(err, ResolutionError::InconsistentContext(_)));

    context.utc_offset_at_capture = -14400;
    assert!(TimeResolver::resolve("2025-07-02 09:00:00", &context).is_ok());
}

#[test]
fn test_non_gregorian_calendar_rejected() {
    let mut context = make_context("UTC", "2024-10-15T10:00:00Z");
    context.calendar = "hebrew".to_string();
    let err = TimeResolver::resolve("2024-10-20 12:00:00", &context).unwrap_err();
    assert!(matches!(err, ResolutionError::InvalidCalendar(_)));
}

#[test]
fn test_ambiguity_kinds_for_missing_hour_and_past() {
    let context = make_context("UTC", "2024-10-15T10:00:00Z");

    let date_only = TimeResolver::resolve("2024-10-10", &context).unwrap();
    assert_eq!(date_only.ambiguity_kind, Some(AmbiguityKind::MissingHour));
    assert!(date_only.is_past);

    let past = TimeResolver::resolve("2024-10-10 14:30:00", &context).unwrap();
    assert_eq!(past.ambiguity_kind, Some(AmbiguityKind::Past));
    assert!(past.resolved_time.is_some());
}

// ---- clock times ----

const NY: &str = "America/New_York";
// Thursday 2026-10-08 10:00 in New York (EDT).
const NY_THURSDAY: &str = "2026-10-08T14:00:00Z";

fn resolved_utc(tz: &str, ref_time: &str, phrase: &str) -> String {
    let context = make_context(tz, ref_time);
    let result = TimeResolver::resolve(phrase, &context).unwrap();
    assert!(
        !result.is_ambiguous,
        "{phrase:?} should resolve: {:?}",
        result.ambiguity_reason
    );
    result.resolved_time.unwrap().to_rfc3339()
}

fn assert_ambiguous_without_instant(tz: &str, ref_time: &str, phrase: &str) {
    let context = make_context(tz, ref_time);
    let result = TimeResolver::resolve(phrase, &context).unwrap();
    assert!(result.is_ambiguous, "{phrase:?} must be ambiguous");
    assert!(
        result.resolved_time.is_none(),
        "{phrase:?} must not resolve"
    );
    assert!(result.candidates.is_empty() || result.ambiguity_kind == Some(AmbiguityKind::DstFold));
}

#[test]
fn clock_grammar_forms_resolve_table() {
    // Friday 2026-10-09 15:00 EDT == 19:00Z.
    let friday_three_pm = "2026-10-09T19:00:00+00:00";
    let cases = [
        ("Friday at 3 p.m.", friday_three_pm),
        ("friday at 3pm", friday_three_pm),
        ("FRIDAY AT 3 PM", friday_three_pm),
        ("Friday 3 p.m", friday_three_pm),
        ("on Friday at 3:00 pm", friday_three_pm),
        ("Friday at 15:00", friday_three_pm),
        ("at 3 p.m. on Friday", friday_three_pm),
        ("3 p.m. Friday", friday_three_pm),
        ("3pm friday", friday_three_pm),
        ("Friday 3:00 p.m.", friday_three_pm),
        ("2026-10-09 at 3 p.m.", friday_three_pm),
        ("2026-10-09 15:00", friday_three_pm),
        ("tomorrow at 9 a.m.", "2026-10-09T13:00:00+00:00"),
        ("tomorrow at 9:30 am", "2026-10-09T13:30:00+00:00"),
        ("today at 5 p.m.", "2026-10-08T21:00:00+00:00"),
        ("today at noon", "2026-10-08T16:00:00+00:00"),
        ("tomorrow at midnight", "2026-10-09T04:00:00+00:00"),
        ("tomorrow at 12 a.m.", "2026-10-09T04:00:00+00:00"),
        ("tomorrow at 12 p.m.", "2026-10-09T16:00:00+00:00"),
        ("tomorrow at 00:00", "2026-10-09T04:00:00+00:00"),
        ("tomorrow at 23:59", "2026-10-10T03:59:00+00:00"),
        ("at 5 p.m.", "2026-10-08T21:00:00+00:00"),
        ("5pm", "2026-10-08T21:00:00+00:00"),
        ("next monday at 8 a.m.", "2026-10-12T12:00:00+00:00"),
    ];
    for (phrase, expected) in cases {
        assert_eq!(resolved_utc(NY, NY_THURSDAY, phrase), expected, "{phrase}");
    }
}

#[test]
fn clock_phrase_resolves_with_local_fields() {
    let context = make_context(NY, NY_THURSDAY);
    let result = TimeResolver::resolve("Friday at 3 p.m.", &context).unwrap();
    assert_eq!(
        result.resolved_local.unwrap().to_string(),
        "2026-10-09 15:00:00"
    );
    assert_eq!(result.original_phrase, "Friday at 3 p.m.");
    assert!(!result.is_past);
}

#[test]
fn clock_without_day_is_today_only_when_later() {
    // 10:00 local: 5 p.m. is later today, 9 a.m. is not, and 10 a.m. is not later.
    assert_eq!(
        resolved_utc(NY, NY_THURSDAY, "at 5 p.m."),
        "2026-10-08T21:00:00+00:00"
    );
    for phrase in ["at 9 a.m.", "at 10 a.m.", "9am", "at midnight"] {
        let context = make_context(NY, NY_THURSDAY);
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert!(result.is_ambiguous, "{phrase}");
        assert!(result.resolved_time.is_none(), "{phrase}");
        assert!(
            !result.is_past,
            "{phrase} is a missing day, not a past time"
        );
        assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::MissingDay));
    }
}

#[test]
fn bare_hour_and_unsupported_clock_forms_are_ambiguous_or_rejected() {
    for phrase in [
        "Friday at 3",
        "Friday 3",
        "3 Friday",
        "on Friday 3",
        "tomorrow 9",
        "at 3",
        "tomorrow at 3:30",
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(AmbiguityKind::MissingMeridiem),
            "{phrase}"
        );
    }
    let context = make_context(NY, NY_THURSDAY);
    for phrase in [
        "Friday at 13 pm",
        "Friday at 0 am",
        "Friday at 24:00",
        "Friday at 9:60 am",
        "Friday at 25:00",
        "Friday at",
    ] {
        let outcome = TimeResolver::resolve(phrase, &context);
        match outcome {
            Err(ResolutionError::InvalidDateFormat(_)) => {}
            Ok(result) => assert!(result.resolved_time.is_none(), "{phrase}"),
            Err(other) => panic!("{phrase}: {other}"),
        }
    }
}

#[test]
fn zone_qualifiers_never_resolve_in_the_capture_zone() {
    for phrase in [
        "tomorrow at 9 a.m. EST",
        "tomorrow at 9 a.m. Tokyo time",
        "tomorrow at 9 a.m. UTC+9",
        "Friday at 3 p.m. PST",
        "tomorrow at 9am UTC",
        "9 a.m. tomorrow EST",
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(AmbiguityKind::UnsupportedQualifier),
            "{phrase}"
        );
    }
}

#[test]
fn past_clock_times_are_reported_as_past() {
    let context = make_context(NY, NY_THURSDAY);
    for phrase in [
        "today at 9 a.m.",
        "2026-10-07 at 3 p.m.",
        "since monday at 8 a.m.",
    ] {
        let result = TimeResolver::resolve(phrase, &context).unwrap();
        assert!(result.is_past, "{phrase}");
        assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::Past), "{phrase}");
        assert!(result.resolved_time.is_some(), "{phrase}");
    }
}

#[test]
fn spring_forward_gap_is_ambiguous() {
    // 2027-03-14 02:30 does not exist in New York.
    let context = make_context(NY, "2027-03-10T15:00:00Z");
    let result = TimeResolver::resolve("2027-03-14 at 2:30 a.m.", &context).unwrap();
    assert!(result.is_ambiguous);
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstGap));
    assert!(result.resolved_time.is_none());
    // Either side of the gap is fine.
    assert_eq!(
        resolved_utc(NY, "2027-03-10T15:00:00Z", "2027-03-14 at 3:30 a.m."),
        "2027-03-14T07:30:00+00:00"
    );
    assert_eq!(
        resolved_utc(NY, "2027-03-10T15:00:00Z", "2027-03-14 at 1:30 a.m."),
        "2027-03-14T06:30:00+00:00"
    );
}

#[test]
fn fall_back_fold_is_ambiguous_with_both_candidates() {
    // 2026-11-01 01:30 happens twice in New York.
    let context = make_context(NY, "2026-10-28T15:00:00Z");
    let result = TimeResolver::resolve("2026-11-01 at 1:30 a.m.", &context).unwrap();
    assert!(result.is_ambiguous);
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstFold));
    assert!(result.resolved_time.is_none());
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        resolved_utc(NY, "2026-10-28T15:00:00Z", "2026-11-01 at 3:30 a.m."),
        "2026-11-01T08:30:00+00:00"
    );
}

#[test]
fn a_day_part_missing_or_unknown_is_not_resolved() {
    let context = make_context(NY, NY_THURSDAY);
    // Day only keeps its date-only behaviour.
    let date_only = TimeResolver::resolve("Friday", &context).unwrap();
    assert!(date_only.resolved_time.is_none());
    assert_eq!(date_only.ambiguity_kind, Some(AmbiguityKind::MissingHour));
    // A bare "today" is still not a phrase on its own.
    assert!(matches!(
        TimeResolver::resolve("today", &context),
        Err(ResolutionError::InvalidDateFormat(_))
    ));
    // Unknown day words do not resolve.
    let unknown = TimeResolver::resolve("someday at 3 p.m.", &context);
    assert!(unknown.map_or(true, |result| result.resolved_time.is_none()));
}

#[test]
fn repeats_with_a_clock_are_still_unsupported() {
    let context = make_context(NY, NY_THURSDAY);
    assert!(matches!(
        TimeResolver::resolve("every day at 9 a.m.", &context),
        Err(ResolutionError::UnsupportedRepeat(_))
    ));
}

#[test]
fn a_day_and_clock_on_either_side_of_the_task_resolve_together() {
    // Thursday 2026-10-08, 10:00 EDT.
    for (phrase, expected) in [
        (
            "today to call the dentist at 5 p.m.",
            "2026-10-08T21:00:00+00:00",
        ),
        ("Friday to call Sam at 3 p.m.", "2026-10-09T19:00:00+00:00"),
        (
            "at 10am to call the dentist tomorrow",
            "2026-10-09T14:00:00+00:00",
        ),
    ] {
        assert_eq!(resolved_utc(NY, NY_THURSDAY, phrase), expected, "{phrase}");
    }
    // Not the split shape: two days stay unresolved.
    let context = make_context(NY, NY_THURSDAY);
    assert!(TimeResolver::resolve("today to call Sam tomorrow at 10am", &context).is_err());
}

// ---------------------------------------------------------------------------------------------
// Day-part words (F62)
// ---------------------------------------------------------------------------------------------

#[test]
fn day_part_phrases_resolve_in_every_shape() {
    // 2026-10-08 10:00 EDT; EDT is UTC-4.
    for (phrase, expected) in [
        // <day> <daypart> [at] <clock>
        ("tomorrow morning at 9:30 AM", "2026-10-09T13:30:00+00:00"),
        ("Tomorrow Morning At 9:30 am", "2026-10-09T13:30:00+00:00"),
        ("tomorrow morning 9:30 am", "2026-10-09T13:30:00+00:00"),
        ("Friday evening at 7 pm", "2026-10-09T23:00:00+00:00"),
        ("on Friday evening at 7 pm", "2026-10-09T23:00:00+00:00"),
        ("tomorrow night at 21:30", "2026-10-10T01:30:00+00:00"),
        ("tomorrow afternoon at noon", "2026-10-09T16:00:00+00:00"),
        ("2026-10-12 evening at 6 p.m.", "2026-10-12T22:00:00+00:00"),
        ("next Monday morning at 8 a.m.", "2026-10-12T12:00:00+00:00"),
        ("today evening at 6 pm", "2026-10-08T22:00:00+00:00"),
        // <daypart> [at] <clock>, today
        ("this evening at 6 pm", "2026-10-08T22:00:00+00:00"),
        ("this afternoon at 3", "2026-10-08T19:00:00+00:00"),
        ("afternoon at 3 pm", "2026-10-08T19:00:00+00:00"),
        ("tonight at 8", "2026-10-09T00:00:00+00:00"),
        ("tonight at 8:15 pm", "2026-10-09T00:15:00+00:00"),
        ("tonight 20:00", "2026-10-09T00:00:00+00:00"),
        ("this night at 9", "2026-10-09T01:00:00+00:00"),
        // <day> [at] <clock> in the <daypart> / at night
        ("tomorrow at 9 in the morning", "2026-10-09T13:00:00+00:00"),
        ("tomorrow 9:30 in the morning", "2026-10-09T13:30:00+00:00"),
        ("Friday at 10 at night", "2026-10-10T02:00:00+00:00"),
        ("Friday at 7 in the evening", "2026-10-09T23:00:00+00:00"),
        ("at 3 in the afternoon", "2026-10-08T19:00:00+00:00"),
    ] {
        assert_eq!(resolved_utc(NY, NY_THURSDAY, phrase), expected, "{phrase}");
    }
}

#[test]
fn a_day_part_supplies_a_missing_meridiem() {
    for (phrase, expected) in [
        ("tomorrow morning at 9", "2026-10-09T13:00:00+00:00"),
        ("tomorrow morning at 5", "2026-10-09T09:00:00+00:00"),
        ("tomorrow morning at 11:59", "2026-10-09T15:59:00+00:00"),
        ("tomorrow morning at 9:30", "2026-10-09T13:30:00+00:00"),
        ("tomorrow morning at 09:30", "2026-10-09T13:30:00+00:00"),
        ("tomorrow afternoon at 12", "2026-10-09T16:00:00+00:00"),
        ("tomorrow afternoon at 5", "2026-10-09T21:00:00+00:00"),
        ("tomorrow evening at 6", "2026-10-09T22:00:00+00:00"),
        ("tomorrow evening at 11", "2026-10-10T03:00:00+00:00"),
        ("tonight at 6", "2026-10-08T22:00:00+00:00"),
    ] {
        assert_eq!(resolved_utc(NY, NY_THURSDAY, phrase), expected, "{phrase}");
    }
}

#[test]
fn a_day_part_never_guesses_past_its_hours() {
    for (phrase, kind, reason_part) in [
        // A bare hour that cannot fall in the range.
        ("tonight at 3", AmbiguityKind::MissingMeridiem, "tonight"),
        (
            "tomorrow evening at 4",
            AmbiguityKind::MissingMeridiem,
            "evening",
        ),
        (
            "tomorrow morning at 3",
            AmbiguityKind::MissingMeridiem,
            "morning",
        ),
        (
            "tomorrow morning at 12",
            AmbiguityKind::MissingMeridiem,
            "morning",
        ),
        (
            "tomorrow afternoon at 6",
            AmbiguityKind::MissingMeridiem,
            "afternoon",
        ),
        (
            "tomorrow morning at 13",
            AmbiguityKind::MissingMeridiem,
            "morning",
        ),
        // A stated meridiem or 24-hour clock outside the range.
        (
            "tomorrow morning at 9:30 PM",
            AmbiguityKind::DayPartConflict,
            "morning",
        ),
        (
            "tomorrow morning at 21:30",
            AmbiguityKind::DayPartConflict,
            "morning",
        ),
        (
            "tomorrow morning at noon",
            AmbiguityKind::DayPartConflict,
            "morning",
        ),
        (
            "tomorrow morning at 4 am",
            AmbiguityKind::DayPartConflict,
            "morning",
        ),
        (
            "tomorrow afternoon at 9 am",
            AmbiguityKind::DayPartConflict,
            "afternoon",
        ),
        (
            "tomorrow afternoon at 18:00",
            AmbiguityKind::DayPartConflict,
            "afternoon",
        ),
        ("tonight at 1 am", AmbiguityKind::DayPartConflict, "tonight"),
        (
            "tonight at midnight",
            AmbiguityKind::DayPartConflict,
            "tonight",
        ),
        (
            "tomorrow evening at 5:59 pm",
            AmbiguityKind::DayPartConflict,
            "evening",
        ),
        (
            "Friday at 3 pm at night",
            AmbiguityKind::DayPartConflict,
            "night",
        ),
        (
            "Friday at 9 pm in the morning",
            AmbiguityKind::DayPartConflict,
            "morning",
        ),
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(result.ambiguity_kind, Some(kind), "{phrase}");
        let reason = result.ambiguity_reason.unwrap();
        assert!(reason.contains(reason_part), "{phrase}: {reason}");
        let expected_date = if phrase.starts_with("tonight") {
            "2026-10-08"
        } else {
            "2026-10-09"
        };
        assert_eq!(
            result.resolved_date,
            Some(expected_date.parse::<NaiveDate>().unwrap()),
            "{phrase}"
        );
    }
}

#[test]
fn a_day_part_without_a_clock_is_date_only_and_missing_the_hour() {
    for (phrase, date) in [
        ("tomorrow morning", "2026-10-09"),
        ("Friday afternoon", "2026-10-09"),
        ("next Monday evening", "2026-10-12"),
        ("tomorrow night", "2026-10-09"),
        ("tonight", "2026-10-08"),
        ("this afternoon", "2026-10-08"),
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(AmbiguityKind::MissingHour),
            "{phrase}"
        );
        assert_eq!(
            result.resolved_date,
            Some(date.parse::<NaiveDate>().unwrap()),
            "{phrase}"
        );
        let reason = result.ambiguity_reason.unwrap();
        let word = phrase.split_whitespace().last().unwrap();
        assert!(reason.contains(word), "{phrase}: {reason}");
    }
}

#[test]
fn in_the_morning_without_a_day_is_ambiguous() {
    for phrase in [
        "in the morning",
        "in the morning at 9",
        "in the evening at 7 pm",
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(AmbiguityKind::MissingDay),
            "{phrase}"
        );
    }
}

#[test]
fn day_part_phrases_keep_the_clock_rules() {
    // A stated zone after the clock is ambiguous.
    for phrase in [
        "tomorrow morning at 9 a.m. EST",
        "tomorrow at 9 in the morning EST",
    ] {
        assert_ambiguous_without_instant(NY, NY_THURSDAY, phrase);
        let result = TimeResolver::resolve(phrase, &make_context(NY, NY_THURSDAY)).unwrap();
        assert_eq!(
            result.ambiguity_kind,
            Some(AmbiguityKind::UnsupportedQualifier),
            "{phrase}"
        );
    }
    // A resolved instant before the capture is past (9 a.m. today, captured at 10:00).
    let result =
        TimeResolver::resolve("this morning at 9", &make_context(NY, NY_THURSDAY)).unwrap();
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::Past));
    assert!(result.is_past);
    // A clock before the capture, with no day word and a trailing day part, is a missing day.
    let result =
        TimeResolver::resolve("at 9 in the morning", &make_context(NY, NY_THURSDAY)).unwrap();
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::MissingDay));
    // The original phrase is kept as written.
    let result = TimeResolver::resolve(
        "Tomorrow Morning at 9:30 AM",
        &make_context(NY, NY_THURSDAY),
    )
    .unwrap();
    assert_eq!(result.original_phrase, "Tomorrow Morning at 9:30 AM");
}

#[test]
fn text_around_a_day_part_is_not_a_time() {
    for phrase in [
        "morning coffee",
        "tomorrow morning evening at 7",
        "tomorrow tonight at 8",
        "banana morning at 9",
        "in the morning tomorrow at 9",
        "at night",
    ] {
        let context = make_context(NY, NY_THURSDAY);
        assert!(
            TimeResolver::resolve(phrase, &context)
                .map(|result| result.resolved_time.is_none())
                .unwrap_or(true),
            "{phrase}"
        );
    }
}

#[test]
fn day_part_phrases_follow_the_zone_offset_across_a_dst_change() {
    // America/New_York leaves DST on 2026-11-01; captured Saturday 2026-10-31 20:00 EDT.
    let reference = "2026-11-01T00:00:00Z";
    assert_eq!(
        resolved_utc(NY, reference, "tomorrow morning at 9:30 AM"),
        "2026-11-01T14:30:00+00:00",
        "09:30 EST is UTC-5 after the change"
    );
    assert_eq!(
        resolved_utc(NY, reference, "tonight at 8"),
        "2026-11-01T00:00:00+00:00",
    );
}

#[test]
fn a_day_part_clock_inside_a_dst_gap_is_ambiguous() {
    // Greenland (America/Nuuk) moves its clocks from 23:00 to 00:00 on 2026-03-28, inside night.
    let zone = "America/Nuuk";
    let reference = "2026-03-26T12:00:00Z";
    let context = make_context(zone, reference);
    let result = TimeResolver::resolve("Saturday night at 11:30 pm", &context).unwrap();
    assert_eq!(result.ambiguity_kind, Some(AmbiguityKind::DstGap));
    assert!(result.resolved_time.is_none());
}

#[test]
fn span_coverage_counts_day_part_words() {
    let text = "Remind me tomorrow morning at 9:30 AM to ask the nursery about the hydrangea";
    let span = |phrase: &str| {
        let start = text.find(phrase).unwrap();
        (start, start + phrase.len())
    };
    let (start, end) = span("tomorrow morning at 9:30 AM");
    assert!(span_covers_time_expression(text, start, end));
    for partial in ["at 9:30 AM", "morning at 9:30 AM", "9:30 AM"] {
        let (start, end) = span(partial);
        assert!(!span_covers_time_expression(text, start, end), "{partial}");
    }

    for (text, phrase) in [
        ("Remind me tonight at 8 to water the ferns", "tonight at 8"),
        (
            "Remind me this evening at 6 pm to water the ferns",
            "this evening at 6 pm",
        ),
        (
            "Remind me tomorrow at 9 in the morning to water the ferns",
            "tomorrow at 9 in the morning",
        ),
        (
            "Remind me Friday at 10 at night to water the ferns",
            "Friday at 10 at night",
        ),
    ] {
        let start = text.find(phrase).unwrap();
        assert!(
            span_covers_time_expression(text, start, start + phrase.len()),
            "{phrase}"
        );
    }
    for (text, partial) in [
        (
            "Remind me this evening at 6 pm to water the ferns",
            "at 6 pm",
        ),
        (
            "Remind me tomorrow at 9 in the morning to water the ferns",
            "tomorrow at 9",
        ),
        (
            "Remind me Friday at 10 at night to water the ferns",
            "Friday at 10",
        ),
    ] {
        let start = text.find(partial).unwrap();
        assert!(
            !span_covers_time_expression(text, start, start + partial.len()),
            "{partial}"
        );
    }
}
