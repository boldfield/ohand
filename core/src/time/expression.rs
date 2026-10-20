// Whether a quoted span covers the whole time expression around it.
//
// The check fails closed, and it is structural rather than positional: it never decides on a
// single neighbouring word, because every word list has holes. One predicate, `is_temporal`,
// says whether a word names a time, and the same predicate is used on both sides of the span.
//
// A span that names its day ("Friday at 3 p.m.", "tomorrow at 9 a.m.", "2026-10-09 09:00") is
// covered when nothing temporal comes earlier in its sentence (a modifier such as "next", "the
// day after", "a week from", or an alternative such as "Friday or ... Saturday"), when no
// relative modifier makes the day inexact ("before Friday", "sometime Friday", "by Friday", "no
// later than Friday", "after the meeting Friday at 3 p.m."), and when the expression ends right
// after it. After the span the expression ends only at the end of the sentence, at a word that
// starts the task ("... 5 p.m. to call Sam"), or, after a comma, colon, dash or ellipsis, at a
// word that starts a new clause. A connector ("or", "and", "then", "but") ends it only when
// nothing temporal or relative follows in the sentence ("or I'll forget" but not "or around 4
// p.m.", "or after the meeting"). A clause, whether it follows a connector ("then I won't need to
// cancel by the weekend") or a clause mark ("..., I don't want to miss it"), is commentary only
// when it names no time itself: no day, date, clock, time of day, zone, counted duration or bare
// number ("or we could do Saturday at 4 p.m.", "..., sorry, Saturday at 3 p.m.", "..., I mean
// 10 a.m.", "..., make it four", "..., it's Tokyo time"). Any other word is taken to continue
// the time (a place, a zone, "on October 15"), so the span is truncated. Task text after a task
// word is open-ended for a day-naming span: "Friday at 3 p.m. to prep for the 9 a.m. meeting"
// schedules Friday.
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
// end, and an ellipsis ("..." or "…") is a pause inside a sentence, never its end.

/// Characters that may close a word after its sentence-ending punctuation.
const CLOSERS: &str = "\"')]\u{201d}\u{2019}";

/// Meridiems whose own dot ends a word without ending the sentence.
const DOTTED_MERIDIEMS: &[&str] = &["a.m", "p.m"];

/// Meridiems that count as temporal words. "am" alone is not one: it is far more often the verb.
const MERIDIEM_WORDS: &[&str] = &["a.m", "p.m", "pm"];

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

/// Zone abbreviations and region words that qualify a clock time ("9 a.m. EST", "Tokyo time").
const ZONE_WORDS: &[&str] = &[
    "utc", "gmt", "est", "edt", "cst", "cdt", "mst", "mdt", "pst", "pdt", "akst", "akdt", "hst",
    "cet", "cest", "bst", "ist", "jst", "kst", "aest", "aedt", "z", "pt", "et", "ct", "mt",
    "pacific", "eastern", "central", "mountain", "atlantic", "standard", "daylight", "time",
    "zone", "timezone",
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
/// it. It is `false` when the span starts or ends inside a word; when the span names a day and
/// anything temporal comes earlier in its sentence, or a relative modifier precedes it ("before
/// Friday at 3 p.m.", "no later than Friday at 3 p.m."); when any word follows the span other
/// than one that starts the task or a new clause, before the sentence ends; when a clause or
/// connector after the span leads to more time text ("or Saturday at 4 p.m.", "or around 4",
/// "or after lunch", "..., I mean 10 a.m.", "..., it's Tokyo time"); or when the span names no
/// day and the capture names any time anywhere else. A bracketed aside after the span counts as
/// task text only when it reads as one ("(call Sam about the quote)") and names nothing
/// temporal; "(EST)" or "(Tokyo)" do not.
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
    if followed_by_more_than_task_text(&after, meridiem_dot_open) {
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

/// A word of the basis text, normalized, flagged when it ends its sentence.
struct Word {
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
/// the sentence other than a plain preposition ("before Friday at 3 p.m.", "sometime Friday at
/// 3 p.m.", "no later than Friday at 3 p.m.", "after the meeting Friday at 3 p.m."), or any
/// relative word right before the span, past hedges, other than the `on`/`at` the grammar
/// accepts there ("by Friday at 3 p.m.", "from Friday at 3 p.m.", "until maybe Friday at 3
/// p.m."). Prepositions inside the task text ("to check in on Sam Friday at 3 p.m.") do not.
fn made_inexact_by_modifier(sentence: &[Word]) -> bool {
    let adjacent = sentence
        .iter()
        .rev()
        .map(|word| word.text.as_str())
        .find(|word| !is_hedge(word));
    sentence
        .iter()
        .any(|word| is_relative_time_word(&word.text) && !is_plain_preposition(&word.text))
        || adjacent.is_some_and(|word| is_relative_time_word(word) && !matches!(word, "on" | "at"))
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
/// Friday", "by, say, Friday").
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
/// me at 5 p.m.", "Set a reminder at noon").
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
    )
}

/// Words that may join one time to another ("Friday at 3 p.m. or Saturday at 4 p.m.").
fn is_connector(word: &str) -> bool {
    matches!(
        word,
        "or" | "and"
            | "then"
            | "nor"
            | "but"
            | "plus"
            | "if"
            | "unless"
            | "otherwise"
            | "else"
            | "except"
            | "alternatively"
    )
}

/// Words that point at another time without naming one ("or after the meeting", "or later",
/// "or when I'm home", "or at lunch"). After a connector they continue the time expression.
fn is_relative_time_word(word: &str) -> bool {
    matches!(
        word,
        "after"
            | "afterwards"
            | "before"
            | "beforehand"
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

/// Whether anything other than the task or the end of a sentence follows the span. A dot right
/// after a span ending in "a.m"/"p.m" belongs to the meridiem, not to a sentence end; an ellipsis
/// is a pause, treated like a comma.
fn followed_by_more_than_task_text(after: &str, mut meridiem_dot_open: bool) -> bool {
    let mut rest = after;
    let mut after_clause_mark = false;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
        let Some(first) = rest.chars().next() else {
            return false;
        };
        match first {
            '\n' | '!' | '?' => return false,
            '.' if meridiem_dot_open => rest = &rest[1..],
            '.' if rest.starts_with("..") => {
                after_clause_mark = true;
                rest = rest.trim_start_matches('.');
            }
            '.' => return false,
            '(' | '[' => {
                let close = if first == '(' { ')' } else { ']' };
                let Some(close_at) = rest.find(close) else {
                    return true;
                };
                if !is_task_aside(&rest[1..close_at]) {
                    return true;
                }
                rest = &rest[close_at + 1..];
            }
            ',' | ':' | ';' | '-' | '\u{2013}' | '\u{2014}' | '\u{2026}' => {
                after_clause_mark = true;
                rest = &rest[first.len_utf8()..];
            }
            '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}' | ')' | ']' => {
                rest = &rest[first.len_utf8()..];
            }
            _ => {
                let all = words(rest);
                let sentence_end = all
                    .iter()
                    .position(|word| word.ends_sentence)
                    .map_or(all.len(), |index| index + 1);
                let sentence = &all[..sentence_end];
                let Some(word) = sentence.first() else {
                    return true;
                };
                if is_connector(&word.text) {
                    return connector_leads_to_more_time(&sentence[1..]);
                }
                if is_task_word(&word.text) {
                    return false;
                }
                // After a clause mark, a new clause is commentary only when it names no time
                // ("..., I don't want to miss it" but not "..., sorry, Saturday at 3 p.m.").
                let next_word = sentence.get(1).map(|word| word.text.as_str());
                return !(after_clause_mark
                    && starts_clause(&word.text, next_word)
                    && !clause_names_time(sentence));
            }
        }
        meridiem_dot_open = false;
    }
}

/// Whether the words after a connector, up to the end of the sentence, name another time. A
/// clause ("then I won't need to cancel by the weekend") is commentary unless it names a day,
/// date, clock, time of day or counted duration itself; anything else fails closed on any
/// temporal or relative word ("or around 4 p.m.", "or maybe Saturday", "or after lunch").
fn connector_leads_to_more_time(rest: &[Word]) -> bool {
    let next_word = rest.get(1).map(|word| word.text.as_str());
    match rest.first() {
        Some(first) if starts_clause(&first.text, next_word) => clause_names_time(rest),
        _ => rest
            .iter()
            .any(|word| is_temporal(&word.text) || is_relative_time_word(&word.text)),
    }
}

/// Whether a clause names a day, date, clock, time of day, ordinal, holiday, relative day word,
/// zone or meridiem; a unit counted by a number or an article ("in two days", "in a week"); or
/// a bare number word that counts nothing ("I mean four", "make it four, to call"). A unit on
/// its own ("by the weekend"), a number counting a noun ("the two quotes") or "time" as an
/// occasion ("last time", "this time", "in time") is not an alternative time.
fn clause_names_time(words: &[Word]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        !part_of_occasion_idiom(words, index)
            && (names_day_or_clock(&word.text)
                || word_in(&word.text, &[ZONE_WORDS, MERIDIEM_WORDS])
                || word_in(&word.text, &[UNITS])
                    && index > 0
                    && (word_in(&words[index - 1].text, &[NUMBERS])
                        || matches!(words[index - 1].text.as_str(), "a" | "an"))
                || word_in(&word.text, &[NUMBERS])
                    && words.get(index + 1).is_none_or(|next| {
                        is_task_word(&next.text)
                            || is_connector(&next.text)
                            || is_temporal(&next.text)
                            || is_relative_time_word(&next.text)
                            || next.text == "instead"
                            || next.text == "sharp"
                    }))
    })
}

/// Whether the word at `index` is part of "time" used as an occasion rather than a zone ("last
/// time", "next time", "the first time", "in time"): neither the lead word nor "time" then names
/// a time.
fn part_of_occasion_idiom(words: &[Word], index: usize) -> bool {
    let leads = |word: &str| {
        matches!(
            word,
            "last"
                | "next"
                | "this"
                | "that"
                | "every"
                | "each"
                | "first"
                | "second"
                | "one"
                | "the"
                | "a"
                | "no"
                | "some"
                | "any"
                | "more"
                | "much"
                | "enough"
                | "long"
                | "little"
                | "of"
                | "in"
                | "on"
                | "my"
                | "your"
                | "our"
                | "their"
                | "his"
                | "her"
                | "its"
                | "same"
                | "another"
                | "whole"
                | "entire"
        )
    };
    let is_time = |word: &Word| word.text == "time";
    is_time(&words[index]) && index > 0 && leads(&words[index - 1].text)
        || leads(&words[index].text) && words.get(index + 1).is_some_and(is_time)
}

/// Words that start the task rather than continue the time ("... 5 p.m. to call Sam").
fn is_task_word(word: &str) -> bool {
    matches!(
        word,
        "to" | "for"
            | "so"
            | "about"
            | "that"
            | "because"
            | "please"
            | "thanks"
            | "thank"
            | "re"
            | "regarding"
            | "ok"
            | "okay"
    )
}

/// Whether, after a comma, colon or dash, `word` starts a new clause rather than continuing the
/// time ("..., I don't want to miss it" but not "..., Tokyo time"). A determiner starts one only
/// when the word after it is not temporal ("..., the quote wasn't cheap" but not "..., the 15th").
fn starts_clause(word: &str, next_word: Option<&str>) -> bool {
    match word {
        "the" | "my" | "our" | "your" | "his" | "her" | "their" => {
            next_word.is_some_and(|next| !is_temporal(next))
        }
        _ => matches!(
            word,
            "i" | "i'm"
                | "i'll"
                | "i've"
                | "i'd"
                | "he"
                | "he's"
                | "she"
                | "she's"
                | "it"
                | "it's"
                | "we"
                | "we're"
                | "you"
                | "you're"
                | "they"
                | "they're"
                | "there"
                | "there's"
                | "there'll"
                | "that's"
                | "don't"
                | "do"
                | "didn't"
                | "can't"
                | "won't"
                | "isn't"
                | "wasn't"
                | "never"
                | "not"
                | "also"
                | "just"
                | "oh"
                | "sorry"
        ),
    }
}

/// Whether bracketed text after a time is an aside about the task: it contains a word that
/// introduces the task and names nothing temporal ("(call Sam about the quote)").
fn is_task_aside(content: &str) -> bool {
    let words: Vec<String> = content.split_whitespace().map(normalized_word).collect();
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "to" | "for" | "about" | "re" | "regarding" | "please"
        )
    }) && !words.iter().any(|word| is_temporal(word))
}

/// Lowercased, without surrounding punctuation or a trailing dot.
fn normalized_word(raw: &str) -> String {
    raw.to_lowercase()
        .replace('\u{2019}', "'")
        .trim_matches(|c: char| ",;:!?()[]\"'\u{201c}\u{201d}\u{2018}\u{2019}\u{2026}".contains(c))
        .trim_end_matches('.')
        .to_string()
}

/// Whether a word names a time, date, duration, number or zone: anything with a digit, an offset
/// or an IANA-style name, or any part of a hyphenated word ("twenty-first", "mid-afternoon")
/// that, past an "-ish" suffix ("fourish") and a plural "s", is a day, day abbreviation, month,
/// time of day, unit, number, ordinal, holiday, meridiem, zone or relative day word. "am" alone
/// is not counted: it is far more often the verb.
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
                ZONE_WORDS,
                RELATIVE_DAY_WORDS,
            ],
        )
}

/// Whether a word names a day, date, clock, time of day, ordinal, holiday or relative day word:
/// the temporal words that can anchor a time on their own, unlike a unit, number, zone or
/// meridiem.
fn names_day_or_clock(word: &str) -> bool {
    word.chars().any(|c| c.is_ascii_digit())
        || word_in(
            word,
            &[
                DAY_NAMES,
                DAY_ABBREVIATIONS,
                MONTH_NAMES,
                MONTH_ABBREVIATIONS,
                TIMES_OF_DAY,
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
            (
                "Remind me tomorrow at 5 p.m. to call Sam",
                "tomorrow at 5 p.m.",
            ),
            ("Remind me Friday at 3 p.m. to call Sam", "Friday at 3 p.m."),
            (
                "Remind me on Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            ("Remind me at 5 p.m. to call Sam", "5 p.m."),
            ("Remind me at 5 p.m. to call Sam", "at 5 p.m."),
            (
                "Remind me tomorrow at 9 a.m. Tokyo time",
                "tomorrow at 9 a.m. Tokyo time",
            ),
            ("Remind me tomorrow at 9 a.m.", "tomorrow at 9 a.m."),
            (
                "Remind me tomorrow at 5 p.m. for the call",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me to call Sam today at noon, thanks",
                "today at noon",
            ),
            ("Remind me to call Sam today at noon.", "today at noon"),
            (
                "Remind me tomorrow at 9 a.m. (call Sam about the quote)",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (to call Sam). Thanks",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m.! Call the roofer",
                "Friday at 3 p.m.",
            ),
            ("Remind me \"Friday at 3 p.m.\" to call", "Friday at 3 p.m."),
            (
                "Remind me tomorrow at 9 a.m and call Sam",
                "tomorrow at 9 a.m",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I don\u{2019}t want to miss it",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., the quote wasn't cheap",
                "Friday at 3 p.m.",
            ),
            ("At 5 p.m. to call Sam", "At 5 p.m."),
            ("Remind us at noon to stretch", "at noon"),
            ("Set a reminder at 5 p.m. to call Sam", "at 5 p.m."),
            ("Hey, can you remind me at 5 p.m. to call Sam", "at 5 p.m."),
            ("I want to be reminded at 5 p.m. to call Sam", "at 5 p.m."),
            (
                "I talked to Sam. Remind me at 5 p.m. to call him back",
                "at 5 p.m.",
            ),
            ("Remind me maybe Saturday at 4 p.m.", "Saturday at 4 p.m."),
            (
                "Remind me to email Ann or maybe Saturday at 4 p.m. to call Sam",
                "Saturday at 4 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or I'll forget the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. and then call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. then the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to email Ann and Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            // A day-naming span's task text is open-ended: it may mention other times.
            (
                "Remind me Friday at 3 p.m. to call the roofer on Saturday",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to prep for the 9 a.m. meeting",
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
            // Prepositions and hedges inside the task text are not modifiers of the time.
            (
                "Remind me to check in on Sam Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to send the quote from Acme by email Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer, maybe Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-09 09:00:00",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-10 09:00:00",
            ),
            ("Remind me Friday at 3 p.m... I mean it", "Friday at 3 p.m."),
            // A clause after a connector is commentary unless it names a time itself.
            (
                "remind me to call the hotel 2026-01-16 09:00:00, then I won't need to cancel by the weekend",
                "2026-01-16 09:00:00",
            ),
            (
                "Remind me Friday at 3 p.m. or I'll forget the two quotes",
                "Friday at 3 p.m.",
            ),
            // A clause after a comma is commentary when it names no time.
            (
                "Remind me Friday at 3 p.m., I don't want to miss it, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., sorry, I forgot the two quotes, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., it's the roofer's number, to call him",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., he said never mind last time",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I hope I make it in time",
                "Friday at 3 p.m.",
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
            (
                "Remind me tomorrow at 9 a.m. Tokyo time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. New York time to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Los Angeles time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Central European Time",
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
                "Remind me tomorrow at 9 a.m. in Tokyo",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me the day after tomorrow at 5 p.m.",
                "tomorrow at 5 p.m.",
            ),
            ("Remind me a week from Friday at 3 p.m.", "Friday at 3 p.m."),
            (
                "Remind me tomorrow at 9 a.m. EST to call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. UTC+9 to call",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me at 5 p.m. tomorrow", "at 5 p.m."),
            ("Remind me Friday at 3 p.m.", "Friday at 3 p"),
            ("Remind me Friday at 3 p.m.", "riday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. on 2026-10-09",
                "Friday at 3 p.m.",
            ),
            // Any word other than a task word continues the time: fail closed.
            (
                "Remind me tomorrow at 9 a.m. Tokyo to join the call",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Ho Chi Minh City time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. America/New_York",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. Tokyo, Japan time",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., Tokyo time",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me tomorrow at 9 a.m -05:00", "tomorrow at 9 a.m"),
            ("Remind me at 9 a.m. Tokyo time", "at 9 a.m"),
            ("Remind me at 3 p.m. on October 15", "at 3 p.m."),
            (
                "Remind me tomorrow at 5 p.m. call me next time",
                "tomorrow at 5 p.m.",
            ),
            ("Remind me tomorrow at 9 a.m. (Tokyo)", "tomorrow at 9 a.m."),
            (
                "Remind me tomorrow at 9 a.m. (Tokyo time, for the call)",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. (Ho Chi Minh City)",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me on the 15th at 3 p.m.", "at 3 p.m."),
            ("Remind me in two days at 5 p.m.", "at 5 p.m."),
            ("Remind me October 15 at 3 p.m.", "at 3 p.m."),
            ("Remind me next Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me every Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me Fri at 3 p.m.", "at 3 p.m."),
            ("Remind me on Friday at 3 p.m.", "at 3 p.m."),
            ("Remind me at 9 a.m. tomorrow", "tomorrow"),
            ("Remind me in 3 hours at 5 p.m.", "at 5 p.m."),
            ("Remind me at 3 p.m., the 15th", "at 3 p.m."),
            (
                "Remind me Friday at 3 p.m., Ho Chi Minh City",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. - in Tokyo", "Friday at 3 p.m."),
            ("Remind me Friday at 3 p.m.; Tokyo time", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m... Tokyo time",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m.\u{2026} Tokyo time",
                "Friday at 3 p.m.",
            ),
            // A span naming no day is anchored to today; any word before it may carry the day.
            ("Remind me on the twelfth at 3 p.m.", "at 3 p.m."),
            ("Remind me on the nineteenth at 3 p.m.", "at 3 p.m."),
            ("Remind me the twenty-first at 3 p.m.", "at 3 p.m."),
            ("Remind me Christmas Eve at 6 p.m.", "at 6 p.m."),
            ("Remind me on Halloween at 3 p.m.", "at 3 p.m."),
            ("Remind me tmrw at 5 p.m.", "at 5 p.m."),
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
                "Remind me at 5 p.m. to call the roofer... tomorrow",
                "at 5 p.m.",
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
            // A relative modifier before a day-naming span makes the day inexact.
            ("Remind me before Friday at 3 p.m.", "Friday at 3 p.m."),
            ("Remind me after Friday at 3 p.m.", "Friday at 3 p.m."),
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
                "Remind me Friday at 3 p.m. or approximately fourish",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or after the meeting",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or at lunch to call",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or whenever I'm home",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. or later", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. or we could do Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or I'll do it tomorrow",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., then I'll need it in a week",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or I could do it in two days",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. but Saturday works too",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. and again Saturday",
                "Friday at 3 p.m.",
            ),
            ("Remind me Friday at 3 p.m. then 5 p.m.", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m. and remind me again Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
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
            // A clause after a comma that names a time: self-corrections and zone asides.
            (
                "Remind me Friday at 3 p.m., sorry, Saturday at 3 p.m., to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m., I mean 10 a.m., to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., we could also do Saturday at 4 p.m., to call the roofer",
                "Friday at 3 p.m.",
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
                "Remind me tomorrow at 9 a.m., it's EST, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I mean four, to call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., oh, make it four instead",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., I mean four",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m.: I'd rather do noon",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. - sorry, next week",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m... I mean in two days",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. or I mean four",
                "Friday at 3 p.m.",
            ),
        ] {
            assert!(!covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }
}
