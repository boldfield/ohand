//! Offline recognition of explicit one-shot reminder commands.
//!
//! The recognizer accepts a small, closed grammar and nothing else. Everything is matched on
//! whole word tokens in the original text, and every offset is a Unicode scalar offset into that
//! same text, so spans always select exactly the evidence they describe.
//!
//! ```text
//! text     := prefix "remind" "me" ( timed | topic-first ) ( "please" | "thanks" | "," | "." | "!" )*
//! prefix   := (filler | pictograph)* [ self-label ( "," | ":" | dash ) filler* ]
//! pictograph := U+1F514 bell | U+23F0 alarm clock | U+1F4CC pushpin | U+1F4DD memo
//!             | U+1F5D3 spiral calendar      (each optionally followed by U+FE0F)
//! timed    := ["on"] time [","] "to" content
//! topic-first := "to" content ["on"] time
//! content  := verb particle? object*      (closed content lexicon, at most six words)
//! object   := determiner noun | noun | object-pronoun | particle
//! time     := YYYY-MM-DD HH:MM:SS | YYYY-MM-DD | "tomorrow" | weekday | "next" weekday
//! filler   := "please" | "hey" | "ok" | ... (closed list)
//! self-label := "note to self" | "note" | "memo" | "reminder" | "todo" | "siri" | ...
//! ```
//!
//! The content is mandatory. A reminder always rides on an action with a sourced target (see
//! `docs/validation/intent-fixtures.md`, "A reminder always rides on an action"), and the
//! reminder state machine only applies a reminder to an item that can carry an obligation. A
//! bare time phrase ("remind me tomorrow") therefore has nothing to remind about and is not this
//! grammar, exactly like a command without a time ("remind me to buy milk").
//!
//! Trailing "please" or "thanks", commas, full stops and exclamation marks may repeat after the
//! command. An ellipsis ("..." or U+2026) is a clause mark, not terminal punctuation: the user
//! trails off, so the command abstains instead of being scheduled.
//!
//! Both open ends of the command are closed by allowlists, not by deny-lists. Nothing but
//! fillers, the five neutral reminder pictographs named above and a self-addressed label may come
//! before the command, in any sentence, so a frame such as "In case it rains, ...", "When I
//! land, ..." or "In the novel, ..." is not this grammar. Any other emoji or symbol may negate
//! ("\u{274C}"), complete ("\u{2705}"), muse ("\u{1F914}"), report ("\u{1F5E3}") or joke
//! ("\u{1F602}") about the command, so it is not this grammar either.
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
//! - `Some(proposal)` with a reminder: a supported command. The proposal is an `Action` whose
//!   source span selects the content exactly. An exact future time is `Explicit` with an
//!   instant; date-only, past, DST-gap and DST-fold times are `Ambiguous` without an instant
//!   (never guessed) and keep their evidence span for the correction path.
//! - `Some(proposal)` with an abstention: the grammar matched but a safety guard applies
//!   (negation, quotation, hypothetical or reported speech, completed work, a retraction,
//!   negation, quote, clause break, condition, attribution or second predicate inside the content,
//!   a speaker label, two competing times, a relation word before the time, or a recurrence
//!   request anywhere in the command). Nothing is scheduled; the
//!   abstention only records why and never suppresses later interpretation of the same text.
//!
//! # Session topics
//!
//! [`recognize_session_topic`] and [`recognize_with_session_topic`] add a second, independent
//! bounded grammar: an explicit phrase that files the note under a session topic. It derives
//! only the session-topic facet. It never sets item scope, a route, upload authorization or
//! preview eligibility, and a topic stated at capture or corrected by the user still outranks it
//! (`resolve_session_topic`).
//!
//! ```text
//! clause    := lead preposition [ "my" | "our" | "the" ] [ "next" ] topic [ "session" ]
//! lead      := "bring" ("this"|"it") "up" | "raise" ("this"|"it") | "mention" ("this"|"it")
//!            | "discuss" ("this"|"it") | "talk" "about" ("this"|"it")
//! preposition := "in" | "at" | "during"
//! topic     := "therapy" | "counseling" | "counselling" | "coaching" | "supervision"
//! sentence  := prefix [ intent ] clause ( "please" | "thanks" | "," )* terminal*
//! intent    := ["i"] ("need" | "want" | "have") "to" | "remember" "to"
//! ```
//!
//! The topic vocabulary is closed and holds no scope name, so a topic cannot be confused with
//! "personal" or "work". The stored topic is the canonical word ("counselling" is stored as
//! "counseling"); the evidence span selects the whole clause. Any other topic word ("home",
//! "5pm", "please", "the car"), a bare "my next session", or any other word in the sentence is
//! not this grammar and is left for the approved interpreter. A prefix before the clause follows
//! the same fillers-and-self-labels rule as reminders. Negation, quotes, brackets, questions,
//! trailing off, hypotheticals, reported speech and completed work abstain.
//!
//! [`recognize_with_session_topic`] is exactly [`recognize_reminder`] when there is no clause.
//! With one, the clause composes with the unchanged reminder logic in two placements, and both
//! yield one proposal whose reminder and session-topic facets carry spans into the original text:
//! - inside the command, as its whole content: "Remind me tomorrow to bring this up in
//!   therapy". The reminder rides on an action whose span is the clause, which is also the
//!   topic evidence.
//! - in a sentence of its own next to a reminder sentence, before or after it: "Bring this up in
//!   therapy. Remind me tomorrow to call mom." The topic sentence is blanked (offsets are
//!   preserved) and the rest goes to the reminder logic.
//!
//! "next" inside a reminder command is competing time evidence, so "Remind me <time> to discuss
//! this in my next therapy session" abstains as ambiguous; the same clause as its own sentence is
//! fine.
//!
//! A topic sentence alone yields only the topic facet. If the topic sentence or the reminder
//! abstains, the result is that abstention and neither facet is derived, since a proposal cannot
//! carry facets and an abstention together. If the rest of the text is not a supported
//! reminder (including a reminder without a time), nothing is derived and the whole text stays
//! with the interpreter. A clause that is part of a longer command ("... to call mom and bring
//! this up in therapy") is not the whole content and derives no topic.

use crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use crate::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SessionTopicProposal, SourceSpan, TextBasis,
    TimeResolutionQuality,
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
        '.' => {
            let adjacent_full_stop =
                (index > 0 && chars[index - 1] == '.') || chars.get(index + 1) == Some(&'.');
            if adjacent_full_stop {
                Some(TokenKind::ClauseBreak)
            } else {
                (!inside_word).then_some(TokenKind::SentenceBreak)
            }
        }
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
/// semicolon, dash or ellipsis ("..." or U+2026, tokenized as clause breaks) is kept, so the
/// content checks still see it.
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
    // A bare time ("remind me tomorrow") has no target to remind about, so it is not this
    // grammar; the content after "to" is mandatory.
    if !body
        .get(next)
        .is_some_and(|token| token.is_word_equal_to("to"))
    {
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

/// The closed set of neutral reminder pictographs that may stand before the command: bell,
/// alarm clock, pushpin, memo and spiral calendar. Every other emoji is excluded because it can
/// carry meaning about the command: a cross, prohibition sign or no-gesture negates it, a check
/// mark marks it done, a thinking face or thought balloon makes it a musing, a speaking head or
/// speech balloon reports it, and a laughing face makes it a joke. Markup and other symbols
/// (">", "`", "*", "#", "|") may quote or delimit the command, so they are not allowed either.
const REMINDER_PICTOGRAPHS: &[char] = &[
    '\u{1F514}', // bell
    '\u{23F0}',  // alarm clock
    '\u{1F4CC}', // pushpin
    '\u{1F4DD}', // memo
    '\u{1F5D3}', // spiral calendar
];

/// The emoji presentation selector and the zero width joiner only change how an allowed
/// pictograph is drawn; they never add a pictograph of their own.
fn is_pictograph_modifier(ch: char) -> bool {
    matches!(ch, '\u{FE0F}' | '\u{200D}')
}

/// A symbol token is allowed only when it is built from the allowlisted pictographs and their
/// presentation modifiers, and holds at least one pictograph ("\u{1F514}", "\u{1F5D3}\u{FE0F}",
/// "\u{1F514}\u{1F514}"). A token that mixes in any other symbol ("\u{1F514}\u{274C}") is not.
fn is_reminder_pictograph_token(token: &Token) -> bool {
    token
        .lower
        .chars()
        .all(|ch| REMINDER_PICTOGRAPHS.contains(&ch) || is_pictograph_modifier(ch))
        && token
            .lower
            .chars()
            .any(|ch| REMINDER_PICTOGRAPHS.contains(&ch))
}

/// Each token before the command must be a word (checked as a filler or self label below), an
/// allowlisted reminder pictograph, a sentence break or one of the comma, semicolon, colon or
/// dash separators. Quotes, brackets, ellipses, question marks, markup and every other emoji or
/// symbol are not allowed.
fn is_allowed_prefix_token(token: &Token) -> bool {
    match token.kind {
        TokenKind::Word => token.has_letters_or_digits() || is_reminder_pictograph_token(token),
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
/// lexicon, and the content ends in a noun, pronoun or particle. Empty content is never a
/// target.
fn content_is_in_lexicon(content: &[Token]) -> bool {
    let Some((verb, objects)) = content.split_first() else {
        return false;
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
    recognize_reminder_accepting(
        text,
        item_id,
        capture_id,
        source_revision,
        text_basis,
        request_version,
        time_context,
        &content_is_in_lexicon,
    )
}

/// The reminder recognizer with the final content-lexicon decision injected. Every guard that
/// precedes that decision (prefix, quotes, negation, conditions, time, recurrence) is identical
/// for all callers, so a caller can only widen which already-guarded content is accepted.
#[allow(clippy::too_many_arguments)]
fn recognize_reminder_accepting(
    text: &str,
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
    time_context: &TimeContext,
    accepts_content: &dyn Fn(&[Token]) -> bool,
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
    if !accepts_content(content) {
        return None;
    }

    // The content check above guarantees a non-empty content; a reminder is only ever proposed
    // on an action with sourced target evidence.
    let (first, last) = (content.first()?, content.last()?);
    Some(
        provenance
            .proposal()
            .with_reminder_proposal(Some(reminder_from(&time, time_context)))
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![SourceSpan::new(first.start, last.end)])),
    )
}

const SESSION_LEADS: &[&[&str]] = &[
    &["bring", "this", "up"],
    &["bring", "it", "up"],
    &["raise", "this"],
    &["raise", "it"],
    &["mention", "this"],
    &["mention", "it"],
    &["discuss", "this"],
    &["discuss", "it"],
    &["talk", "about", "this"],
    &["talk", "about", "it"],
];

const SESSION_PREPOSITIONS: &[&str] = &["in", "at", "during"];

const SESSION_DETERMINERS: &[&str] = &["my", "our", "the"];

/// Spoken form, then the topic stored for it. The topic vocabulary is closed and holds no scope
/// name ("personal", "work"), so a topic can never be mistaken for an item scope.
const SESSION_TOPICS: &[(&str, &str)] = &[
    ("therapy", "therapy"),
    ("counseling", "counseling"),
    ("counselling", "counseling"),
    ("coaching", "coaching"),
    ("supervision", "supervision"),
];

/// Stated intent that may stand directly before the lead ("I need to bring this up in therapy").
/// Longest first, so the longer form wins.
const SESSION_INTENT_LEADS: &[&[&str]] = &[
    &["i", "need", "to"],
    &["i", "want", "to"],
    &["i", "have", "to"],
    &["need", "to"],
    &["want", "to"],
    &["have", "to"],
    &["remember", "to"],
];

/// A matched session-topic clause: token range and the evidence span it covers.
struct TopicClause {
    first_token: usize,
    end_token: usize,
    topic: &'static str,
    span: SourceSpan,
}

fn words_equal(tokens: &[Token], expected: &[&str]) -> bool {
    tokens.len() == expected.len()
        && tokens
            .iter()
            .zip(expected)
            .all(|(token, word)| token.is_word_equal_to(word))
}

fn word_in(token: Option<&Token>, vocabulary: &[&str]) -> bool {
    token.is_some_and(|token| in_lexicon(token, vocabulary))
}

/// Length in tokens of the clause that starts at `start`, with its topic, when the tokens match
/// `lead preposition [determiner] [next] topic [session]`.
fn session_clause_at(tokens: &[Token], start: usize) -> Option<(usize, &'static str)> {
    let lead = SESSION_LEADS.iter().find(|lead| {
        tokens
            .get(start..start + lead.len())
            .is_some_and(|window| words_equal(window, lead))
    })?;
    let mut next = start + lead.len();
    if !word_in(tokens.get(next), SESSION_PREPOSITIONS) {
        return None;
    }
    next += 1;
    if word_in(tokens.get(next), SESSION_DETERMINERS) {
        next += 1;
    }
    if tokens
        .get(next)
        .is_some_and(|token| token.is_word_equal_to("next"))
    {
        next += 1;
    }
    let spoken = tokens.get(next).filter(|token| token.is_word())?;
    let (_, topic) = SESSION_TOPICS
        .iter()
        .find(|(word, _)| *word == spoken.lower)?;
    next += 1;
    if tokens
        .get(next)
        .is_some_and(|token| token.is_word_equal_to("session"))
    {
        next += 1;
    }
    Some((next - start, topic))
}

/// The earliest session-topic clause in the text, by position.
fn find_topic_clause(tokens: &[Token]) -> Option<TopicClause> {
    (0..tokens.len()).find_map(|start| {
        let (length, topic) = session_clause_at(tokens, start)?;
        let end_token = start + length;
        Some(TopicClause {
            first_token: start,
            end_token,
            topic,
            span: SourceSpan::new(tokens[start].start, tokens[end_token - 1].end),
        })
    })
}

/// True when the clause sits inside a reminder command ("remind me <time> to <clause>") rather
/// than in a sentence of its own.
fn clause_is_inside_command(tokens: &[Token], clause: &TopicClause) -> bool {
    let before = &tokens[..clause.first_token];
    current_sentence(before)
        .windows(2)
        .any(|pair| pair[0].is_word_equal_to("remind") && pair[1].is_word_equal_to("me"))
}

fn is_unsafe_context_token(token: &Token) -> bool {
    token.kind == TokenKind::Quote
        || token.ch == '?'
        || (token.kind == TokenKind::ClauseBreak
            && matches!(token.ch, '(' | ')' | '[' | ']' | '\u{2026}' | '.'))
        || is_negation_word(token)
        || (token.is_word()
            && (HYPOTHETICAL_WORDS.contains(&token.lower.as_str())
                || REPORTING_WORDS.contains(&token.lower.as_str())
                || COMPLETED_WORDS.contains(&token.lower.as_str())))
}

enum TopicSentence {
    /// An explicit command in a sentence of its own; the range is the sentence with its
    /// terminal punctuation, in tokens.
    Accepted {
        first_token: usize,
        end_token: usize,
    },
    Abstain(AbstentionReason),
    NotGrammar,
}

/// Judge the sentence holding `clause`. The sentence may hold only allowed fillers, one stated
/// intent and terminal "please", "thanks", commas, full stops and exclamation marks around the
/// clause. Negation, quotes, brackets, questions, trailing off, hypotheticals, reported speech
/// and completed work abstain; anything else is not this grammar.
fn judge_topic_sentence(tokens: &[Token], clause: &TopicClause) -> TopicSentence {
    let sentence_start = tokens[..clause.first_token]
        .iter()
        .rposition(|token| token.kind == TokenKind::SentenceBreak)
        .map_or(0, |index| index + 1);
    let terminator_start = tokens[clause.end_token..]
        .iter()
        .position(|token| token.kind == TokenKind::SentenceBreak)
        .map_or(tokens.len(), |offset| clause.end_token + offset);
    let terminators = tokens[terminator_start..]
        .iter()
        .take_while(|token| token.kind == TokenKind::SentenceBreak)
        .count();
    let end_token = terminator_start + terminators;

    let mut lead_start = clause.first_token;
    for intent in SESSION_INTENT_LEADS {
        let candidate = clause.first_token.saturating_sub(intent.len());
        if candidate >= sentence_start
            && words_equal(&tokens[candidate..clause.first_token], intent)
        {
            lead_start = candidate;
            break;
        }
    }
    let prefix = &tokens[sentence_start..lead_start];
    let suffix = &tokens[clause.end_token..terminator_start];
    let terminal = &tokens[terminator_start..end_token];

    let preceding_quotes = tokens[..sentence_start]
        .iter()
        .filter(|token| token.kind == TokenKind::Quote)
        .count();
    if preceding_quotes % 2 == 1 {
        return TopicSentence::Abstain(AbstentionReason::UncertainTarget);
    }
    if prefix.iter().chain(suffix).any(is_negation_word) {
        return TopicSentence::Abstain(AbstentionReason::Negated);
    }
    if prefix_abstention(prefix).is_some()
        || prefix
            .iter()
            .chain(suffix)
            .chain(terminal)
            .any(is_unsafe_context_token)
    {
        return TopicSentence::Abstain(AbstentionReason::UncertainTarget);
    }
    let suffix_is_allowed = suffix.iter().all(|token| {
        token.is_word_equal_to("please") || token.is_word_equal_to("thanks") || token.ch == ','
    });
    if !prefix_is_allowed(prefix) || !suffix_is_allowed {
        return TopicSentence::NotGrammar;
    }
    TopicSentence::Accepted {
        first_token: sentence_start,
        end_token,
    }
}

fn topic_facet(clause: &TopicClause) -> SessionTopicProposal {
    SessionTopicProposal {
        topic: clause.topic.to_string(),
        source_span: Some(clause.span),
    }
}

/// The text with the characters in `[start, end)` replaced by spaces, so every offset in the
/// result is an offset into the original text.
fn blank_out(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .enumerate()
        .map(|(index, ch)| {
            if (start..end).contains(&index) {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

/// Recognize an explicit offline session-topic phrase and nothing else. The text must be the
/// topic sentence alone; it yields a proposal holding only the derived session-topic facet. See
/// the module documentation for the grammar. Use [`recognize_with_session_topic`] when a
/// reminder may be present.
pub fn recognize_session_topic(
    text: &str,
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
) -> Option<Proposal> {
    let tokens = tokenize(text);
    let clause = find_topic_clause(&tokens)?;
    if clause_is_inside_command(&tokens, &clause) {
        return None;
    }
    let provenance = Provenance {
        item_id,
        capture_id,
        source_revision,
        text_basis,
        request_version,
    };
    match judge_topic_sentence(&tokens, &clause) {
        TopicSentence::Abstain(reason) => Some(provenance.abstention(reason)),
        TopicSentence::NotGrammar => None,
        TopicSentence::Accepted {
            first_token,
            end_token,
        } if first_token == 0 && end_token == tokens.len() => Some(
            provenance
                .proposal()
                .with_session_topic_proposal(Some(topic_facet(&clause))),
        ),
        TopicSentence::Accepted { .. } => None,
    }
}

/// Recognize a reminder command, a session-topic phrase, or both in one text. Without a topic
/// phrase this is exactly [`recognize_reminder`]. With one, the topic is composed with the
/// unchanged reminder logic into a single sourced proposal; see the module documentation for the
/// supported placements and for when the composition abstains or recognizes nothing.
pub fn recognize_with_session_topic(
    text: &str,
    item_id: &str,
    capture_id: &str,
    source_revision: i32,
    text_basis: TextBasis,
    request_version: &str,
    time_context: &TimeContext,
) -> Option<Proposal> {
    let tokens = tokenize(text);
    let Some(clause) = find_topic_clause(&tokens) else {
        return recognize_reminder(
            text,
            item_id,
            capture_id,
            source_revision,
            text_basis,
            request_version,
            time_context,
        );
    };

    if clause_is_inside_command(&tokens, &clause) {
        let is_clause_content = |content: &[Token]| {
            content.first().map(|token| token.start) == Some(clause.span.start)
                && content.last().map(|token| token.end) == Some(clause.span.end)
        };
        let proposal = recognize_reminder_accepting(
            text,
            item_id,
            capture_id,
            source_revision,
            text_basis,
            request_version,
            time_context,
            &|content| is_clause_content(content) || content_is_in_lexicon(content),
        )?;
        let rides_on_clause = proposal.reminder_proposal.is_some()
            && proposal.source_spans.as_deref() == Some(&[clause.span][..]);
        return Some(if rides_on_clause {
            proposal.with_session_topic_proposal(Some(topic_facet(&clause)))
        } else {
            proposal
        });
    }

    let provenance = Provenance {
        item_id,
        capture_id,
        source_revision,
        text_basis: text_basis.clone(),
        request_version,
    };
    match judge_topic_sentence(&tokens, &clause) {
        TopicSentence::Abstain(reason) => Some(provenance.abstention(reason)),
        TopicSentence::NotGrammar => recognize_reminder(
            text,
            item_id,
            capture_id,
            source_revision,
            text_basis,
            request_version,
            time_context,
        ),
        TopicSentence::Accepted {
            first_token,
            end_token,
        } => {
            let sentence_start = tokens[first_token].start;
            let sentence_end = tokens[end_token - 1].end;
            let remainder = blank_out(text, sentence_start, sentence_end);
            if tokenize(&remainder).is_empty() {
                return Some(
                    provenance
                        .proposal()
                        .with_session_topic_proposal(Some(topic_facet(&clause))),
                );
            }
            let proposal = recognize_reminder(
                &remainder,
                item_id,
                capture_id,
                source_revision,
                text_basis,
                request_version,
                time_context,
            )?;
            Some(if proposal.reminder_proposal.is_some() {
                proposal.with_session_topic_proposal(Some(topic_facet(&clause)))
            } else {
                proposal
            })
        }
    }
}
