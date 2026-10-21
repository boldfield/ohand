// Whether a quoted span covers the whole time expression around it.
//
// The check is closed by allowlists, like the offline grammar in
// `crate::interpretation::fast_path`. A time expression has no closed end: a place ("9 a.m.
// Tokyo"), a zone ("EST", "my time"), a second clock ("or 4 p.m.") or a self-correction ("sorry,
// the day after") may all continue it, and no list of such words converges. So the text right
// after the span must take one of a few listed shapes, and anything else means the span may be
// narrower than its expression. The timing is then ambiguous, and the guard in
// `crate::interpretation::apply` never schedules it.
//
// After the span, whichever order the time and the task come in, the sentence may only:
// - end: the end of the text, a line break, `.`, `!` or `?`; or
// - carry an optional single comma, then at least one of `to`, `please` or `thanks`, then the
//   task content: plain words up to the sentence end that hold no clause mark (comma,
//   semicolon, colon, dash, bracket or ellipsis) and do not open with a time word ("to 4
//   p.m."). `to` introduces the task ("Friday at 3 p.m. to call Sam", "Friday at 3 p.m., to
//   call Sam"); the fillers are the ones the fast path accepts after a command ("Friday at 3
//   p.m., thanks").
//
// A comma clause ("..., I will be in Tokyo, ..."), a parenthetical, a dash aside, a zone or
// place ("Tokyo time", "my time", "EST"), a connector ("or Saturday", "and then ...") or any
// other word next to the span fails closed, whether or not it names a time. A list after the
// time ("tomorrow at 9 a.m., buy eggs, milk and bread") needs correction; the same list with
// the time last ("buy eggs, milk and bread tomorrow at 9 a.m.") still schedules. An aside with
// no punctuation inside the task content ("to call Sam Tokyo time") is out of this guard's
// reach: the model's own reading is the defence there. Sentences are the unit: a later sentence
// is outside the expression of a span that names its day.
//
// Before the span the task content is open-ended, so it is closed by what may not sit there. A
// span that names its day ("Friday at 3 p.m.", "tomorrow at 9 a.m.", "2026-10-09 09:00") is
// covered when nothing temporal comes earlier in its sentence (a modifier such as "next", "the
// day after", "a week from", or an alternative such as "Friday or ... Saturday"), when no
// relative modifier makes the day inexact ("before Friday", "prior to Friday", "sometime Friday",
// "by Friday", "no later than Friday", "after the meeting Friday at 3 p.m."), and when the word
// right before it is not a hedge ("maybe Friday at 3 p.m."); "this Friday" is the same Friday.
//
// A span that names no day ("at 5 p.m.") is anchored to today by the resolver, which is a guess
// the capture must not contradict anywhere. It is covered only when its own sentence holds
// nothing but request words before it ("Hey, can you remind me at 5 p.m."), and when no other
// sentence before it and nothing after it names any time at all ("the twelfth", "tmrw",
// "Christmas Eve", "on October 15", "in two days", "Tomorrow... remind me", "... to call the
// roofer tomorrow"). It is not enough that the day word is far away or in another sentence.
//
// Sentence ends: a line break, or a word ending in `!`, `?` or a single `.`. The dot of "a.m."
// or "p.m." and of a day, month or unit abbreviation ("Tues.", "Sept.", "hrs.") is not a sentence
// end, and an ellipsis ("..." or "…") is a clause mark, never a sentence end.

/// Characters that may close a word after its sentence-ending punctuation, or close a quote or
/// bracket wrapped around the span.
const CLOSERS: &str = "\"')]\u{201d}\u{2019}";

/// Meridiems whose own dot ends a word without ending the sentence.
const DOTTED_MERIDIEMS: &[&str] = &["a.m", "p.m"];

/// Meridiems that count as temporal words. "am" alone is not one: it is far more often the verb.
const MERIDIEM_WORDS: &[&str] = &["a.m", "p.m", "pm"];

/// The closed set of words that may stand between the time expression and the task content, in
/// either order: the task introducer and the fillers the fast path accepts after a command.
const TASK_SEPARATORS: &[&str] = &["to", "please", "thanks"];

const DAY_NAMES: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "today",
    "tomorrow",
    "tonight",
    "yesterday",
];

/// Spoken and typed abbreviations of a day; written with a trailing dot they do not end a
/// sentence.
const DAY_ABBREVIATIONS: &[&str] = &[
    "mon", "tue", "tues", "wed", "weds", "thu", "thur", "thurs", "fri", "sat", "sun", "tmrw",
    "tmr", "tmw", "tmro", "tomo", "tdy", "tonite", "wknd",
];

const MONTH_NAMES: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

const MONTH_ABBREVIATIONS: &[&str] = &[
    "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

const TIMES_OF_DAY: &[&str] = &[
    "noon",
    "midnight",
    "midday",
    "morning",
    "afternoon",
    "evening",
    "night",
    "dawn",
    "dusk",
    "sunrise",
    "sunset",
    "lunchtime",
    "dinnertime",
    "bedtime",
    "daytime",
    "nighttime",
    "overnight",
    "o'clock",
    "oclock",
];

/// Units of time, including the abbreviations that do not end a sentence when dotted.
const UNITS: &[&str] = &[
    "minute",
    "min",
    "hour",
    "hr",
    "second",
    "sec",
    "day",
    "week",
    "wk",
    "weekend",
    "weekday",
    "weeknight",
    "midweek",
    "fortnight",
    "month",
    "mo",
    "year",
    "yr",
    "decade",
];

const NUMBERS: &[&str] = &[
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
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "half",
    "quarter",
    "couple",
    "few",
    "several",
];

/// Ordinals up to the thirtieth; "twenty-first" is checked part by part.
const ORDINALS: &[&str] = &[
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
    "thirtieth",
];

const HOLIDAYS: &[&str] = &[
    "christmas",
    "xmas",
    "easter",
    "halloween",
    "thanksgiving",
    "valentine",
    "hanukkah",
    "chanukah",
    "diwali",
    "ramadan",
    "eid",
    "passover",
    "juneteenth",
    "solstice",
];

/// Words that shift or repeat a day without naming one ("next", "every", "two days ago").
const RELATIVE_DAY_WORDS: &[&str] = &[
    "next",
    "last",
    "coming",
    "following",
    "every",
    "each",
    "ago",
    "asap",
    "eod",
    "eow",
    "eom",
    "cob",
    "daily",
    "weekly",
    "monthly",
    "yearly",
    "annually",
    "hourly",
    "nightly",
    "biweekly",
    "fortnightly",
];

/// Whether `span` (character offsets into `basis_text`) covers the whole time expression around
/// it. It is `false` when the span starts or ends inside a word; when anything other than the
/// sentence end, or an optional comma and `to`/`please`/`thanks` followed by punctuation-free
/// task content, follows the span ("... 9 a.m. Tokyo time", "..., I will be in Tokyo, ...",
/// "... (I'll be in Lisbon)", "... or Saturday", "... my time", "... to 4 p.m."); when the span
/// names a day and anything temporal comes earlier in its sentence, a relative modifier precedes
/// it ("before Friday at 3 p.m.", "prior to Friday at 3 p.m.") or a hedge sits right before it
/// ("maybe Friday at 3 p.m."); or when the span names no day and the capture names any time
/// anywhere else.
pub fn span_covers_time_expression(basis_text: &str, start: usize, end: usize) -> bool {
    let characters: Vec<char> = basis_text.chars().collect();
    if start > end || end > characters.len() {
        return false;
    }
    let before: String = characters[..start].iter().collect();
    let after: String = characters[end..].iter().collect();
    let spans_inside_word = before
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        && characters.get(start).is_some_and(|c| c.is_alphanumeric())
        || end > start
            && characters[end - 1].is_alphanumeric()
            && (after.chars().next().is_some_and(char::is_alphanumeric)
                || after.starts_with('.')
                    && after.chars().nth(1).is_some_and(char::is_alphanumeric));
    if spans_inside_word {
        return false;
    }
    let span_text: String = characters[start..end]
        .iter()
        .collect::<String>()
        .to_lowercase();
    let meridiem_dot_open = span_text.ends_with("a.m") || span_text.ends_with("p.m");
    if !expression_is_closed_after(&after, meridiem_dot_open) {
        return false;
    }
    let before_words = words(&before);
    if names_day(&span_text) {
        let sentence = sentence_before_span(&before_words);
        !sentence.iter().any(|word| is_temporal(&word.text)) && !made_inexact_by_modifier(sentence)
    } else {
        // The resolver anchors the clock to today: nothing else in the capture may name a time.
        let (earlier_sentences, own_sentence) = split_at_last_sentence_end(&before_words);
        own_sentence.iter().all(|word| is_request_word(&word.text))
            && !earlier_sentences.iter().any(|word| is_temporal(&word.text))
            && !words(&after).iter().any(|word| is_temporal(&word.text))
    }
}

/// Whether the text after the span is one of the allowed shapes: the sentence ends, or an
/// optional single comma and at least one task separator (`to`, `please`, `thanks`) lead into
/// task content that holds no clause mark and does not open with a time word. A dot right after
/// a span ending in "a.m"/"p.m" belongs to the meridiem; closing quotes and brackets wrapped
/// around the span are skipped.
fn expression_is_closed_after(after: &str, meridiem_dot_open: bool) -> bool {
    let mut rest = after;
    if meridiem_dot_open {
        rest = rest.strip_prefix('.').unwrap_or(rest);
    }
    rest = rest.trim_start_matches(|c: char| CLOSERS.contains(c));
    rest = rest.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
    match rest.chars().next() {
        None | Some('\n' | '!' | '?') => return true,
        Some('.') => return !rest[1..].starts_with('.'),
        Some(',') => {
            rest = &rest[1..];
            rest = rest.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
            if rest.is_empty() || rest.starts_with('\n') {
                return true;
            }
        }
        Some(_) => {}
    }
    let sentence = sentence_words(rest);
    if sentence.iter().any(|word| has_clause_mark(&word.raw)) {
        return false;
    }
    let separators = sentence
        .iter()
        .take_while(|word| TASK_SEPARATORS.contains(&word.text.as_str()))
        .count();
    if separators == 0 && !sentence.is_empty() {
        return false;
    }
    !sentence
        .get(separators)
        .is_some_and(|word| is_temporal(&word.text) || is_relative_time_word(&word.text))
}

/// The words of the first sentence of `text`, up to and including the word that ends it.
fn sentence_words(text: &str) -> Vec<Word> {
    let mut sentence = words(text);
    if let Some(end) = sentence.iter().position(|word| word.ends_sentence) {
        sentence.truncate(end + 1);
    }
    sentence
}

/// Whether a raw token carries a clause mark: a comma, semicolon, colon, bracket, en or em dash,
/// ellipsis, or a hyphen at the token's edge ("- in Tokyo", "-05:00"). A hyphen inside a word
/// ("e-mail", "2026-10-09") and a colon between digits ("9:00") are not clause marks.
fn has_clause_mark(raw: &str) -> bool {
    let characters: Vec<char> = raw.chars().collect();
    characters.iter().enumerate().any(|(index, &c)| match c {
        ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '\u{2013}' | '\u{2014}' | '\u{2026}' => {
            true
        }
        ':' => {
            !(index > 0
                && characters[index - 1].is_ascii_digit()
                && characters.get(index + 1).is_some_and(char::is_ascii_digit))
        }
        '-' => index == 0 || index + 1 == characters.len(),
        '.' => characters.get(index + 1) == Some(&'.'),
        _ => false,
    })
}

/// Whether the span names the day its clock falls on: `today`, `tomorrow`, a weekday or an ISO
/// date. Without one the resolver anchors the clock to today.
fn names_day(span_text: &str) -> bool {
    span_text
        .split_whitespace()
        .map(normalized_word)
        .any(|word| {
            matches!(
                word.as_str(),
                "today"
                    | "tonight"
                    | "tomorrow"
                    | "monday"
                    | "tuesday"
                    | "wednesday"
                    | "thursday"
                    | "friday"
                    | "saturday"
                    | "sunday"
            ) || is_iso_date(&word)
        })
}

fn is_iso_date(word: &str) -> bool {
    let parts: Vec<&str> = word.split('-').collect();
    parts.len() == 3
        && [4, 2, 2]
            .iter()
            .zip(&parts)
            .all(|(length, part)| part.len() == *length && part.chars().all(|c| c.is_ascii_digit()))
}

/// A word of the basis text: as written, normalized, and flagged when it ends its sentence.
struct Word {
    raw: String,
    text: String,
    ends_sentence: bool,
}

/// The words of `text` in order. A line break ends a sentence, as does a word ending in `!`, `?`
/// or a single `.` that is not a meridiem's or an abbreviation's dot. A lone `.` is kept as an
/// empty word so that it still marks the sentence end.
fn words(text: &str) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line_start = words.len();
        for raw in line.split_whitespace() {
            let word = Word {
                raw: raw.to_string(),
                text: normalized_word(raw),
                ends_sentence: ends_sentence(raw),
            };
            if !word.text.is_empty() || word.ends_sentence {
                words.push(word);
            }
        }
        if lines.peek().is_some() {
            if let Some(last) = words[line_start..].last_mut() {
                last.ends_sentence = true;
            }
        }
    }
    words
}

/// Whether the words before a day-naming span make its day inexact: a relative word anywhere in
/// the sentence other than a plain preposition ("before Friday at 3 p.m.", "prior to Friday at
/// 3 p.m.", "sometime Friday at 3 p.m.", "no later than Friday at 3 p.m.", "after the meeting
/// Friday at 3 p.m."); any relative word right before the span, past hedges, other than the
/// `on`/`at` the grammar accepts there ("by Friday at 3 p.m.", "from Friday at 3 p.m.", "until
/// maybe Friday at 3 p.m."); or a hedge right before the span ("maybe Friday at 3 p.m.", "not
/// Friday at 3 p.m."), except "this", which names the same day. Prepositions inside the task
/// text ("to check in on Sam Friday at 3 p.m.") do not.
fn made_inexact_by_modifier(sentence: &[Word]) -> bool {
    let adjacent = sentence
        .iter()
        .rev()
        .map(|word| word.text.as_str())
        .find(|word| !is_hedge(word));
    let last = sentence.last().map(|word| word.text.as_str());
    sentence
        .iter()
        .any(|word| is_relative_time_word(&word.text) && !is_plain_preposition(&word.text))
        || adjacent.is_some_and(|word| is_relative_time_word(word) && !matches!(word, "on" | "at"))
        || last.is_some_and(|word| is_hedge(word) && word != "this")
}

/// Prepositions that only shift a time when they sit right before it, and are otherwise ordinary
/// task text ("check in on Sam", "the quote from Acme").
fn is_plain_preposition(word: &str) -> bool {
    matches!(
        word,
        "at" | "on" | "in" | "by" | "from" | "around" | "past" | "within"
    )
}

/// Words that may sit between a modifier and the time it modifies ("no later than maybe
/// Friday", "by, say, Friday"), and that make the time inexact when they sit right before it.
fn is_hedge(word: &str) -> bool {
    matches!(
        word,
        "maybe"
            | "perhaps"
            | "possibly"
            | "probably"
            | "roughly"
            | "approximately"
            | "approx"
            | "about"
            | "like"
            | "say"
            | "than"
            | "right"
            | "just"
            | "even"
            | "ideally"
            | "preferably"
            | "no"
            | "not"
            | "or"
            | "and"
            | "the"
            | "this"
            | "that"
    )
}

/// Whether a raw whitespace-delimited token ends its sentence.
fn ends_sentence(raw: &str) -> bool {
    let trimmed = raw.trim_end_matches(|c: char| CLOSERS.contains(c));
    if trimmed.ends_with(['!', '?']) {
        return true;
    }
    if trimmed.ends_with('\u{2026}') || trimmed.ends_with("..") {
        return false;
    }
    if !trimmed.ends_with('.') {
        return false;
    }
    let word = normalized_word(raw);
    !(DOTTED_MERIDIEMS.contains(&word.as_str()) || is_dotted_abbreviation(&word))
}

/// Day, month and unit abbreviations that are written with a trailing dot ("Tues.", "Sept.",
/// "hrs.").
fn is_dotted_abbreviation(word: &str) -> bool {
    let singular = word.strip_suffix('s').unwrap_or(word);
    [word, singular].iter().any(|form| {
        DAY_ABBREVIATIONS.contains(form)
            || MONTH_ABBREVIATIONS.contains(form)
            || matches!(*form, "min" | "hr" | "sec" | "wk" | "mo" | "yr" | "approx")
    })
}

/// The words of the sentence holding the span, up to the span.
fn sentence_before_span(before: &[Word]) -> &[Word] {
    split_at_last_sentence_end(before).1
}

/// The words of earlier sentences and the words of the sentence holding the span.
fn split_at_last_sentence_end(before: &[Word]) -> (&[Word], &[Word]) {
    let start = before
        .iter()
        .rposition(|word| word.ends_sentence)
        .map_or(0, |index| index + 1);
    before.split_at(start)
}

/// Words that may open a request before a clock without carrying its day ("Hey, can you remind
/// me at 5 p.m.", "Set a reminder for noon").
fn is_request_word(word: &str) -> bool {
    matches!(
        word,
        "me" | "us"
            | "you"
            | "him"
            | "her"
            | "them"
            | "remind"
            | "reminder"
            | "ping"
            | "alert"
            | "please"
            | "pls"
            | "plz"
            | "hey"
            | "hi"
            | "oh"
            | "um"
            | "uh"
            | "ok"
            | "okay"
            | "so"
            | "and"
            | "also"
            | "just"
            | "can"
            | "could"
            | "would"
            | "will"
            | "set"
            | "add"
            | "create"
            | "make"
            | "a"
            | "i"
            | "need"
            | "want"
            | "to"
            | "be"
            | "reminded"
            | "at"
            | "on"
            | "for"
    )
}

/// Words that point at another time without naming one ("before Friday", "prior to Friday",
/// "later", "when I'm home", "at lunch"). Before a day-naming span they make its day inexact;
/// right after `to` they continue the time ("Friday at 3 p.m. to later").
fn is_relative_time_word(word: &str) -> bool {
    matches!(
        word,
        "after"
            | "afterwards"
            | "before"
            | "beforehand"
            | "prior"
            | "ahead"
            | "advance"
            | "later"
            | "earlier"
            | "sooner"
            | "when"
            | "whenever"
            | "once"
            | "sometime"
            | "soon"
            | "now"
            | "latest"
            | "earliest"
            | "during"
            | "until"
            | "till"
            | "around"
            | "within"
            | "past"
            | "between"
            | "at"
            | "on"
            | "in"
            | "by"
            | "from"
    )
}

/// Lowercased, without surrounding punctuation or a trailing dot.
fn normalized_word(raw: &str) -> String {
    raw.to_lowercase()
        .replace('\u{2019}', "'")
        .trim_matches(|c: char| {
            ",;:!?()[]{}\"'\u{201c}\u{201d}\u{2018}\u{2019}\u{2026}".contains(c)
        })
        .trim_end_matches('.')
        .to_string()
}

/// Whether a word names a time, date, duration or number: anything with a digit, an offset or an
/// IANA-style name, or any part of a hyphenated word ("twenty-first", "mid-afternoon") that,
/// past an "-ish" suffix ("fourish") and a plural "s", is a day, day abbreviation, month, time
/// of day, unit, number, ordinal, holiday, meridiem or relative day word. "am" alone is not
/// counted: it is far more often the verb.
fn is_temporal(word: &str) -> bool {
    word.chars().any(|c| c.is_ascii_digit())
        || word.contains(['/', '+'])
        || word_in(
            word,
            &[
                MERIDIEM_WORDS,
                DAY_NAMES,
                DAY_ABBREVIATIONS,
                MONTH_NAMES,
                MONTH_ABBREVIATIONS,
                TIMES_OF_DAY,
                UNITS,
                NUMBERS,
                ORDINALS,
                HOLIDAYS,
                RELATIVE_DAY_WORDS,
            ],
        )
}

/// Whether any part of a hyphenated word ("twenty-first", "mid-afternoon"), past an "-ish"
/// suffix ("fourish") and a plural "s", is in one of `lists`.
fn word_in(word: &str, lists: &[&[&str]]) -> bool {
    word.split('-').filter(|part| !part.is_empty()).any(|part| {
        let base = part
            .strip_suffix("ish")
            .filter(|stem| stem.len() >= 3)
            .unwrap_or(part);
        let singular = base.strip_suffix('s').unwrap_or(base);
        lists
            .iter()
            .any(|list| list.contains(&base) || list.contains(&singular))
    })
}

#[cfg(test)]
mod tests {
    use super::span_covers_time_expression;

    fn covers(text: &str, quoted: &str) -> bool {
        let byte_start = text.find(quoted).expect("quoted text is in the basis");
        let start = text[..byte_start].chars().count();
        span_covers_time_expression(text, start, start + quoted.chars().count())
    }

    #[test]
    fn whole_expressions_are_covered() {
        for (text, quoted) in [
            // Time first: nothing, a comma, `to` or a filler, then plain task content.
            (
                "Remind me tomorrow at 5 p.m. to call Sam",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me, Friday at 3 p.m., to call the roofer",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. to call Sam", "Friday at 3 p.m."),
            (
                "Remind me on Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. please to call Sam",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m., please", "Friday at 3 p.m."),
            ("Remind me Friday at 3 p.m. thanks!", "Friday at 3 p.m."),
            ("Remind me Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me tomorrow at 9 a.m.", "tomorrow at 9 a.m."),
            (
                "Remind me tomorrow at 9 a.m. Tokyo time",
                "tomorrow at 9 a.m. Tokyo time",
            ),
            (
                "Remind me Friday at 3 p.m.! Call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m.\nCall the roofer",
                "Friday at 3 p.m.",
            ),
            ("Remind me \"Friday at 3 p.m.\" to call", "Friday at 3 p.m."),
            (
                "Remind me (Friday at 3 p.m.) to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m to call Sam",
                "tomorrow at 9 a.m",
            ),
            (
                "Remind me tomorrow at 9 a.m. to e-mail Sam",
                "tomorrow at 9 a.m.",
            ),
            (
                "note to self: remind me tomorrow at 9 a.m. to call Sam",
                "tomorrow at 9 a.m.",
            ),
            // Task content is open-ended for a span that names its day: it may mention other
            // times, and a later sentence is outside the expression.
            (
                "Remind me Friday at 3 p.m. to call the roofer on Saturday",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to prep for the 9 a.m. meeting",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to prep for the 9:00 meeting",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam. Tomorrow is busy",
                "Friday at 3 p.m.",
            ),
            (
                "Call Sam on Friday. Remind me tomorrow at 5 p.m. to check",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me this Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            // Task first: only nothing, a comma or a filler between the content and the time,
            // and only the sentence end or a filler after it.
            (
                "Remind me to call the roofer Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer tomorrow at 9 a.m.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me to call Sam today at noon, thanks",
                "today at noon",
            ),
            ("Remind me to call Sam today at noon.", "today at noon"),
            (
                "Remind me to call the roofer, Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to buy eggs, milk and bread tomorrow at 9 a.m.",
                "tomorrow at 9 a.m.",
            ),
            // Prepositions inside the task text are not modifiers of the time.
            (
                "Remind me to check in on Sam Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to send the quote from Acme by email Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "set a reminder for Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            // `to` introduces more task content whichever side the task started on.
            (
                "remind me to call the roofer 2026-01-16 09:00:00 to stop the leak",
                "2026-01-16 09:00:00",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-09 09:00:00",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-10 09:00:00",
            ),
            // A span naming no day: only request words before it, no other time anywhere.
            ("Remind me at 5 p.m. to call Sam", "5 p.m."),
            ("Remind me at 5 p.m. to call Sam", "at 5 p.m."),
            ("At 5 p.m. to call Sam", "At 5 p.m."),
            ("Remind us at noon to stretch", "at noon"),
            ("Set a reminder at 5 p.m. to call Sam", "at 5 p.m."),
            ("Set a reminder for 5 p.m. to call Sam", "5 p.m."),
            ("Hey, can you remind me at 5 p.m. to call Sam", "at 5 p.m."),
            ("I want to be reminded at 5 p.m. to call Sam", "at 5 p.m."),
            (
                "I talked to Sam. Remind me at 5 p.m. to call him back",
                "at 5 p.m.",
            ),
        ] {
            assert!(covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }

    #[test]
    fn truncated_expressions_are_not_covered() {
        for (text, quoted) in [
            ("Remind me tomorrow at 5 p.m. to call Sam", "at 5 p.m."),
            ("Remind me tomorrow at 5 p.m. to call Sam", "5 p.m."),
            ("Remind me tomorrow at 5 p.m. to call Sam", "tomorrow"),
            ("Remind me Friday at 3 p.m.", "Friday at 3 p"),
            ("Remind me Friday at 3 p.m.", "riday at 3 p.m."),
            // Anything after the span other than the sentence end, a comma, `to` or a filler
            // may continue the time: a zone, a place, an offset, a date, a connector or a verb.
            (
                "Remind me tomorrow at 9 a.m. Tokyo time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., Tokyo time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. New York time to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Ho Chi Minh City time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Central European Time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. EST to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (EST) to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. PT to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Pacific to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. UTC+9 to call",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me tomorrow at 9 a.m -05:00", "tomorrow at 9 a.m"),
            (
                "Remind me tomorrow at 9 a.m. America/New_York",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. in Tokyo",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Tokyo to join the call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Tokyo, Japan time",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me tomorrow at 9 a.m. (Tokyo)", "tomorrow at 9 a.m."),
            (
                "Remind me tomorrow at 9 a.m. (Ho Chi Minh City)",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me at 9 a.m. Tokyo time", "at 9 a.m"),
            ("Remind me Friday at 3 p.m. my time", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. my time to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. their time to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me at 3 p.m. their time", "at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. on 2026-10-09",
                "Friday at 3 p.m.",
            ),
            ("Remind me at 5 p.m. tomorrow", "at 5 p.m."),
            ("Remind me at 3 p.m. on October 15", "at 3 p.m."),
            ("Remind me at 3 p.m., the 15th", "at 3 p.m."),
            (
                "Remind me tomorrow at 5 p.m. call me next time",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. about the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 5 p.m. for the call",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. and call Sam",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m. and then call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or I'll forget the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. then the roofer",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m... I mean it", "Friday at 3 p.m."),
            // `to` followed by a time word is a range or a shift, not the task.
            (
                "Remind me Friday at 3 p.m. to 4 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to tomorrow",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. to later", "Friday at 3 p.m."),
            // Alternatives: the span holds only one of two explicit times.
            ("Remind me Friday at 3 p.m. or Saturday", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. or Saturday at 4 p.m. to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or maybe at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or around 4 p.m. to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or roughly four to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or after the meeting",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. or later", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. or we could do Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. but Saturday works too",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. then 5 p.m.", "Friday at 3 p.m."),
            (
                "Remind me Friday or Saturday at 4 p.m.",
                "Saturday at 4 p.m.",
            ),
            (
                "Remind me Friday or maybe Saturday at 4 p.m.",
                "Saturday at 4 p.m.",
            ),
            (
                "Remind me Friday, or perhaps Saturday at 4 p.m.",
                "Saturday at 4 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., or Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            // A clause, parenthetical or dash aside after the time fails closed whether or not
            // it names a time or a place.
            (
                "Remind me tomorrow at 9 a.m., I will be in Tokyo, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., I\u{2019}ll be in Tokyo, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (I'll be in Lisbon) to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. - I'll be in Lisbon - to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. \u{2014} I'll be in Lisbon \u{2014} to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., after the meeting, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I don't want to miss it",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I don't want to miss it, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., the quote wasn't cheap",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (call Sam about the quote)",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (to call Sam). Thanks",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., sorry, Saturday at 3 p.m., to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., I mean 10 a.m., to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., oh, make it 4 p.m., to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., it's Tokyo time, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., sorry, the day after, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I mean later, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., oh, make it earlier, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., I mean their time, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m.: I'd rather do noon",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m.; Tokyo time", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. - sorry, next week",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m.\u{2026} Tokyo time",
                "Friday at 3 p.m.",
            ),
            // Task content with a clause mark: a list after the time needs correction.
            (
                "Remind me tomorrow at 9 a.m., buy eggs, milk and bread",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. to buy eggs, milk and bread",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer, thanks",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer (about the quote)",
                "Friday at 3 p.m.",
            ),
            (
                "remind me to call the hotel 2026-01-16 09:00:00, then I won't need to cancel by the weekend",
                "2026-01-16 09:00:00",
            ),
            // Task first: nothing but the sentence end or a filler may follow the time.
            (
                "Remind me to call the roofer Friday at 3 p.m. or Saturday",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m. Tokyo time",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m., I don't want to miss it",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m., Ho Chi Minh City",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. - in Tokyo", "Friday at 3 p.m."),
            // A span naming no day is anchored to today; any word before it may carry the day.
            ("Remind me on the twelfth at 3 p.m.", "at 3 p.m."),
            ("Remind me on the nineteenth at 3 p.m.", "at 3 p.m."),
            ("Remind me the twenty-first at 3 p.m.", "at 3 p.m."),
            ("Remind me Christmas Eve at 6 p.m.", "at 6 p.m."),
            ("Remind me on Halloween at 3 p.m.", "at 3 p.m."),
            ("Remind me tmrw at 5 p.m.", "at 5 p.m."),
            ("Remind me on the 15th at 3 p.m.", "at 3 p.m."),
            ("Remind me in two days at 5 p.m.", "at 5 p.m."),
            ("Remind me October 15 at 3 p.m.", "at 3 p.m."),
            ("Remind me Fri at 3 p.m.", "at 3 p.m."),
            ("Remind me on Friday at 3 p.m.", "at 3 p.m."),
            ("Remind me at 9 a.m. tomorrow", "tomorrow"),
            ("Remind me in 3 hours at 5 p.m.", "at 5 p.m."),
            ("Remind me to call Sam at noon", "at noon"),
            // ... or anywhere else in the capture: earlier sentences, pauses and abbreviations
            // included, or the task text after it.
            (
                "Friday, remind me at 3 p.m. to call the roofer",
                "at 3 p.m.",
            ),
            (
                "On Friday, remind me at 3 p.m. to call the roofer",
                "at 3 p.m.",
            ),
            (
                "Tomorrow, remind me at 5 p.m. to call the roofer",
                "at 5 p.m.",
            ),
            ("On October 15, remind me at 3 p.m.", "at 3 p.m."),
            ("In two days, remind me at 3 p.m.", "at 3 p.m."),
            ("Done.\nFriday, remind me at 3 p.m.", "at 3 p.m."),
            ("Friday\nremind me at 3 p.m.", "at 3 p.m."),
            (
                "Tomorrow... remind me at 5 p.m. to call the roofer",
                "at 5 p.m.",
            ),
            (
                "Tomorrow\u{2026} remind me at 5 p.m. to call the roofer",
                "at 5 p.m.",
            ),
            (
                "Tomorrow! Remind me at 5 p.m. to call the roofer",
                "at 5 p.m.",
            ),
            ("Tues. remind me at 3 p.m. to call the roofer", "at 3 p.m."),
            (
                "Sept. 15. Remind me at 3 p.m. to call the roofer",
                "at 3 p.m.",
            ),
            (
                "Call Sam on Friday. Remind me at 5 p.m. to check",
                "at 5 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call the roofer tomorrow",
                "at 5 p.m.",
            ),
            ("Remind me at 5 p.m. to call the roofer tmrw", "at 5 p.m."),
            (
                "Remind me at 3 p.m. to call the roofer on October 15",
                "at 3 p.m.",
            ),
            (
                "Remind me at 3 p.m. to call the roofer in two days",
                "at 3 p.m.",
            ),
            (
                "Remind me at 3 p.m. to call the roofer on the twelfth",
                "at 3 p.m.",
            ),
            (
                "Remind me at 3 p.m. to call the roofer next week",
                "at 3 p.m.",
            ),
            (
                "Remind me at 6 p.m. to call the roofer Christmas Eve",
                "at 6 p.m.",
            ),
            ("Remind me at 3 p.m. to call the roofer on Fri", "at 3 p.m."),
            (
                "Remind me at 3 p.m. to call the roofer on Fri.",
                "at 3 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call Sam. Tomorrow is busy",
                "at 5 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call Sam\nTomorrow: dentist",
                "at 5 p.m.",
            ),
            ("Remind me at 5 p.m. (to call Sam tomorrow)", "at 5 p.m."),
            ("Remind me at 5 p.m. to call Sam at 4ish", "at 5 p.m."),
            // A day-naming span: anything temporal earlier in its sentence may change the day.
            (
                "Friday, remind me tomorrow at 5 p.m. to call",
                "tomorrow at 5 p.m.",
            ),
            (
                "Next week sometime, remind me Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "In two days, remind me Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me the Monday after next, Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me the day after tomorrow at 5 p.m.",
                "tomorrow at 5 p.m.",
            ),
            ("Remind me a week from Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me next Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me every Friday at 3 p.m.", "Friday at 3 p.m."),
            // A relative modifier or a hedge before a day-naming span makes the day inexact.
            ("Remind me before Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me after Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me prior to Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me ahead of Friday at 3 p.m.", "Friday at 3 p.m."),
            (
                "Remind me in advance of Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me sometime Friday at 3 p.m. to call",
                "Friday at 3 p.m.",
            ),
            ("Remind me by Friday at 3 p.m. to call", "Friday at 3 p.m."),
            ("Remind me until Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me from Friday at 3 p.m.", "Friday at 3 p.m."),
            (
                "Remind me no later than Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me not before Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me by maybe Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            ("Remind me maybe Saturday at 4 p.m.", "Saturday at 4 p.m."),
            (
                "Remind me to call the roofer, maybe Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to email Ann or maybe Saturday at 4 p.m. to call Sam",
                "Saturday at 4 p.m.",
            ),
            (
                "Remind me to email Ann and Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call Sam after the meeting Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me when I'm home Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me later today at 5 p.m. to call Sam",
                "today at 5 p.m.",
            ),
        ] {
            assert!(!covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }
}
