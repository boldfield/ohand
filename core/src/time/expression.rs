// Whether a quoted span covers the whole time expression around it.
//
// The check fails closed. After the span, the expression ends only where the text or a sentence
// ends, or where the next word starts the task ("... 5 p.m. to call Sam"). Any other word there
// (a place, a zone, "on October 15") is taken to continue the time, so the span is truncated.
// Task text before a time is open-ended ("call Sam today at noon"), so on that side it is the
// closed class of temporal words that marks a continuation: numbers, day and month names, units,
// zones, and relative words such as "after", "from" or "next".

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
/// other than one that starts the task (`to`, `for`, `and`, ...), before the sentence ends; or
/// when the word before the span (past an `at`/`on`) is temporal. A bracketed aside after the
/// span counts as task text only when it reads as one ("(call Sam about the quote)") and names
/// nothing temporal; "(EST)" or "(Tokyo)" do not.
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
    !preceded_by_time_text(&before) && !followed_by_more_than_task_text(&after, meridiem_dot_open)
}

/// Whether the word before the span, past one `at` or `on`, continues the time expression.
fn preceded_by_time_text(before: &str) -> bool {
    let mut words = before
        .split_whitespace()
        .rev()
        .map(normalized_word)
        .filter(|word| !word.is_empty());
    let mut previous = words.next();
    if matches!(previous.as_deref(), Some("at" | "on")) {
        previous = words.next();
    }
    previous.is_some_and(|word| is_temporal(&word) || TEMPORAL_MODIFIERS.contains(&word.as_str()))
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
            | "and"
            | "then"
            | "about"
            | "that"
            | "because"
            | "please"
            | "but"
            | "thanks"
            | "thank"
            | "re"
            | "regarding"
            | "or"
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
            ("Remind me Friday at 3 p.m. or Saturday", "Friday at 3 p.m."),
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
        ] {
            assert!(!covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }
}
