// Whether a quoted span covers the whole time expression around it.
//
// A time expression has no closed end: a place ("9 a.m. Tokyo time"), a zone ("EST", "my
// time"), a second clock ("or 4 p.m."), a self-correction ("sorry, the day after") or a day in
// the next sentence ("Actually, make it Saturday") may all continue it, and no list of such
// words converges. So the check is closed by allowlists, like the offline grammar in
// `crate::interpretation::fast_path`, and it looks at the whole capture. When it fails, the span
// may be narrower than its expression: the timing is ambiguous, and the guard in
// `crate::interpretation::apply` keeps the request for correction and never schedules it.
//
// Within the sentence that holds the span, in either order of time and task:
// - Time first. After the expression come optionally a single comma, optionally `to` or a
//   filler (`please`, `thanks`), then the task content: plain words up to the sentence end
//   ("Friday at 3 p.m. to call Sam", "Friday at 3 p.m., call Sam", "tomorrow at 9 a.m. about
//   the dentist"). The content may hold no clause mark (comma, semicolon, colon, dash, bracket
//   or ellipsis). Content that sits directly against the clock may not continue the time: it
//   may not open with a time or relative word ("Friday at 3 p.m. tomorrow", "... in Tokyo"),
//   hold a time, number, zone or "time" word before the first `to` or filler ("... Tokyo time",
//   "... my time", "... EST", "... UTC+9", "... New York time to call"), or lead through a
//   connector to one ("or Saturday", "or at lunch", "but Saturday works too"). After `to` or a
//   filler, only the first content word is held to that ("to 4 p.m.", "to later" are a range or
//   a shift); the rest is open, as the model's own reading is the defence there.
// - Task first. Between the content and the expression only nothing, a single comma or a filler
//   may appear, and the content may hold no clause mark ("to call the roofer - urgent - Friday
//   at 3 p.m." is ambiguous). After the expression only the end of the text, closing punctuation
//   or a filler may follow ("... today at noon, thanks"); "... Friday at 3 p.m. to fix the leak"
//   is ambiguous.
// - Before the expression, in either order, nothing temporal may come earlier in the sentence
//   ("Friday, remind me tomorrow at 5 p.m.", "a week from Friday"), and no relative modifier
//   or hedge may make the day inexact ("before Friday at 3 p.m.", "prior to Friday", "maybe
//   Saturday at 4 p.m."); "this Friday" is the same Friday.
// - A request prefix is the closed set of words that open a request ("Hey, can you remind me",
//   "Set a reminder for"); it is not task content, so a comma inside it is allowed.
//
// A span that names no day ("at 5 p.m.") is anchored to today by the resolver, which is a guess
// the capture must not contradict anywhere. It is covered only when its own sentence holds
// nothing but request words before it, and nothing temporal after it ("... to call the roofer
// tomorrow", "... on the twelfth", "... tmrw").
//
// Every other sentence of the capture, before or after the span's, may hold no temporal token:
// a digit, a number word, or a word from the closed day, month, relative-time, clock and zone
// lexicons below ("Talked to Jim on Monday. Remind me Friday at 3 p.m. to call the roofer" needs
// correction, as does "... to call the roofer. Actually, make it Saturday at 4 p.m."). Context
// with no temporal token (a place, a person) and an aside with no punctuation inside task
// content are out of this guard's reach.
//
// Sentence ends: a line break, or a word ending in `!`, `?` or a single `.`. The only dots that
// do not end a sentence are a meridiem's own ("a.m.", "p.m.") and any dot inside the span
// itself, which is never tokenized. A dotted abbreviation outside the span ("call Jan. Actually
// …", "in from the sun. Make it …") ends its sentence like any other word, as many
// abbreviations are also names or ordinary words that would otherwise hide the sentence after
// them. An ellipsis ("..." or "…") is a clause mark, never a sentence end.

/// Characters that may close a word after its sentence-ending punctuation, or close a quote or
/// bracket wrapped around the span.
const CLOSERS: &str = "\"')]\u{201d}\u{2019}";

/// Characters that may open a quote or bracket wrapped around the span.
const OPENERS: &str = "\"'([\u{201c}\u{2018}";

/// Meridiems whose own dot ends a word without ending the sentence.
const DOTTED_MERIDIEMS: &[&str] = &["a.m", "p.m"];

/// Meridiems that count as temporal words. "am" alone is not one: it is far more often the verb.
const MERIDIEM_WORDS: &[&str] = &["a.m", "p.m", "pm"];

/// The closed set of words that may stand between the time expression and the task content:
/// the task introducer and the fillers the fast path accepts after a command.
const TASK_SEPARATORS: &[&str] = &["to", "please", "thanks"];

/// The fillers alone: all that may follow a time expression that comes after its task.
const FILLERS: &[&str] = &["please", "thanks"];

/// Connectors that may join a second time to the first ("or Saturday", "and then 5 p.m.").
const CONNECTORS: &[&str] = &["or", "and", "then", "nor", "but"];

/// Zone names and abbreviations, and the words that qualify a clock with a zone ("Tokyo time",
/// "my time", "EST", "Pacific"). Abbreviations that are also ordinary words ("cat", "eat",
/// "west", "sat") are left out; an offset ("UTC+9", "-05:00") and an IANA name
/// ("America/New_York") are caught by their digits, `+` or `/`.
const ZONE_WORDS: &[&str] = &[
    "time", "timezone", "tz", "utc", "gmt", "zulu", "est", "edt", "et", "cst", "cdt", "ct", "mst",
    "mdt", "mt", "pst", "pdt", "pt", "akst", "akdt", "akt", "hst", "hdt", "ast", "adt", "nst",
    "ndt", "bst", "ist", "cet", "cest", "wet", "eet", "eest", "msk", "jst", "kst", "hkt", "sgt",
    "aest", "aedt", "acst", "acdt", "awst", "nzst", "nzdt", "sast", "pacific", "eastern",
    "central", "mountain", "atlantic",
];

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

/// Spoken and typed abbreviations of a day.
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

/// Units of time, including their abbreviations.
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

/// Number words: the complete cardinal set (zero to nineteen, the tens, hundred, thousand,
/// million, billion and dozen) and the vague quantities that count time ("half", "a few").
const NUMBERS: &[&str] = &[
    "zero",
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
    "thousand",
    "million",
    "billion",
    "dozen",
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

/// Whether one word of a capture is a temporal token as the whole-capture rule counts them (a
/// digit, a number word, a day, month, clock or zone word, or a relative time word other than a
/// plain preposition), past surrounding punctuation and any clitic ("Saturday's", "10").
pub fn is_temporal_word(raw: &str) -> bool {
    is_temporal_token(&normalized_word(raw))
}

/// Whether `span` (character offsets into `basis_text`) covers the whole time expression around
/// it, under the rule described at the top of this module. It is `false` when the span starts or
/// ends inside a word.
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

    let before = text_before_span(&before);
    let before_words = words(before);
    let (earlier_sentences, prefix) = split_at_last_sentence_end(&before_words);
    let after = text_after_span(&after, meridiem_dot_open);
    let after_words = words(after);
    let (own_after, later_sentences) = if after.starts_with('\n') {
        (&after_words[..0], &after_words[..])
    } else {
        split_at_first_sentence_end(&after_words)
    };

    // Every other sentence of the capture: no temporal token.
    if earlier_sentences
        .iter()
        .chain(later_sentences)
        .any(|word| is_temporal_token(&word.text))
    {
        return false;
    }

    // The sentence before the span: a request prefix, or task content.
    let request_prefix = prefix.iter().all(is_request_prefix_token);
    if names_day(&span_text) {
        if prefix.iter().any(|word| is_temporal_token(&word.text))
            || made_inexact_by_modifier(prefix)
        {
            return false;
        }
        if !request_prefix && prefix.iter().any(|word| has_clause_mark(&word.raw)) {
            return false;
        }
    } else if !request_prefix {
        // The resolver anchors the clock to today: any word before it may carry the day.
        return false;
    }

    // The sentence after the span.
    if own_after.iter().any(|word| has_clause_mark(&word.raw)) {
        return false;
    }
    let content: Vec<&Word> = own_after
        .iter()
        .filter(|word| !word.text.is_empty())
        .collect();
    if !request_prefix {
        // Task first: only the sentence end or a filler may follow the time.
        return content
            .iter()
            .all(|word| FILLERS.contains(&word.text.as_str()));
    }
    let separators = content
        .iter()
        .take_while(|word| TASK_SEPARATORS.contains(&word.text.as_str()))
        .count();
    let task = &content[separators..];
    if separators == 0 {
        if continues_time(task) {
            return false;
        }
    } else if task
        .first()
        .is_some_and(|word| is_temporal_token(&word.text) || is_relative_time_word(&word.text))
    {
        return false;
    }
    names_day(&span_text) || !task.iter().any(|word| is_temporal_token(&word.text))
}

/// Whether task content that sits directly against the clock continues the time instead: the
/// run of words up to the first `to` or filler opens with a time or relative word ("tomorrow",
/// "in Tokyo"), holds a time, number or zone word ("Tokyo time", "my time", "EST", "call me
/// next time") or a clock qualifier ("sharp", "ish", "or so", "maybe"), or opens with a
/// connector that leads to a time anywhere in the content ("or Saturday", "or at lunch", "but
/// Saturday works too").
fn continues_time(task: &[&Word]) -> bool {
    let run = task
        .iter()
        .take_while(|word| !TASK_SEPARATORS.contains(&word.text.as_str()));
    let mut after_connector = false;
    for (index, word) in run.enumerate() {
        let text = word.text.as_str();
        if is_temporal_token(text)
            || is_clock_qualifier(text)
            || after_connector && text == "so"
            || is_relative_time_word(text)
                && (index == 0 || after_connector || !is_plain_preposition(text))
        {
            return true;
        }
        after_connector = CONNECTORS.contains(&text);
    }
    task.first()
        .is_some_and(|word| CONNECTORS.contains(&word.text.as_str()))
        && task.iter().any(|word| {
            is_temporal_token(&word.text)
                || is_relative_time_word(&word.text) && !is_plain_preposition(&word.text)
        })
}

/// Words that qualify the clock they follow ("9 a.m. sharp", "9 a.m. ish", "9 a.m. roughly",
/// "9 a.m. maybe"): part of the time expression, so a span that stops before them is narrower
/// than it. "about" and "like" are not here, as they open ordinary task content ("9 a.m. about
/// the dentist").
fn is_clock_qualifier(word: &str) -> bool {
    matches!(
        word,
        "ish"
            | "sharp"
            | "thereabouts"
            | "approximately"
            | "approx"
            | "roughly"
            | "exactly"
            | "precisely"
            | "maybe"
            | "perhaps"
            | "possibly"
            | "probably"
    )
}

/// The text before the span without an opening quote or bracket wrapped around the span, and
/// without the single boundary comma that may separate task content from the time.
fn text_before_span(before: &str) -> &str {
    let rest = before
        .trim_end_matches(|c: char| OPENERS.contains(c))
        .trim_end_matches(is_horizontal_space);
    rest.strip_suffix(',').unwrap_or(rest)
}

/// The text after the span without the meridiem's own dot, a closing quote or bracket wrapped
/// around the span, and the single boundary comma that may follow the time. A line break is
/// kept: it ends the sentence.
fn text_after_span(after: &str, meridiem_dot_open: bool) -> &str {
    let mut rest = after;
    if meridiem_dot_open {
        rest = rest.strip_prefix('.').unwrap_or(rest);
    }
    rest = rest
        .trim_start_matches(|c: char| CLOSERS.contains(c))
        .trim_start_matches(is_horizontal_space);
    if let Some(after_comma) = rest.strip_prefix(',') {
        rest = after_comma.trim_start_matches(is_horizontal_space);
    }
    rest
}

fn is_horizontal_space(c: char) -> bool {
    c.is_whitespace() && c != '\n'
}

/// Whether a token may be part of a request prefix: a request word, or punctuation alone, with
/// no clause mark other than a comma ("Hey, can you remind me").
fn is_request_prefix_token(word: &Word) -> bool {
    (word.text.is_empty() || is_request_word(&word.text))
        && !has_clause_mark(&word.raw.replace(',', ""))
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

/// A word of the basis text: as written, normalized, and flagged when it ends its sentence. A
/// token of punctuation alone (",", "-", ".") is kept with an empty normalized form, so that its
/// clause mark or sentence end still counts.
struct Word {
    raw: String,
    text: String,
    ends_sentence: bool,
}

/// The words of `text` in order. A line break ends a sentence, as does a word ending in `!`, `?`
/// or a single `.` that is not a meridiem's dot.
fn words(text: &str) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line_start = words.len();
        for raw in line.split_whitespace() {
            words.push(Word {
                raw: raw.to_string(),
                text: normalized_word(raw),
                ends_sentence: ends_sentence(raw),
            });
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

/// Whether a raw whitespace-delimited token ends its sentence: it ends in `!`, `?` or a single
/// `.` that is not a meridiem's own. A dotted abbreviation ("Jan.", "Tues.", "hrs.") ends the
/// sentence like any other word: outside the span, the rule makes no exception for it.
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
    !DOTTED_MERIDIEMS.contains(&normalized_word(raw).as_str())
}

/// The words of earlier sentences and the words of the sentence holding the span.
fn split_at_last_sentence_end(before: &[Word]) -> (&[Word], &[Word]) {
    let start = before
        .iter()
        .rposition(|word| word.ends_sentence)
        .map_or(0, |index| index + 1);
    before.split_at(start)
}

/// The rest of the sentence holding the span, including the word that ends it, and the words of
/// later sentences.
fn split_at_first_sentence_end(after: &[Word]) -> (&[Word], &[Word]) {
    let end = after
        .iter()
        .position(|word| word.ends_sentence)
        .map_or(after.len(), |index| index + 1);
    after.split_at(end)
}

/// Words that may open a request before a clock without carrying its day ("Hey, can you remind
/// me at 5 p.m.", "Set a reminder for noon"); "this" names the same day ("this Friday").
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
            | "this"
    )
}

/// Words that point at another time without naming one ("before Friday", "prior to Friday",
/// "later", "when I'm home", "at lunch"). Before a day-naming span they make its day inexact;
/// right after `to` they continue the time ("Friday at 3 p.m. to later").
fn is_relative_time_word(word: &str) -> bool {
    matches!(
        without_clitic(word),
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
        .replace(['\u{2019}', '\u{2032}', '`'], "'")
        .trim_matches(|c: char| {
            ",;:!?()[]{}\"'\u{201c}\u{201d}\u{2018}\u{2019}\u{2026}".contains(c)
        })
        .trim_end_matches('.')
        .to_string()
}

/// A temporal token, as the whole-capture rule counts them: anything [`is_temporal`] accepts (a
/// digit, a number word, or a word from the day, month, clock and zone lexicons) or a relative
/// time word other than a plain preposition ("later", "when", "until").
fn is_temporal_token(word: &str) -> bool {
    is_temporal(word) || is_relative_time_word(word) && !is_plain_preposition(word)
}

/// Whether a word names a time, date, duration, zone or number: anything with a digit, an offset
/// or an IANA-style name, or any part of a hyphenated word ("twenty-first", "mid-afternoon")
/// that, past an "-ish" suffix ("fourish") and a plural "s", is a day, day abbreviation, month,
/// time of day, unit, number, ordinal, holiday, meridiem, relative day or zone word. "am" alone
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
                RELATIVE_DAY_WORDS,
                ZONE_WORDS,
            ],
        )
}

/// Whether any part of a hyphenated word ("twenty-first", "mid-afternoon"), as written or past a
/// clitic ("Saturday's", "Saturday'll"), an "-ish" suffix ("fourish") and a plural "s", is in one
/// of `lists`. The word as written is tried too, so "o'clock" still matches.
fn word_in(word: &str, lists: &[&[&str]]) -> bool {
    [word, without_clitic(word)].iter().any(|candidate| {
        candidate
            .split('-')
            .filter(|part| !part.is_empty())
            .any(|part| {
                let base = part
                    .strip_suffix("ish")
                    .filter(|stem| stem.len() >= 3)
                    .unwrap_or(part);
                let singular = base.strip_suffix('s').unwrap_or(base);
                lists
                    .iter()
                    .any(|list| list.contains(&base) || list.contains(&singular))
            })
    })
}

/// A word without anything from its last apostrophe on, so a contraction or possessive clitic
/// ('s, 'll, 'd, 're, 've, 'm) never hides a time word from the lexicons ("Saturday's better",
/// "Saturday'll work", "Tomorrow'd be better"). [`normalized_word`] has already turned the
/// typographic apostrophe that dictation produces, and the prime and backtick sometimes typed for
/// one, into an ASCII one.
fn without_clitic(word: &str) -> &str {
    match word.rfind('\'') {
        Some(apostrophe) if apostrophe > 0 => &word[..apostrophe],
        _ => word,
    }
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
                "Hey, can you remind me Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "set a reminder for Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me this Friday at 3 p.m. to call Sam",
                "Friday at 3 p.m.",
            ),
            // Plain words after the time are task content, with or without `to`, as long as
            // they do not continue the time.
            (
                "Remind me tomorrow at 9 a.m. about the dentist",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m., call the roofer",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. call the roofer",
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
            // A single comma and commentary that names no time is task content.
            (
                "Remind me Friday at 3 p.m., I don't want to miss it",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m., the quote wasn't cheap",
                "Friday at 3 p.m.",
            ),
            // A bare place with no zone word next to the clock is plain content under the
            // rule: an accepted limit, since places are unbounded and no list is kept.
            (
                "Remind me tomorrow at 9 a.m. Tokyo to join the call",
                "tomorrow at 9 a.m.",
            ),
            // After `to`, task content is open-ended for a span that names its day.
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
                "Remind me to buy eggs and milk and bread tomorrow at 9 a.m.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m.! Thanks a lot",
                "Friday at 3 p.m.",
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
            // Other sentences that hold no temporal token: a closing filler, a person, a
            // place, a correction of the task rather than the time.
            (
                "Remind me Friday at 3 p.m. to call Sam. Thanks",
                "Friday at 3 p.m.",
            ),
            (
                "I talked to Sam. Remind me Friday at 3 p.m. to call him back",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. He is in Tokyo",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Jan. Thanks",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Sorry, I mean the plumber",
                "Friday at 3 p.m.",
            ),
            // A "'s" clitic on a word that names no time is still not temporal.
            (
                "Remind me Friday at 3 p.m. to call the roofer. It's urgent.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. That\u{2019}s all.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam. Sam's roof leaks.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam. I'll bring the quote, he'd like it.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam. Sam\u{2019}ll know, we've asked.",
                "Friday at 3 p.m.",
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
            ("Remind me at 5 p.m. about the dentist", "at 5 p.m."),
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
            // Content directly against the clock that continues the time: a zone, an offset,
            // a date, a day, a relative word, or any time word before the first `to`.
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
            ("Remind me Friday at 3 p.m. tomorrow", "Friday at 3 p.m."),
            ("Remind me at 5 p.m. tomorrow", "at 5 p.m."),
            ("Remind me at 3 p.m. on October 15", "at 3 p.m."),
            ("Remind me at 3 p.m., the 15th", "at 3 p.m."),
            (
                "Remind me tomorrow at 5 p.m. call me next time",
                "tomorrow at 5 p.m.",
            ),
            ("Remind me Friday at 3 p.m... I mean it", "Friday at 3 p.m."),
            // A qualifier of the clock is part of the expression.
            (
                "Remind me tomorrow at 9 a.m. or so to call Sam",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me tomorrow at 9 a.m. ish", "tomorrow at 9 a.m."),
            (
                "Remind me tomorrow at 9 a.m. sharp to call Sam",
                "tomorrow at 9 a.m.",
            ),
            ("Remind me Friday at 3 p.m. maybe", "Friday at 3 p.m."),
            (
                "Remind me Friday at 3 p.m., roughly, to call Sam",
                "Friday at 3 p.m.",
            ),
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
            (
                "Remind me Friday at 3 p.m. or at lunch to call the roofer",
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
                "Remind me Friday at 3 p.m., I don't want to miss it, to call the roofer",
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
            // Task content with a clause mark, in either order.
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
            (
                "Remind me to buy eggs, milk and bread tomorrow at 9 a.m.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me to call the roofer - urgent - Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer \u{2014} urgent \u{2014} Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer (urgent) Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer: Friday at 3 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "note to self: remind me tomorrow at 9 a.m. to call Sam",
                "tomorrow at 9 a.m.",
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
            (
                "Remind me to call the roofer Friday at 3 p.m. to fix the leak",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer Friday at 3 p.m. about the leak",
                "Friday at 3 p.m.",
            ),
            (
                "remind me to call the roofer 2026-01-16 09:00:00 to stop the leak",
                "2026-01-16 09:00:00",
            ),
            ("Remind me Friday at 3 p.m. - in Tokyo", "Friday at 3 p.m."),
            // Another sentence of the capture holds a temporal token: a self-correction, a
            // zone note, a day, a number.
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, make it Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Sorry, I mean Saturday.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, make it 4.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. to call the roofer. That's Tokyo time.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m.! Tokyo time, to call the roofer",
                "tomorrow at 9 a.m.",
            ),
            (
                "Talked to Jim on Monday. Remind me Friday at 3 p.m. to call the roofer.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me to call the roofer tomorrow at 9 a.m. No wait, 10 a.m.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam. Tomorrow is busy",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Sam\nTomorrow: dentist",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Do it later",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. He has two quotes",
                "Friday at 3 p.m.",
            ),
            (
                "Call Sam on Friday. Remind me tomorrow at 5 p.m. to check",
                "tomorrow at 5 p.m.",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-09 09:00:00",
            ),
            (
                "Remind me 2026-10-09 09:00:00 to call Sam. Remind me again 2026-10-10 09:00:00",
                "2026-10-10 09:00:00",
            ),
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
            // ... or anywhere else in the capture: earlier sentences, pauses and dotted
            // abbreviations included, or the task text after it.
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
            // A dotted abbreviation outside the span ends its sentence like any other word, so
            // the sentence after it is checked: "Jan" and "sun" are also a name and a word.
            (
                "Remind me Friday at 3 p.m. to call Jan. Actually make it Saturday at 4 p.m.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to bring the cushions in from the sun. Make it Saturday instead.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me tomorrow at 9 a.m. to call Jan. That's Tokyo time.",
                "tomorrow at 9 a.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call Jan. Make it 4.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to wait a min. Actually, make it Saturday.",
                "Friday at 3 p.m.",
            ),
            // Any number word in another sentence: the complete cardinal set.
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, make it zero.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Make it a dozen.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. A thousand thanks.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. It's a million to one.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Billions, he said.",
                "Friday at 3 p.m.",
            ),
            // A time word with a "'s" clitic is still a time word, with either apostrophe.
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday's better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday\u{2019}s better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Four's better.",
                "Friday at 3 p.m.",
            ),
            (
                "Saturday's no good. Remind me Friday at 3 p.m. to call the roofer.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call the roofer. Tomorrow's better.",
                "at 5 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Later's fine too.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. October's busy.",
                "Friday at 3 p.m.",
            ),
            // Any clitic, not only "'s", and any apostrophe dictation or typing produces.
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday'll work.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday\u{2019}ll work.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Saturday'd be better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Four'll do.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me at 5 p.m. to call the roofer. Tomorrow'd be better.",
                "at 5 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Saturday've been better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Saturday\u{2032}s better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Saturday`s better.",
                "Friday at 3 p.m.",
            ),
            (
                "Remind me Friday at 3 p.m. to call the roofer. Four o'clock's better.",
                "Friday at 3 p.m.",
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
