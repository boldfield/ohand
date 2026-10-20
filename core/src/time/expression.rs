// Whether a quoted span covers the whole time expression around it.
//
// The check fails closed. After the span, the expression ends only where the text or a sentence
// ends, or where the next word starts the task ("... 5 p.m. to call Sam"). Any other word there
// (a place, a zone, "on October 15") is taken to continue the time, so the span is truncated.
// Before the span it also fails closed when the span names no day: the resolver anchors such a
// clock to today, so any word between the start of the sentence and the span other than a request
// word ("Remind me at 5 p.m.") may carry the day ("the twelfth at 3 p.m.", "tmrw at 5 p.m.",
// "Friday, remind me at 3 p.m."), and a day named later in the sentence ("... to call the roofer
// tomorrow") does too.
// When the span does name a day, task text before it is open-ended ("call Sam today at noon"), so
// there it is the closed class of temporal words that marks a continuation: numbers, day and month
// names, units, zones, and relative words such as "after", "from" or "next". A connector ("or",
// "and", "then") joins a further time when the word beyond it is temporal ("Friday at 3 p.m. or
// Saturday at 4 p.m.", "Friday or maybe Saturday at 4 p.m."), so the span then covers only one
// alternative.

const MERIDIEMS: &[&str] = &["a.m.", "a.m", "am", "p.m.", "p.m", "pm"];
/// Zone abbreviations and region words that qualify a clock time ("9 a.m. EST").
const ZONE_WORDS: &[&str] = &[
    "utc", "gmt", "est", "edt", "cst", "cdt", "mst", "mdt", "pst", "pdt", "akst", "akdt", "hst",
    "cet", "cest", "bst", "ist", "jst", "kst", "aest", "aedt", "z", "pt", "et", "ct", "mt",
    "pacific", "eastern", "central", "mountain", "atlantic", "standard", "daylight", "time",
    "zone", "timezone",
];
const DAY_WORDS: &[&str] = &[
    "today",
    "tomorrow",
    "tonight",
    "yesterday",
    "noon",
    "midnight",
    "morning",
    "afternoon",
    "evening",
    "night",
    "weekday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "tues",
    "wed",
    "thu",
    "thur",
    "thurs",
    "fri",
    "sat",
    "sun",
    "o'clock",
    "oclock",
];
const MONTH_WORDS: &[&str] = &[
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
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];
const UNIT_AND_NUMBER_WORDS: &[&str] = &[
    "minute",
    "min",
    "hour",
    "hr",
    "day",
    "week",
    "weekend",
    "fortnight",
    "month",
    "year",
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
    "fifteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "half",
    "quarter",
    "couple",
    "few",
    "several",
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
    "twentieth",
    "thirtieth",
];
/// Words that tie a following time to more time text before it ("the day after tomorrow").
const TEMPORAL_MODIFIERS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "in",
    "after",
    "before",
    "from",
    "next",
    "this",
    "last",
    "coming",
    "following",
    "every",
    "each",
    "within",
    "until",
    "till",
    "past",
    "ago",
    "end",
    "early",
    "late",
    "later",
];

/// Whether `span` (character offsets into `basis_text`) covers the whole time expression around
/// it. It is `false` when the span starts or ends inside a word; when any word follows the span
/// other than one that starts the task (`to`, `for`, `about`, ...), before the sentence ends; when
/// a connector after the span leads to more time text ("or Saturday at 4 p.m."); when the span
/// names no day and its sentence holds anything but request words before it, or names a day
/// after it; or when the span names a day and the word before it, past hedges, is temporal or a
/// connector back to a temporal word. A bracketed aside after the span counts as task text only
/// when it reads as one ("(call Sam about the quote)") and names nothing temporal; "(EST)" or
/// "(Tokyo)" do not.
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
    let span_names_day = names_day(&span_text);
    // "Remind me at 5 p.m. to call the roofer tomorrow": the day may sit in the task text.
    let day_later_in_sentence = !span_names_day && names_day(&rest_of_sentence(&after).join(" "));
    !day_later_in_sentence
        && !preceded_by_time_text(&before, span_names_day)
        && !followed_by_more_than_task_text(&after, meridiem_dot_open)
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

/// Whether the text before the span, past one `at` or `on`, continues the time expression. For a
/// span naming no day, any word between the start of its sentence and the span other than a
/// request word continues it ("Friday, remind me at 3 p.m."). For a span naming a day, a temporal
/// word before it does, as does a connector back to one ("Friday or maybe Saturday at 4 p.m.").
fn preceded_by_time_text(before: &str, span_names_day: bool) -> bool {
    if !span_names_day {
        return !sentence_so_far(before)
            .into_iter()
            .all(|word| is_request_word(&word));
    }
    let mut words = before
        .split_whitespace()
        .rev()
        .map(normalized_word)
        .filter(|word| !word.is_empty())
        .skip_while(|word| matches!(word.as_str(), "at" | "on"))
        .skip_while(|word| is_hedge(word));
    let Some(previous) = words.next() else {
        return false;
    };
    if is_connector(&previous) {
        // "Friday or Saturday at 3 p.m.": the span holds only the last alternative.
        return words
            .find(|word| !is_filler(word))
            .is_some_and(|word| is_temporal(&word));
    }
    is_temporal(&previous) || TEMPORAL_MODIFIERS.contains(&previous.as_str())
}

/// The words of the sentence holding the span, up to the span. A sentence ends at a line break or
/// at a word ending in `.`, `!` or `?`; a meridiem's dot ("p.m.") does not end one, so the walk
/// goes on and fails closed on it.
fn sentence_so_far(before: &str) -> Vec<String> {
    let line = before.rsplit('\n').next().unwrap_or_default();
    let mut words: Vec<String> = Vec::new();
    for raw in line.split_whitespace().rev() {
        let ends_sentence = raw
            .trim_end_matches(|c: char| "\"')]\u{201d}\u{2019}".contains(c))
            .ends_with(['.', '!', '?']);
        let word = normalized_word(raw);
        if ends_sentence && !MERIDIEMS.contains(&word.as_str()) {
            break;
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
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
            | "hey"
            | "hi"
            | "ok"
            | "okay"
            | "so"
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

/// Words that soften an alternative without naming a time ("Friday or maybe Saturday").
fn is_hedge(word: &str) -> bool {
    matches!(
        word,
        "maybe"
            | "perhaps"
            | "possibly"
            | "probably"
            | "instead"
            | "even"
            | "also"
            | "again"
            | "else"
    )
}

/// Words after a span naming no day, up to the end of its sentence ("... to call the roofer
/// tomorrow").
fn rest_of_sentence(after: &str) -> Vec<String> {
    let line = after.split('\n').next().unwrap_or_default();
    let mut words: Vec<String> = Vec::new();
    for raw in line.split_whitespace() {
        let word = normalized_word(raw);
        if !word.is_empty() {
            words.push(word.clone());
        }
        let ends_sentence = raw
            .trim_end_matches(|c: char| "\"')]\u{201d}\u{2019}".contains(c))
            .ends_with(['.', '!', '?']);
        if ends_sentence && !MERIDIEMS.contains(&word.as_str()) {
            break;
        }
    }
    words
}

/// Hedges and joiners that sit between alternative times without naming one ("or maybe at 4").
fn is_filler(word: &str) -> bool {
    matches!(
        word,
        "at" | "on"
            | "in"
            | "by"
            | "the"
            | "a"
            | "an"
            | "maybe"
            | "perhaps"
            | "possibly"
            | "probably"
            | "else"
            | "then"
            | "again"
            | "also"
            | "if"
            | "not"
            | "instead"
            | "even"
    )
}

/// Words that may join one time to another ("Friday at 3 p.m. or Saturday at 4 p.m.").
fn is_connector(word: &str) -> bool {
    matches!(word, "or" | "and" | "then" | "nor")
}

/// Whether the words after a connector name another time, past fillers such as "at", "maybe" or
/// "then" ("... or maybe at 4 p.m.").
fn connector_joins_time(mut words: impl Iterator<Item = String>) -> bool {
    words
        .find(|word| !is_filler(word))
        .is_some_and(|word| is_temporal(&word) || TEMPORAL_MODIFIERS.contains(&word.as_str()))
}

/// Whether anything other than the task or the end of a sentence follows the span. A dot right
/// after a span ending in "a.m"/"p.m" belongs to the meridiem, not to a sentence end.
fn followed_by_more_than_task_text(after: &str, mut meridiem_dot_open: bool) -> bool {
    let mut rest = after;
    let mut after_clause_mark = false;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
        let Some(first) = rest.chars().next() else {
            return false;
        };
        match first {
            '\n' | '!' | '?' | ';' => return false,
            '.' if !meridiem_dot_open => return false,
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
            ',' | ':' | '-' | '\u{2013}' | '\u{2014}' => {
                after_clause_mark = true;
                rest = &rest[first.len_utf8()..];
            }
            '.' | '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}' | ')' | ']' => {
                rest = &rest[first.len_utf8()..];
            }
            _ => {
                let mut words = rest
                    .split(|c: char| c.is_whitespace() || ",;:!?()[]\"".contains(c))
                    .map(normalized_word)
                    .filter(|word| !word.is_empty());
                let word = words.next().unwrap_or_default();
                if is_connector(&word) {
                    return connector_joins_time(words);
                }
                if is_task_word(&word) {
                    return false;
                }
                return !(after_clause_mark && starts_clause(&word, words.next().as_deref()));
            }
        }
        meridiem_dot_open = false;
    }
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
            | "but"
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
        .trim_matches(|c: char| ",;:!?()[]\"'\u{201c}\u{201d}\u{2018}\u{2019}".contains(c))
        .trim_end_matches('.')
        .to_string()
}

/// A word that names a time, date, duration or zone: anything with a digit, an offset or an
/// IANA-style name, a day, month, unit, number, meridiem or zone word.
fn is_temporal(word: &str) -> bool {
    let singular = word.strip_suffix('s').unwrap_or(word);
    word.chars().any(|c| c.is_ascii_digit())
        || word.contains(['/', '+'])
        || MERIDIEMS.contains(&word)
        || [word, singular].iter().any(|form| {
            ZONE_WORDS.contains(form)
                || DAY_WORDS.contains(form)
                || MONTH_WORDS.contains(form)
                || UNIT_AND_NUMBER_WORDS.contains(form)
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
                "Call Sam on Friday. Remind me at 5 p.m. to check",
                "at 5 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call Sam. Tomorrow is busy",
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
            // A span naming no day is anchored to today; any word before it may carry the day.
            ("Remind me on the twelfth at 3 p.m.", "at 3 p.m."),
            ("Remind me on the nineteenth at 3 p.m.", "at 3 p.m."),
            ("Remind me the twenty-first at 3 p.m.", "at 3 p.m."),
            ("Remind me Christmas Eve at 6 p.m.", "at 6 p.m."),
            ("Remind me on Halloween at 3 p.m.", "at 3 p.m."),
            ("Remind me tmrw at 5 p.m.", "at 5 p.m."),
            ("Remind me to call Sam at noon", "at noon"),
            // ... or earlier in its sentence, before the request, or later in the task text.
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
            (
                "Remind me at 5 p.m. to call the roofer tomorrow",
                "at 5 p.m.",
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
                "Remind me Friday at 3 p.m. and again Saturday",
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
        ] {
            assert!(!covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }
}
