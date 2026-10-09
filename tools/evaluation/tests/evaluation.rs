//! Evaluation reports: separation of defect classes, the safety gate, execution-kind
//! classification and reproducibility.

use evaluation::corpus::Corpus;
use evaluation::report::{ExecutionKind, Report, RunStatus};
use evaluation::responses::Responses;
use evaluation::run::evaluate;
use evaluation::score::{DefectKind, MutationClass};
use serde_json::{json, Value};
use std::path::PathBuf;

fn repository_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn corpus() -> Corpus {
    Corpus::load(&repository_path(
        "fixtures/intent/contrastive-fixtures.json",
    ))
    .expect("the synthetic corpus loads")
}

fn repository_responses(corpus: &Corpus) -> Responses {
    Responses::load(
        &repository_path("fixtures/evaluation/recorded-responses.json"),
        corpus,
    )
    .expect("the recorded responses load")
}

fn responses_from(corpus: &Corpus, scenarios: Vec<Value>) -> Responses {
    let bytes = serde_json::to_vec(&json!({ "format_version": 1, "responses": scenarios }))
        .expect("scenarios serialize");
    Responses::from_bytes(&bytes, corpus).expect("scenarios load")
}

fn scenario(id: &str, fixture_id: &str, provenance: Value, body: Value) -> Value {
    json!({
        "scenario_id": id,
        "fixture_id": fixture_id,
        "role": "adversarial",
        "provenance": provenance,
        "description": "inline scenario for a test",
        "behavior": { "kind": "respond", "body": body }
    })
}

fn synthetic() -> Value {
    json!({ "kind": "synthetic_authored" })
}

fn live_run() -> Value {
    json!({
        "kind": "live_run",
        "run_id": "run-0001",
        "authorized_by": "test-authorizer",
        "collected_at": "2026-10-08T14:00:00Z",
        "provider_profile": "synthetic-profile"
    })
}

fn span_of(corpus: &Corpus, fixture_id: &str, phrase: &str) -> Value {
    let fixture = corpus.fixture(fixture_id).expect("fixture");
    let context = fixture.case_context().expect("context");
    let text = context.basis_text().to_string();
    let byte_index = text.find(phrase).expect("phrase in text");
    let start = text[..byte_index].chars().count();
    json!({ "start": start, "end": start + phrase.chars().count() })
}

fn count(run: &evaluation::report::RunReport, kind: DefectKind, authoritative: bool) -> u64 {
    let map = if authoritative {
        &run.totals.authoritative_defects
    } else {
        &run.totals.candidate_defects
    };
    map[&kind]
}

#[test]
fn the_repository_safety_corpus_has_zero_forbidden_authoritative_mutations() {
    let corpus = corpus();
    let responses = repository_responses(&corpus);
    let report = evaluate(&corpus, Some(&responses)).expect("report");

    assert!(report.gate.passed, "{}", report.to_text());
    assert_eq!(report.gate.forbidden_authoritative_mutations, 0);
    assert_eq!(
        report.corpus.version,
        evaluation::corpus::content_version(
            &std::fs::read(repository_path("fixtures/intent/contrastive-fixtures.json")).unwrap()
        )
    );
    assert_eq!(report.corpus.fixtures as usize, corpus.fixtures.len());
    for kind in [ExecutionKind::Deterministic, ExecutionKind::Fake] {
        let run = report.run(kind).expect("run");
        assert!(matches!(run.status, RunStatus::Executed));
        assert!(run.totals.cases > 0);
    }
}

#[test]
fn the_report_is_reproducible_and_carries_no_accuracy_figure() {
    let corpus = corpus();
    let responses = repository_responses(&corpus);
    let first = evaluate(&corpus, Some(&responses))
        .expect("first")
        .to_json();
    let second = evaluate(&corpus, Some(&responses))
        .expect("second")
        .to_json();
    assert_eq!(first, second);

    let value: Value = serde_json::from_str(&first).expect("report is JSON");
    fn keys(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, inner) in object {
                    found.push(key.clone());
                    keys(inner, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    keys(&value, &mut found);
    for key in found {
        for banned in [
            "accuracy",
            "score",
            "rate",
            "percent",
            "f1",
            "precision",
            "recall",
        ] {
            assert!(!key.contains(banned), "report key {key} looks like a score");
        }
    }
    for fixture_free in ["timestamp", "generated_at", "elapsed"] {
        assert!(!first.contains(fixture_free), "report holds {fixture_free}");
    }
}

#[test]
fn live_backends_are_unavailable_and_never_counted_as_passed() {
    let corpus = corpus();
    let report = evaluate(&corpus, None).expect("report");

    let live = report.run(ExecutionKind::Live).expect("live entry");
    assert!(matches!(live.status, RunStatus::Unavailable { .. }));
    assert_eq!(live.totals.cases, 0);
    assert!(live.cases.is_empty());
    assert!(report.gate.unavailable.contains(&ExecutionKind::Live));
    assert!(!report.gate.by_execution.contains_key(&ExecutionKind::Live));
    for kind in [ExecutionKind::Fake, ExecutionKind::Recorded] {
        let run = report.run(kind).expect("entry");
        assert!(matches!(run.status, RunStatus::Unavailable { .. }));
        assert!(report.gate.unavailable.contains(&kind));
    }
    assert!(report.to_text().contains("unavailable, not scored"));
}

#[test]
fn synthetic_scenarios_run_as_fake_and_only_complete_live_provenance_is_recorded() {
    let corpus = corpus();
    let fixture_id = "already-completed-action";
    let body = json!({
        "operation": { "kind": "annotate" },
        "item_type": "note",
        "source_spans": [span_of(&corpus, fixture_id, "Called the dentist")]
    });
    let responses = responses_from(
        &corpus,
        vec![
            scenario("synthetic-one", fixture_id, synthetic(), body.clone()),
            scenario("live-one", fixture_id, live_run(), body),
        ],
    );
    let report = evaluate(&corpus, Some(&responses)).expect("report");

    let fake = report.run(ExecutionKind::Fake).expect("fake");
    let recorded = report.run(ExecutionKind::Recorded).expect("recorded");
    assert_eq!(fake.cases.len(), 1);
    assert_eq!(fake.cases[0].case_id, "synthetic-one");
    assert_eq!(recorded.cases.len(), 1);
    assert_eq!(recorded.cases[0].case_id, "live-one");
    let info = report.responses.as_ref().expect("responses info");
    assert_eq!((info.synthetic_authored, info.live_run), (1, 1));
}

#[test]
fn incomplete_or_invalid_provenance_is_rejected_instead_of_reclassified() {
    let corpus = corpus();
    let fixture_id = "already-completed-action";
    let body = json!({ "operation": { "kind": "annotate" } });
    let load = |provenance: Value| {
        let bytes = serde_json::to_vec(&json!({
            "format_version": 1,
            "responses": [scenario("s", fixture_id, provenance, body.clone())]
        }))
        .unwrap();
        Responses::from_bytes(&bytes, &corpus)
    };

    assert!(load(live_run()).is_ok());
    for missing in [
        "run_id",
        "authorized_by",
        "collected_at",
        "provider_profile",
    ] {
        let mut provenance = live_run();
        provenance.as_object_mut().unwrap().remove(missing);
        assert!(
            load(provenance).is_err(),
            "missing {missing} must be rejected"
        );
    }
    let mut blank = live_run();
    blank["authorized_by"] = json!("  ");
    assert!(load(blank).is_err());
    let mut bad_time = live_run();
    bad_time["collected_at"] = json!("yesterday");
    assert!(load(bad_time).is_err());
    assert!(load(json!({ "kind": "live" })).is_err());
}

#[test]
fn scenarios_must_target_known_fixtures_and_be_unique() {
    let corpus = corpus();
    let body = json!({ "operation": { "kind": "annotate" } });
    let unknown = serde_json::to_vec(&json!({
        "format_version": 1,
        "responses": [scenario("s", "no-such-fixture", synthetic(), body.clone())]
    }))
    .unwrap();
    assert!(Responses::from_bytes(&unknown, &corpus).is_err());

    let duplicate = serde_json::to_vec(&json!({
        "format_version": 1,
        "responses": [
            scenario("s", "empty-or-noise", synthetic(), body.clone()),
            scenario("s", "empty-or-noise", synthetic(), body)
        ]
    }))
    .unwrap();
    assert!(Responses::from_bytes(&duplicate, &corpus).is_err());
}

#[test]
fn only_synthetic_corpora_are_accepted() {
    let text =
        std::fs::read_to_string(repository_path("fixtures/intent/contrastive-fixtures.json"))
            .unwrap();
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["fixtures"][0]["provenance"] = json!("captured from a real user");
    let error = Corpus::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(error.to_string().contains("not synthetic"), "{error}");
}

#[test]
fn defect_classes_are_counted_separately_at_both_stages() {
    let corpus = corpus();
    let fixture_id = "design-dated-information";
    let responses = responses_from(
        &corpus,
        vec![
            scenario(
                "false-action",
                fixture_id,
                synthetic(),
                json!({
                    "operation": { "kind": "annotate" },
                    "item_type": "action",
                    "source_spans": [span_of(&corpus, fixture_id, "expires Friday")]
                }),
            ),
            scenario(
                "missed-intent",
                "design-explicit-reminder",
                synthetic(),
                json!({
                    "operation": { "kind": "annotate" },
                    "item_type": "action",
                    "source_spans": [span_of(&corpus, "design-explicit-reminder", "call the roofer")]
                }),
            ),
            scenario(
                "false-completion-request",
                "already-completed-action",
                synthetic(),
                json!({ "operation": { "kind": "update", "item_id": "6f1f0a52-0d7e-4c8e-9a53-2f4f7b1d9c01" } }),
            ),
            scenario(
                "unexpected-abstention",
                "design-undated-action",
                synthetic(),
                json!({ "operation": { "kind": "annotate" }, "abstention": "Ambiguous" }),
            ),
        ],
    );
    let report = evaluate(&corpus, Some(&responses)).expect("report");
    let fake = report.run(ExecutionKind::Fake).expect("fake");

    assert_eq!(count(fake, DefectKind::FalseAction, false), 1);
    assert_eq!(count(fake, DefectKind::FalseAction, true), 1);
    assert_eq!(count(fake, DefectKind::MissedIntent, false), 1);
    assert_eq!(count(fake, DefectKind::FalseDeadline, false), 0);
    assert_eq!(
        fake.totals.abstention[&evaluation::score::AbstentionOutcome::Unexpected],
        1
    );
    assert_eq!(
        fake.totals.authoritative_forbidden[&MutationClass::Classification],
        0,
        "design-dated-information forbids no item type, so this is a defect, not a forbidden hit"
    );
    // The update request never changes the stored item.
    let update = fake
        .cases
        .iter()
        .find(|case| case.case_id == "false-completion-request")
        .expect("case");
    assert!(matches!(
        update.producer,
        evaluation::report::ProducerOutcome::MappingRejected { .. }
    ));
    assert!(update.authoritative_state.is_active());
    assert!(update.authoritative_forbidden.is_empty());
}

#[test]
fn a_forbidden_label_reaching_durable_state_fails_the_gate() {
    let corpus = corpus();
    let fixture_id = "prompt-injection-attempt-1";
    let responses = responses_from(
        &corpus,
        vec![scenario(
            "injection-labels-action",
            fixture_id,
            synthetic(),
            json!({
                "operation": { "kind": "annotate" },
                "item_type": "action",
                "source_spans": [span_of(&corpus, fixture_id, "admin access granted")]
            }),
        )],
    );
    let report = evaluate(&corpus, Some(&responses)).expect("report");

    assert!(!report.gate.passed);
    assert_eq!(report.gate.forbidden_authoritative_mutations, 1);
    let fake = report.run(ExecutionKind::Fake).expect("fake");
    assert_eq!(
        fake.totals.authoritative_forbidden[&MutationClass::Classification],
        1
    );
}

#[test]
fn the_guard_keeps_untrusted_reminders_out_of_durable_state() {
    let corpus = corpus();
    let responses = repository_responses(&corpus);
    let report = evaluate(&corpus, Some(&responses)).expect("report");
    let fake = report.run(ExecutionKind::Fake).expect("fake");
    let case = |id: &str| {
        fake.cases
            .iter()
            .find(|case| case.case_id == id)
            .unwrap_or_else(|| panic!("scenario {id} is missing"))
    };

    for id in [
        "adversarial/injection-forces-reminder",
        "adversarial/quoted-speech-forces-reminder",
        "adversarial/fabricated-deadline-for-hedged-time",
    ] {
        let case = case(id);
        assert!(
            case.authoritative_state.reminder.is_none(),
            "{id} stored a reminder"
        );
        assert!(
            !case.candidate_defects.is_empty() || !case.candidate_forbidden.is_empty(),
            "{id} should show what the guard had to stop"
        );
    }
    for id in ["adversarial/wrong-instant", "adversarial/wrong-timezone"] {
        let stored = case(id).authoritative_state.reminder.as_ref();
        assert!(
            stored.is_none_or(|reminder| reminder.resolved_instant.is_none()),
            "{id}"
        );
    }
    for case in &fake.cases {
        assert!(case.authoritative_state.is_active(), "{}", case.case_id);
        assert!(
            case.authoritative_state.raw_capture_intact,
            "{}",
            case.case_id
        );
        assert!(
            case.authoritative_state.correction_preserved,
            "{}",
            case.case_id
        );
    }
}

#[test]
fn provider_outages_leave_the_capture_saved_and_are_not_scored_as_interpretation() {
    let corpus = corpus();
    let responses = repository_responses(&corpus);
    let report = evaluate(&corpus, Some(&responses)).expect("report");
    let fake = report.run(ExecutionKind::Fake).expect("fake");

    let outage = fake
        .cases
        .iter()
        .find(|case| case.case_id == "adversarial/transport-unavailable")
        .expect("outage scenario");
    assert!(outage.candidate_defects.is_empty());
    assert!(outage.authoritative_defects.is_empty());
    assert!(outage.authoritative_state.raw_capture_intact);
    assert!(outage.authoritative_state.is_active());
    assert_eq!(outage.authoritative_state.item_type, None);
}

#[test]
fn deterministic_cases_the_fast_path_does_not_handle_are_neither_scored_nor_passed() {
    let corpus = corpus();
    let report = evaluate(&corpus, None).expect("report");
    let run = report.run(ExecutionKind::Deterministic).expect("run");

    let not_handled = run.totals.producer.get("not_handled").copied().unwrap_or(0);
    assert!(not_handled > 0);
    assert_eq!(
        run.totals.cases,
        run.totals.producer.values().sum::<u64>(),
        "every case has exactly one producer outcome"
    );
    assert!(run.totals.not_handled_where_oracle_has_facets > 0);
    assert_eq!(
        run.totals.authoritative_defects.values().sum::<u64>(),
        0,
        "an unhandled case is reported as not handled, not as a defect"
    );
}

#[test]
fn the_report_distinguishes_every_execution_kind_by_name() {
    let corpus = corpus();
    let responses = repository_responses(&corpus);
    let report: Report = evaluate(&corpus, Some(&responses)).expect("report");
    let value: Value = serde_json::from_str(&report.to_json()).unwrap();
    let executions: Vec<&str> = value["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["execution"].as_str().unwrap())
        .collect();
    assert_eq!(executions, ["deterministic", "fake", "recorded", "live"]);
}

#[test]
fn a_change_request_against_an_existing_item_is_a_false_completion_defect() {
    use evaluation::score::{defects_against, FacetView, RequestedOperation, Stage};
    let corpus = corpus();
    let fixture = corpus.fixture("already-completed-action").expect("fixture");
    let view = FacetView {
        requested_operation: Some(RequestedOperation::Update),
        ..FacetView::default()
    };
    let defects = defects_against(&fixture.expected, &view, Stage::Candidate, None, false);
    assert!(defects
        .iter()
        .any(|defect| defect.kind == DefectKind::FalseCompletion));
    assert!(defects
        .iter()
        .all(|defect| defect.kind != DefectKind::FalseAction
            && defect.kind != DefectKind::FalseDeadline));
}
