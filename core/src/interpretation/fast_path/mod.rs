//! Offline recognition of explicit one-shot reminder commands.
//!
//! The recognizer accepts a small, closed grammar and nothing else. Everything is matched on
//! whole word tokens in the original text, and every offset is a Unicode scalar offset into that
//! same text, so spans always select exactly the evidence they describe.
//!
//! ```text
//! command  := filler* "remind" "me" ( timed | topic-first )
//! timed    := ["on"] time ["," ] [ "to" content ]
//! topic-first := "to" content ["on"] time
//! time     := YYYY-MM-DD HH:MM:SS | YYYY-MM-DD | "tomorrow" | weekday | "next" weekday
//! filler   := "please" | "hey" | "ok" | ... (closed list)
//! ```
//!
//! The command must start its clause. Anything else that merely contains "remind me" (for
//! example "can the calendar remind me ..." or "my friend asked me to remind me ...") is not
//! this grammar and is left alone.
//!
//! Outcomes of [`recognize_reminder`]:
//! - `None`: the text is not the supported grammar. Nothing is derived, nothing is scheduled and
//!   the full text remains available to the approved interpreter.
//! - `Some(proposal)` with a reminder: a supported command. An exact future time is `Explicit`
//!   with an instant; date-only, past, DST-gap and DST-fold times are `Ambiguous` without an
//!   instant (never guessed) and keep their evidence span for the correction path.
//! - `Some(proposal)` with an abstention: the grammar matched but a safety guard applies
//!   (negation, quotation, hypothetical or reported speech, completed work, a retraction or
//!   condition after the command, two competing times, or recurrence). Nothing is scheduled; the
//!   abstention only records why and never suppresses later interpretation of the same text.

use crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
};
use crate::store::events::ItemType;
use crate::time::{ResolutionResult, TimeContext, TimeResolver};
use chrono::SecondsFormat;

const FILLERS: &[&str] = &[
    "please", "hey", "hi", "hello", "ok", "okay", "oh", "so", "and", "also", "then", "well", "um",
    "uh", "alright", "yo",
];

const NEGATION_WORDS: &[&str] = &[
    "not", "never", "dont", "wont", "cant", "cannot", "stop", "without",
];

const HYPOTHETICAL_WORDS: &[&str] = &[
    "if",
    "unless",
    "maybe",
    "perhaps",
    "possibly",
    "might",
    "could",
    "would",
    "should",
    "may",
    "whether",
    "suppose",
    "imagine",
    "wonder",
    "hypothetically",
];

const REPORTING_WORDS: &[&str] = &[
    "said",
    "says",
    "say",
    "told",
    "tells",
    "tell",
    "asked",
    "asks",
    "ask",
    "wants",
    "wanted",
    "want",
    "thinks",
    "thought",
    "think",
    "claims",
    "claimed",
    "mentioned",
    "mentions",
    "wrote",
    "writes",
    "texted",
    "replied",
    "suggested",
    "suggests",
];

const COMPLETED_WORDS: &[&str] = &["already", "done", "finished", "completed", "did"];

const CONTENT_CONDITION_WORDS: &[&str] = &[
    "if",
    "unless",
    "maybe",
    "perhaps",
    "possibly",
    "whether",
    "nevermind",
];

const CONTENT_RETRACTION_SEQUENCES: &[&[&str]] = &[
    &["never", "mind"],
    &["scratch", "that"],
    &["forget", "it"],
    &["or", "not"],
    &["or", "maybe"],
];

const RECURRENCE_WORDS: &[&str] = &[
    "every",
    "daily",
    "weekly",
    "monthly",
    "yearly",
    "hourly",
    "nightly",
    "annually",
    "recurring",
    "repeat",
    "repeating",
    "repeatedly",
    "weekdays",
    "weekends",
];

const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Word,
    ClauseBreak,
    SentenceBreak,
    Quote,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    ch: char,
    start: usize,
    end: usize,
    lower: String,
}

impl Token {
    fn is_word(&self) -> bool {
        self.kind == TokenKind::Word
    }

    fn is_word_equal_to(&self, expected: &str) -> bool {
        self.is_word() && self.lower == expected
    }

    fn has_letters_or_digits(&self) -> bool {
        self.lower.chars().any(char::is_alphanumeric)
    }
}

fn classify_punctuation(chars: &[char], index: usize) -> Option<TokenKind> {
    let ch = chars[index];
    let inside_word = index > 0
        && chars[index - 1].is_alphanumeric()
        && chars
            .get(index + 1)
            .is_some_and(|next| next.is_alphanumeric());
    match ch {
        '"' | '\u{201C}' | '\u{201D}' | '\u{AB}' | '\u{BB}' => Some(TokenKind::Quote),
        '\'' | '\u{2018}' | '\u{2019}' => (!inside_word).then_some(TokenKind::Quote),
        '!' | '?' => Some(TokenKind::SentenceBreak),
        '.' => (!inside_word).then_some(TokenKind::SentenceBreak),
        ',' | ';' | ':' => (!inside_word).then_some(TokenKind::ClauseBreak),
        '(' | ')' | '[' | ']' | '\u{2014}' | '\u{2013}' | '\u{2026}' => {
            Some(TokenKind::ClauseBreak)
        }
        _ => None,
    }
}

fn normalized_lowercase(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' => '\'',
            other => other,
        })
        .collect()
}

fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            if ch == '\n' || ch == '\r' {
                tokens.push(Token {
                    kind: TokenKind::SentenceBreak,
                    ch: '\n',
                    start: index,
                    end: index + 1,
                    lower: String::new(),
                });
            }
            index += 1;
            continue;
        }
        if let Some(kind) = classify_punctuation(&chars, index) {
            tokens.push(Token {
                kind,
                ch,
                start: index,
                end: index + 1,
                lower: String::new(),
            });
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len()
            && !chars[index].is_whitespace()
            && classify_punctuation(&chars, index).is_none()
        {
            index += 1;
        }
        let word: String = chars[start..index].iter().collect();
        tokens.push(Token {
            kind: TokenKind::Word,
            ch: ' ',
            start,
            end: index,
            lower: normalized_lowercase(&word),
        });
    }
    tokens
}

fn has_word(tokens: &[Token], vocabulary: &[&str]) -> bool {
    tokens
        .iter()
        .any(|token| token.is_word() && vocabulary.contains(&token.lower.as_str()))
}

fn has_sequence(tokens: &[Token], sequence: &[&str]) -> bool {
    tokens.windows(sequence.len()).any(|window| {
        window
            .iter()
            .zip(sequence)
            .all(|(token, expected)| token.is_word_equal_to(expected))
    })
}

fn is_negation_word(token: &Token) -> bool {
    token.is_word()
        && (NEGATION_WORDS.contains(&token.lower.as_str()) || token.lower.ends_with("n't"))
}

fn weekday_from(word: &str) -> Option<&'static str> {
    WEEKDAYS.iter().copied().find(|weekday| *weekday == word)
}

fn is_plural_weekday(word: &str) -> bool {
    word.strip_suffix('s')
        .is_some_and(|singular| weekday_from(singular).is_some())
}

fn is_recurrence_word(token: &Token) -> bool {
    token.is_word()
        && (RECURRENCE_WORDS.contains(&token.lower.as_str()) || is_plural_weekday(&token.lower))
}

fn is_digits(text: &str, expected_length: usize) -> bool {
    text.chars().count() == expected_length && text.chars().all(|ch| ch.is_ascii_digit())
}

fn is_iso_date(word: &str) -> bool {
    let parts: Vec<&str> = word.split('-').collect();
    parts.len() == 3 && is_digits(parts[0], 4) && is_digits(parts[1], 2) && is_digits(parts[2], 2)
}

fn is_iso_clock(word: &str) -> bool {
    let parts: Vec<&str> = word.split(':').collect();
    parts.len() == 3 && parts.iter().all(|part| is_digits(part, 2))
}

fn looks_like_time_phrase(window: &[Token]) -> bool {
    if !window.iter().all(Token::is_word) {
        return false;
    }
    match window {
        [only] => {
            is_iso_date(&only.lower)
                || only.lower == "tomorrow"
                || weekday_from(&only.lower).is_some()
        }
        [first, second] => {
            (is_iso_date(&first.lower) && is_iso_clock(&second.lower))
                || (first.lower == "next" && weekday_from(&second.lower).is_some())
        }
        _ => false,
    }
}

struct MatchedTime {
    first_token: usize,
    token_count: usize,
    span: SourceSpan,
    resolution: ResolutionResult,
}

fn resolve_window(
    tokens: &[Token],
    first_token: usize,
    token_count: usize,
    time_context: &TimeContext,
) -> Option<MatchedTime> {
    let window = tokens.get(first_token..first_token + token_count)?;
    if !looks_like_time_phrase(window) {
        return None;
    }
    let phrase = window
        .iter()
        .map(|token| token.lower.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let resolution = TimeResolver::resolve(&phrase, time_context).ok()?;
    Some(MatchedTime {
        first_token,
        token_count,
        span: SourceSpan::new(window[0].start, window[token_count - 1].end),
        resolution,
    })
}

fn time_starting_at(
    tokens: &[Token],
    first_token: usize,
    time_context: &TimeContext,
) -> Option<MatchedTime> {
    [2, 1]
        .into_iter()
        .find_map(|count| resolve_window(tokens, first_token, count, time_context))
}

fn time_ending_at_end(tokens: &[Token], time_context: &TimeContext) -> Option<MatchedTime> {
    [2, 1].into_iter().find_map(|count| {
        let first_token = tokens.len().checked_sub(count)?;
        resolve_window(tokens, first_token, count, time_context)
    })
}

struct ParsedCommand<'a> {
    time: MatchedTime,
    content: &'a [Token],
}

enum Shape<'a> {
    Recurrence,
    Scheduled(Box<ParsedCommand<'a>>),
}

fn trim_breaks(tokens: &[Token]) -> &[Token] {
    let mut start = 0;
    let mut end = tokens.len();
    while start < end && !tokens[start].is_word() {
        start += 1;
    }
    while end > start && !tokens[end - 1].is_word() {
        end -= 1;
    }
    &tokens[start..end]
}

fn content_is_usable(content: &[Token]) -> bool {
    content.iter().any(Token::has_letters_or_digits)
        && content
            .iter()
            .all(|token| token.kind != TokenKind::SentenceBreak)
}

fn command_body(rest: &[Token]) -> &[Token] {
    let mut end = rest.len();
    while end > 0 {
        let token = &rest[end - 1];
        let is_trailing_filler = token.is_word_equal_to("please")
            || token.is_word_equal_to("thanks")
            || token.kind == TokenKind::ClauseBreak
            || token.kind == TokenKind::Quote
            || (token.kind == TokenKind::SentenceBreak && (token.ch == '.' || token.ch == '!'));
        if !is_trailing_filler {
            break;
        }
        end -= 1;
    }
    &rest[..end]
}

fn parse_shape<'a>(rest: &'a [Token], time_context: &TimeContext) -> Option<Shape<'a>> {
    let first = rest.first()?;
    let lead_is_grammar = first.is_word_equal_to("to")
        || first.is_word_equal_to("on")
        || is_recurrence_word(first)
        || looks_like_time_phrase(&rest[..1]);
    if lead_is_grammar && rest.iter().any(is_recurrence_word) {
        return Some(Shape::Recurrence);
    }

    let body = command_body(rest);
    if body.iter().any(|token| token.ch == '?') {
        return None;
    }

    if first.is_word_equal_to("to") {
        let after_to = body.get(1..)?;
        let time = time_ending_at_end(after_to, time_context)?;
        let mut content_end = time.first_token;
        if content_end > 0 && after_to[content_end - 1].is_word_equal_to("on") {
            content_end -= 1;
        }
        let content = trim_breaks(&after_to[..content_end]);
        if !content_is_usable(content) {
            return None;
        }
        return Some(Shape::Scheduled(Box::new(ParsedCommand { time, content })));
    }

    let time_start = usize::from(first.is_word_equal_to("on"));
    let time = time_starting_at(body, time_start, time_context)?;
    let mut next = time.first_token + time.token_count;
    if body.get(next).is_some_and(|token| token.ch == ',') {
        next += 1;
    }
    if next >= body.len() {
        return Some(Shape::Scheduled(Box::new(ParsedCommand {
            time,
            content: &[],
        })));
    }
    if !body[next].is_word_equal_to("to") {
        return None;
    }
    let content = trim_breaks(&body[next + 1..]);
    if !content_is_usable(content) {
        return None;
    }
    Some(Shape::Scheduled(Box::new(ParsedCommand { time, content })))
}

fn find_command(tokens: &[Token]) -> Option<usize> {
    tokens
        .windows(2)
        .position(|pair| pair[0].is_word_equal_to("remind") && pair[1].is_word_equal_to("me"))
}

fn current_sentence(prefix: &[Token]) -> &[Token] {
    let start = prefix
        .iter()
        .rposition(|token| token.kind == TokenKind::SentenceBreak)
        .map_or(0, |index| index + 1);
    &prefix[start..]
}

fn current_clause(prefix: &[Token]) -> &[Token] {
    let start = prefix
        .iter()
        .rposition(|token| {
            matches!(
                token.kind,
                TokenKind::ClauseBreak | TokenKind::SentenceBreak
            )
        })
        .map_or(0, |index| index + 1);
    &prefix[start..]
}

fn prefix_abstention(prefix: &[Token]) -> Option<AbstentionReason> {
    let sentence = current_sentence(prefix);
    if sentence.iter().any(is_negation_word) {
        return Some(AbstentionReason::Negated);
    }
    let open_quote_count = prefix
        .iter()
        .filter(|token| token.kind == TokenKind::Quote)
        .count();
    if open_quote_count % 2 == 1
        || has_word(sentence, HYPOTHETICAL_WORDS)
        || has_word(sentence, REPORTING_WORDS)
        || has_word(sentence, COMPLETED_WORDS)
    {
        return Some(AbstentionReason::UncertainTarget);
    }
    None
}

fn starts_its_clause(prefix: &[Token]) -> bool {
    current_clause(prefix).iter().all(|token| {
        token.kind == TokenKind::Quote
            || !token.has_letters_or_digits()
            || FILLERS.contains(&token.lower.as_str())
    })
}

fn content_abstention(content: &[Token]) -> Option<AbstentionReason> {
    if CONTENT_RETRACTION_SEQUENCES
        .iter()
        .any(|sequence| has_sequence(content, sequence))
    {
        return Some(AbstentionReason::Negated);
    }
    if has_word(content, CONTENT_CONDITION_WORDS) {
        return Some(AbstentionReason::UncertainTarget);
    }
    None
}

struct Provenance<'a> {
    item_id: &'a str,
    capture_id: &'a str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &'a str,
}

impl Provenance<'_> {
    fn proposal(&self) -> Proposal {
        Proposal::new(
            uuid::Uuid::new_v4().to_string(),
            self.item_id.to_string(),
            self.capture_id.to_string(),
            self.source_revision,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            self.text_basis.clone(),
            self.request_version.to_string(),
        )
    }

    fn abstention(&self, reason: AbstentionReason) -> Proposal {
        self.proposal().with_abstention(Some(reason))
    }
}

fn reminder_from(matched: &MatchedTime, time_context: &TimeContext) -> ReminderProposal {
    let resolution = &matched.resolution;
    let exact_instant = match resolution.resolved_time {
        Some(instant) if !resolution.is_ambiguous && !resolution.is_past => Some(instant),
        _ => None,
    };
    ReminderProposal {
        instant: exact_instant.map(|instant| instant.to_rfc3339_opts(SecondsFormat::Secs, true)),
        timezone_id: Some(time_context.timezone.clone()),
        quality: if exact_instant.is_some() {
            TimeResolutionQuality::Explicit
        } else {
            TimeResolutionQuality::Ambiguous
        },
        source_span: Some(matched.span),
    }
}

/// Recognize an explicit offline reminder command in `text`, the exact text identified by
/// `text_basis`. See the module documentation for the grammar and the meaning of the outcomes.
pub fn recognize_reminder(
    text: &str,
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
    time_context: &TimeContext,
) -> Option<Proposal> {
    let tokens = tokenize(text);
    let command_index = find_command(&tokens)?;
    let prefix = &tokens[..command_index];
    let rest = &tokens[command_index + 2..];
    let shape = parse_shape(rest, time_context)?;
    let provenance = Provenance {
        item_id,
        capture_id,
        source_revision,
        text_basis,
        request_version,
    };

    if let Some(reason) = prefix_abstention(prefix) {
        return Some(provenance.abstention(reason));
    }
    if !starts_its_clause(prefix) {
        return None;
    }
    let ParsedCommand { time, content } = match shape {
        Shape::Recurrence => {
            return Some(provenance.abstention(AbstentionReason::UnsupportedOperation));
        }
        Shape::Scheduled(parsed) => *parsed,
    };
    if let Some(reason) = content_abstention(content) {
        return Some(provenance.abstention(reason));
    }
    if time_ending_at_end(content, time_context).is_some() {
        return Some(provenance.abstention(AbstentionReason::Ambiguous));
    }

    let mut proposal = provenance
        .proposal()
        .with_reminder_proposal(Some(reminder_from(&time, time_context)));
    if let (Some(first), Some(last)) = (content.first(), content.last()) {
        proposal = proposal
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![SourceSpan::new(first.start, last.end)]));
    }
    Some(proposal)
}
