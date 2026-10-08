// Offline reminder and session-topic recognition

use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
};
use crate::providers::contracts::TextBasis;
use crate::time::TimeContext;
use crate::time::TimeResolver;

fn char_index_of(text: &str, pattern: &str) -> Option<usize> {
    text.find(pattern)
        .map(|byte_pos| text[..byte_pos].chars().count())
}

fn word_contains(text: &str, word: &str) -> bool {
    if let Some(mut start) = text.find(word) {
        loop {
            let end = start + word.len();
            let before_ok = start == 0
                || !text
                    .chars()
                    .nth(start - 1)
                    .is_some_and(|c| c.is_alphanumeric());
            let after_ok =
                end >= text.len() || !text.chars().nth(end).is_some_and(|c| c.is_alphanumeric());
            if before_ok && after_ok {
                return true;
            }
            if let Some(next_offset) = text[end..].find(word) {
                start = end + next_offset;
            } else {
                return false;
            }
        }
    }
    false
}

fn is_negated(text_lower: &str) -> bool {
    let has_negation = text_lower.contains("don't")
        || text_lower.contains("dont")
        || text_lower.contains("do not")
        || (text_lower.contains("never") && !text_lower.contains("whenever"));
    let has_verb = text_lower.contains("remind") || text_lower.contains("tell");

    has_negation && has_verb
}

fn is_quoted(text: &str) -> bool {
    text.contains("\"") || text.contains("'")
}

fn is_hypothetical(text_lower: &str) -> bool {
    let hypothetical_markers = [
        "what if", "maybe", "perhaps", "possibly", "might", "could", "would", "may",
    ];
    let has_hypothetical_marker = hypothetical_markers
        .iter()
        .any(|marker| text_lower.contains(marker));

    let has_speaker = ["they", "he", "she", "it", "someone", "people"]
        .iter()
        .any(|s| word_contains(text_lower, s));
    let has_indirect = ["said", "think", "asked", "want", "told", "need"]
        .iter()
        .any(|v| text_lower.contains(v));
    let has_remind = text_lower.contains("remind");

    has_hypothetical_marker || (has_speaker && has_indirect && has_remind)
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

    if is_negated(&text_lower) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis.clone(),
                request_version,
            )
            .with_abstention(Some(AbstentionReason::Negated)),
        );
    }

    if is_quoted(text) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis.clone(),
                request_version,
            )
            .with_abstention(Some(AbstentionReason::UncertainTarget)),
        );
    }

    if is_hypothetical(&text_lower) {
        return Some(
            create_proposal(
                item_id,
                capture_id,
                source_revision,
                text_basis.clone(),
                request_version,
            )
            .with_abstention(Some(AbstentionReason::UncertainTarget)),
        );
    }

    let (cmd, cmd_len, cmd_str) = if text_lower.find("remind me").is_some() {
        let byte_pos = text_lower.find("remind me").unwrap();
        (byte_pos, 9, "remind me")
    } else if text_lower.find("tell me").is_some() {
        let byte_pos = text_lower.find("tell me").unwrap();
        (byte_pos, 7, "tell me")
    } else {
        return None;
    };

    let time_phrase = text[cmd + cmd_len..].trim();

    if time_phrase.is_empty() {
        let char_cmd_pos = char_index_of(&text_lower, cmd_str).unwrap_or(0);
        let reminder = ReminderProposal {
            instant: None,
            timezone_id: None,
            quality: TimeResolutionQuality::Ambiguous,
            source_span: Some(SourceSpan::new(char_cmd_pos, char_cmd_pos + cmd_len)),
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

    let time_phrase_lower = time_phrase.to_lowercase();
    let char_pos = char_index_of(&text_lower, &time_phrase_lower);

    match TimeResolver::resolve(time_phrase, time_context) {
        Ok(result) => {
            if let Some(resolved_time) = result.resolved_time {
                if result.is_ambiguous {
                    if let Some(pos) = char_pos {
                        let reminder = ReminderProposal {
                            instant: None,
                            timezone_id: None,
                            quality: TimeResolutionQuality::Ambiguous,
                            source_span: Some(SourceSpan::new(
                                pos,
                                pos + time_phrase.chars().count(),
                            )),
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
                } else if let Some(pos) = char_pos {
                    let reminder = ReminderProposal {
                        instant: Some(resolved_time.to_rfc3339()),
                        timezone_id: Some(time_context.timezone.clone()),
                        quality: TimeResolutionQuality::Explicit,
                        source_span: Some(SourceSpan::new(pos, pos + time_phrase.chars().count())),
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
            } else if let Some(pos) = char_pos {
                let reminder = ReminderProposal {
                    instant: None,
                    timezone_id: None,
                    quality: TimeResolutionQuality::Ambiguous,
                    source_span: Some(SourceSpan::new(pos, pos + time_phrase.chars().count())),
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
        }
        Err(_) => {
            return Some(
                create_proposal(
                    item_id,
                    capture_id,
                    source_revision,
                    text_basis,
                    request_version,
                )
                .with_abstention(Some(AbstentionReason::Ambiguous)),
            );
        }
    }

    Some(
        create_proposal(
            item_id,
            capture_id,
            source_revision,
            text_basis,
            request_version,
        )
        .with_abstention(Some(AbstentionReason::Ambiguous)),
    )
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
    word_contains(&lower, "every")
}
