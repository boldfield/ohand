// Offline reminder and session-topic recognition

use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TimeResolutionQuality,
};
use crate::providers::contracts::TextBasis;
use crate::time::TimeContext;
use crate::time::TimeResolver;

// Find a word in text case-insensitively, respecting word boundaries.
// Returns the char position of the start of the word in the original text.
fn find_word_case_insensitive(text: &str, word: &str) -> Option<usize> {
    let word_lower = word.to_lowercase();

    for idx in 0..text.chars().count() {
        let remaining_text_from_byte = text
            .char_indices()
            .nth(idx)
            .map(|(i, _)| i)
            .unwrap_or_else(|| text.len());
        let remaining = &text[remaining_text_from_byte..];
        let remaining_lower = remaining.to_lowercase();

        if remaining_lower.starts_with(&word_lower) {
            // Check word boundary before
            let before_ok = idx == 0
                || text
                    .chars()
                    .nth(idx.saturating_sub(1))
                    .is_some_and(|c| !c.is_alphanumeric());

            // Check word boundary after
            let word_char_count = word.chars().count();
            let after_idx = idx + word_char_count;
            let after_ok = after_idx >= text.chars().count()
                || text
                    .chars()
                    .nth(after_idx)
                    .is_some_and(|c| !c.is_alphanumeric());

            if before_ok && after_ok {
                return Some(idx);
            }
        }
    }
    None
}

fn is_negated_around(text: &str, cmd_pos: usize) -> bool {
    let before_byte = text
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = text[..before_byte].to_lowercase();

    // Match negation words with word boundaries
    find_word_case_insensitive(&before, "don't").is_some()
        || find_word_case_insensitive(&before, "dont").is_some()
        || find_word_case_insensitive(&before, "do not").is_some()
        || (find_word_case_insensitive(&before, "never").is_some() && !before.contains("whenever"))
}

fn is_quoted_around(text: &str, cmd_pos: usize) -> bool {
    let before_byte = text
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = &text[..before_byte];

    // Count unescaped double quotes and single quotes used as delimiters, not contractions.
    // An apostrophe is a quote delimiter if there's no alphanumeric character before or after it.
    let double_quote_count = before.matches('"').count();
    let mut single_quote_count = 0;
    let mut prev_char = ' ';
    for ch in before.chars() {
        if ch == '\'' && !prev_char.is_alphanumeric() {
            single_quote_count += 1;
        }
        prev_char = ch;
    }

    (double_quote_count + single_quote_count) % 2 == 1
}

fn is_hypothetical_around(text: &str, cmd_pos: usize) -> bool {
    let before_byte = text
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = &text[..before_byte];
    let before_lower = before.to_lowercase();

    let hypothetical_markers = [
        "what if", "maybe", "perhaps", "possibly", "might", "could", "would", "may",
    ];
    let has_hypothetical_marker = hypothetical_markers
        .iter()
        .any(|marker| find_word_case_insensitive(&before_lower, marker).is_some());

    if has_hypothetical_marker {
        return true;
    }

    let has_speaker = ["they", "he", "she", "it", "someone", "people"]
        .iter()
        .any(|s| find_word_case_insensitive(&before_lower, s).is_some());
    let has_indirect = [
        "said", "says", "say", "think", "thinks", "thought", "asked", "asks", "ask", "want",
        "wants", "wanted", "told", "tells", "tell", "need", "needs", "needed",
    ]
    .iter()
    .any(|v| find_word_case_insensitive(&before_lower, v).is_some());

    has_speaker && has_indirect
}

fn is_completed_work(text: &str, cmd_pos: usize) -> bool {
    let before_byte = text
        .char_indices()
        .nth(cmd_pos)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = text[..before_byte].to_lowercase();

    let completed_markers = ["already", "done", "finished", "completed", "set"];
    completed_markers
        .iter()
        .any(|marker| find_word_case_insensitive(&before, marker).is_some())
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

// Find "remind me" command in the original text case-insensitively.
// Only "remind me" is supported; "tell me" is not a reminder command.
fn find_command(text: &str) -> Option<(usize, usize)> {
    find_word_case_insensitive(text, "remind me").map(|pos| (pos, 9))
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
    let (cmd_pos, cmd_len) = find_command(text)?;

    if is_negated_around(text, cmd_pos) {
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

    if is_hypothetical_around(text, cmd_pos) {
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

    if is_completed_work(text, cmd_pos) {
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

    // Get the byte position after the command in the original text
    let byte_after_cmd = text
        .char_indices()
        .nth(cmd_pos + cmd_len)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    let after_cmd = &text[byte_after_cmd..];
    let trimmed_phrase = after_cmd.trim_start();

    if trimmed_phrase.is_empty() {
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

    if is_unsupported_recurrence(trimmed_phrase) {
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

    // Calculate the char offset where the trimmed phrase starts in the original text
    let trimmed_byte_offset = after_cmd.len() - trimmed_phrase.len();
    let time_phrase_char_start =
        cmd_pos + cmd_len + after_cmd[..trimmed_byte_offset].chars().count();

    match TimeResolver::resolve(trimmed_phrase, time_context) {
        Ok(result) => {
            if let Some(resolved_time) = result.resolved_time {
                if result.is_ambiguous {
                    let reminder = ReminderProposal {
                        instant: None,
                        timezone_id: None,
                        quality: TimeResolutionQuality::Ambiguous,
                        source_span: Some(SourceSpan::new(
                            time_phrase_char_start,
                            time_phrase_char_start + trimmed_phrase.chars().count(),
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
                            time_phrase_char_start + trimmed_phrase.chars().count(),
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
                        time_phrase_char_start + trimmed_phrase.chars().count(),
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
    find_word_case_insensitive(&lower, "every").is_some()
}
