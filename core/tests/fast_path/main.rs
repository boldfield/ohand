use chrono::{DateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use ohand_core::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
};
use ohand_core::interpretation::fast_path::{recognize_reminder, recognize_session_topic};
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

/// Every reminder rides on an action: the proposal must be an `Action` with a non-empty
/// content span (`docs/validation/intent-fixtures.md`, "A reminder always rides on an action").
fn assert_rides_on_action(proposal: &Proposal, text: &str) {
    assert_eq!(proposal.item_type, Some(ItemType::Action), "{text:?}");
    let spans = proposal
        .source_spans
        .as_ref()
        .unwrap_or_else(|| panic!("{text:?} must carry a content span"));
    assert_eq!(spans.len(), 1, "{text:?}");
    assert!(
        !selected(text, spans[0]).trim().is_empty(),
        "{text:?} must select a target"
    );
}

fn assert_action_target(proposal: &Proposal, text: &str, content: &str) {
    assert_rides_on_action(proposal, text);
    let spans = proposal.source_spans.as_ref().unwrap();
    assert_eq!(selected(text, spans[0]), content, "{text:?}");
}

fn expect_explicit(text: &str, instant: &str, time_phrase: &str) -> Proposal {
    let proposal = recognize(text).unwrap_or_else(|| panic!("{text:?} should be recognized"));
    assert_eq!(proposal.abstention, None, "{text:?}");
    assert_rides_on_action(&proposal, text);
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
    assert_rides_on_action(&proposal, text);
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
const COMMAND: &str = "remind me 2025-10-20 14:30:00 to call mom";

#[test]
fn explicit_datetime_produces_exact_sourced_instant() {
    let proposal = expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, COMMAND, "call mom");
    let text = "Remind me 2025-10-20 14:30:00 to call mom.";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, text, "call mom");
    let text = "REMIND ME 2025-10-20 14:30:00 TO CALL MOM";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, text, "CALL MOM");
    let text = "Please remind me on 2025-10-20 14:30:00, to call mom!";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, text, "call mom");
}

#[test]
fn absent_target_minimal_pair() {
    let proposal = expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, COMMAND, "call mom");
    let text = "remind me to call mom on 2025-10-20 14:30:00";
    let proposal = expect_explicit(text, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_action_target(&proposal, text, "call mom");
    // A bare time phrase has nothing to remind about: it is not the grammar and is left for
    // the approved interpreter rather than becoming a reminder without an action target.
    for text in [
        "remind me 2025-10-20 14:30:00",
        "Remind me 2025-10-20 14:30:00.",
        "REMIND ME 2025-10-20 14:30:00",
        "Please remind me on 2025-10-20 14:30:00!",
        "remind me 2025-10-20 14:30:00,",
        "remind me 2025-10-20 14:30:00 to",
        "remind me 2025-10-20 14:30:00 to, please",
        "remind me 2025-10-20 14:30:00 please",
        "remind me tomorrow",
        "remind me next friday",
        "remind me 2025-10-20",
        "remind me on 2025-10-20 14:30:00",
        "remind me 2025-10-01 10:00:00",
        "\u{1F514} remind me 2025-10-20 14:30:00",
        "Note to self: remind me tomorrow",
        "Hey, remind me 2025-10-20 14:30:00",
        "remind me 2025-10-20 14:30:00\"",
    ] {
        expect_unrecognized(text);
    }
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
    expect_ambiguous("remind me tomorrow to call mom", "tomorrow");
    expect_ambiguous("Please remind me Friday to call the roofer.", "Friday");
    expect_ambiguous("remind me next friday to call mom", "next friday");
    expect_ambiguous("remind me 2025-10-20 to call mom", "2025-10-20");
}

#[test]
fn past_time_is_ambiguous_without_an_instant() {
    expect_ambiguous(
        "remind me 2025-10-01 10:00:00 to call mom",
        "2025-10-01 10:00:00",
    );
    expect_ambiguous(
        "remind me to call mom on 2025-10-01 10:00:00",
        "2025-10-01 10:00:00",
    );
}

#[test]
fn dst_fold_and_gap_are_ambiguous_without_an_instant() {
    expect_ambiguous(
        "remind me 2025-11-02 01:30:00 to call mom",
        "2025-11-02 01:30:00",
    );
    let text = "remind me 2025-03-09 02:30:00 to call mom";
    let proposal = recognize_in(text, &context_at("2025-03-08T10:00:00Z")).unwrap();
    assert_action_target(&proposal, text, "call mom");
    let reminder = reminder_of(&proposal);
    assert_eq!(reminder.quality, TimeResolutionQuality::Ambiguous);
    assert_eq!(reminder.instant, None);
    assert_eq!(
        selected(text, reminder.source_span.unwrap()),
        "2025-03-09 02:30:00"
    );
}

#[test]
fn spans_are_unicode_scalar_offsets_in_the_original_text() {
    let bell = "\u{1F514} remind me 2025-10-20 14:30:00 to call mom";
    let proposal = expect_explicit(bell, FUTURE_INSTANT, FUTURE_PHRASE);
    assert_eq!(
        reminder_of(&proposal).source_span,
        Some(SourceSpan::new(12, 31))
    );
    assert_eq!(
        proposal.source_spans.as_deref(),
        Some(&[SourceSpan::new(35, 43)][..])
    );
    assert_action_target(&proposal, bell, "call mom");

    let symbols = "\u{1F514}\u{1F514} \u{2014} remind me tomorrow to buy milk";
    let proposal = expect_ambiguous(symbols, "tomorrow");
    assert_eq!(
        selected(symbols, proposal.source_spans.unwrap()[0]),
        "buy milk"
    );
    expect_unrecognized("\u{130}\u{130}\u{130}\u{130}, remind me 2025-10-20 14:30:00");
    expect_unrecognized("Caf\u{e9} r\u{e9}sum\u{e9}. Remind me tomorrow");
    expect_unrecognized("remind me 2025-10-20 14:30:00 to buy \u{e9}clairs");
    assert!(recognize("\u{130}\u{130}\u{130}\u{130}, remind me ma\u{f1}ana").is_none());
}

#[test]
fn leading_text_that_is_not_a_command_prefix_is_not_the_grammar() {
    expect_unrecognized("it's fine, remind me 2025-10-20 14:30:00 to call mom");
    expect_explicit(
        "Hey, remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "Note to self: remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_unrecognized("unremind me 2025-10-20 14:30:00 to call mom");
    expect_unrecognized("reminded me 2025-10-20 14:30:00 to call mom");
    expect_unrecognized("can the calendar remind me 2025-10-20 14:30:00 to call mom");
    expect_unrecognized("I need to remind me 2025-10-20 14:30:00 to call mom");
    expect_unrecognized("will you remind me 2025-10-20 14:30:00 to call mom");
}

#[test]
fn negation_minimal_pair() {
    expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    for text in [
        "don't remind me 2025-10-20 14:30:00 to call mom",
        "Do not remind me 2025-10-20 14:30:00 to call mom",
        "never remind me 2025-10-20 14:30:00 to call mom",
        "I don\u{2019}t want you to remind me 2025-10-20 14:30:00 to call mom",
    ] {
        expect_abstention(text, AbstentionReason::Negated);
    }
    expect_unrecognized("whenever you can, remind me 2025-10-20 14:30:00 to call mom");
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
    expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    for text in [
        "He wrote \"remind me 2025-10-20 14:30:00 to call mom\"",
        "The sign says 'remind me 2025-10-20 14:30:00 to call mom'",
        "\u{201C}Remind me 2025-10-20 14:30:00 to call mom\u{201D}",
    ] {
        expect_abstention(text, AbstentionReason::UncertainTarget);
    }
    expect_unrecognized("\"Fine.\" Then remind me 2025-10-20 14:30:00 to call mom");
}

#[test]
fn hypothetical_minimal_pair() {
    expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    for text in [
        "what if you remind me 2025-10-20 14:30:00 to call mom",
        "maybe remind me 2025-10-20 14:30:00 to call mom",
        "if it rains remind me 2025-10-20 14:30:00 to call mom",
        "you could remind me 2025-10-20 14:30:00 to call mom",
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
    expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    for text in [
        "Sam said remind me 2025-10-20 14:30:00 to call mom",
        "Sam said, remind me 2025-10-20 14:30:00 to call mom",
        "my friend asked me to remind me 2025-10-20 14:30:00 to call mom",
        "\u{fc}nder the cap he said remind me 2025-10-20 14:30:00 to call mom",
    ] {
        expect_abstention(text, AbstentionReason::UncertainTarget);
    }
}

#[test]
fn completed_work_minimal_pair() {
    expect_explicit(COMMAND, FUTURE_INSTANT, FUTURE_PHRASE);
    expect_abstention(
        "I already did it, remind me 2025-10-20 14:30:00 to call mom",
        AbstentionReason::UncertainTarget,
    );
    expect_abstention(
        "It is done so remind me 2025-10-20 14:30:00 to call mom",
        AbstentionReason::UncertainTarget,
    );
    expect_unrecognized("I already did it. Remind me 2025-10-20 14:30:00 to call mom");
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
    expect_ambiguous("remind me tomorrow to ask the landlord", "tomorrow");
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
    let text = COMMAND;
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
    let text = COMMAND;
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
    assert_action_target(&proposal, text, "call mom");
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
        "to call mom",
        "ma\u{f1}ana",
        ",",
        ".",
        "...",
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
    expect_unrecognized("remind me tomorrow to tell Sam");
    expect_unrecognized("remind me tomorrow to get this done");
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
    expect_ambiguous("remind me tomorrow to read the report", "tomorrow");
    expect_ambiguous("remind me to read the report tomorrow", "tomorrow");
    expect_unrecognized("remind me tomorrow to read the weekly report");
    expect_unrecognized("remind me to read the monthly report tomorrow");
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
    expect_unrecognized("remind me 2025-10-20 14:30:00 to cancel the subscription");
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
    expect_ambiguous("remind me tomorrow to review the report", "tomorrow");
    expect_unrecognized("remind me tomorrow to review the Mondays report");
    expect_unrecognized("remind me tomorrow to check every door");
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
fn content_outside_the_lexicon_is_left_for_the_interpreter() {
    expect_ambiguous("remind me tomorrow to pick up the kids", "tomorrow");
    expect_ambiguous("remind me tomorrow to take the trash out", "tomorrow");
    expect_ambiguous("remind me tomorrow to call her back", "tomorrow");
    for text in [
        "remind me to stop by mom's tomorrow",
        "remind me tomorrow to stop smoking",
        "remind me tomorrow to call Sam",
        "remind me tomorrow to buy milk and eggs",
        "remind me tomorrow to pick up the",
        "remind me tomorrow to milk",
        "remind me tomorrow to call mom call mom call mom",
    ] {
        expect_unrecognized(text);
    }
}

#[test]
fn multi_word_relations_before_a_topic_first_time_never_schedule() {
    expect_explicit(
        "remind me to call mom on 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        "remind me to pay rent 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_ambiguous("remind me to call mom next friday", "next friday");
    for time in ["2025-10-20 14:30:00", "tomorrow", "next friday"] {
        for content in [
            "call mom ahead of",
            "call mom prior to",
            "call mom in advance of",
            "call mom leading up to",
            "call mom the eve of",
            "pay rent due",
            "call mom no later than",
            "call mom at the latest by",
            "call mom roughly",
            "call mom the",
        ] {
            let text = format!("remind me to {content} {time}");
            expect_no_reminder(&text);
        }
    }
}

#[test]
fn every_before_a_time_is_unsupported_recurrence() {
    expect_explicit(
        "remind me to call mom 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_abstention(
        "remind me to call mom every 2025-10-20 14:30:00",
        AbstentionReason::UnsupportedOperation,
    );
    expect_abstention(
        "remind me to call mom each tomorrow",
        AbstentionReason::UnsupportedOperation,
    );
}

#[test]
fn hypothetical_attribution_and_second_predicate_tails_never_schedule() {
    let positive = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(positive, FUTURE_INSTANT, FUTURE_PHRASE);
    expect_unrecognized("remind me 2025-10-20 14:30:00 to be nice to mom");
    expect_unrecognized("remind me 2025-10-20 14:30:00 to have lunch with Sam");
    for suffix in [
        "hypothetically",
        "hypothetically speaking",
        "in theory",
        "theoretically",
        "supposedly",
        "according to Sam",
        "per Sam",
        "via Sam",
        "was the joke",
        "was the plan",
        "would be nice",
        "might be good",
        "could wait",
        "is a good idea",
        "sounds good",
        "seems wise",
        "has been postponed",
    ] {
        let text = format!("{positive} {suffix}");
        expect_no_reminder(&text);
    }
}

#[test]
fn speaker_label_before_the_command_never_schedules() {
    let command = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(command, FUTURE_INSTANT, FUTURE_PHRASE);
    for label in [
        "Note to self: ",
        "Hey Siri: ",
        "Reminder: ",
        "Hey, ",
        "Ok \u{2014} ",
    ] {
        let text = format!("{label}{command}");
        expect_explicit(&text, FUTURE_INSTANT, FUTURE_PHRASE);
    }
    for label in [
        "Sam: ",
        "From Sam: ",
        "Mom (via text): ",
        "Sam (",
        "[Sam] ",
        "Sam \u{2014} ",
        "Caf\u{e9} r\u{e9}sum\u{e9}: ",
    ] {
        let text = format!("{label}{command}");
        expect_abstention(&text, AbstentionReason::UncertainTarget);
    }
    expect_abstention(
        "Mom (via text): remind me 2025-10-20 14:30:00 to call her",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn recurrence_anywhere_in_the_command_never_schedules_a_one_shot() {
    expect_unrecognized("remind me 2025-10-20 14:30:00 to call mom at work");
    expect_unrecognized("remind me 2025-10-20 14:30:00 to file the weekly report at work");
    for text in [
        "remind me 2025-10-20 14:30:00 to call mom weekly at work",
        "remind me 2025-10-20 14:30:00 to call mom daily with Sam",
        "remind me 2025-10-20 14:30:00 to call mom on Sundays at home",
        "remind me 2025-10-20 14:30:00 to call mom every week at work",
        "remind me to call mom weekly at work 2025-10-20 14:30:00",
    ] {
        expect_abstention(text, AbstentionReason::UnsupportedOperation);
    }
}

#[test]
fn only_fillers_or_a_self_label_may_precede_the_command() {
    let command = "remind me 2025-10-20 14:30:00 to bring an umbrella";
    expect_explicit(command, FUTURE_INSTANT, FUTURE_PHRASE);
    expect_explicit(
        &format!("Siri, please {command}"),
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for frame in [
        "In case it rains, ",
        "Assuming it rains, ",
        "Provided it rains, ",
        "When I land, ",
        "Once I land, ",
        "In my dream, ",
        "In the novel, ",
        "Theoretically, ",
        "Supposedly, ",
        "I got home. ",
        "In case it rains. ",
        "Siri ",
    ] {
        expect_unrecognized(&format!("{frame}{command}"));
    }
}

#[test]
fn unlisted_frequency_words_never_reduce_recurrence_to_a_one_shot() {
    let positive = "remind me 2025-10-20 14:30:00 to pay the rent";
    expect_explicit(positive, FUTURE_INSTANT, FUTURE_PHRASE);
    for suffix in [
        "quarterly",
        "biweekly",
        "fortnightly",
        "regularly",
        "periodically",
        "always",
        "forever",
        "every so often",
        "again and again",
        "twice",
        "thrice",
        "each time",
    ] {
        expect_unrecognized(&format!("{positive} {suffix}"));
    }
    expect_explicit(
        "remind me to call mom on 2025-10-20 14:30:00",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_unrecognized("remind me to call mom fortnightly on 2025-10-20 14:30:00");
}

#[test]
fn unlisted_conditions_zones_and_retractions_in_the_content_never_schedule() {
    let positive = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(positive, FUTURE_INSTANT, FUTURE_PHRASE);
    for suffix in [
        "assuming it rains",
        "in case it rains",
        "UTC",
        "Pacific time",
        "nah",
        "done",
        "otherwise",
        "lest",
        "except",
        "until answered",
    ] {
        expect_unrecognized(&format!("{positive} {suffix}"));
    }
    expect_abstention(&format!("{positive} GMT+2"), AbstentionReason::Ambiguous);
}

#[test]
fn quotes_and_brackets_after_the_command_never_schedule() {
    let positive = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(positive, FUTURE_INSTANT, FUTURE_PHRASE);
    expect_explicit(&format!("{positive}."), FUTURE_INSTANT, FUTURE_PHRASE);
    expect_explicit(
        &format!("{positive}, please!"),
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    expect_explicit(
        &format!("{positive} please please, thanks!!"),
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for text in [
        format!("{positive}\""),
        format!("{positive}\u{201D}"),
        format!("{positive}'"),
        format!("{positive})"),
        format!("({positive})"),
        format!("[{positive}]"),
        "remind me to call mom on 2025-10-20 14:30:00\u{201D}".to_string(),
    ] {
        expect_abstention(&text, AbstentionReason::UncertainTarget);
    }
}

#[test]
fn trailing_clause_marks_are_not_trimmed_away() {
    let positive = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(&format!("{positive}."), FUTURE_INSTANT, FUTURE_PHRASE);
    for suffix in [
        ":",
        ";",
        " \u{2014}",
        "\u{2026}",
        " \u{2026}",
        "...",
        " ...",
        "..",
        ":(",
    ] {
        expect_no_reminder(&format!("{positive}{suffix}"));
    }
    expect_unrecognized("remind me 2025-10-20 14:30:00:");
    expect_unrecognized("remind me to call mom 2025-10-20 14:30:00 \u{2014}");
    expect_unrecognized("remind me to call mom 2025-10-20 14:30:00 ...");
}

#[test]
fn markup_and_symbol_prefixes_are_not_the_grammar() {
    let command = "remind me 2025-10-20 14:30:00 to call mom";
    expect_explicit(
        &format!("\u{1F514} {command}"),
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
    for prefix in [
        "> ",
        "` ",
        "``` ",
        "* ",
        "# ",
        "| ",
        "- ",
        "( ",
        "[ ",
        "? ",
        "\u{2026} ",
        "... ",
        "Hey... ",
    ] {
        assert!(
            recognize(&format!("{prefix}{command}"))
                .is_none_or(|proposal| proposal.reminder_proposal.is_none()),
            "{prefix:?} must never schedule"
        );
    }
    for prefix in [
        "> ",
        "` ",
        "``` ",
        "* ",
        "# ",
        "| ",
        "- ",
        "( ",
        "[ ",
        "\u{2026} ",
        "... ",
        "Hey... ",
    ] {
        expect_unrecognized(&format!("{prefix}{command}"));
    }
}

#[test]
fn only_neutral_reminder_pictographs_may_precede_the_command() {
    let command = "remind me 2025-10-20 14:30:00 to call mom";
    for pictograph in [
        "\u{1F514}",         // bell
        "\u{23F0}",          // alarm clock
        "\u{1F4CC}",         // pushpin
        "\u{1F4DD}",         // memo
        "\u{1F5D3}",         // spiral calendar
        "\u{1F5D3}\u{FE0F}", // spiral calendar, emoji presentation
        "\u{1F514}\u{1F514}",
        "\u{1F514} \u{23F0}",
    ] {
        expect_explicit(
            &format!("{pictograph} {command}"),
            FUTURE_INSTANT,
            FUTURE_PHRASE,
        );
    }
    for symbol in [
        "\u{274C}",          // cross mark: negation
        "\u{1F6AB}",         // prohibited: negation
        "\u{26D4}",          // no entry: negation
        "\u{1F645}",         // person gesturing no: negation
        "\u{2705}",          // check mark: completed work
        "\u{1F914}",         // thinking face: musing
        "\u{1F4AD}",         // thought balloon: musing
        "\u{1F5E3}\u{FE0F}", // speaking head: reported speech
        "\u{1F4AC}",         // speech balloon: reported speech
        "\u{1F602}",         // face with tears of joy: a joke
        "\u{1F514}\u{274C}", // an allowed pictograph glued to a cross mark
        "\u{FE0F}",          // a bare presentation selector with no pictograph
        "\u{1F514} \u{274C}",
        "\u{274C},",
    ] {
        expect_unrecognized(&format!("{symbol} {command}"));
        expect_unrecognized(&format!("{symbol} remind me tomorrow to buy milk"));
    }
    expect_unrecognized("\u{274C} \u{1F514} remind me 2025-10-20 14:30:00 to call mom");
    expect_unrecognized("Note to self: \u{274C} remind me 2025-10-20 14:30:00 to call mom");
    expect_explicit(
        "\u{1F514} Note to self: remind me 2025-10-20 14:30:00 to call mom",
        FUTURE_INSTANT,
        FUTURE_PHRASE,
    );
}

fn recognize_topic_in(text: &str) -> Option<Proposal> {
    let proposal = recognize_session_topic(
        text,
        ITEM_ID,
        CAPTURE_ID,
        0,
        TextBasis::Original { item_revision: 0 },
        REQUEST_VERSION,
    );
    if let Some(proposal) = &proposal {
        proposal
            .validate(text)
            .unwrap_or_else(|error| panic!("{text:?} produced an invalid proposal: {error}"));
    }
    proposal
}

fn recognize_topic(text: &str) -> Option<Proposal> {
    recognize_topic_in(text)
}

fn assert_session_topic(proposal: &Proposal, expected_topic: &str, text: &str) {
    let topic_proposal = proposal
        .session_topic_proposal
        .as_ref()
        .unwrap_or_else(|| panic!("{text:?} must have a session-topic proposal"));
    assert_eq!(
        topic_proposal.topic, expected_topic,
        "{text:?}: expected topic {expected_topic:?}"
    );
    let span = topic_proposal
        .source_span
        .expect("session-topic evidence span required");
    assert_eq!(selected(text, span), expected_topic, "{text:?}");
}

fn expect_session_topic(text: &str, expected_topic: &str) {
    let proposal = recognize_topic(text).unwrap_or_else(|| panic!("{text:?} should be recognized"));
    assert_eq!(proposal.abstention, None, "{text:?}");
    assert_eq!(proposal.reminder_proposal, None, "{text:?}");
    assert_eq!(proposal.item_type, None, "{text:?}");
    assert_eq!(proposal.source_spans, None, "{text:?}");
    assert_session_topic(&proposal, expected_topic, text);
}

fn expect_topic_abstention(text: &str, reason: AbstentionReason) {
    let proposal = recognize_topic(text).unwrap_or_else(|| panic!("{text:?} should abstain"));
    assert_eq!(proposal.abstention, Some(reason), "{text:?}");
    assert!(proposal.session_topic_proposal.is_none(), "{text:?}");
}

fn expect_topic_unrecognized(text: &str) {
    assert!(
        recognize_topic(text).is_none(),
        "{text:?} must be left for the approved interpreter"
    );
}

#[test]
fn session_topic_basic_bring_this_up() {
    expect_session_topic("Bring this up in therapy", "therapy");
}

#[test]
fn session_topic_basic_discuss() {
    expect_session_topic("Discuss this at supervision", "supervision");
}

#[test]
fn session_topic_basic_mention() {
    expect_session_topic("Mention this in counseling", "counseling");
}

#[test]
fn session_topic_basic_talk_about() {
    expect_session_topic("Talk about this in coaching", "coaching");
}

#[test]
fn session_topic_with_trailing_please() {
    expect_session_topic("Bring this up in therapy, please", "therapy");
}

#[test]
fn session_topic_with_trailing_thanks() {
    expect_session_topic("Discuss this at supervision thanks", "supervision");
}

#[test]
fn session_topic_with_trailing_punctuation() {
    expect_session_topic("Bring this up in therapy.", "therapy");
    expect_session_topic("Discuss this at supervision!", "supervision");
    expect_session_topic("Mention this in coaching?", "coaching");
}

#[test]
fn session_topic_with_prefix_filler() {
    expect_session_topic("Hey, bring this up in therapy", "therapy");
    expect_session_topic("Ok, discuss this at supervision", "supervision");
}

#[test]
fn session_topic_false_positives_unbounded() {
    // Unbounded topics should not be accepted; these should be left for the interpreter
    expect_topic_unrecognized("Bring this in please");
    expect_topic_unrecognized("Bring this in tomorrow");
    expect_topic_unrecognized("Discuss this at 5pm");
    expect_topic_unrecognized("Talk about this in detail");
    expect_topic_unrecognized("Mention this in passing");
    expect_topic_unrecognized("Discuss this at home");
    expect_topic_unrecognized("Discuss this at work");
    expect_topic_unrecognized("Talk about this in doctor");
    expect_topic_unrecognized("Discuss this at dentist");
}

#[test]
fn session_topic_and_reminder_are_independent() {
    // Reminder-only input should still work for reminders
    let text = "Remind me tomorrow to call mom";
    let reminder_prop = recognize(text);
    assert!(reminder_prop.is_some());
    assert_eq!(
        reminder_prop.as_ref().unwrap().session_topic_proposal,
        None,
        "reminder-only input should not produce a topic"
    );

    let topic_prop = recognize_topic(text);
    assert!(
        topic_prop.is_none(),
        "reminder-only input should not match topic grammar"
    );

    // Session-topic-only input should work for topics
    let text = "Bring this up in therapy";
    let topic_prop = recognize_topic(text);
    assert!(topic_prop.is_some());
    assert_eq!(
        topic_prop.as_ref().unwrap().reminder_proposal,
        None,
        "topic-only input should not produce a reminder"
    );

    let reminder_prop = recognize(text);
    assert!(
        reminder_prop.is_none(),
        "topic-only input should not match reminder grammar"
    );
}

#[test]
fn session_topic_negation_abstains() {
    expect_topic_abstention("Don't bring this up in therapy", AbstentionReason::Negated);
}

#[test]
fn session_topic_quoted_abstains() {
    expect_topic_abstention(
        "He said \"bring this up in therapy\"",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn session_topic_hypothetical_abstains() {
    expect_topic_abstention(
        "If I can, bring this up in therapy",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn session_topic_reported_speech_abstains() {
    expect_topic_abstention(
        "She asked me to bring this up in therapy",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn session_topic_with_closing_delimiter_abstains() {
    expect_topic_abstention(
        "Bring this up in therapy (please)",
        AbstentionReason::UncertainTarget,
    );
}

#[test]
fn session_topic_various_bounded_topics() {
    expect_session_topic("Bring this up in therapy", "therapy");
    expect_session_topic("Bring this up in counseling", "counseling");
    expect_session_topic("Bring this up in coaching", "coaching");
    expect_session_topic("Discuss this at supervision", "supervision");
    expect_session_topic("Discuss this at 1:1", "1:1");
}

#[test]
fn session_topic_unrecognized_shapes() {
    expect_topic_unrecognized("bring up therapy");
    expect_topic_unrecognized("up in therapy");
    expect_topic_unrecognized("Remember therapy");
    expect_topic_unrecognized("therapy");
}

#[test]
fn session_topic_no_reminder_when_topic_recognized() {
    let proposal =
        recognize_topic("Bring this up in therapy").expect("should recognize session-topic phrase");
    assert!(proposal.reminder_proposal.is_none());
    assert!(proposal.session_topic_proposal.is_some());
}

#[test]
fn session_topic_standalone_phrase_recognized() {
    // A session-topic phrase alone (not part of a reminder) should be recognized
    let text = "Bring this up in therapy";
    let proposal = recognize_topic(text).expect("should recognize session-topic phrase");
    assert_eq!(proposal.abstention, None);
    assert_eq!(
        proposal.reminder_proposal, None,
        "should not have reminder facet"
    );
    assert_eq!(proposal.item_type, None, "should not specify item type");
    assert_eq!(proposal.source_spans, None, "should not have action spans");
    let topic_proposal = proposal
        .session_topic_proposal
        .expect("should have session-topic facet");
    assert_eq!(topic_proposal.topic, "therapy");
}

#[test]
fn session_topic_source_span_accuracy() {
    // The source span must select exactly the topic word
    let text = "Bring this up in therapy";
    let proposal = recognize_topic(text).expect("should recognize");
    let topic_proposal = proposal.session_topic_proposal.expect("should have topic");
    let span = topic_proposal.source_span.expect("should have span");
    let selected_text: String = text
        .chars()
        .skip(span.start)
        .take(span.end - span.start)
        .collect();
    assert_eq!(
        selected_text, "therapy",
        "span should select exactly the topic word"
    );
}

#[test]
fn session_topic_permissions_unaffected() {
    // Verify that a topic named "1:1" (which resembles an item scope name) is recognized
    // as a topic, not as a scope constraint, and does not affect the proposal structure.
    let text = "Bring this up in 1:1";
    let proposal = recognize_topic(text).expect("should recognize 1:1 as bounded topic");
    let topic_proposal = proposal.session_topic_proposal.expect("should have topic");
    assert_eq!(
        topic_proposal.topic, "1:1",
        "topic should be recognized as a session topic"
    );
    // For a standalone session-topic phrase, there should be no item type, action span, or reminder.
    // Scope and permission details are applied at the store level, not by the recognizer.
    assert_eq!(proposal.item_type, None, "should not specify item type");
    assert_eq!(
        proposal.reminder_proposal, None,
        "should not have reminder facet"
    );
    assert_eq!(proposal.source_spans, None, "should not have action spans");
}

#[test]
fn reminder_behavior_unchanged_without_topic() {
    // Verify that reminder recognition behavior is unchanged when no session-topic is present
    let text = "Remind me tomorrow to call mom";
    let proposal = recognize(text).expect("should recognize reminder");
    assert_eq!(proposal.abstention, None);
    assert!(
        proposal.reminder_proposal.is_some(),
        "should have reminder facet"
    );
    assert_eq!(
        proposal.item_type,
        Some(ItemType::Action),
        "should be an Action"
    );
    assert!(proposal.source_spans.is_some(), "should have action span");
    assert_eq!(
        proposal.session_topic_proposal, None,
        "should not have session-topic facet"
    );

    // Verify the reminder details are correct
    let reminder = proposal.reminder_proposal.as_ref().unwrap();
    assert_eq!(reminder.quality, TimeResolutionQuality::Ambiguous);
}
