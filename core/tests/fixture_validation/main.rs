//! Validates `fixtures/intent/contrastive-fixtures.json` against the landed I01 proposal contract.
//!
//! Every fixture is decoded into strict `deny_unknown_fields` types (duplicate keys fail too),
//! its expected outcome is rebuilt as a real [`Proposal`] and run through `Proposal::validate`,
//! and its forbidden rules are applied back to that expected proposal. The semantics are
//! documented in `docs/features/fixture-schema.md`.

use chrono::{DateTime, Datelike, Duration, Offset, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use ohand_core::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use ohand_core::interpretation::contracts::{
    AbstentionReason, Operation, Proposal, ReminderProposal, SessionTopicProposal, SourceSpan,
    TextBasis, TimeResolutionQuality,
};
use ohand_core::store::events::ItemType;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const PROPOSAL_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
const ITEM_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
const CAPTURE_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
const REQUEST_VERSION: &str = "550e8400-e29b-41d4-a716-446655440004";
const CORRECTION_RECORD_ID: &str = "550e8400-e29b-41d4-a716-446655440005";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    id: String,
    category: Category,
    input: String,
    provenance: String,
    text_basis: BasisKind,
    preserve: Preserve,
    capture_context: CaptureContext,
    expected: Expected,
    forbidden: Forbidden,
    recoverable: bool,
    recovery_notes: Option<String>,
    notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Category {
    Design,
    Dates,
    Mixed,
    Corrections,
    Ambiguity,
    BroadIntention,
    Negation,
    PromptInjection,
    DroppedAsrWord,
}

const REQUIRED_CATEGORIES: [Category; 9] = [
    Category::Design,
    Category::Dates,
    Category::Mixed,
    Category::Corrections,
    Category::Ambiguity,
    Category::BroadIntention,
    Category::Negation,
    Category::PromptInjection,
    Category::DroppedAsrWord,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BasisKind {
    Original,
    Corrected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preserve {
    raw_input: bool,
    user_correction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CaptureType {
    Text,
    Voice,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureContext {
    #[allow(dead_code)]
    capture_type: CaptureType,
    capture_instant: Option<String>,
    device_timezone: Option<String>,
    user_correction: Option<String>,
    correction_spans: Option<Vec<CorrectionSpan>>,
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectionSpan {
    original: String,
    corrected: String,
}

/// Expected facets. An absent facet means the reference outcome has no such facet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    item_type: Option<ItemType>,
    source_spans: Option<Vec<OracleSpan>>,
    reminder_proposal: Option<OracleReminder>,
    session_topic_proposal: Option<OracleTopic>,
    abstention: Option<AbstentionReason>,
}

/// An I01 `SourceSpan` plus the oracle-only `text` that the span must slice out of the basis.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleSpan {
    start: usize,
    end: usize,
    text: String,
}

impl OracleSpan {
    fn source_span(&self) -> SourceSpan {
        SourceSpan::new(self.start, self.end)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleReminder {
    instant: Option<String>,
    timezone_id: Option<String>,
    quality: TimeResolutionQuality,
    source_span: OracleSpan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OracleTopic {
    topic: String,
    source_span: OracleSpan,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Forbidden {
    item_types: Vec<ItemType>,
    reminder: Option<ReminderBan>,
    reminder_qualities: Vec<TimeResolutionQuality>,
    reminder_timezones: Vec<String>,
    session_topic: bool,
    facets: Option<FacetBan>,
    operations: Vec<ForbiddenOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReminderBan {
    Any,
    AnyInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FacetBan {
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ForbiddenOperation {
    Update,
    Create,
}

impl Forbidden {
    fn is_empty(&self) -> bool {
        self.item_types.is_empty()
            && self.reminder.is_none()
            && self.reminder_qualities.is_empty()
            && self.reminder_timezones.is_empty()
            && !self.session_topic
            && self.facets.is_none()
            && self.operations.is_empty()
    }
}

/// Every way `proposal` violates the forbidden rules. Empty means the proposal is acceptable.
fn violations(proposal: &Proposal, forbidden: &Forbidden) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(item_type) = proposal.item_type {
        if forbidden.item_types.contains(&item_type) {
            found.push(format!("forbidden item_type {item_type:?}"));
        }
    }
    if let Some(reminder) = &proposal.reminder_proposal {
        match forbidden.reminder {
            Some(ReminderBan::Any) => found.push("forbidden reminder".to_string()),
            Some(ReminderBan::AnyInstant) if reminder.instant.is_some() => {
                found.push("forbidden reminder instant".to_string())
            }
            _ => {}
        }
        if forbidden.reminder_qualities.contains(&reminder.quality) {
            found.push(format!("forbidden reminder quality {:?}", reminder.quality));
        }
        if let Some(timezone) = &reminder.timezone_id {
            if forbidden.reminder_timezones.contains(timezone) {
                found.push(format!("forbidden reminder timezone {timezone}"));
            }
        }
    }
    if forbidden.session_topic && proposal.session_topic_proposal.is_some() {
        found.push("forbidden session topic".to_string());
    }
    let has_facet = proposal.item_type.is_some()
        || proposal.reminder_proposal.is_some()
        || proposal.session_topic_proposal.is_some();
    if forbidden.facets == Some(FacetBan::Any) && has_facet {
        found.push("forbidden facet".to_string());
    }
    let legal_unsupported_abstention =
        proposal.abstention == Some(AbstentionReason::UnsupportedOperation) && !has_facet;
    let operation = match proposal.operation {
        Operation::Annotate {} => None,
        Operation::Create {} => Some(ForbiddenOperation::Create),
        Operation::Update { .. } => Some(ForbiddenOperation::Update),
    };
    if let Some(operation) = operation {
        if forbidden.operations.contains(&operation) && !legal_unsupported_abstention {
            found.push(format!("forbidden operation {operation:?}"));
        }
    }
    found
}

fn basis_text(fixture: &Fixture) -> Result<&str, String> {
    match fixture.text_basis {
        BasisKind::Original => Ok(&fixture.input),
        BasisKind::Corrected => fixture
            .capture_context
            .user_correction
            .as_deref()
            .ok_or_else(|| "text_basis corrected requires capture_context.user_correction".into()),
    }
}

fn check_span_text(basis: &str, span: &OracleSpan, label: &str) -> Result<(), String> {
    span.source_span()
        .is_valid(basis)
        .map_err(|error| format!("{label}: {error}"))?;
    let sliced: String = basis
        .chars()
        .skip(span.start)
        .take(span.end - span.start)
        .collect();
    if sliced != span.text {
        return Err(format!(
            "{label}: span [{}, {}) slices {sliced:?} but the fixture says {:?}",
            span.start, span.end, span.text
        ));
    }
    Ok(())
}

fn build_expected_proposal(fixture: &Fixture) -> Result<Proposal, String> {
    let expected = &fixture.expected;
    let (text_basis, revision) = match fixture.text_basis {
        BasisKind::Original => (TextBasis::Original { item_revision: 0 }, 0),
        BasisKind::Corrected => (
            TextBasis::Correction {
                correction_record_id: CORRECTION_RECORD_ID.to_string(),
                item_revision: 1,
            },
            1,
        ),
    };
    let reminder = expected
        .reminder_proposal
        .as_ref()
        .map(|reminder| ReminderProposal {
            instant: reminder.instant.clone(),
            timezone_id: reminder.timezone_id.clone(),
            quality: reminder.quality,
            source_span: Some(reminder.source_span.source_span()),
        });
    let topic = expected
        .session_topic_proposal
        .as_ref()
        .map(|topic| SessionTopicProposal {
            topic: topic.topic.clone(),
            source_span: Some(topic.source_span.source_span()),
        });
    let spans = expected
        .source_spans
        .as_ref()
        .map(|spans| spans.iter().map(OracleSpan::source_span).collect());
    Ok(Proposal::new(
        PROPOSAL_ID.to_string(),
        ITEM_ID.to_string(),
        CAPTURE_ID.to_string(),
        revision,
        SUPPORTED_PROPOSAL_SCHEMA_VERSION,
        text_basis,
        REQUEST_VERSION.to_string(),
    )
    .with_item_type(expected.item_type)
    .with_source_spans(spans)
    .with_reminder_proposal(reminder)
    .with_session_topic_proposal(topic)
    .with_abstention(expected.abstention.clone()))
}

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Mon),
    ("tuesday", Weekday::Tue),
    ("wednesday", Weekday::Wed),
    ("thursday", Weekday::Thu),
    ("friday", Weekday::Fri),
    ("saturday", Weekday::Sat),
    ("sunday", Weekday::Sun),
];

/// Recompute what the time phrase says and compare it with the expected instant, so a wrong
/// weekday, hour, UTC offset or relative day cannot be hidden behind a plausible-looking value.
fn check_reminder_time(fixture: &Fixture, reminder: &OracleReminder) -> Result<(), String> {
    let context = &fixture.capture_context;
    let capture_instant = context
        .capture_instant
        .as_deref()
        .ok_or("a fixture with a reminder needs capture_context.capture_instant")?;
    let capture = DateTime::parse_from_rfc3339(capture_instant)
        .map_err(|error| format!("capture_instant: {error}"))?
        .with_timezone(&Utc);
    let device_timezone = context
        .device_timezone
        .as_deref()
        .ok_or("a fixture with a reminder needs capture_context.device_timezone")?;
    device_timezone
        .parse::<Tz>()
        .map_err(|_| format!("device_timezone {device_timezone} is not an IANA zone"))?;

    let Some(instant) = &reminder.instant else {
        return Ok(());
    };
    let at = DateTime::parse_from_rfc3339(instant).map_err(|error| format!("instant: {error}"))?;
    let timezone: Tz = reminder
        .timezone_id
        .as_deref()
        .ok_or("a resolved reminder needs timezone_id")?
        .parse()
        .map_err(|_| "timezone_id is not an IANA zone".to_string())?;
    let local = at.with_timezone(&timezone);
    let zone_offset = local.offset().fix().local_minus_utc();
    if at.offset().local_minus_utc() != zone_offset {
        return Err(format!(
            "instant {instant} has offset {}s but {timezone} observes {zone_offset}s then",
            at.offset().local_minus_utc()
        ));
    }
    if at.with_timezone(&Utc) <= capture {
        return Err(format!(
            "instant {instant} is not after the capture instant"
        ));
    }

    let capture_local = capture.with_timezone(&timezone);
    let phrase = reminder.source_span.text.to_lowercase();
    let words: Vec<&str> = phrase
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .collect();
    let day_offset = (local.date_naive() - capture_local.date_naive()).num_days();
    if words.contains(&"tomorrow")
        && local.date_naive() != capture_local.date_naive() + Duration::days(1)
    {
        return Err(format!(
            "{instant} is not the day after the capture in {timezone}"
        ));
    }
    if words.contains(&"today") && day_offset != 0 {
        return Err(format!("{instant} is not the capture day in {timezone}"));
    }
    for (name, weekday) in WEEKDAYS {
        if words.contains(&name) && (local.weekday() != weekday || !(0..=7).contains(&day_offset)) {
            return Err(format!(
                "{instant} is {} in {timezone}, which is not the upcoming {name}",
                local.weekday()
            ));
        }
    }
    for pair in words.windows(2) {
        let meridiem = match pair[1] {
            "a.m." | "am" => 0,
            "p.m." | "pm" => 12,
            _ => continue,
        };
        if let Ok(hour) = pair[0].parse::<u32>() {
            let expected_hour = hour % 12 + meridiem;
            if local.hour() != expected_hour || local.minute() != 0 {
                return Err(format!(
                    "{instant} is {:02}:{:02} in {timezone}, the phrase says {expected_hour}:00",
                    local.hour(),
                    local.minute()
                ));
            }
        }
    }
    Ok(())
}

fn check_correction(fixture: &Fixture) -> Result<(), String> {
    let context = &fixture.capture_context;
    let has_correction = context.user_correction.is_some();
    if fixture.preserve.user_correction != has_correction {
        return Err("preserve.user_correction must equal the presence of user_correction".into());
    }
    if (fixture.text_basis == BasisKind::Corrected) != has_correction {
        return Err("text_basis must be corrected exactly when user_correction exists".into());
    }
    match (&context.user_correction, &context.correction_spans) {
        (Some(corrected_text), Some(spans)) if !spans.is_empty() => {
            let mut rebuilt = fixture.input.clone();
            for span in spans {
                if !rebuilt.contains(&span.original) {
                    return Err(format!(
                        "correction_spans: {:?} not in input",
                        span.original
                    ));
                }
                rebuilt = rebuilt.replacen(&span.original, &span.corrected, 1);
            }
            if &rebuilt != corrected_text {
                return Err("correction_spans do not turn the input into user_correction".into());
            }
        }
        (Some(_), _) => return Err("user_correction requires non-empty correction_spans".into()),
        (None, Some(_)) => return Err("correction_spans requires user_correction".into()),
        (None, None) => {}
    }
    Ok(())
}

fn check_fixture(fixture: &Fixture) -> Result<(), String> {
    let id_is_kebab = !fixture.id.is_empty()
        && fixture
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !id_is_kebab {
        return Err("id must be lowercase kebab-case".into());
    }
    if !fixture.provenance.starts_with("synthetic") {
        return Err("provenance must start with \"synthetic\"".into());
    }
    if fixture.notes.trim().is_empty() || fixture.input.trim().is_empty() {
        return Err("input and notes must not be blank".into());
    }
    if !fixture.preserve.raw_input {
        return Err("preserve.raw_input must be true".into());
    }
    check_correction(fixture)?;
    match (fixture.recoverable, &fixture.recovery_notes) {
        (false, Some(notes)) if !notes.trim().is_empty() => {}
        (false, _) => return Err("recoverable false requires recovery_notes".into()),
        (true, Some(_)) => return Err("recovery_notes requires recoverable false".into()),
        (true, None) => {}
    }
    if fixture.forbidden.is_empty() {
        return Err("forbidden must state at least one rule".into());
    }
    for timezone in &fixture.forbidden.reminder_timezones {
        timezone
            .parse::<Tz>()
            .map_err(|_| format!("forbidden timezone {timezone} is not an IANA zone"))?;
    }

    let basis = basis_text(fixture)?;
    let expected = &fixture.expected;
    for (index, span) in expected.source_spans.iter().flatten().enumerate() {
        check_span_text(basis, span, &format!("source_spans[{index}]"))?;
    }
    if let Some(reminder) = &expected.reminder_proposal {
        check_span_text(
            basis,
            &reminder.source_span,
            "reminder_proposal.source_span",
        )?;
    }
    if let Some(topic) = &expected.session_topic_proposal {
        check_span_text(
            basis,
            &topic.source_span,
            "session_topic_proposal.source_span",
        )?;
    }

    let proposal = build_expected_proposal(fixture)?;
    proposal
        .validate(basis)
        .map_err(|error| format!("expected outcome is not a valid I01 proposal: {error}"))?;
    if let Some(reminder) = &expected.reminder_proposal {
        check_reminder_time(fixture, reminder)?;
    }
    let contradictions = violations(&proposal, &fixture.forbidden);
    if !contradictions.is_empty() {
        return Err(format!(
            "expected outcome violates its own forbidden rules: {contradictions:?}"
        ));
    }
    Ok(())
}

fn parse_corpus(json_text: &str) -> Result<Corpus, String> {
    let corpus: Corpus = serde_json::from_str(json_text)
        .map_err(|error| format!("corpus does not parse: {error}"))?;
    let mut seen = BTreeSet::new();
    for fixture in &corpus.fixtures {
        if !seen.insert(fixture.id.clone()) {
            return Err(format!("duplicate fixture id {}", fixture.id));
        }
        check_fixture(fixture).map_err(|error| format!("{}: {error}", fixture.id))?;
    }
    Ok(corpus)
}

fn repository_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core crate has a parent directory")
        .join(relative)
}

fn corpus_text() -> String {
    fs::read_to_string(repository_path("fixtures/intent/contrastive-fixtures.json"))
        .expect("fixtures/intent/contrastive-fixtures.json must be readable")
}

fn load_corpus() -> Corpus {
    parse_corpus(&corpus_text()).unwrap_or_else(|error| panic!("{error}"))
}

fn fixture_by_id<'a>(corpus: &'a Corpus, id: &str) -> &'a Fixture {
    corpus
        .fixtures
        .iter()
        .find(|fixture| fixture.id == id)
        .unwrap_or_else(|| panic!("fixture {id} is missing"))
}

#[test]
fn every_fixture_is_a_valid_i01_outcome() {
    let corpus = load_corpus();
    assert!(corpus.fixtures.len() >= 30, "corpus shrank unexpectedly");
}

#[test]
fn every_required_category_is_present() {
    let corpus = load_corpus();
    for required in REQUIRED_CATEGORIES {
        assert!(
            corpus
                .fixtures
                .iter()
                .any(|fixture| fixture.category == required),
            "no fixture has category {required:?}"
        );
    }
}

#[test]
fn design_examples_are_present_with_the_documented_outcomes() {
    let corpus = load_corpus();
    let design_inputs = [
        (
            "design-broad-intention",
            "Maybe a roof garden would be nice",
            Some(ItemType::Idea),
        ),
        (
            "design-undated-action",
            "I need to call the roofer",
            Some(ItemType::Action),
        ),
        (
            "design-dated-information",
            "The roof quote expires Friday",
            Some(ItemType::Note),
        ),
        (
            "design-explicit-reminder",
            "Remind me Friday at 3 p.m. to call the roofer",
            Some(ItemType::Action),
        ),
        ("design-session-topic", "Bring this up in therapy", None),
        (
            "design-unsupported-spoken-update",
            "Done with the roofer call",
            None,
        ),
    ];
    for (id, input, item_type) in design_inputs {
        let fixture = fixture_by_id(&corpus, id);
        assert_eq!(fixture.input, input, "{id}");
        assert_eq!(fixture.expected.item_type, item_type, "{id}");
        assert!(
            matches!(
                fixture.category,
                Category::Design | Category::BroadIntention
            ),
            "{id}"
        );
    }
    let reminder = fixture_by_id(&corpus, "design-explicit-reminder")
        .expected
        .reminder_proposal
        .as_ref()
        .expect("explicit reminder is expected");
    assert_eq!(reminder.quality, TimeResolutionQuality::Explicit);
    assert_eq!(
        reminder.instant.as_deref(),
        Some("2026-10-09T15:00:00-04:00")
    );
    for id in ["design-undated-action", "design-dated-information"] {
        assert_eq!(
            fixture_by_id(&corpus, id).forbidden.reminder,
            Some(ReminderBan::Any),
            "{id} must not invent a notification"
        );
    }
}

#[test]
fn spoken_update_is_unsupported_and_mutates_nothing() {
    let corpus = load_corpus();
    let fixture = fixture_by_id(&corpus, "design-unsupported-spoken-update");
    assert_eq!(
        fixture.expected.abstention,
        Some(AbstentionReason::UnsupportedOperation)
    );
    assert!(fixture.expected.item_type.is_none());
    assert!(fixture.expected.reminder_proposal.is_none());
    assert!(fixture.expected.session_topic_proposal.is_none());
    assert_eq!(fixture.forbidden.facets, Some(FacetBan::Any));
    assert!(fixture
        .forbidden
        .operations
        .contains(&ForbiddenOperation::Update));
    assert!(fixture
        .forbidden
        .operations
        .contains(&ForbiddenOperation::Create));
    assert!(fixture.preserve.raw_input);
    assert!(!fixture.recoverable);
}

#[test]
fn dropped_asr_word_fixtures_are_labeled_unrecoverable() {
    let corpus = load_corpus();
    let dropped: Vec<&Fixture> = corpus
        .fixtures
        .iter()
        .filter(|fixture| fixture.category == Category::DroppedAsrWord)
        .collect();
    assert!(dropped.len() >= 2);
    for fixture in dropped {
        assert!(!fixture.recoverable, "{}", fixture.id);
        assert!(!fixture.forbidden.is_empty(), "{}", fixture.id);
    }
}

#[test]
fn correction_fixture_keeps_raw_and_corrected_text() {
    let corpus = load_corpus();
    let fixture = fixture_by_id(&corpus, "correction-user-edit");
    assert_eq!(fixture.category, Category::Corrections);
    assert_eq!(fixture.text_basis, BasisKind::Corrected);
    assert!(fixture.preserve.raw_input && fixture.preserve.user_correction);
    assert!(fixture.input.contains("Frisday"));
    assert_eq!(
        fixture.capture_context.user_correction.as_deref(),
        Some("Remind me Friday at 10 a.m.")
    );
}

#[test]
fn negation_and_injection_fixtures_forbid_side_effects() {
    let corpus = load_corpus();
    for id in [
        "negation-do-not-remind",
        "negation-do-not-remind-me-to-call",
    ] {
        let fixture = fixture_by_id(&corpus, id);
        assert_eq!(
            fixture.expected.abstention,
            Some(AbstentionReason::Negated),
            "{id}"
        );
        assert_eq!(fixture.forbidden.facets, Some(FacetBan::Any), "{id}");
    }
    for id in ["prompt-injection-attempt-1", "prompt-injection-attempt-2"] {
        let fixture = fixture_by_id(&corpus, id);
        assert!(fixture.forbidden.session_topic, "{id}");
        assert_eq!(fixture.forbidden.reminder, Some(ReminderBan::Any), "{id}");
        assert!(!fixture.forbidden.operations.is_empty(), "{id}");
    }
}

#[test]
fn validation_doc_lists_every_fixture() {
    let corpus = load_corpus();
    let doc = fs::read_to_string(repository_path("docs/validation/intent-fixtures.md"))
        .expect("docs/validation/intent-fixtures.md must be readable");
    for fixture in &corpus.fixtures {
        assert!(
            doc.contains(&format!("`{}`", fixture.id)),
            "docs/validation/intent-fixtures.md does not list `{}`",
            fixture.id
        );
    }
}

fn mutated(fixture_id: &str, mutate: impl FnOnce(&mut Value)) -> Result<Corpus, String> {
    let mut corpus: Value = serde_json::from_str(&corpus_text()).expect("corpus is JSON");
    let fixture = corpus["fixtures"]
        .as_array_mut()
        .expect("fixtures array")
        .iter_mut()
        .find(|fixture| fixture["id"] == fixture_id)
        .unwrap_or_else(|| panic!("fixture {fixture_id} is missing"));
    mutate(fixture);
    parse_corpus(&corpus.to_string())
}

fn assert_rejected(fixture_id: &str, needle: &str, mutate: impl FnOnce(&mut Value)) {
    match mutated(fixture_id, mutate) {
        Ok(_) => panic!("mutation of {fixture_id} was accepted, expected an error with {needle:?}"),
        Err(error) => assert!(
            error.contains(needle),
            "mutation of {fixture_id} failed with {error:?}, expected {needle:?}"
        ),
    }
}

#[test]
fn unmodified_corpus_passes_the_mutation_harness() {
    mutated("design-explicit-reminder", |_| {}).expect("unmutated corpus must pass");
}

#[test]
fn validator_rejects_unknown_vocabulary() {
    assert_rejected("design-undated-action", "unknown variant", |f| {
        f["expected"]["item_type"] = json!("banana");
    });
    assert_rejected("design-undated-action", "unknown field `bogus_key`", |f| {
        f["expected"]["bogus_key"] = json!(true);
    });
    assert_rejected("design-undated-action", "unknown field `made_up`", |f| {
        f["forbidden"]["made_up"] = json!(true);
    });
    assert_rejected("design-undated-action", "unknown variant", |f| {
        f["forbidden"]["reminder"] = json!("invented");
    });
    assert_rejected("design-undated-action", "unknown variant", |f| {
        f["expected"]["abstention"] = json!("uncertain-target");
    });
    assert_rejected("design-undated-action", "unknown variant", |f| {
        f["category"] = json!("misc");
    });
}

#[test]
fn validator_rejects_duplicate_keys() {
    let text = corpus_text().replacen(
        "\"quality\": \"explicit\",",
        "\"quality\": \"explicit\", \"quality\": \"inferred\",",
        1,
    );
    let error = parse_corpus(&text).expect_err("duplicate keys must be rejected");
    assert!(error.contains("duplicate field"), "{error}");
}

#[test]
fn validator_enforces_i01_semantics() {
    assert_rejected("design-undated-action", "cannot coexist", |f| {
        f["expected"]["abstention"] = json!("Negated");
    });
    assert_rejected("design-undated-action", "requires source evidence", |f| {
        f["expected"]["source_spans"] = json!([]);
    });
    assert_rejected("design-explicit-reminder", "not valid RFC 3339", |f| {
        f["expected"]["reminder_proposal"]["instant"] = json!("not-a-date");
    });
    assert_rejected(
        "design-explicit-reminder",
        "not a valid IANA timezone",
        |f| {
            f["expected"]["reminder_proposal"]["timezone_id"] = json!("Mars/Base");
        },
    );
    assert_rejected(
        "date-ambiguous-friday",
        "must not carry a resolved instant",
        |f| {
            f["expected"]["reminder_proposal"]["instant"] = json!("2026-10-09T09:00:00-04:00");
            f["expected"]["reminder_proposal"]["timezone_id"] = json!("America/New_York");
        },
    );
    assert_rejected(
        "design-explicit-reminder",
        "missing field `source_span`",
        |f| {
            f["expected"]["reminder_proposal"]
                .as_object_mut()
                .unwrap()
                .remove("source_span");
        },
    );
    assert_rejected("design-session-topic", "missing field `source_span`", |f| {
        f["expected"]["session_topic_proposal"]
            .as_object_mut()
            .unwrap()
            .remove("source_span");
    });
    assert_rejected("quoted-text", "abstention reason must not be empty", |f| {
        f["expected"]["abstention"] = json!({"Other": "   "});
    });
    assert_rejected("empty-or-noise", "at least one facet", |f| {
        f["expected"].as_object_mut().unwrap().remove("abstention");
    });
}

#[test]
fn validator_checks_span_text_against_the_basis() {
    assert_rejected("design-undated-action", "slices", |f| {
        f["expected"]["source_spans"][0]["end"] = json!(24);
        f["expected"]["source_spans"][0]["text"] = json!("I need to call the roofer");
    });
    assert_rejected("design-undated-action", "out of bounds", |f| {
        f["expected"]["source_spans"][0]["end"] = json!(99);
    });
    assert_rejected("correction-user-edit", "slices", |f| {
        f["expected"]["reminder_proposal"]["source_span"]["text"] = json!("Frisday at 10 a.m");
    });
    assert_rejected(
        "correction-user-edit",
        "text_basis must be corrected",
        |f| {
            f["text_basis"] = json!("original");
        },
    );
}

#[test]
fn validator_recomputes_reminder_times() {
    let instant = |value: &'static str| {
        move |f: &mut Value| f["expected"]["reminder_proposal"]["instant"] = json!(value)
    };
    assert_rejected(
        "design-explicit-reminder",
        "not the upcoming friday",
        instant("2026-10-10T15:00:00-04:00"),
    );
    assert_rejected(
        "design-explicit-reminder",
        "observes",
        instant("2026-10-09T15:00:00-05:00"),
    );
    assert_rejected(
        "design-explicit-reminder",
        "the phrase says 15:00",
        instant("2026-10-09T13:00:00-04:00"),
    );
    assert_rejected(
        "timezone-implicit-local",
        "the phrase says 9:00",
        instant("2026-10-09T13:00:00-04:00"),
    );
    assert_rejected(
        "date-explicit-today",
        "not the capture day",
        instant("2026-10-09T17:00:00-04:00"),
    );
    assert_rejected(
        "design-explicit-reminder",
        "not after the capture",
        instant("2026-10-08T09:00:00-04:00"),
    );
    assert_rejected("design-explicit-reminder", "capture_instant", |f| {
        f["capture_context"]
            .as_object_mut()
            .unwrap()
            .remove("capture_instant");
    });
    assert_rejected("design-explicit-reminder", "device_timezone", |f| {
        f["capture_context"]
            .as_object_mut()
            .unwrap()
            .remove("device_timezone");
    });
}

#[test]
fn validator_requires_labels_and_consistent_forbidden_rules() {
    assert_rejected("empty-or-noise", "requires recovery_notes", |f| {
        f.as_object_mut().unwrap().remove("recovery_notes");
    });
    assert_rejected("design-undated-action", "requires recoverable false", |f| {
        f["recovery_notes"] = json!("not needed");
    });
    assert_rejected("design-undated-action", "at least one rule", |f| {
        f["forbidden"] = json!({});
    });
    assert_rejected("design-undated-action", "violates its own forbidden", |f| {
        f["forbidden"]["item_types"] = json!(["action"]);
    });
    assert_rejected(
        "design-explicit-reminder",
        "violates its own forbidden",
        |f| {
            f["forbidden"]["reminder_qualities"] = json!(["explicit"]);
        },
    );
    assert_rejected(
        "design-explicit-reminder",
        "violates its own forbidden",
        |f| {
            f["forbidden"]["reminder_timezones"] = json!(["America/New_York"]);
        },
    );
    assert_rejected("design-session-topic", "violates its own forbidden", |f| {
        f["forbidden"]["session_topic"] = json!(true);
    });
    assert_rejected("quoted-text", "cannot coexist", |f| {
        f["expected"]["item_type"] = json!("idea");
        f["expected"]["source_spans"] = json!([{"start": 0, "end": 7, "text": "He said"}]);
    });
    assert_rejected("design-undated-action", "provenance", |f| {
        f["provenance"] = json!("recorded from a live account");
    });
    assert_rejected("design-undated-action", "raw_input", |f| {
        f["preserve"]["raw_input"] = json!(false);
    });
}

#[test]
fn forbidden_rules_flag_each_kind_of_violation() {
    let base = || {
        Proposal::new(
            PROPOSAL_ID.to_string(),
            ITEM_ID.to_string(),
            CAPTURE_ID.to_string(),
            0,
            SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            TextBasis::Original { item_revision: 0 },
            REQUEST_VERSION.to_string(),
        )
    };
    let reminder = |quality, instant: Option<&str>, timezone: Option<&str>| ReminderProposal {
        instant: instant.map(str::to_string),
        timezone_id: timezone.map(str::to_string),
        quality,
        source_span: Some(SourceSpan::new(0, 1)),
    };
    let topic = SessionTopicProposal {
        topic: "therapy".to_string(),
        source_span: Some(SourceSpan::new(0, 1)),
    };

    let item_ban = Forbidden {
        item_types: vec![ItemType::Action],
        ..Forbidden::default()
    };
    assert!(violations(&base().with_item_type(Some(ItemType::Idea)), &item_ban).is_empty());
    assert!(!violations(&base().with_item_type(Some(ItemType::Action)), &item_ban).is_empty());

    let any_reminder = Forbidden {
        reminder: Some(ReminderBan::Any),
        ..Forbidden::default()
    };
    let ambiguous = reminder(TimeResolutionQuality::Ambiguous, None, None);
    assert!(violations(&base(), &any_reminder).is_empty());
    assert!(!violations(
        &base().with_reminder_proposal(Some(ambiguous.clone())),
        &any_reminder
    )
    .is_empty());

    let no_instant = Forbidden {
        reminder: Some(ReminderBan::AnyInstant),
        ..Forbidden::default()
    };
    let resolved = reminder(
        TimeResolutionQuality::Explicit,
        Some("2026-10-09T15:00:00-04:00"),
        Some("America/New_York"),
    );
    assert!(violations(&base().with_reminder_proposal(Some(ambiguous)), &no_instant).is_empty());
    assert!(!violations(
        &base().with_reminder_proposal(Some(resolved.clone())),
        &no_instant
    )
    .is_empty());

    let quality_ban = Forbidden {
        reminder_qualities: vec![TimeResolutionQuality::Inferred],
        ..Forbidden::default()
    };
    assert!(violations(
        &base().with_reminder_proposal(Some(resolved.clone())),
        &quality_ban
    )
    .is_empty());
    let inferred = reminder(
        TimeResolutionQuality::Inferred,
        Some("2026-10-09T15:00:00-04:00"),
        Some("America/New_York"),
    );
    assert!(!violations(&base().with_reminder_proposal(Some(inferred)), &quality_ban).is_empty());

    let timezone_ban = Forbidden {
        reminder_timezones: vec!["UTC".to_string()],
        ..Forbidden::default()
    };
    assert!(violations(
        &base().with_reminder_proposal(Some(resolved.clone())),
        &timezone_ban
    )
    .is_empty());
    let utc = reminder(
        TimeResolutionQuality::Explicit,
        Some("2026-10-09T15:00:00Z"),
        Some("UTC"),
    );
    assert!(!violations(&base().with_reminder_proposal(Some(utc)), &timezone_ban).is_empty());

    let topic_ban = Forbidden {
        session_topic: true,
        ..Forbidden::default()
    };
    assert!(violations(&base(), &topic_ban).is_empty());
    assert!(!violations(
        &base().with_session_topic_proposal(Some(topic.clone())),
        &topic_ban
    )
    .is_empty());

    let facet_ban = Forbidden {
        facets: Some(FacetBan::Any),
        ..Forbidden::default()
    };
    let abstains = base().with_abstention(Some(AbstentionReason::Negated));
    assert!(violations(&abstains, &facet_ban).is_empty());
    assert!(!violations(&base().with_item_type(Some(ItemType::Note)), &facet_ban).is_empty());
    assert!(!violations(&base().with_reminder_proposal(Some(resolved)), &facet_ban).is_empty());
    assert!(!violations(&base().with_session_topic_proposal(Some(topic)), &facet_ban).is_empty());

    let operation_ban = Forbidden {
        operations: vec![ForbiddenOperation::Update],
        ..Forbidden::default()
    };
    let update = Operation::Update {
        item_id: ITEM_ID.to_string(),
    };
    let unsupported = Some(AbstentionReason::UnsupportedOperation);
    assert!(violations(&base().with_item_type(Some(ItemType::Note)), &operation_ban).is_empty());
    assert!(violations(
        &base()
            .with_operation(update.clone())
            .with_abstention(unsupported.clone()),
        &operation_ban
    )
    .is_empty());
    assert!(!violations(&base().with_operation(update.clone()), &operation_ban).is_empty());
    assert!(!violations(
        &base()
            .with_operation(update)
            .with_abstention(unsupported)
            .with_item_type(Some(ItemType::Note)),
        &operation_ban
    )
    .is_empty());
}
