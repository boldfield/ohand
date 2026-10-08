use chrono::{DateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use ohand_core::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
};
use ohand_core::interpretation::fast_path::recognize_reminder;
use ohand_core::store::events::ItemType;
use ohand_core::time::TimeContext;

const ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
const CAPTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
const REQUEST_VERSION: &str = "550e8400-e29b-41d4-a716-446655440004";

fn context_at(reference: &str) -> TimeContext {
    let reference_time = DateTime::parse_from_rfc3339(reference)
        .unwrap()
        .with_timezone(&Utc);
    let zone: Tz = "America/New_York".parse().unwrap();
    TimeContext {
        timezone: "America/New_York".to_string(),
        locale: "en-US".to_string(),
        reference_time,
        utc_offset_at_capture: zone
            .offset_from_utc_datetime(&reference_time.naive_utc())
            .fix()
            .local_minus_utc(),
        calendar: "gregorian".to_string(),
    }
}

// Wednesday 2025-10-15 06:00 in New York.
fn context() -> TimeContext {
    context_at("2025-10-15T10:00:00Z")
}

fn recognize_in(text: &str, time_context: &TimeContext) -> Option<Proposal> {
    let proposal = recognize_reminder(
        text,
        ITEM_ID,
        CAPTURE_ID,
        0,
        TextBasis::Original { item_revision: 0 },
        REQUEST_VERSION,
        time_context,
    );
    if let Some(proposal) = &proposal {
        proposal
            .validate(text)
            .unwrap_or_else(|error| panic!("{text:?} produced an invalid proposal: {error}"));
    }
    proposal
}

fn recognize(text: &str) -> Option<Proposal> {
    recognize_in(text, &context())
}

fn selected(text: &str, span: SourceSpan) -> String {
    text.chars()
        .skip(span.start)
        .take(span.end - span.start)
        .collect()
}

fn reminder_of(proposal: &Proposal) -> &ReminderProposal {
    proposal
        .reminder_proposal
        .as_ref()
        .expect("expected a reminder proposal")
}

fn expect_explicit(text: &str, instant: &str, time_phrase: &str) -> Proposal {
    let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should be recognized"));
    assert_eq!(proposal.abstention, None, "{text:?}");
    let reminder = reminder_of(&proposal);
    assert_eq!(
        reminder.quality,
        TimeResolutionQuality::Explicit,
        "{text:?}"
    );
    assert_eq!(reminder.instant.as_deref(), Some(instant), "{text:?}");
    assert_eq!(
        reminder.timezone_id.as_deref(),
        Some("America/New_York"),
        "{text:?}"
    );
    let span = reminder.source_span.expect("reminder evidence span");
    assert_eq!(selected(text, span), time_phrase, "{text:?}");
    proposal
}

fn expect_ambiguous(text: &str, time_phrase: &str) -> Proposal {
    let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should be recognized"));
    assert_eq!(proposal.abstention, None, "{text:?}");
    let reminder = reminder_of(&proposal);
    assert_eq!(
        reminder.quality,
        TimeResolutionQuality::Ambiguous,
        "{text:?}"
    );
    assert_eq!(reminder.instant, None, "{text:?}");
    let span = reminder.source_span.expect("reminder evidence span");
    assert_eq!(selected(text, span), time_phrase, "{text:?}");
    proposal
}

fn expect_abstention(text: &str, reason: AbstentionReason) {
    let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should abstain"));
    assert_eq!(proposal.abstention, Some(reason), "{text:?}");
    assert!(proposal.reminder_proposal.is_none(), "{text:?}");
}

fn expect_unrecognized(text: &str) {
    assert!(
        recognize(text).is_none(),
        "{text:?} must be left for the approved interpreter"
    );
}

const FUTURE_INSTANT: &str = "2025-10-20T18:30:00Z";
const FUTURE_PHRASE: &str = "2025-10-20 14:30:00";

#[test]
fn explicit_datetime_produces_exact_sourced_instant() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "Remind me 2025-10-20 14:30:00.",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "REMIND ME 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "Please remind me on 2025-10-20 14:30:00!",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
}

#[test]
fn content_after_the_time_is_a_sourced_action_span() {
    let text = "remind me 2025-10-20 14:30:00 to buy milk";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_eq!(proposal.item_type, Some(ItemType::Action));
    let spans = proposal.source_spans.expect("content span");
    assert_eq!(spans.len(), 1);
    assert_eq!(selected(text, spans[0]), "buy milk");
}

#[test]
fn content_before_the_time_is_a_sourced_action_span() {
    let text = "Remind me to call the roofer on 2025-10-20 14:30:00.";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    let spans = proposal.source_spans.expect("content span");
    assert_eq!(selected(text, spans[0]), "call the roofer");
}

#[test]
fn date_without_hour_is_ambiguous_and_never_guessed() {
    let text = "remind me to call mom tomorrow";
    let proposal = expect_ambiguous(text, "tomorrow");
    assert_eq!(
        selected(text, proposal.source_spans.unwrap()[0]),
        "call mom"
    );
    expect_ambiguous("remind me tomorrow", "tomorrow");
    expect_ambiguous("Please remind me Friday to call the roofer.", "Friday");
    expect_ambiguous("remind me next friday", "next friday");
    expect_ambiguous("remind me 2025-10-20", "2025-10-20");
}

#[test]
fn past_time_is_ambiguous_without_an_instant() {
    expect_ambiguous("remind me 2025-10-01 10:00:00", "2025-10-01 10:00:00");
}

#[test]
fn dst_fold_and_gap_are_ambiguous_without_an_instant() {
    expect_ambiguous("remind me 2025-11-02 01:30:00", "2025-11-02 01:30:00");
    let proposal = recognize_in(
        "remind me 2025-03-09 02:30:00",
        &context_at("2025-03-08T10:00:00Z"),
    )
    .unwrap();
    let reminder = reminder_of(&proposal);
    assert_eq!(reminder.quality, TimeResolutionQuality::Ambiguous);
    assert_eq!(reminder.instant, None);
}

#[test]
fn spans_are_unicode_scalar_offsets_in_the_original_text() {
    let bell = "\u{1F514} remind me 2025-10-20 14:30:00";
    let proposal = expect_explicit(bell, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_eq!(
        reminder_of(&proposal).source_span,
        Some(SourceSpan::new(12, 31))
    );

    expect_explicit(
        "\u{130}\u{130}\u{130}\u{130}, remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_ambiguous("Caf\u{e9} r\u{e9}sum\u{e9}: remind me tomorrow", "tomorrow");
    expect_explicit(
        "remind me 2025-10-20 14:30:00 to buy \u{e9}clairs",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    assert!(recognize("\u{130}\u{130}\u{130}\u{130}, remind me ma\u{f1}ana").is_none());
}

#[test]
fn leading_text_that_is_not_a_command_prefix_is_not_the_grammar() {
    expect_explicit(
        "it's fine, remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "Hey, remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "Note to self: remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_unrecognized("unremind me 2025-10-20 14:30:00");
    expect_unrecognized("reminded me 2025-10-20 14:30:00");
    expect_unrecognized("can the calendar remind me 2025-10-20 14:30:00");
    expect_unrecognized("I need to remind me 2025-10-20 14:30:00");
    expect_unrecognized("will you remind me 2025-10-20 14:30:00");
}

#[test]
fn negation_minimal_pair() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_abstention(
        "don't remind me 2025-10-20 14:30:00",
        AbstentionReason::Negated,
    );
    expect_abstention(
        "Do not remind me 2025-10-20 14:30:00",
        AbstentionReason::Negated,
    );
    expect_abstention(
        "never remind me 2025-10-20 14:30:00",
        AbstentionReason::Negated,
    );
    expect_abstention(
        "I don\u{2019}t want you to remind me 2025-10-20 14:30:00",
        AbstentionReason::Negated,
    );
    expect_explicit(
        "whenever you can, remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
}

#[test]
fn retraction_after_the_command_minimal_pair() {
    expect_ambiguous("remind me tomorrow to buy milk", "tomorrow");
    expect_abstention(
        "remind me tomorrow to buy milk, never mind",
        AbstentionReason::Negated,
    );
    expect_abstention(
        "remind me tomorrow to buy milk or not",
        AbstentionReason::Negated,
    );
    expect_unrecognized("remind me 2025-10-20 14:30:00 never mind");
    expect_unrecognized("remind me 2025-10-20 14:30:00, everyone is out");
}

#[test]
fn quotation_minimal_pair() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_abstention(
        "He wrote \"remind me 2025-10-20 14:30:00\"",
        AbstentionReason::UncertainTarget,
    );
    expect_abstention(
        "The sign says 'remind me 2025-10-20 14:30:00'",
        AbstentionReason::UncertainTarget,
    );
    expect_abstention(
        "\u{201C}Remind me 2025-10-20 14:30:00\u{201D}",
        AbstentionReason::UncertainTarget,
    );
    expect_explicit(
        "\"Fine.\" Then remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
}

#[test]
fn hypothetical_minimal_pair() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        "what if you remind me 2025-10-20 14:30:00",
        "maybe remind me 2025-10-20 14:30:00",
        "if it rains remind me 2025-10-20 14:30:00",
        "you could remind me 2025-10-20 14:30:00",
    ] {
        expect_abstention(text, AbstentionReason::UncertainTarget);
    }
    expect_abstention(
        "remind me tomorrow to water the plants if it rains",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn reported_speech_minimal_pair() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        "Sam said remind me 2025-10-20 14:30:00",
        "Sam said, remind me 2025-10-20 14:30:00",
        "my friend asked me to remind me 2025-10-20 14:30:00",
        "\u{fc}nder the cap he said remind me 2025-10-20 14:30:00",
    ] {
        expect_abstention(text, AbstentionReason::UncertainTarget);
    }
}

#[test]
fn completed_work_minimal_pair() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_abstention(
        "I already did it, remind me 2025-10-20 14:30:00",
        AbstentionReason::UncertainTarget,
    );
    expect_abstention(
        "It is done so remind me 2025-10-20 14:30:00",
        AbstentionReason::UncertainTarget,
    );
    expect_explicit(
        "I already did it. Remind me 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
}

#[test]
fn absent_time_minimal_pair() {
    expect_ambiguous("remind me to buy milk tomorrow", "tomorrow");
    expect_unrecognized("remind me to buy milk");
    expect_unrecognized("remind me");
    expect_unrecognized("please remind me about the roof");
    expect_unrecognized("remind me tomorrow at 3pm");
    expect_unrecognized("remind me in two hours");
    expect_unrecognized("remind me 2025-13-45 10:00:00");
    expect_unrecognized("remind me tomorrow?");
}

#[test]
fn recurrence_minimal_pair() {
    expect_ambiguous("remind me tomorrow to ask everyone", "tomorrow");
    for text in [
        "remind me every Friday to call mom",
        "remind me daily",
        "remind me to water the plants every day",
        "remind me on mondays to take out the trash",
        "remind me weekly 2025-10-20 14:30:00",
    ] {
        expect_abstention(text, AbstentionReason::UnsupportedOperation);
    }
    expect_unrecognized("remind me about the weekly report");
}

#[test]
fn competing_times_abstain() {
    expect_abstention(
        "remind me tomorrow to call mom on friday",
        AbstentionReason::Ambiguous,
    );
}

#[test]
fn unmatched_language_is_left_for_the_interpreter() {
    for text in [
        "",
        "tell me a joke",
        "tell me 2025-10-20 14:30:00 what you think",
        "John's note about the roof",
        "the mayor called",
        "maybe a roof garden would be nice",
        "it's fine",
        "call the dentist tomorrow at 9am about therapy",
    ] {
        expect_unrecognized(text);
    }
}

#[test]
fn proposals_carry_trusted_provenance_and_a_fresh_proposal_id() {
    let text = "remind me 2025-10-20 14:30:00";
    let first = recognize(text).unwrap();
    let second = recognize(text).unwrap();
    assert_eq!(first.item_id, ITEM_ID);
    assert_eq!(first.capture_id, CAPTURE_ID);
    assert_eq!(first.request_version, REQUEST_VERSION);
    assert_eq!(first.text_basis, TextBasis::Original { item_revision: 0 });
    assert_ne!(first.proposal_id, second.proposal_id);
    assert_eq!(first.session_topic_proposal, None);
}

#[test]
fn correction_basis_is_preserved() {
    let basis = TextBasis::Correction {
        correction_record_id: "550e8400-e29b-41d4-a716-446655440009".to_string(),
        item_revision: 3,
    };
    let text = "remind me 2025-10-20 14:30:00";
    let proposal = recognize_reminder(
        text,
        ITEM_ID,
        CAPTURE_ID,
        3,
        basis.clone(),
        REQUEST_VERSION,
        &context(),
    )
    .unwrap();
    assert_eq!(proposal.text_basis, basis);
    proposal.validate(text).unwrap();
}

#[test]
fn unusual_text_never_panics_and_every_proposal_validates() {
    let fragments = [
        "remind me",
        "REMIND   ME",
        "\u{130}",
        "\u{1F514}",
        "\"",
        "'",
        "tomorrow",
        "2025-10-20 14:30:00",
        "to",
        "on",
        "ma\u{f1}ana",
        ",",
        ".",
        "?",
        "\n",
        "never mind",
    ];
    for first in fragments {
        for second in fragments {
            for third in fragments {
                let text = format!("{first} {second} {third}");
                recognize(&text);
            }
        }
    }
}

#[test]
fn unanchored_unicode_prefix_is_not_the_grammar() {
    expect_unrecognized("\u{130}\u{130}\u{130}\u{130} remind me 2025-10-20 14:30:00");
}

#[test]
fn trailing_retraction_negation_and_reported_speech_never_schedule() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        "remind me 2025-10-20 14:30:00 to call mom, actually don't",
        "remind me 2025-10-20 14:30:00 to call mom, no wait, cancel that",
        "remind me 2025-10-20 14:30:00 to call mom, just kidding",
        "remind me 2025-10-20 14:30:00 to call mom, I don't want to",
        "remind me 2025-10-20 14:30:00 to call mom (not really)",
        "remind me 2025-10-20 14:30:00 to call mom; never",
        "remind me 2025-10-20 14:30:00 to call mom, he said",
        "remind me 2025-10-20 14:30:00 to call mom\" he said",
        "remind me 2025-10-20 14:30:00 to call mom actually",
        "remind me 2025-10-20 14:30:00 to not call mom",
        "remind me to call mom, actually don't 2025-10-20 14:30:00",
    ] {
        let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should abstain"));
        assert!(proposal.reminder_proposal.is_none(), "{text:?}");
        assert!(proposal.abstention.is_some(), "{text:?}");
    }
}

#[test]
fn trailing_completed_work_and_reported_speech_minimal_pairs() {
    expect_ambiguous("remind me tomorrow to buy milk", "tomorrow");
    expect_ambiguous("remind me tomorrow to ask the landlord", "tomorrow");
    expect_ambiguous("remind me tomorrow to tell Sam", "tomorrow");
    expect_ambiguous("remind me tomorrow to get this done", "tomorrow");
    for text in [
        "remind me tomorrow to buy milk, which I already did",
        "remind me tomorrow to buy milk which I already did",
        "remind me tomorrow to buy milk which is done",
        "remind me tomorrow to call mom, Sam said",
        "remind me tomorrow to call mom Sam said",
        "remind me tomorrow to call mom as Sam told me",
    ] {
        let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should abstain"));
        assert!(proposal.reminder_proposal.is_none(), "{text:?}");
        assert!(
            proposal.abstention.is_some(),
            "{text:?} must record an abstention"
        );
    }
}

#[test]
fn competing_times_in_the_content_never_pick_one() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        "remind me to call mom on 2025-10-20 14:30:00 or 2025-10-21 09:00:00",
        "remind me to call mom 2025-10-21 09:00:00 or on 2025-10-20 14:30:00",
        "remind me 2025-10-20 14:30:00 to call mom 2025-10-22 10:00:00 at the latest",
        "remind me 2025-10-20 14:30:00 to call mom on tuesday at the office",
        "remind me tomorrow to call mom or friday",
    ] {
        expect_abstention(text, AbstentionReason::Ambiguous);
    }
}

#[test]
fn recurrence_is_a_request_not_content_vocabulary() {
    expect_ambiguous("remind me tomorrow to read the weekly report", "tomorrow");
    expect_ambiguous("remind me to read the monthly report tomorrow", "tomorrow");
    expect_unrecognized("remind me to read the monthly report");
    for text in [
        "remind me tomorrow to read the report weekly",
        "remind me to read the report every week",
        "remind me tomorrow to stretch each morning",
        "remind me to take pills every 8 hours",
        "remind me tomorrow weekly to read the report",
    ] {
        expect_abstention(text, AbstentionReason::UnsupportedOperation);
    }
}

fn expect_no_reminder(text: &str) {
    let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should abstain"));
    assert!(proposal.reminder_proposal.is_none(), "{text:?}");
    assert!(
        proposal.abstention.is_some(),
        "{text:?} must record an abstention"
    );
}

#[test]
fn punctuation_free_retractions_in_the_content_never_schedule() {
    expect_explicit(
        "remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "remind me 2025-10-20 14:30:00 to cancel the subscription",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        "remind me 2025-10-20 14:30:00 to call mom forget about it",
        "remind me 2025-10-20 14:30:00 to call mom cancel the reminder",
        "remind me 2025-10-20 14:30:00 to call mom I changed my mind",
        "remind me 2025-10-20 14:30:00 to call mom changed my mind",
        "remind me 2025-10-20 14:30:00 to call mom ignore that",
        "remind me 2025-10-20 14:30:00 to call mom disregard this",
        "remind me 2025-10-20 14:30:00 to call mom or whatever",
        "remind me 2025-10-20 14:30:00 to call mom but it's fine",
        "remind me 2025-10-20 14:30:00 to call mom she will understand",
        "remind me 2025-10-20 14:30:00 to call mom and then tell the whole family about the long day",
    ] {
        expect_no_reminder(text);
    }
}

#[test]
fn non_grammar_times_in_the_content_never_schedule() {
    let positive = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(positive, FUTURE_INSTANT, FUTURE_PHRASE);
    for suffix in [
        "at 5pm",
        "at 9",
        "at noon",
        "at nine",
        "tonight",
        "in two hours",
        "in the morning",
        "next week",
        "later",
        "soon",
        "on March 5",
        "this weekend",
        "on tuesdays",
        "in october",
    ] {
        let text = format!("{positive} {suffix}");
        expect_no_reminder(&text);
    }
}

#[test]
fn relation_words_before_a_trailing_time_never_schedule() {
    expect_explicit(
        "remind me to call mom on 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "remind me to call mom 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_ambiguous("remind me to call mom tomorrow", "tomorrow");
    for time in ["2025-10-20 14:30:00", "tomorrow"] {
        for relation in [
            "except",
            "before",
            "after",
            "by",
            "until",
            "till",
            "from",
            "since",
            "around",
            "about",
            "the day after",
            "a week before",
            "an hour after",
        ] {
            let text = format!("remind me to call mom {relation} {time}");
            expect_no_reminder(&text);
        }
    }
}

#[test]
fn recurrence_is_syntax_aware() {
    expect_ambiguous(
        "remind me tomorrow to review the Mondays report",
        "tomorrow",
    );
    expect_ambiguous("remind me tomorrow to check every door", "tomorrow");
    for text in [
        "remind me every two days to call mom",
        "remind me to call mom every two days",
        "remind me every other day to call mom",
        "remind me to call mom every other day",
        "remind me every 2 weeks to call mom",
        "remind me to call mom on Mondays",
        "remind me Mondays to call mom",
    ] {
        expect_abstention(text, AbstentionReason::UnsupportedOperation);
    }
}

#[test]
fn stop_in_the_content_is_an_ordinary_verb() {
    expect_ambiguous("remind me to stop by mom's tomorrow", "tomorrow");
    expect_ambiguous("remind me tomorrow to stop smoking", "tomorrow");
}
