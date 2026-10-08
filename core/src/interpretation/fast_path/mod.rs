//! Offline recognition of explicit one-shot reminder commands.
//!
//! The recognizer accepts a small, closed grammar and nothing else. Everything is matched on
//! whole word tokens in the original text, and every offset is a Unicode scalar offset into that
//! same text, so spans always select exactly the evidence they describe.
//!
//! ```text
//! text     := prefix "remind" "me" ( timed | topic-first ) ["please" | "thanks"] ["," | "." | "!"]*
//! prefix   := (filler | pictograph)* [ self-label ( "," | ":" | dash ) filler* ]
//! timed    := ["on"] time ["," ] [ "to" content ]
//! topic-first := "to" content ["on"] time
//! content  := verb particle? object*      (closed content lexicon, at most six words)
//! object   := determiner noun | noun | object-pronoun | particle
//! time     := YYYY-MM-DD HH:MM:SS | YYYY-MM-DD | "tomorrow" | weekday | "next" weekday
//! filler   := "please" | "hey" | "ok" | ... (closed list)
//! self-label := "note to self" | "note" | "memo" | "reminder" | "todo" | "siri" | ...
//! ```
//!
//! Both open ends of the command are closed by allowlists, not by deny-lists. Nothing but
//! fillers and a self-addressed label may come before the command, in any sentence, so a frame
//! such as "In case it rains, ...", "When I land, ..." or "In the novel, ..." is not this grammar.
//! The content is built only from the closed content lexicon (action verbs, particles,
//! determiners, object pronouns and a short noun list), which holds no negation, condition,
//! frequency, zone, time, reporting, control word or preposition. Any unlisted word ("quarterly",
//! "nah", "UTC", "assuming", "Sam") therefore makes the text not this grammar, and it is left for
//! the approved interpreter. The word lists used below to classify known unsafe forms only choose
//! the abstention reason; they are not the safety boundary.
//!
//! A speaker label before a colon, bracket or dash ("Sam:") is attribution and abstains. A quote
//! or bracket anywhere after the command ("... to call mom\"", "(remind me ...)") abstains, and
//! markup or brackets before it ("> remind me", "` remind me") are not this grammar. Text
//! that merely contains "remind me" (for example "can the calendar remind me ..." or "my friend
//! asked me to remind me ...") is not this grammar and is left alone.
//!
//! Outcomes of [`recognize_reminder`]:
//! - `None`: the text is not the supported grammar. Nothing is derived, nothing is scheduled and
//!   the full text remains available to the approved interpreter.
//! - `Some(proposal)` with a reminder: a supported command. An exact future time is `Explicit`
//!   with an instant; date-only, past, DST-gap and DST-fold times are `Ambiguous` without an
//!   instant (never guessed) and keep their evidence span for the correction path.
//! - `Some(proposal)` with an abstention: the grammar matched but a safety guard applies
//!   (negation, quotation, hypothetical or reported speech, completed work, a retraction,
//!   negation, quote, clause break, condition, attribution or second predicate inside the content,
//!   a speaker label, two competing times, a relation word before the time, or a recurrence
//!   request anywhere in the command). Nothing is scheduled; the
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

const MAX_CONTENT_WORDS: usize = 8;

const CONTENT_NEGATION_WORDS: &[&str] = &["not", "never", "dont", "wont", "cant", "cannot"];

const CONTENT_TIME_WORDS: &[&str] = &[
    "today",
    "tonight",
    "tomorrow",
    "yesterday",
    "noon",
    "midnight",
    "morning",
    "afternoon",
    "evening",
    "night",
    "later",
    "soon",
    "now",
    "asap",
    "eventually",
    "someday",
    "sometime",
    "am",
    "pm",
    "oclock",
    "second",
    "seconds",
    "minute",
    "minutes",
    "hour",
    "hours",
    "day",
    "days",
    "week",
    "weeks",
    "weekend",
    "weekends",
    "weekday",
    "weekdays",
    "month",
    "months",
    "year",
    "years",
    "fortnight",
    "ago",
    "next",
    "tues",
    "thurs",
    "january",
    "february",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

const NUMBER_WORDS: &[&str] = &[
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "half",
    "quarter",
    "couple",
    "few",
];

const NUMBER_INTRODUCERS: &[&str] = &[
    "at", "in", "by", "around", "within", "until", "till", "after", "before", "about",
];

// In the topic-first form the time must follow the content directly (or after "on"). When the
// content ends in a preposition, particle, determiner or relation word ("before", "ahead of",
// "prior to", "the eve of", "due", "every"), the time is a relation target or deadline, not the
// reminder time. Prepositions and determiners are closed classes, so this list is bounded.
const TEMPORAL_RELATION_WORDS: &[&str] = &[
    "about",
    "above",
    "across",
    "after",
    "against",
    "ahead",
    "along",
    "amid",
    "among",
    "approximately",
    "around",
    "as",
    "at",
    "before",
    "behind",
    "below",
    "beneath",
    "beside",
    "besides",
    "between",
    "beyond",
    "by",
    "circa",
    "despite",
    "down",
    "due",
    "during",
    "earliest",
    "effective",
    "eve",
    "except",
    "for",
    "from",
    "in",
    "inside",
    "into",
    "latest",
    "like",
    "near",
    "of",
    "off",
    "onto",
    "out",
    "outside",
    "over",
    "past",
    "per",
    "prior",
    "roughly",
    "since",
    "starting",
    "than",
    "through",
    "throughout",
    "thru",
    "till",
    "to",
    "toward",
    "towards",
    "under",
    "until",
    "unto",
    "up",
    "upon",
    "via",
    "with",
    "within",
    "without",
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "my",
    "your",
    "our",
    "their",
    "his",
    "her",
    "its",
    "every",
    "each",
];

const CONTENT_CLAUSE_WORDS: &[&str] = &[
    "i", "we", "he", "she", "they", "or", "but", "though", "however", "anyway", "whatever",
    "whenever", "instead", "rather",
];

const CONTENT_SUBJECT_CONTRACTION_PREFIXES: &[&str] =
    &["i", "we", "he", "she", "they", "it", "that", "there", "let"];

const CONTENT_CONTROL_VERBS: &[&str] = &[
    "ignore",
    "disregard",
    "forget",
    "cancel",
    "delete",
    "remove",
    "skip",
    "scrap",
    "drop",
    "abort",
    "undo",
    "revoke",
    "mind",
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
    "hypothetically",
    "theoretically",
    "supposedly",
    "allegedly",
    "reportedly",
    "apparently",
    "presumably",
    "probably",
    "hopefully",
    "ideally",
    "suppose",
    "imagine",
];

const CONTENT_CONDITION_SEQUENCES: &[&[&str]] = &[&["in", "theory"], &["in", "principle"]];

// Attribution markers: "according to Sam", "per Sam", "via Sam".
const CONTENT_ATTRIBUTION_WORDS: &[&str] = &["according", "per", "via"];

// A modal, finite copula/auxiliary or linking verb in the content starts a second predicate
// ("to call mom would be nice", "to call mom was the plan"), so the content is not the single
// clause the grammar accepts. Base forms ("be", "have", "do") stay ordinary reminder verbs.
const CONTENT_PREDICATE_WORDS: &[&str] = &[
    "would", "might", "could", "should", "may", "must", "will", "shall", "can", "is", "was",
    "were", "are", "has", "had", "does", "sounds", "sounded", "seems", "seemed", "feels", "looks",
    "looked", "appears", "appeared",
];

const CONTENT_RETRACTION_WORDS: &[&str] = &[
    "actually",
    "kidding",
    "joking",
    "jk",
    "nvm",
    "nope",
    "nevermind",
    "no",
];

const CONTENT_RETRACTION_SEQUENCES: &[&[&str]] = &[
    &["never", "mind"],
    &["scratch", "that"],
    &["forget", "it"],
    &["forget", "that"],
    &["cancel", "that"],
    &["cancel", "it"],
    &["or", "not"],
    &["or", "maybe"],
];

// Past or third-person forms only: the base forms ("tell", "ask", "want") are ordinary reminder
// verbs ("remind me to ask the landlord ...").
const CONTENT_REPORTING_WORDS: &[&str] = &[
    "said",
    "says",
    "told",
    "tells",
    "asked",
    "asks",
    "wants",
    "wanted",
    "thinks",
    "thought",
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

const CONTENT_COMPLETED_WORDS: &[&str] = &["already", "did"];

const CONTENT_COMPLETION_AUXILIARIES: &[&str] = &[
    "is", "was", "been", "has", "had", "have", "i", "we", "all", "it's", "that's",
];

const CONTENT_COMPLETION_PARTICIPLES: &[&str] = &["done", "finished", "completed"];

const RECURRENCE_ADVERBS: &[&str] = &[
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

const RECURRENCE_UNITS: &[&str] = &[
    "day",
    "week",
    "month",
    "year",
    "hour",
    "minute",
    "morning",
    "afternoon",
    "evening",
    "night",
    "weekday",
    "weekend",
    "other",
];

const DETERMINERS: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "his", "her",
    "its",
];

// A label before a colon, bracket or dash in the command's sentence is attribution ("Sam:",
// "From Sam:", "Mom (via text):"), not the user's own command, unless it is one of these
// self-addressed labels or assistant names.
const SELF_LABELS: &[&[&str]] = &[
    &[],
    &["note", "to", "self"],
    &["note"],
    &["memo"],
    &["reminder"],
    &["todo"],
    &["to-do"],
    &["to", "do"],
    &["siri"],
    &["assistant"],
];

// The content lexicon is the safety boundary of the content. A content is accepted only when it
// is one action verb followed by an object built from these closed word lists, so no unlisted
// word (a retraction, condition, frequency, zone, attribution or any other qualifier) can ever
// reach a schedule: anything outside the lexicon is not this grammar and is left for the
// approved interpreter. The lists hold no negation, time, frequency, condition, reporting or
// control words, and no prepositions, so a lexicon content cannot carry a competing time or a
// relation to the time. The deny-lists above only name the abstention reason for known unsafe
// forms; they are not what keeps unlisted language from being scheduled.
const CONTENT_VERBS: &[&str] = &[
    "call", "phone", "text", "email", "buy", "get", "pick", "pay", "book", "check", "take",
    "bring", "return", "water", "feed", "walk", "clean", "wash", "pack", "send", "renew", "charge",
    "refill", "order", "mail", "visit", "fix", "submit", "print", "read", "review", "ask",
];

// Verb particles ("pick up", "take out", "call back"). They may follow the verb or end the content.
const CONTENT_PARTICLES: &[&str] = &["up", "out", "back"];

// Determiners and possessives. Each must be followed by a noun.
const CONTENT_DETERMINERS: &[&str] = &["the", "a", "an", "my", "our", "some"];

const CONTENT_OBJECT_PRONOUNS: &[&str] = &["her", "him", "them"];

const CONTENT_NOUNS: &[&str] = &[
    "mom",
    "dad",
    "grandma",
    "grandpa",
    "sister",
    "brother",
    "landlord",
    "dentist",
    "doctor",
    "roofer",
    "plumber",
    "vet",
    "bank",
    "pharmacy",
    "school",
    "milk",
    "bread",
    "eggs",
    "groceries",
    "coffee",
    "rent",
    "bill",
    "bills",
    "taxes",
    "invoice",
    "report",
    "prescription",
    "medicine",
    "pills",
    "plants",
    "dog",
    "cat",
    "kids",
    "trash",
    "laundry",
    "dishes",
    "car",
    "bike",
    "package",
    "parcel",
    "library",
    "books",
    "passport",
    "license",
    "insurance",
    "gift",
    "flowers",
    "tickets",
    "umbrella",
    "keys",
    "charger",
    "recycling",
    "letter",
    "form",
    "appointment",
    "roof",
];

const MAX_LEXICON_CONTENT_WORDS: usize = 6;

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

fn is_plural_weekday_token(token: &Token) -> bool {
    token.is_word() && is_plural_weekday(&token.lower)
}

fn is_recurrence_adverb(token: &Token) -> bool {
    token.is_word() && RECURRENCE_ADVERBS.contains(&token.lower.as_str())
}

fn is_number_token(token: &Token) -> bool {
    token.is_word()
        && (NUMBER_WORDS.contains(&token.lower.as_str())
            || token.lower.starts_with(|ch: char| ch.is_ascii_digit()))
}

fn is_unit_token(token: &Token) -> bool {
    token.is_word()
        && (RECURRENCE_UNITS.contains(&token.lower.as_str())
            || weekday_from(&token.lower).is_some()
            || is_plural_weekday(&token.lower)
            || token
                .lower
                .strip_suffix('s')
                .is_some_and(|singular| RECURRENCE_UNITS.contains(&singular)))
}

/// Number of tokens in the recurrence phrase starting at `index`, or 0 when there is none.
fn recurrence_phrase_length(tokens: &[Token], index: usize) -> usize {
    let Some(first) = tokens.get(index) else {
        return 0;
    };
    if is_recurrence_adverb(first) || is_plural_weekday_token(first) {
        return 1;
    }
    if !(first.is_word_equal_to("every") || first.is_word_equal_to("each")) {
        return 0;
    }
    let Some(second) = tokens.get(index + 1) else {
        return 0;
    };
    if second.is_word()
        && (is_iso_date(&second.lower) || second.lower == "tomorrow" || second.lower == "next")
    {
        return 2;
    }
    if is_unit_token(second) && !second.is_word_equal_to("other") {
        return 2;
    }
    if second.is_word_equal_to("other") || is_number_token(second) {
        return if tokens.get(index + 2).is_some_and(is_unit_token) {
            3
        } else if second.is_word_equal_to("other") {
            0
        } else {
            2
        };
    }
    0
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
    dangling_relation: bool,
}

enum Shape<'a> {
    Recurrence,
    Scheduled(Box<ParsedCommand<'a>>),
}

/// Drops commas that end the content ("to call mom, on <time>"). Every other break stays in the
/// content so that the content checks see quotes, brackets and clause marks.
fn trim_trailing_commas(tokens: &[Token]) -> &[Token] {
    let mut end = tokens.len();
    while end > 0 && tokens[end - 1].ch == ',' {
        end -= 1;
    }
    &tokens[..end]
}

fn content_is_usable(content: &[Token]) -> bool {
    content.iter().any(Token::has_letters_or_digits)
        && content
            .iter()
            .all(|token| token.kind != TokenKind::SentenceBreak)
}

fn is_closing_delimiter(token: &Token) -> bool {
    token.kind == TokenKind::Quote || matches!(token.ch, ')' | ']')
}

/// The command without terminal "please", "thanks", commas, full stops, exclamation marks and
/// closing quotes or brackets. Closing quotes and brackets are only set aside to find the shape:
/// any quote or bracket after the command makes [`recognize_reminder`] abstain. A trailing colon,
/// semicolon, dash or ellipsis is kept, so the content checks still see it.
fn command_body(rest: &[Token]) -> &[Token] {
    let mut end = rest.len();
    while end > 0 {
        let token = &rest[end - 1];
        let is_trailing_filler = token.is_word_equal_to("please")
            || token.is_word_equal_to("thanks")
            || token.ch == ','
            || is_closing_delimiter(token)
            || (token.kind == TokenKind::SentenceBreak && (token.ch == '.' || token.ch == '!'));
        if !is_trailing_filler {
            break;
        }
        end -= 1;
    }
    &rest[..end]
}

fn is_every_unit_lead(rest: &[Token]) -> bool {
    rest.first().is_some_and(|first| {
        (first.is_word_equal_to("every") || first.is_word_equal_to("each")) && rest.len() > 1
    })
}

/// Recurrence is a request, not vocabulary. A recurrence phrase ("every <unit>", "every two
/// days", "each morning", a recurrence adverb, a plural weekday) anywhere in the command is a
/// recurrence request, except where an adverb or plural weekday modifies a noun after a
/// determiner ("the weekly report", "the Mondays report").
fn is_recurrence_request(rest: &[Token]) -> bool {
    let body = command_body(rest);
    (0..body.len()).any(|index| {
        let length = recurrence_phrase_length(body, index);
        let is_noun_modifier = index > 0
            && !body[index].is_word_equal_to("every")
            && !body[index].is_word_equal_to("each")
            && DETERMINERS.contains(&body[index - 1].lower.as_str())
            && body[index - 1].is_word();
        length > 0 && !is_noun_modifier
    })
}

fn parse_shape<'a>(rest: &'a [Token], time_context: &TimeContext) -> Option<Shape<'a>> {
    let first = rest.first()?;
    let lead_is_grammar = first.is_word_equal_to("to")
        || first.is_word_equal_to("on")
        || is_recurrence_adverb(first)
        || is_every_unit_lead(rest)
        || is_plural_weekday_token(first)
        || looks_like_time_phrase(&rest[..1]);
    if lead_is_grammar && is_recurrence_request(rest) {
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
        let content = trim_trailing_commas(&after_to[..content_end]);
        if !content_is_usable(content) {
            return None;
        }
        let dangling_relation = content.last().is_some_and(|token| {
            token.is_word() && TEMPORAL_RELATION_WORDS.contains(&token.lower.as_str())
        });
        return Some(Shape::Scheduled(Box::new(ParsedCommand {
            time,
            content,
            dangling_relation,
        })));
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
            dangling_relation: false,
        })));
    }
    if !body[next].is_word_equal_to("to") {
        return None;
    }
    let content = trim_trailing_commas(&body[next + 1..]);
    if !content_is_usable(content) {
        return None;
    }
    Some(Shape::Scheduled(Box::new(ParsedCommand {
        time,
        content,
        dangling_relation: false,
    })))
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

fn is_label_break(token: &Token) -> bool {
    token.kind == TokenKind::ClauseBreak && token.ch != ',' && token.ch != ';'
}

fn has_speaker_label(sentence: &[Token]) -> bool {
    if !sentence.iter().any(is_label_break) {
        return false;
    }
    let label: Vec<&str> = sentence
        .iter()
        .filter(|token| {
            token.is_word()
                && token.has_letters_or_digits()
                && !FILLERS.contains(&token.lower.as_str())
        })
        .map(|token| token.lower.as_str())
        .collect();
    !SELF_LABELS.contains(&label.as_slice())
}

fn prefix_abstention(prefix: &[Token]) -> Option<AbstentionReason> {
    let sentence = current_sentence(prefix);
    if sentence.iter().any(is_negation_word) {
        return Some(AbstentionReason::Negated);
    }
    if has_speaker_label(sentence) {
        return Some(AbstentionReason::UncertainTarget);
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

/// Decorative pictographs ("\u{1F514}") may stand before the command. Markup and other symbols
/// (">", "`", "*", "#", "|") may quote or delimit it, so they are not allowed.
fn is_decorative_symbol(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x1F300..=0x1FAFF | 0x2600..=0x26FF | 0x2705..=0x2757 | 0xFE0F | 0x200D
    )
}

/// Each token before the command must be a word (checked as a filler or self label below), a
/// decorative pictograph, a sentence break or one of the comma, semicolon, colon or dash
/// separators. Quotes, brackets, ellipses, question marks and other symbols are not allowed.
fn is_allowed_prefix_token(token: &Token) -> bool {
    match token.kind {
        TokenKind::Word => {
            token.has_letters_or_digits() || token.lower.chars().all(is_decorative_symbol)
        }
        TokenKind::ClauseBreak => matches!(token.ch, ',' | ';' | ':' | '\u{2014}' | '\u{2013}'),
        TokenKind::SentenceBreak => token.ch != '?',
        TokenKind::Quote => false,
    }
}

/// A quote or bracket after the command means the command is quoted, cited or set apart from
/// the user's own words ("... to call mom\"", "(remind me ...)"), so it is never scheduled.
fn has_delimiter_after_command(rest: &[Token]) -> bool {
    rest.iter()
        .any(|token| is_closing_delimiter(token) || matches!(token.ch, '(' | '['))
}

/// The whole text before the command may only hold fillers ("please", "hey") and, before a
/// comma, colon or dash, a self-addressed label or assistant name ("Note to self:", "Siri,").
/// Any other earlier word, in this sentence or an earlier one, may frame the command ("In case
/// it rains, ...", "When I land, ...", "In the novel, ..."), so the text is not this grammar.
fn prefix_is_allowed(prefix: &[Token]) -> bool {
    if !prefix.iter().all(is_allowed_prefix_token) {
        return false;
    }
    let label: Vec<&str> = prefix
        .iter()
        .filter(|token| {
            token.is_word()
                && token.has_letters_or_digits()
                && !FILLERS.contains(&token.lower.as_str())
        })
        .map(|token| token.lower.as_str())
        .collect();
    if label.is_empty() {
        return true;
    }
    let ends_with_label_break = prefix
        .iter()
        .rev()
        .find(|token| !(token.is_word() && FILLERS.contains(&token.lower.as_str())))
        .is_some_and(|token| {
            token.kind == TokenKind::ClauseBreak
                && matches!(token.ch, ',' | ':' | '\u{2014}' | '\u{2013}')
        });
    ends_with_label_break
        && prefix
            .iter()
            .all(|token| token.kind != TokenKind::SentenceBreak)
        && SELF_LABELS[1..].contains(&label.as_slice())
}

fn has_completion(content: &[Token]) -> bool {
    has_word(content, CONTENT_COMPLETED_WORDS)
        || content.windows(2).any(|pair| {
            pair[0].is_word()
                && CONTENT_COMPLETION_AUXILIARIES.contains(&pair[0].lower.as_str())
                && pair[1].is_word()
                && CONTENT_COMPLETION_PARTICIPLES.contains(&pair[1].lower.as_str())
        })
}

fn has_time_phrase(content: &[Token]) -> bool {
    (0..content.len()).any(|first_token| {
        [2, 1].into_iter().any(|count| {
            content
                .get(first_token..first_token + count)
                .is_some_and(looks_like_time_phrase)
        })
    })
}

fn is_content_negation_word(token: &Token) -> bool {
    token.is_word()
        && (CONTENT_NEGATION_WORDS.contains(&token.lower.as_str()) || token.lower.ends_with("n't"))
}

fn is_subject_contraction(token: &Token) -> bool {
    token.is_word()
        && token
            .lower
            .split_once('\'')
            .is_some_and(|(prefix, _)| CONTENT_SUBJECT_CONTRACTION_PREFIXES.contains(&prefix))
}

fn has_competing_time_evidence(content: &[Token]) -> bool {
    has_time_phrase(content)
        || content.iter().any(|token| {
            token.is_word()
                && (token.lower.chars().any(char::is_numeric)
                    || CONTENT_TIME_WORDS.contains(&token.lower.as_str())
                    || weekday_from(&token.lower).is_some())
        })
        || content.windows(2).any(|pair| {
            pair[0].is_word()
                && NUMBER_INTRODUCERS.contains(&pair[0].lower.as_str())
                && is_number_token(&pair[1])
        })
}

fn has_plain_word_shape(word: &str) -> bool {
    word.chars()
        .all(|ch| ch.is_alphabetic() || ch == '\'' || ch == '-')
}

fn is_single_plain_clause(content: &[Token]) -> bool {
    let words: Vec<&Token> = content.iter().filter(|token| token.is_word()).collect();
    words.len() <= MAX_CONTENT_WORDS
        && words.iter().enumerate().all(|(position, token)| {
            has_plain_word_shape(&token.lower)
                && !CONTENT_CLAUSE_WORDS.contains(&token.lower.as_str())
                && !CONTENT_PREDICATE_WORDS.contains(&token.lower.as_str())
                && !is_subject_contraction(token)
                && (position == 0 || !CONTENT_CONTROL_VERBS.contains(&token.lower.as_str()))
        })
}

fn in_lexicon(token: &Token, vocabulary: &[&str]) -> bool {
    token.is_word() && vocabulary.contains(&token.lower.as_str())
}

/// The content grammar: `verb particle? object*`, where an object word is a determiner followed
/// by a noun, a noun, an object pronoun or a particle, every word comes from the closed content
/// lexicon, and the content ends in a noun, pronoun or particle.
fn content_is_in_lexicon(content: &[Token]) -> bool {
    let Some((verb, objects)) = content.split_first() else {
        return true;
    };
    if !in_lexicon(verb, CONTENT_VERBS) || content.len() > MAX_LEXICON_CONTENT_WORDS {
        return false;
    }
    let objects_are_lexicon = objects.iter().enumerate().all(|(position, token)| {
        if in_lexicon(token, CONTENT_DETERMINERS) {
            return objects
                .get(position + 1)
                .is_some_and(|next| in_lexicon(next, CONTENT_NOUNS));
        }
        in_lexicon(token, CONTENT_NOUNS)
            || in_lexicon(token, CONTENT_OBJECT_PRONOUNS)
            || in_lexicon(token, CONTENT_PARTICLES)
    });
    objects_are_lexicon
        && content.last().is_some_and(|last| {
            in_lexicon(last, CONTENT_NOUNS)
                || in_lexicon(last, CONTENT_OBJECT_PRONOUNS)
                || in_lexicon(last, CONTENT_PARTICLES)
        })
}

/// The reminder content is bounded: it must be a single short plain clause. Negation,
/// retraction, reported speech or attribution, completed work, conditions or hypotheticals,
/// quotes, brackets, clause breaks, any further time and any second clause or predicate all
/// mean the user may not be asking for this schedule, so nothing is scheduled.
fn content_abstention(content: &[Token]) -> Option<AbstentionReason> {
    if content.iter().any(is_content_negation_word)
        || has_word(content, CONTENT_RETRACTION_WORDS)
        || CONTENT_RETRACTION_SEQUENCES
            .iter()
            .any(|sequence| has_sequence(content, sequence))
    {
        return Some(AbstentionReason::Negated);
    }
    if content
        .iter()
        .any(|token| matches!(token.kind, TokenKind::ClauseBreak | TokenKind::Quote))
        || has_word(content, CONTENT_CONDITION_WORDS)
        || CONTENT_CONDITION_SEQUENCES
            .iter()
            .any(|sequence| has_sequence(content, sequence))
        || has_word(content, CONTENT_ATTRIBUTION_WORDS)
        || has_word(content, CONTENT_REPORTING_WORDS)
        || has_completion(content)
    {
        return Some(AbstentionReason::UncertainTarget);
    }
    if has_competing_time_evidence(content) {
        return Some(AbstentionReason::Ambiguous);
    }
    if !is_single_plain_clause(content) {
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
    if has_delimiter_after_command(rest) {
        return Some(provenance.abstention(AbstentionReason::UncertainTarget));
    }
    if !prefix_is_allowed(prefix) {
        return None;
    }
    let ParsedCommand {
        time,
        content,
        dangling_relation,
    } = match shape {
        Shape::Recurrence => {
            return Some(provenance.abstention(AbstentionReason::UnsupportedOperation));
        }
        Shape::Scheduled(parsed) => *parsed,
    };
    if let Some(reason) = content_abstention(content) {
        return Some(provenance.abstention(reason));
    }
    if dangling_relation {
        return Some(provenance.abstention(AbstentionReason::Ambiguous));
    }
    if !content_is_in_lexicon(content) {
        return None;
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
