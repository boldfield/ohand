// Whether a quoted span covers the whole time expression around it.

const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const MERIDIEMS: &[&str] = &["a.m.", "a.m", "am", "p.m.", "p.m", "pm"];
/// Zone abbreviations that qualify a clock time ("9 a.m. EST").
const ZONE_ABBREVIATIONS: &[&str] = &[
    "utc", "gmt", "est", "edt", "cst", "cdt", "mst", "mdt", "pst", "pdt", "akst", "akdt", "hst",
    "cet", "cest", "bst", "ist", "jst", "kst", "aest", "aedt", "z",
];

/// Whether `span` (character offsets into `basis_text`) covers the whole time expression around
/// it. It is `false` when the text just before or after the span continues the expression: a day
/// word, weekday, date, clock, meridiem, `noon`/`midnight`, an `on`/`at` leading into one of
/// those, or a zone or offset qualifier ("EST", "UTC+9", "Tokyo time"). Text that merely follows
/// ("tomorrow at 5 p.m. to call Sam") does not count, and neither does an `on` or `at` that
/// directly precedes the span. A span that starts or ends inside a word is also not whole.
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

    let before_tokens = tokens(&before);
    let mut preceding = before_tokens.iter().rev();
    let mut previous = preceding.next();
    if matches!(previous.map(String::as_str), Some("at" | "on")) {
        previous = preceding.next();
    }
    if previous
        .is_some_and(|token| is_time_word(token) || matches!(token.as_str(), "after" | "from"))
    {
        return false;
    }

    let after_tokens = tokens(&after);
    let mut following = after_tokens.iter();
    let next = following.next();
    if matches!(next.map(String::as_str), Some("at" | "on")) {
        // A bare trailing "at"/"on" leads into more time text only when that text follows.
        match following.next() {
            Some(token) if is_time_word(token) => return false,
            _ => return true,
        }
    }
    let Some(next) = next else {
        return true;
    };
    if is_time_word(next) || is_zone_qualifier(next) {
        return false;
    }
    !names_zone_after_clock(after.as_str())
}

/// Words that start the task rather than continue the time ("... 5 p.m. to call Sam").
fn is_task_word(word: &str) -> bool {
    matches!(
        word,
        "to" | "for" | "so" | "and" | "then" | "about" | "that" | "because" | "please" | "but"
    )
}

/// Region words and place introducers that qualify a clock when they follow it directly.
fn is_zone_lead(word: &str) -> bool {
    matches!(
        word,
        "in" | "pt"
            | "et"
            | "ct"
            | "mt"
            | "pacific"
            | "eastern"
            | "central"
            | "mountain"
            | "atlantic"
    )
}

/// Whether the text right after a clock continues it with a zone qualifier: a bracketed or
/// abbreviated zone, a region word, "in <place>", or any run of up to four words that reaches
/// "time" ("New York time", "Central European Time", "Pacific Standard Time") before a task word
/// or a clause break.
fn names_zone_after_clock(after: &str) -> bool {
    let mut run: Vec<String> = Vec::new();
    for raw in after.split_whitespace().take(4) {
        let word = raw
            .to_lowercase()
            .trim_matches(|c: char| {
                matches!(c, ',' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']')
            })
            .trim_end_matches('.')
            .to_string();
        if is_task_word(&word) {
            break;
        }
        if run.is_empty() && (raw.starts_with(['(', '[']) || is_zone_lead(&word)) {
            return true;
        }
        let previous_is_filler = run.last().is_some_and(|previous| is_filler(previous));
        if word == "time" && !run.is_empty() && !previous_is_filler {
            return true;
        }
        run.push(word);
        if raw.ends_with([',', ';', '!', '?', ':']) {
            break;
        }
    }
    false
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            let lowered = token.to_lowercase();
            let trimmed = lowered.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '!' | '?'));
            if MERIDIEMS.contains(&trimmed) {
                trimmed.to_string()
            } else {
                trimmed.trim_end_matches('.').to_string()
            }
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Words that are not a place when they stand before "time" ("the time", "next time").
fn is_filler(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "a"
            | "an"
            | "next"
            | "this"
            | "that"
            | "any"
            | "some"
            | "every"
            | "each"
            | "my"
            | "your"
            | "our"
            | "same"
            | "first"
            | "last"
            | "one"
            | "no"
    )
}

fn is_time_word(token: &str) -> bool {
    matches!(token, "today" | "tomorrow" | "noon" | "midnight" | "next")
        || WEEKDAYS.contains(&token)
        || MERIDIEMS.contains(&token)
        || is_clock_like(token)
        || chrono::NaiveDate::parse_from_str(token, "%Y-%m-%d").is_ok()
}

/// `3`, `3:30`, `15:30`, `3pm`, `3:30p.m` and similar.
fn is_clock_like(token: &str) -> bool {
    let numeric_end = token
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(token.len());
    let (numeric, suffix) = token.split_at(numeric_end);
    !numeric.is_empty()
        && numeric.chars().next().is_some_and(|c| c.is_ascii_digit())
        && (suffix.is_empty() || MERIDIEMS.contains(&suffix))
}

fn is_zone_qualifier(token: &str) -> bool {
    ZONE_ABBREVIATIONS.contains(&token)
        || token.starts_with("utc")
        || token.starts_with("gmt")
        || token
            .strip_prefix(['+', '-'])
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
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
                "Remind me tomorrow at 5 p.m. call me next time",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me tomorrow at 5 p.m. for the call",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me to call Sam today at noon, thanks",
                "today at noon",
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
        ] {
            assert!(!covers(text, quoted), "{text:?} / {quoted:?}");
        }
    }
}
