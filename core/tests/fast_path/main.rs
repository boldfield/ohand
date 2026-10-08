use chrono::prelude::*;
use ohand_core::interpretation::contracts::{AbstentionReason, TimeResolutionQuality};
use ohand_core::interpretation::fast_path::recognize_reminder;
use ohand_core::providers::contracts::TextBasis;
use ohand_core::time::TimeContext;

fn test_time_context() -> TimeContext {
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
fn test_explicit_reminder_with_date_and_time() {
    let context = test_time_context();
    let text = "remind me 2025-10-20 14:30:00";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert!(p.reminder_proposal.is_some());
    let reminder = p.reminder_proposal.unwrap();
    assert!(reminder.instant.is_some());
    assert_eq!(reminder.quality, TimeResolutionQuality::Explicit);
}

#[test]
fn test_reminder_without_time_is_ambiguous() {
    let context = test_time_context();
    let text = "remind me";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert!(p.reminder_proposal.is_some());
    let reminder = p.reminder_proposal.unwrap();
    assert!(reminder.instant.is_none());
    assert_eq!(reminder.quality, TimeResolutionQuality::Ambiguous);
}

#[test]
fn test_negation_dont_remind_me() {
    let context = test_time_context();
    let text = "don't remind me";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert!(p.abstention.is_some());
    assert_eq!(p.abstention, Some(AbstentionReason::Negated));
    assert!(p.reminder_proposal.is_none());
}

#[test]
fn test_negation_do_not_remind() {
    let context = test_time_context();
    let text = "do not remind me tomorrow";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::Negated));
}

#[test]
fn test_negation_never() {
    let context = test_time_context();
    let text = "never remind me about this";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::Negated));
}

#[test]
fn test_quoted_speech_abstains() {
    let context = test_time_context();
    let text = "he said \"remind me tomorrow\"";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UncertainTarget));
}

#[test]
fn test_quoted_speech_single_quotes() {
    let context = test_time_context();
    let text = "she said 'remind me later'";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UncertainTarget));
}

#[test]
fn test_hypothetical_they_said() {
    let context = test_time_context();
    let text = "they said to remind me";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UncertainTarget));
}

#[test]
fn test_hypothetical_she_thinks() {
    let context = test_time_context();
    let text = "she thinks I should remind me tomorrow";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    // This should abstain because of the hypothetical context (she thinks)
    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UncertainTarget));
}

#[test]
fn test_completed_work() {
    let context = test_time_context();
    let text = "already reminded myself";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    // Should abstain or return None since there's no "remind me" pattern
    assert!(proposal.is_none() || proposal.unwrap().abstention.is_some());
}

#[test]
fn test_unsupported_daily_recurrence() {
    let context = test_time_context();
    let text = "remind me daily";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UnsupportedOperation));
}

#[test]
fn test_unsupported_weekly_recurrence() {
    let context = test_time_context();
    let text = "remind me weekly";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UnsupportedOperation));
}

#[test]
fn test_unsupported_every_pattern() {
    let context = test_time_context();
    let text = "remind me every day";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::UnsupportedOperation));
}

#[test]
fn test_non_reminder_text_returns_none() {
    let context = test_time_context();
    let text = "this is just a note about something else";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_none());
}

#[test]
fn test_tell_me_pattern() {
    let context = test_time_context();
    let text = "tell me tomorrow";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    // "tell me" should be recognized similarly to "remind me"
    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert!(p.reminder_proposal.is_some());
}

#[test]
fn test_source_span_validity() {
    let context = test_time_context();
    let text = "remind me tomorrow";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    if let Some(reminder) = p.reminder_proposal {
        if let Some(span) = reminder.source_span {
            // The span should be valid within the text
            assert!(span.start < span.end);
            assert!(span.end <= text.chars().count());
        }
    }
}

#[test]
fn test_case_insensitive_matching() {
    let context = test_time_context();
    let uppercase = "REMIND ME TOMORROW";
    let proposal = recognize_reminder(
        uppercase,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert!(p.reminder_proposal.is_some());
}

#[test]
fn test_invalid_time_phrase_abstains() {
    let context = test_time_context();
    let text = "remind me whenever";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    // Should abstain with ambiguous reason since "whenever" cannot be resolved
    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::Ambiguous));
}

#[test]
fn test_multiple_negations() {
    let context = test_time_context();
    let text = "never ever remind me";
    let proposal = recognize_reminder(
        text,
        "550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-446655440001",
        0,
        TextBasis::Original { item_revision: 0 },
        "550e8400-e29b-41d4-a716-446655440002",
        &context,
    );

    assert!(proposal.is_some());
    let p = proposal.unwrap();
    assert_eq!(p.abstention, Some(AbstentionReason::Negated));
}
