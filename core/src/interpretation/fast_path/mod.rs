// Offline reminder and session-topic recognition

use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
};
use crate::providers::contracts::TextBasis;
use crate::time::TimeContext;
use crate::time::TimeResolver;

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
    let has_speaker = ["they", "he", "she", "it", "someone", "people"]
        .iter()
        .any(|s| text_lower.contains(s));
    let has_indirect = [
        "said", "said to", "think", "asked", "want", "told", "should", "need to",
    ]
    .iter()
    .any(|v| text_lower.contains(v));
    let has_remind = text_lower.contains("remind");

    has_speaker && has_indirect && has_remind
}

/// Attempt to recognize an explicit offline reminder command from captured text.
///
/// Returns `Some(proposal)` if a supported reminder pattern is recognized with enough
/// information to create a proposal, or `None` if the text is ambiguous, negated, or
/// does not match supported patterns. Unsupported patterns (e.g., recurring reminders)
/// return a proposal with an explicit abstention.
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

    // Reject negations: "don't remind me", "never remind me", etc.
    if is_negated(&text_lower) {
        let proposal = Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            item_id.to_string(),
            capture_id.to_string(),
            source_revision,
            crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            text_basis,
            request_version.to_string(),
        )
        .with_abstention(Some(AbstentionReason::Negated));
        return Some(proposal);
    }

    // Reject quoted speech: the reminder is part of a quote, not an instruction
    if is_quoted(text) {
        let proposal = Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            item_id.to_string(),
            capture_id.to_string(),
            source_revision,
            crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            text_basis,
            request_version.to_string(),
        )
        .with_abstention(Some(AbstentionReason::UncertainTarget));
        return Some(proposal);
    }

    // Reject hypothetical/reported speech: "they said to remind me", etc.
    if is_hypothetical(&text_lower) {
        let proposal = Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            item_id.to_string(),
            capture_id.to_string(),
            source_revision,
            crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            text_basis,
            request_version.to_string(),
        )
        .with_abstention(Some(AbstentionReason::UncertainTarget));
        return Some(proposal);
    }

    // Extract the time phrase after "remind me" or "tell me"
    let time_phrase = if let Some(pos) = text_lower.find("remind me") {
        text[pos + 9..].trim()
    } else if let Some(pos) = text_lower.find("tell me") {
        text[pos + 8..].trim()
    } else if text_lower.contains("remind") && text_lower.contains("me") {
        // Loose match for "remind" and "me" in the same sentence
        return None;
    } else {
        return None;
    };

    // Check for unsupported recurrence patterns
    if is_unsupported_recurrence(time_phrase) {
        let proposal = Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            item_id.to_string(),
            capture_id.to_string(),
            source_revision,
            crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            text_basis,
            request_version.to_string(),
        )
        .with_abstention(Some(AbstentionReason::UnsupportedOperation));
        return Some(proposal);
    }

    // Try to resolve the time
    if time_phrase.is_empty() {
        // No time specified: ambiguous
        let proposal = Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            item_id.to_string(),
            capture_id.to_string(),
            source_revision,
            crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            text_basis,
            request_version.to_string(),
        );

        // Find the span of "remind me" or "tell me"
        if let Some(pos) = text_lower.find("remind me") {
            let start = pos;
            let end = pos + 9;
            let reminder = ReminderProposal {
                instant: None,
                timezone_id: None,
                quality: TimeResolutionQuality::Ambiguous,
                source_span: Some(SourceSpan::new(start, end)),
            };
            return Some(proposal.with_reminder_proposal(Some(reminder)));
        } else if let Some(pos) = text_lower.find("tell me") {
            let start = pos;
            let end = pos + 7;
            let reminder = ReminderProposal {
                instant: None,
                timezone_id: None,
                quality: TimeResolutionQuality::Ambiguous,
                source_span: Some(SourceSpan::new(start, end)),
            };
            return Some(proposal.with_reminder_proposal(Some(reminder)));
        }

        return None;
    }

    // Try to resolve the time phrase
    match TimeResolver::resolve(time_phrase, time_context) {
        Ok(result) => {
            if let Some(resolved_time) = result.resolved_time {
                // Explicit or inferred resolution
                let quality = if result.is_ambiguous {
                    TimeResolutionQuality::Ambiguous
                } else {
                    TimeResolutionQuality::Explicit
                };

                let proposal = Proposal::new(
                    uuid::Uuid::new_v4().to_string(),
                    item_id.to_string(),
                    capture_id.to_string(),
                    source_revision,
                    crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
                    text_basis,
                    request_version.to_string(),
                );

                // Find the source span of the time phrase in the original text
                if let Some(phrase_pos) = text_lower.find(time_phrase.to_lowercase().as_str()) {
                    let reminder = ReminderProposal {
                        instant: Some(resolved_time.to_rfc3339()),
                        timezone_id: Some(time_context.timezone.clone()),
                        quality,
                        source_span: Some(SourceSpan::new(
                            phrase_pos,
                            phrase_pos + time_phrase.chars().count(),
                        )),
                    };
                    return Some(proposal.with_reminder_proposal(Some(reminder)));
                }
            } else if result.is_ambiguous {
                // Ambiguous time (no resolved instant)
                let proposal = Proposal::new(
                    uuid::Uuid::new_v4().to_string(),
                    item_id.to_string(),
                    capture_id.to_string(),
                    source_revision,
                    crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
                    text_basis,
                    request_version.to_string(),
                );

                if let Some(phrase_pos) = text_lower.find(time_phrase.to_lowercase().as_str()) {
                    let reminder = ReminderProposal {
                        instant: None,
                        timezone_id: None,
                        quality: TimeResolutionQuality::Ambiguous,
                        source_span: Some(SourceSpan::new(
                            phrase_pos,
                            phrase_pos + time_phrase.chars().count(),
                        )),
                    };
                    return Some(proposal.with_reminder_proposal(Some(reminder)));
                }
            }
        }
        Err(_) => {
            // Could not resolve the time: abstain
            let proposal = Proposal::new(
                uuid::Uuid::new_v4().to_string(),
                item_id.to_string(),
                capture_id.to_string(),
                source_revision,
                crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION,
                text_basis,
                request_version.to_string(),
            )
            .with_abstention(Some(AbstentionReason::Ambiguous));
            return Some(proposal);
        }
    }

    None
}

fn is_unsupported_recurrence(phrase: &str) -> bool {
    let lower = phrase.to_lowercase();
    [
        "daily",
        "every day",
        "weekly",
        "every week",
        "monthly",
        "every month",
        "yearly",
        "every year",
        "recurring",
        "repeat",
        "every",
        "always",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}
