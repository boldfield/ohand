// Offline reminder and session-topic recognition

use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
};
use crate::providers::contracts::TextBasis;
use crate::time::TimeContext;
use crate::time::TimeResolver;

fn find_word_in_text(text: &str, word: &str) -> Option<usize> {
    for (char_idx, _) in text.char_indices() {
        let char_count = text[..char_idx].chars().count();
        if text[char_idx..].starts_with(word) {
            let before_ok = char_count == 0
                || text[..char_idx]
                    .chars()
                    .last()
                    .is_some_and(|c| !c.is_alphanumeric());
            let after_char_count = char_count + word.chars().count();
            let after_ok = after_char_count >= text.chars().count()
                || text
                    .chars()
                    .nth(after_char_count)
                    .is_some_and(|c| !c.is_alphanumeric());
            if before_ok && after_ok {
                return Some(char_count);
            }
        }
    }
    None
}

fn is_negated_around(text_lower: &str, cmd_pos: usize, _cmd_len: usize) -> bool {
    let before = &text_lower[..text_lower
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0)];
    before.contains("don't")
        || before.contains("dont")
        || before.contains("do not")
        || (before.contains("never") && !before.contains("whenever"))
}

fn is_quoted_around(text: &str, cmd_pos: usize) -> bool {
    let byte_pos = text
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = &text[..byte_pos];
    (before.matches("\"").count() + before.matches("'").count()) % 2 == 1
}

fn is_hypothetical_around(text_lower: &str, cmd_pos: usize) -> bool {
    let byte_pos = text_lower
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = &text_lower[..byte_pos];

    let hypothetical_markers = [
        "what if", "maybe", "perhaps", "possibly", "might", "could", "would", "may",
    ];
    let has_hypothetical_marker = hypothetical_markers.iter().any(|marker| {
        if let Some(pos) = before.rfind(marker) {
            let before_marker = &before[..pos];
            let end_byte = pos + marker.len();
            let after_marker = &before[end_byte..];
            (before_marker.chars().last().is_none()
                || !before_marker.chars().last().unwrap().is_alphanumeric())
                && (after_marker.chars().next().is_none()
                    || !after_marker.chars().next().unwrap().is_alphanumeric())
        } else {
            false
        }
    });

    if has_hypothetical_marker {
        return true;
    }

    let has_speaker = ["they", "he", "she", "it", "someone", "people"]
        .iter()
        .any(|s| find_word_in_text(before, s).is_some());
    let has_indirect = [
        "said", "says", "say", "think", "thinks", "thought", "asked", "asks", "ask", "want",
        "wants", "wanted", "told", "tells", "tell", "need", "needs", "needed",
    ]
    .iter()
    .any(|v| find_word_in_text(before, v).is_some());

    has_speaker && has_indirect
}

fn create_proposal(
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
) -> Proposal {
    Proposal::new(
        uuid::Uuid::new_v4().to_string(),
        item_id.to_string(),
        capture_id.to_string(),
        source_revision,
        crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
        text_basis,
        request_version.to_string(),
    )
}

fn find_command(text_lower: &str) -> Option<(usize, usize)> {
    find_word_in_text(text_lower, "remind me")
        .map(|pos| (pos, 9))
        .or_else(|| find_word_in_text(text_lower, "tell me").map(|pos| (pos, 7)))
}

/// Attempt to recognize an explicit offline reminder command from captured text.
///
/// Returns `Some(proposal)` if a supported reminder pattern is recognized with enough
/// information to create a proposal, or `None` if the text does not match supported patterns.
/// Negated, quoted, or hypothetical patterns return a proposal with an explicit abstention.
/// Unmatched language is left for the approved interpreter (returns None).
pub fn recognize_reminder(
    text: &str,
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
    time_context: &TimeContext,
) -> Option<Proposal> {
    let text_lower = text.to_lowercase();

    let (cmd_pos, cmd_len) = find_command(&text_lower)?;

    if is_negated_around(&text_lower, cmd_pos, cmd_len) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_abstention(Some(AbstentionReason::Negated)),
        );
    }

    if is_quoted_around(text, cmd_pos) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_abstention(Some(AbstentionReason::UncertainTarget)),
        );
    }

    if is_hypothetical_around(&text_lower, cmd_pos) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_abstention(Some(AbstentionReason::UncertainTarget)),
        );
    }

    let byte_after_cmd = text
        .char_indices()
        .nth(cmd_pos + cmd_len)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    let time_phrase = text[byte_after_cmd..].trim();

    if time_phrase.is_empty() {
        let reminder = ReminderProposal {
            instant: None,
            timezone_id: None,
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(cmd_pos, cmd_pos + cmd_len)),
        };
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_reminder_proposal(Some(reminder)),
        );
    }

    if is_unsupported_recurrence(time_phrase) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_abstention(Some(AbstentionReason::UnsupportedOperation)),
        );
    }

    let time_phrase_char_start = cmd_pos + cmd_len;

    match TimeResolver::resolve(time_phrase, time_context) {
        Ok(result) => {
            if let Some(resolved_time) = result.resolved_time {
                if result.is_ambiguous {
                    let reminder = ReminderProposal {
                        instant: None,
                        timezone_id: None,
                        quality: TimeResolutionQuality::Ambiguous,
                        source_span: Some(SourceSpan::new(
                            time_phrase_char_start,
                            time_phrase_char_start + time_phrase.chars().count(),
                        )),
                    };
                    Some(
                        create_proposal(
                            item_id,
                            capture_id,
                            source_revision,
                            text_basis,
                            request_version,
                        )
                        .with_reminder_proposal(Some(reminder)),
                    )
                } else {
                    let reminder = ReminderProposal {
                        instant: Some(resolved_time.to_rfc3339()),
                        timezone_id: Some(time_context.timezone.clone()),
                        quality: TimeResolutionQuality::Explicit,
                        source_span: Some(SourceSpan::new(
                            time_phrase_char_start,
                            time_phrase_char_start + time_phrase.chars().count(),
                        )),
                    };
                    Some(
                        create_proposal(
                            item_id,
                            capture_id,
                            source_revision,
                            text_basis,
                            request_version,
                        )
                        .with_reminder_proposal(Some(reminder)),
                    )
                }
            } else {
                let reminder = ReminderProposal {
                    instant: None,
                    timezone_id: None,
                    quality: TimeResolutionQuality::Ambiguous,
                    source_span: Some(SourceSpan::new(
                        time_phrase_char_start,
                        time_phrase_char_start + time_phrase.chars().count(),
                    )),
                };
                Some(
                    create_proposal(
                        item_id,
                        capture_id,
                        source_revision,
                        text_basis,
                        request_version,
                    )
                    .with_reminder_proposal(Some(reminder)),
                )
            }
        }
        Err(_) => Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
            )
            .with_abstention(Some(AbstentionReason::Ambiguous)),
        ),
    }
}

fn is_unsupported_recurrence(phrase: &str) -> bool {
    let lower = phrase.to_lowercase();
    let patterns = [
        "daily",
        "weekly",
        "monthly",
        "yearly",
        "every day",
        "every week",
        "every month",
        "every year",
        "recurring",
        "repeat",
        "always",
    ];
    for pattern in &patterns {
        if lower.contains(pattern) {
            return true;
        }
    }
    find_word_in_text(&lower, "every").is_some()
}
