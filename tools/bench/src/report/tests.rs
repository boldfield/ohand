//! Reports built from hand-written journals. Every journal is synthetic: the corpus is the
//! committed fixture file, replies are the evaluation tool's authored synthetic responses, and
//! no provider, credential or network is involved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use evaluation::comparison::{DisagreementFacet, Pairing, SideStatus};
use evaluation::corpus::Corpus;
use evaluation::responses::{Behavior, Responses};
use evaluation::score::DefectKind;
use ohand_core::interpretation::instructions::{content_version, M1_INSTRUCTION_TEXT};
use ohand_core::providers::contracts::{
    DiagnosticMetadata, EffectiveSetting, ProviderProtocol, ProviderProvenance, ReportedModel,
    SettingOutcome, SettingValue, UsageAvailability,
};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;
use crate::journal::JournalWriter;
use crate::records::{Attempt, Case, Experiment, UsageMetadata};

const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/intent/contrastive-fixtures.json"
);
const RESPONSES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../evaluation/fixtures/recorded-responses.json"
);
const VERSION_A: &str = "11111111-1111-4111-8111-111111111111";
const VERSION_B: &str = "22222222-2222-4222-8222-222222222222";

const BROAD: &str = "design-broad-intention";
const UNDATED: &str = "design-undated-action";
const REMINDER: &str = "design-explicit-reminder";
const HEDGED: &str = "date-ambiguous-maybe-friday";

fn corpus() -> Corpus {
    Corpus::load(Path::new(CORPUS)).expect("the synthetic corpus loads")
}

fn reply(scenario_id: &str) -> Value {
    let corpus = corpus();
    let responses = Responses::load(Path::new(RESPONSES), &corpus).expect("responses load");
    let scenario = responses
        .scenarios
        .iter()
        .find(|scenario| scenario.scenario_id == scenario_id)
        .unwrap_or_else(|| panic!("no scenario {scenario_id}"));
    match &scenario.behavior {
        Behavior::Respond { body } => body.clone(),
        other => panic!("scenario {scenario_id} is not a reply: {other:?}"),
    }
}

fn conforming(fixture: &str) -> Value {
    reply(&format!("conforming/{fixture}"))
}

#[derive(Clone)]
struct Meta {
    protocol: ProviderProtocol,
    confirmed: bool,
    reported: ReportedModel,
    version: Option<&'static str>,
    default_seed: bool,
}

impl Meta {
    fn new(protocol: ProviderProtocol) -> Meta {
        Meta {
            protocol,
            confirmed: true,
            reported: ReportedModel::Pinned,
            version: None,
            default_seed: false,
        }
    }
}

#[derive(Clone)]
enum Kind {
    Completed,
    Failed,
    Reserved,
}

#[derive(Clone)]
struct Att {
    kind: Kind,
    dispatched: bool,
    body: Option<Value>,
    class: Option<&'static str>,
    elapsed_ms: u64,
    usage: Option<(Option<u64>, Option<u64>)>,
    meta: Option<Meta>,
    rerun_of: Option<String>,
    retry_of: Option<String>,
    id: Option<String>,
    fingerprint: Option<&'static str>,
}

impl Att {
    fn base(kind: Kind) -> Att {
        Att {
            kind,
            dispatched: true,
            body: None,
            class: None,
            elapsed_ms: 0,
            usage: None,
            meta: None,
            rerun_of: None,
            retry_of: None,
            id: None,
            fingerprint: Some("fingerprint-0001"),
        }
    }

    fn completed(body: Value, elapsed_ms: u64) -> Att {
        Att {
            body: Some(body),
            elapsed_ms,
            ..Att::base(Kind::Completed)
        }
    }

    fn failed(class: &'static str) -> Att {
        Att {
            class: Some(class),
            ..Att::base(Kind::Failed)
        }
    }

    fn reserved() -> Att {
        Att::base(Kind::Reserved)
    }

    fn skipped(class: &'static str) -> Att {
        Att {
            dispatched: false,
            ..Att::failed(class)
        }
    }

    fn usage(mut self, input: Option<u64>, output: Option<u64>) -> Att {
        self.usage = Some((input, output));
        self
    }

    fn meta(mut self, meta: Meta) -> Att {
        self.meta = Some(meta);
        self
    }
}

struct Setup {
    fixtures: Vec<&'static str>,
    source_context: String,
    profile_a_id: String,
    corpus_version: Option<String>,
    content_override: Option<(&'static str, &'static str)>,
    unknown_fields: Vec<String>,
}

impl Setup {
    fn new(fixtures: &[&'static str]) -> Setup {
        Setup {
            fixtures: fixtures.to_vec(),
            source_context:
                "synthetic-corpus mode-fake settings-temperature-0-max-output-tokens-1024"
                    .to_string(),
            profile_a_id: "anthropic-model-a".to_string(),
            corpus_version: None,
            content_override: None,
            unknown_fields: Vec::new(),
        }
    }
}

struct TestJournal {
    _dir: TempDir,
    path: PathBuf,
    writer: JournalWriter,
    case_records: HashMap<&'static str, String>,
}

fn write_journal(setup: Setup) -> TestJournal {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.ndjson");
    let corpus = corpus();
    let mut experiment = Experiment::new(
        setup
            .corpus_version
            .unwrap_or_else(|| corpus.version.clone()),
        setup.source_context,
        content_version(M1_INSTRUCTION_TEXT),
        setup.profile_a_id,
        VERSION_A,
        "openai-model-b",
        VERSION_B,
        "bench-test",
    )
    .expect("experiment is valid");
    if !setup.unknown_fields.is_empty() {
        experiment
            .set_unknown_fields(setup.unknown_fields)
            .expect("unknown fields are valid");
    }
    let mut writer = JournalWriter::open(&path).unwrap();
    writer.write_experiment(&experiment).unwrap();
    let mut case_records = HashMap::new();
    for fixture in &setup.fixtures {
        let text = match setup.content_override {
            Some((overridden, text)) if overridden == *fixture => text.to_string(),
            _ => corpus
                .fixture(fixture)
                .unwrap()
                .case_context()
                .unwrap()
                .basis_text()
                .to_string(),
        };
        let case = Case::new(&experiment.id, *fixture, text).unwrap();
        writer.write_case(&case).unwrap();
        case_records.insert(*fixture, case.id.clone());
    }
    TestJournal {
        _dir: dir,
        path,
        writer,
        case_records,
    }
}

fn metadata_json(meta: &Meta, elapsed_ms: u64, arm_version: &str) -> Value {
    let mut settings = Vec::new();
    for requested in [
        SettingValue::TemperatureMilli(0),
        SettingValue::MaxOutputTokens(1024),
    ] {
        settings.push(SettingOutcome {
            setting: requested.setting(),
            requested: Some(requested),
            effective: if meta.confirmed {
                EffectiveSetting::Reported { value: requested }
            } else {
                EffectiveSetting::Unknown
            },
        });
    }
    if meta.default_seed {
        settings.push(SettingOutcome {
            setting: ohand_core::providers::contracts::DiagnosticSetting::Seed,
            requested: None,
            effective: EffectiveSetting::Unknown,
        });
    }
    serde_json::to_value(DiagnosticMetadata {
        settings,
        usage: UsageAvailability::Unavailable,
        provenance: ProviderProvenance {
            protocol: meta.protocol,
            profile_version: meta.version.unwrap_or(arm_version).to_string(),
            reported_model: meta.reported,
        },
        elapsed_ms,
    })
    .unwrap()
}

impl TestJournal {
    fn put(&mut self, fixture: &'static str, arm: &str, att: Att) -> String {
        let arm_version = if arm == "a" { VERSION_A } else { VERSION_B };
        let phase = match att.kind {
            Kind::Completed => "completed",
            Kind::Failed if att.dispatched => "failed",
            Kind::Failed => "skipped",
            Kind::Reserved => "reserved",
        };
        let mut envelope = json!({
            "executor_record": 1,
            "phase": phase,
            "dispatched": att.dispatched,
            "run": 1,
            "arm_order": ["a", "b"],
            "pair_fingerprint": att.fingerprint,
            "class": att.class,
            "rerun_of": att.rerun_of,
            "retry_of": att.retry_of,
        });
        if let Some(body) = &att.body {
            envelope["output"] = body.clone();
        }
        if let Some(meta) = &att.meta {
            envelope["metadata"] = metadata_json(meta, att.elapsed_ms, arm_version);
        }
        let mut attempt = Attempt::new(self.case_records[fixture].clone(), arm).unwrap();
        if let Some(id) = &att.id {
            attempt.id = id.clone();
        }
        match att.kind {
            Kind::Completed => attempt.complete(att.elapsed_ms, Some(envelope)).unwrap(),
            Kind::Failed => {
                attempt.fail("provider call failed").unwrap();
                attempt.provider_output = Some(envelope);
            }
            Kind::Reserved => attempt.provider_output = Some(envelope),
        }
        attempt.usage_tokens = att.usage.map(|(input, output)| UsageMetadata {
            input_tokens: input,
            output_tokens: output,
        });
        self.writer.write_attempt(&attempt).unwrap();
        attempt.id
    }

    fn report(&self, rates: Option<&CostRates>) -> Result<BenchReport, ReportError> {
        build_report(&corpus(), &self.path, rates)
    }

    fn report_ok(&self) -> BenchReport {
        self.report(None).expect("the journal reports")
    }
}

/// Case 1: both arms conform. Case 2: arm b times out. Case 3: arm a conforms and arm b is
/// wrong about the reminder instant.
fn three_case_journal() -> TestJournal {
    let mut journal = write_journal(Setup::new(&[BROAD, UNDATED, REMINDER]));
    journal.put(
        BROAD,
        "a",
        Att::completed(conforming(BROAD), 100).usage(Some(1000), Some(200)),
    );
    journal.put(
        BROAD,
        "b",
        Att::completed(conforming(BROAD), 300).usage(Some(800), Some(100)),
    );
    journal.put(UNDATED, "a", Att::completed(conforming(UNDATED), 700));
    journal.put(UNDATED, "b", Att::failed("timed_out").usage(Some(50), None));
    journal.put(
        REMINDER,
        "a",
        Att::completed(conforming(REMINDER), 200).usage(Some(500), None),
    );
    journal.put(
        REMINDER,
        "b",
        Att::completed(reply("adversarial/wrong-instant"), 500),
    );
    journal
}

fn rates(profiles: &[(&str, u64, u64)]) -> CostRates {
    let profiles: Vec<Value> = profiles
        .iter()
        .map(|(version, input, output)| {
            json!({
                "profile_version": version,
                "input_micros_per_million_tokens": input,
                "output_micros_per_million_tokens": output,
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&json!({
        "rates_version": "rates-2026-10-test",
        "currency": "USD",
        "profiles": profiles,
    }))
    .unwrap();
    CostRates::from_bytes(&bytes).expect("rates are valid")
}

fn flag<'a>(report: &'a BenchReport, code: &str) -> Option<&'a comparability::Flag> {
    report
        .comparability
        .flags
        .iter()
        .find(|flag| flag.code == code)
}

fn error_text(result: Result<BenchReport, ReportError>) -> String {
    result.expect_err("the report is refused").to_string()
}

// ---- pairs, objective results and disagreement ---------------------------------------------

#[test]
fn pair_counts_objective_results_and_disagreement_come_from_the_journal() {
    let report = three_case_journal().report_ok();

    assert_eq!(report.pairs.eligible, 3);
    assert_eq!(report.pairs.matched, 2);
    assert_eq!(report.pairs.failed, 1);
    assert_eq!(report.pairs.missing, 0);
    assert_eq!(report.arms[0].sides.completed, 3);
    assert_eq!(report.arms[1].sides.completed, 2);
    assert_eq!(report.arms[1].sides.timed_out, 1);
    assert_eq!(report.arms[1].sides.reasons["timed_out/timed_out"], 1);

    assert_eq!(report.paired_objective.matched_pairs, 2);
    assert_eq!(report.paired_objective.both_clean, 1);
    // The guard drops the wrong instant for both arms, so neither is objectively clean even
    // though only arm b proposed a wrong deadline.
    assert_eq!(report.paired_objective.only_a_clean, 0);
    assert_eq!(report.paired_objective.only_b_clean, 0);
    assert_eq!(report.paired_objective.neither_clean, 1);
    assert_eq!(
        report.arms[1].totals.candidate_defects[&DefectKind::FalseDeadline],
        1
    );
    assert_eq!(
        report.arms[0].totals.candidate_defects[&DefectKind::FalseDeadline],
        0
    );

    assert_eq!(report.disagreement.matched_pairs, 2);
    assert_eq!(report.disagreement.agree, 1);
    assert_eq!(report.disagreement.differ, 1);
    assert_eq!(
        report.disagreement.by_facet[&DisagreementFacet::Reminder],
        1
    );

    let rows: Vec<(&str, Pairing)> = report
        .cases
        .iter()
        .map(|row| (row.fixture_id.as_str(), row.pairing))
        .collect();
    assert_eq!(
        rows,
        vec![
            (BROAD, Pairing::Matched),
            (UNDATED, Pairing::Failed),
            (REMINDER, Pairing::Matched)
        ]
    );
    assert_eq!(report.cases[1].sides[1].status, SideStatus::TimedOut);
    assert!(report.cases[1].objective.is_none());
    assert_eq!(report.cases[2].sides[1].defects, vec!["false_deadline"]);
    assert_eq!(report.cases_omitted, 0);
}

#[test]
fn the_report_chooses_no_winner_in_either_format() {
    let report = three_case_journal().report_ok();

    assert!(report.diagnostic_only);
    assert!(report.selection.winner.is_none());
    let json: Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["selection"]["winner"], Value::Null);
    assert_eq!(json["diagnostic_only"], json!(true));
    assert_eq!(json["preference"]["status"], json!("not_collected"));
    assert_eq!(json["pairs"]["matched"], json!(2));
    assert_eq!(
        json["comparability"]["model_identity_only_variable"],
        json!("not_established")
    );

    let text = report.to_text();
    assert!(text.contains("No winner is chosen and nothing is activated"));
    assert!(text.contains("matched 2  failed 1  missing 0"));
    assert!(text.contains("timed_out/timed_out"));
    assert!(text.contains("(no response)"));
    assert!(text.contains("agreement is not accuracy"));
    for banned in ["winner:", "ranking", "score:", "best model"] {
        assert!(!text.contains(banned), "text contains {banned:?}");
    }
}

#[test]
fn json_and_text_agree_on_the_counts() {
    let report = three_case_journal().report_ok();
    let json: Value = serde_json::from_str(&report.to_json()).unwrap();
    let text = report.to_text();

    let sides = &json["arms"][1]["sides"];
    assert_eq!(sides["completed"], json!(2));
    assert_eq!(sides["timed_out"], json!(1));
    assert!(text.contains("responses: completed 2/3 failed 0 timed_out 1"));
    assert_eq!(json["operations"][0]["requests"]["dispatched"], json!(3));
    assert!(text.contains("requests: dispatched 3 completed 3 failed 0"));
}

// ---- incomplete journals ---------------------------------------------------------------------

#[test]
fn incomplete_journals_count_every_unanswered_side_by_kind() {
    let mut journal = write_journal(Setup::new(&[BROAD, UNDATED, REMINDER]));
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    journal.put(BROAD, "b", Att::reserved());
    journal.put(REMINDER, "a", Att::skipped("not_authorized"));
    journal.put(REMINDER, "b", Att::completed(conforming(REMINDER), 20));
    let report = journal.report_ok();

    assert_eq!(report.pairs.eligible, 3);
    assert_eq!(report.pairs.matched, 0);
    assert_eq!(report.pairs.failed, 0);
    assert_eq!(report.pairs.missing, 3);
    assert_eq!(report.pairs.no_record, 1);
    let a = &report.arms[0].sides;
    assert_eq!((a.completed, a.skipped, a.missing), (1, 1, 1));
    assert_eq!(a.reasons["skipped/not_authorized"], 1);
    let b = &report.arms[1].sides;
    assert_eq!((b.completed, b.unknown, b.missing), (1, 1, 1));
    assert_eq!(b.reasons["unknown/outcome_unknown"], 1);

    assert_eq!(report.operations[1].requests.outcome_unknown, 1);
    assert_eq!(report.operations[0].requests.dispatched, 1);
    assert_eq!(flag(&report, "incomplete_pairs").unwrap().count, 3);
    assert!(report
        .to_text()
        .contains("missing 3  (of which no record 1)"));
}

#[test]
fn a_journal_without_cases_or_experiment_is_refused() {
    let journal = write_journal(Setup::new(&[]));
    assert!(error_text(journal.report(None)).contains("no cases"));

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.ndjson");
    std::fs::write(&path, "").unwrap();
    let message = build_report(&corpus(), &path, None)
        .expect_err("empty journal is refused")
        .to_string();
    assert!(message.contains("no experiment"));
}

#[test]
fn a_run_that_dispatched_nothing_reports_every_case_as_missing_without_inventing_numbers() {
    let journal = write_journal(Setup::new(&[BROAD, UNDATED]));
    let report = journal.report_ok();

    assert_eq!(report.pairs.eligible, 2);
    assert_eq!(
        (
            report.pairs.matched,
            report.pairs.failed,
            report.pairs.missing
        ),
        (0, 0, 2)
    );
    assert_eq!(report.pairs.no_record, 2);
    assert_eq!(report.paired_objective.matched_pairs, 0);
    assert_eq!(report.disagreement.matched_pairs, 0);
    for arm in 0..2 {
        assert_eq!(report.arms[arm].sides.missing, 2);
        assert_eq!(report.arms[arm].sides.completed, 0);
        assert_eq!(report.operations[arm].requests.dispatched, 0);
        assert_eq!(report.operations[arm].latency_all_completed.samples, 0);
        assert_eq!(
            report.operations[arm].usage_all_dispatched.input_tokens,
            None
        );
    }
    assert_eq!(flag(&report, "incomplete_pairs").unwrap().count, 2);
    assert!(report
        .cases
        .iter()
        .all(|row| row.sides[0].status == SideStatus::Missing));
    assert!(report
        .to_text()
        .contains("matched 0  failed 0  missing 2  (of which no record 2)"));
}

// ---- reruns and retries -----------------------------------------------------------------------

#[test]
fn a_rerun_replaces_the_earlier_attempt_and_is_flagged() {
    let mut journal = write_journal(Setup::new(&[BROAD]));
    let first = journal.put(BROAD, "a", Att::failed("rate_limited"));
    journal.put(
        BROAD,
        "a",
        Att {
            rerun_of: Some(first),
            ..Att::completed(conforming(BROAD), 40)
        },
    );
    journal.put(BROAD, "b", Att::completed(conforming(BROAD), 50));
    let report = journal.report_ok();

    assert_eq!(report.pairs.matched, 1);
    assert_eq!(report.arms[0].sides.completed, 1);
    let requests = &report.operations[0].requests;
    assert_eq!(
        (
            requests.dispatched,
            requests.completed,
            requests.failed,
            requests.reruns
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(flag(&report, "reruns_present").unwrap().count, 1);
}

#[test]
fn a_reservation_is_superseded_by_the_terminal_record_with_the_same_id() {
    let mut journal = write_journal(Setup::new(&[BROAD]));
    let id = journal.put(BROAD, "a", Att::reserved());
    journal.put(
        BROAD,
        "a",
        Att {
            id: Some(id),
            ..Att::completed(conforming(BROAD), 15)
        },
    );
    journal.put(BROAD, "b", Att::completed(conforming(BROAD), 25));
    let report = journal.report_ok();

    assert_eq!(report.operations[0].requests.dispatched, 1);
    assert_eq!(report.operations[0].requests.completed, 1);
    assert_eq!(report.operations[0].requests.outcome_unknown, 0);
    assert_eq!(report.pairs.matched, 1);
}

// ---- latency, tokens and cost ----------------------------------------------------------------

#[test]
fn latency_and_usage_are_summarized_over_matched_pairs_and_over_everything() {
    let report = three_case_journal().report_ok();
    let a = &report.operations[0];
    let b = &report.operations[1];

    assert_eq!(
        (
            a.requests.dispatched,
            a.requests.completed,
            a.requests.failed
        ),
        (3, 3, 0)
    );
    assert_eq!(
        (
            b.requests.dispatched,
            b.requests.completed,
            b.requests.failed
        ),
        (3, 2, 1)
    );

    let matched = &a.latency_matched;
    assert_eq!(matched.samples, 2);
    assert_eq!(
        (
            matched.min_ms,
            matched.median_ms,
            matched.max_ms,
            matched.total_ms
        ),
        (Some(100), Some(100), Some(200), Some(300))
    );
    let all = &a.latency_all_completed;
    assert_eq!(all.samples, 3);
    assert_eq!(
        (all.min_ms, all.median_ms, all.max_ms, all.total_ms),
        (Some(100), Some(200), Some(700), Some(1000))
    );
    let b_matched = &b.latency_matched;
    assert_eq!(
        (
            b_matched.samples,
            b_matched.min_ms,
            b_matched.max_ms,
            b_matched.total_ms
        ),
        (2, Some(300), Some(500), Some(800))
    );
    assert_eq!(b.latency_all_completed.samples, 2);

    let usage = &a.usage_matched;
    assert_eq!((usage.attempts, usage.attempts_with_usage), (2, 2));
    assert_eq!(
        (usage.input_tokens, usage.input_tokens_attempts),
        (Some(1500), 2)
    );
    assert_eq!(
        (usage.output_tokens, usage.output_tokens_attempts),
        (Some(200), 1)
    );
    let usage = &a.usage_all_dispatched;
    assert_eq!(
        (
            usage.attempts,
            usage.attempts_with_usage,
            usage.attempts_without_usage
        ),
        (3, 2, 1)
    );
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(1500), Some(200))
    );

    let usage = &b.usage_matched;
    assert_eq!((usage.attempts, usage.attempts_with_usage), (2, 1));
    assert_eq!(usage.input_tokens, Some(800));
    let usage = &b.usage_all_dispatched;
    assert_eq!(
        (
            usage.attempts,
            usage.attempts_with_usage,
            usage.attempts_without_usage
        ),
        (3, 2, 1)
    );
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(850), Some(100))
    );
    assert_eq!(usage.output_tokens_attempts, 1);
}

#[test]
fn missing_usage_and_duration_stay_unknown_and_are_never_zero() {
    let mut journal = write_journal(Setup::new(&[BROAD]));
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    journal.put(BROAD, "b", Att::failed("rate_limited"));
    let report = journal.report_ok();

    let usage = &report.operations[0].usage_all_dispatched;
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.attempts_without_usage, 1);
    let latency = &report.operations[1].latency_all_completed;
    assert_eq!(
        (
            latency.samples,
            latency.min_ms,
            latency.median_ms,
            latency.total_ms
        ),
        (0, None, None, None)
    );
    let json: Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(
        json["operations"][0]["usage_all_dispatched"]["input_tokens"],
        Value::Null
    );
    let text = report.to_text();
    assert!(text.contains("input tokens unknown (reported by 0)"));
    assert!(text.contains("latency, every completed: none recorded"));
}

#[test]
fn without_rates_every_cost_is_unknown() {
    let report = three_case_journal().report_ok();
    for operations in &report.operations {
        assert!(matches!(
            operations.cost,
            CostSummary::Unknown {
                reason: "no_rates_supplied"
            }
        ));
    }
    assert!(report
        .to_text()
        .contains("cost: unknown (no_rates_supplied)"));
}

#[test]
fn a_cost_estimate_names_its_rates_and_never_claims_to_be_billed() {
    let supplied = rates(&[(VERSION_A, 3_000_000, 15_000_000)]);
    let report = three_case_journal().report(Some(&supplied)).unwrap();

    match &report.operations[0].cost {
        CostSummary::Estimated {
            provenance,
            input_micros,
            output_micros,
            total_micros,
            attempts_priced,
            attempts_dispatched,
            complete,
            billed,
        } => {
            assert_eq!(provenance.kind, "supplied_rate_estimate");
            assert_eq!(provenance.rates_version.as_str(), "rates-2026-10-test");
            assert_eq!(provenance.currency.as_str(), "USD");
            assert_eq!(provenance.profile_version, VERSION_A);
            assert_eq!(*input_micros, Some(4_500));
            assert_eq!(*output_micros, Some(3_000));
            assert_eq!(*total_micros, 7_500);
            assert_eq!((*attempts_priced, *attempts_dispatched), (2, 3));
            assert!(
                !complete,
                "one attempt reported no usage, so this is a lower bound"
            );
            assert!(!billed);
        }
        other => panic!("expected an estimate, got {other:?}"),
    }
    assert!(matches!(
        report.operations[1].cost,
        CostSummary::Unknown {
            reason: "no_rate_for_profile_version"
        }
    ));
    let text = report.to_text();
    assert!(text.contains("estimate 0.007500 USD from rates rates-2026-10-test"));
    assert!(text.contains("a lower bound"));
    assert!(text.contains("not a billed cost"));
}

#[test]
fn cost_rounds_down_and_is_unknown_when_no_usage_was_reported() {
    let mut journal = write_journal(Setup::new(&[BROAD]));
    journal.put(
        BROAD,
        "a",
        Att::completed(conforming(BROAD), 10).usage(Some(1), Some(1)),
    );
    journal.put(BROAD, "b", Att::completed(conforming(BROAD), 10));
    let supplied = rates(&[
        (VERSION_A, 1_500_000, 1_999_999),
        (VERSION_B, 1_000_000, 1_000_000),
    ]);
    let report = journal.report(Some(&supplied)).unwrap();

    match &report.operations[0].cost {
        CostSummary::Estimated {
            input_micros,
            output_micros,
            total_micros,
            complete,
            ..
        } => {
            assert_eq!(
                (*input_micros, *output_micros, *total_micros),
                (Some(1), Some(1), 2)
            );
            assert!(*complete);
        }
        other => panic!("expected an estimate, got {other:?}"),
    }
    assert!(matches!(
        report.operations[1].cost,
        CostSummary::Unknown {
            reason: "no_usage_reported"
        }
    ));
}

#[test]
fn rates_files_are_validated_and_never_echo_their_values() {
    let accept = |value: Value| CostRates::from_bytes(&serde_json::to_vec(&value).unwrap());
    let profile = |version: &str| {
        json!({
            "profile_version": version,
            "input_micros_per_million_tokens": 1,
            "output_micros_per_million_tokens": 2,
        })
    };
    let file = |profiles: Vec<Value>| json!({"rates_version": "rates-1", "currency": "USD", "profiles": profiles});

    assert!(accept(file(vec![profile(VERSION_A)])).is_ok());
    assert!(accept(file(vec![])).is_err());
    assert!(accept(file(vec![profile(VERSION_A), profile(VERSION_A)])).is_err());
    let mut too_high = profile(VERSION_A);
    too_high["input_micros_per_million_tokens"] = json!(rates::MAX_MICROS_PER_MILLION_TOKENS + 1);
    assert!(accept(file(vec![too_high])).is_err());
    let mut unknown_field = file(vec![profile(VERSION_A)]);
    unknown_field["price_list_url"] = json!("https://example.com/prices");
    assert!(accept(unknown_field).is_err());
    let mut negative = profile(VERSION_A);
    negative["output_micros_per_million_tokens"] = json!(-1);
    assert!(accept(file(vec![negative])).is_err());

    let mut credential = file(vec![profile(VERSION_A)]);
    credential["rates_version"] = json!("sk-live-abcdefghijklmnopqrstuvwx");
    let message = accept(credential)
        .expect_err("credential-shaped label")
        .to_string();
    assert!(
        !message.contains("sk-live"),
        "message echoes the value: {message}"
    );
    assert!(CostRates::from_bytes(b"not json").is_err());
    assert!(CostRates::from_bytes(&vec![b' '; 70 * 1024]).is_err());
    assert!(CostRates::load(Path::new("/nonexistent/rates.json")).is_err());
}

// ---- comparability ---------------------------------------------------------------------------

#[test]
fn comparability_flags_what_the_arms_do_not_share() {
    let mut journal = write_journal(Setup::new(&[BROAD, UNDATED]));
    let anthropic = Meta {
        confirmed: false,
        reported: ReportedModel::NotReported,
        version: Some("33333333-3333-4333-8333-333333333333"),
        default_seed: true,
        ..Meta::new(ProviderProtocol::Anthropic)
    };
    let openai = Meta {
        reported: ReportedModel::Different,
        ..Meta::new(ProviderProtocol::OpenAi)
    };
    journal.put(
        BROAD,
        "a",
        Att::completed(conforming(BROAD), 10).meta(anthropic),
    );
    journal.put(
        BROAD,
        "b",
        Att::completed(conforming(BROAD), 10).meta(openai),
    );
    journal.put(
        UNDATED,
        "a",
        Att {
            fingerprint: None,
            ..Att::completed(conforming(UNDATED), 10)
        },
    );
    journal.put(
        UNDATED,
        "b",
        Att {
            fingerprint: Some("fingerprint-0002"),
            ..Att::completed(conforming(UNDATED), 10)
        },
    );
    let report = journal.report_ok();
    let comparability = &report.comparability;

    assert_eq!(
        comparability.model_identity_only_variable,
        "not_established"
    );
    assert_eq!(comparability.arms[0].protocol.as_deref(), Some("anthropic"));
    assert_eq!(comparability.arms[1].protocol.as_deref(), Some("open_ai"));
    assert_eq!(
        report.experiment.profiles[1].protocol.as_deref(),
        Some("open_ai")
    );
    assert_eq!(
        comparability.arms[0].requested,
        vec!["max_output_tokens=1024", "temperature_milli=0"]
    );
    assert_eq!(comparability.arms[0].effective_unknown, 2);
    assert_eq!(comparability.arms[1].effective_confirmed, 2);

    let count = |code: &str| flag(&report, code).map(|flag| flag.count);
    assert_eq!(count("different_protocols"), Some(1));
    assert_eq!(count("effective_settings_unknown"), Some(2));
    assert_eq!(count("reported_model_differs"), Some(1));
    assert_eq!(count("reported_model_not_reported"), Some(1));
    assert_eq!(count("profile_version_mismatch"), Some(1));
    assert_eq!(count("provider_defaults_in_use"), Some(1));
    assert_eq!(count("pairs_not_derived"), Some(1));
    assert_eq!(count("request_fingerprint_conflict"), None);
    assert_eq!(count("fake_execution"), Some(2));
    assert_eq!(count("small_sample"), Some(2));
    assert_eq!(count("requested_settings_differ"), None);
    assert_eq!(count("effective_settings_differ"), None);
    assert!(report.to_text().contains("[different_protocols] x1"));
}

#[test]
fn attempts_of_one_case_with_different_request_fingerprints_are_flagged() {
    let mut journal = write_journal(Setup::new(&[BROAD]));
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    journal.put(
        BROAD,
        "b",
        Att {
            fingerprint: Some("fingerprint-0002"),
            ..Att::completed(conforming(BROAD), 10)
        },
    );
    let report = journal.report_ok();
    assert_eq!(
        flag(&report, "request_fingerprint_conflict").unwrap().count,
        1
    );
}

#[test]
fn live_journals_are_labeled_live_and_unknown_metadata_is_listed() {
    let mut setup = Setup::new(&[BROAD]);
    setup.source_context =
        "synthetic-corpus mode-live settings-temperature-0-max-output-tokens-1024".to_string();
    setup.unknown_fields = vec!["source_revision".to_string()];
    let mut journal = write_journal(setup);
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    journal.put(BROAD, "b", Att::completed(conforming(BROAD), 10));
    let report = journal.report_ok();

    assert_eq!(report.experiment.execution, ExecutionKind::Live);
    assert!(flag(&report, "fake_execution").is_none());
    assert_eq!(
        flag(&report, "unknown_experiment_metadata").unwrap().count,
        1
    );
    assert_eq!(
        report.experiment.unknown_metadata_fields,
        vec!["source_revision"]
    );
}

// ---- refusals --------------------------------------------------------------------------------

#[test]
fn a_journal_that_does_not_match_the_corpus_is_refused() {
    let mut other_corpus = Setup::new(&[BROAD]);
    other_corpus.corpus_version = Some("sha256:other".to_string());
    assert!(
        error_text(write_journal(other_corpus).report(None)).contains("corpus is not the version")
    );

    let mut changed_text = Setup::new(&[BROAD]);
    changed_text.content_override = Some((BROAD, "Something else entirely"));
    assert!(
        error_text(write_journal(changed_text).report(None)).contains("differs from the corpus")
    );

    let mut unknown_case = write_journal(Setup::new(&[BROAD]));
    let case = Case::new("unused", "no-such-fixture", "text").unwrap();
    let mut raw = std::fs::read_to_string(&unknown_case.path).unwrap();
    let experiment_id = {
        let first: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        first["id"].as_str().unwrap().to_string()
    };
    let case = Case {
        experiment_id,
        ..case
    };
    raw.push_str(&serde_json::to_string(&crate::journal::JournalRecord::Case(case)).unwrap());
    raw.push('\n');
    std::fs::write(&unknown_case.path, raw).unwrap();
    unknown_case.writer = JournalWriter::open(&unknown_case.path).unwrap();
    assert!(error_text(unknown_case.report(None)).contains("does not contain"));
}

#[test]
fn the_mode_must_be_stated_by_the_experiment() {
    let mut setup = Setup::new(&[BROAD]);
    setup.source_context = "synthetic-corpus".to_string();
    let mut journal = write_journal(setup);
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    assert!(error_text(journal.report(None)).contains("fake or live mode"));
}

#[test]
fn a_journal_with_a_credential_shaped_record_is_refused_without_echoing_it() {
    let journal = write_journal(Setup::new(&[BROAD]));
    let secret = "sk-live-abcdefghijklmnopqrstuvwx";
    let raw = std::fs::read_to_string(&journal.path).unwrap();
    std::fs::write(&journal.path, raw.replace("anthropic-model-a", secret)).unwrap();

    let message = error_text(journal.report(None));
    assert!(message.contains("could not be read"));
    assert!(!message.contains("sk-live"));
}

#[test]
fn a_missing_journal_is_refused_and_not_created() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent.ndjson");
    let message = build_report(&corpus(), &path, None)
        .unwrap_err()
        .to_string();
    assert!(message.contains("does not exist"));
    assert!(!path.exists());
    let message = build_report(&corpus(), dir.path(), None)
        .unwrap_err()
        .to_string();
    assert!(message.contains("not a file"));
}

#[test]
fn a_torn_journal_is_reported_and_left_byte_identical() {
    let mut journal = three_case_journal();
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    let mut bytes = std::fs::read(&journal.path).unwrap();
    bytes.extend_from_slice(br#"{"record_type":"attempt","id":"torn"#);
    std::fs::write(&journal.path, &bytes).unwrap();

    let report = journal.report_ok();
    assert!(report.journal.torn_tail);
    assert_eq!(flag(&report, "journal_tail_unreadable").unwrap().count, 1);
    assert_eq!(report.pairs.eligible, 3);
    assert_eq!(std::fs::read(&journal.path).unwrap(), bytes);
}

// ---- text safety -----------------------------------------------------------------------------

#[test]
fn journal_text_is_flattened_and_secret_looking_text_is_withheld() {
    assert_eq!(safe("a\u{1b}[31mb\u{202e}c\nd"), "a [31mb c d");
    assert_eq!(safe("zero\u{200b}width\u{feff}"), "zero width ");
    assert_eq!(safe("see http://10.0.0.5/admin"), "[withheld]");
    assert_eq!(safe("model-a"), "model-a");
    assert_eq!(safe(&"x".repeat(500)).chars().count(), MAX_FREE_TEXT);
}

#[test]
fn terminal_control_characters_in_a_profile_id_never_reach_the_output() {
    let mut setup = Setup::new(&[BROAD]);
    setup.profile_a_id = "model\u{1b}[2Jbidi\u{202e}end".to_string();
    let mut journal = write_journal(setup);
    journal.put(BROAD, "a", Att::completed(conforming(BROAD), 10));
    journal.put(BROAD, "b", Att::completed(conforming(BROAD), 10));
    let report = journal.report_ok();

    for output in [report.to_text(), report.to_json()] {
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{202e}'));
        assert!(output
            .chars()
            .all(|character| character == '\n' || !character.is_control()));
    }
    assert!(report.to_text().contains("model [2J"));
}

#[test]
fn per_case_rows_carry_fixed_vocabulary_only() {
    let mut journal = write_journal(Setup::new(&[HEDGED]));
    journal.put(HEDGED, "a", Att::completed(conforming(HEDGED), 10));
    journal.put(
        HEDGED,
        "b",
        Att::completed(reply("adversarial/fabricated-deadline-for-hedged-time"), 10),
    );
    let report = journal.report_ok();

    let json = report.to_json();
    let row = serde_json::to_value(&report.cases[0]).unwrap();
    let allowed_side_keys = [
        "status",
        "reason",
        "producer",
        "defects",
        "abstention",
        "forbidden_hits",
    ];
    for side in row["sides"].as_array().unwrap() {
        for key in side.as_object().unwrap().keys() {
            assert!(
                allowed_side_keys.contains(&key.as_str()),
                "unexpected key {key}"
            );
        }
    }
    let row_json = row.to_string();
    assert!(!row_json.contains("\"detail\""));
    assert!(!row_json.contains("\"error\""));
    assert!(!json.contains("\"error\""));
}
