use super::arm::score_in_scratch;
use super::*;
use crate::corpus::Corpus;
use crate::report::ProducerOutcome;
use crate::responses::{Behavior, Responses};
use crate::score::{AbstentionOutcome, DefectKind, MutationClass};
use crate::scratch::ScratchCase;
use serde_json::{json, Map, Value};
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

/// The reply of a recorded synthetic scenario, as a completed arm.
fn scripted(scenario_id: &str) -> ArmOutcome {
    let corpus = corpus();
    let responses = Responses::load(
        &repository_path("tools/evaluation/fixtures/recorded-responses.json"),
        &corpus,
    )
    .expect("the recorded responses load");
    let scenario = responses
        .scenarios
        .iter()
        .find(|scenario| scenario.scenario_id == scenario_id)
        .unwrap_or_else(|| panic!("no scenario {scenario_id}"));
    match &scenario.behavior {
        Behavior::Respond { body } => ArmOutcome::completed(object(body.clone())),
        other => panic!("scenario {scenario_id} is not a reply: {other:?}"),
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn none(kind: NoResponseKind, reason: &str) -> ArmOutcome {
    ArmOutcome::no_response(kind, reason).expect("valid reason")
}

fn pair(fixture_id: &str, arm_a: ArmOutcome, arm_b: ArmOutcome) -> PairedOutcome {
    PairedOutcome {
        fixture_id: fixture_id.to_string(),
        arm_a,
        arm_b,
    }
}

fn input(eligible: &[&str], outcomes: Vec<PairedOutcome>) -> ComparisonInput {
    ComparisonInput {
        experiment_id: Label::new("experiment-0001").unwrap(),
        execution: ExecutionKind::Fake,
        arm_profiles: [
            Label::new("profile-a.v1").unwrap(),
            Label::new("profile-b.v1").unwrap(),
        ],
        eligible_fixture_ids: eligible.iter().map(|id| id.to_string()).collect(),
        outcomes,
    }
}

fn score(input: &ComparisonInput) -> ComparisonReport {
    score_comparison(&corpus(), input).expect("comparison scores")
}

fn assert_partition(report: &ComparisonReport) {
    let pairs = &report.pairs;
    assert_eq!(pairs.matched + pairs.failed + pairs.missing, pairs.eligible);
    assert_eq!(report.cases.len() as u64, pairs.eligible);
    for arm in &report.arms {
        let sides = &arm.sides;
        assert_eq!(
            sides.completed
                + sides.failed
                + sides.timed_out
                + sides.cancelled
                + sides.unknown
                + sides.skipped
                + sides.missing,
            sides.eligible
        );
        assert_eq!(arm.totals.cases, sides.completed);
    }
}

#[test]
fn two_conforming_arms_are_matched_agree_and_both_clean() {
    let report = score(&input(
        &["design-broad-intention", "design-undated-action"],
        vec![
            pair(
                "design-broad-intention",
                scripted("conforming/design-broad-intention"),
                scripted("conforming/design-broad-intention"),
            ),
            pair(
                "design-undated-action",
                scripted("conforming/design-undated-action"),
                scripted("conforming/design-undated-action"),
            ),
        ],
    ));

    assert_partition(&report);
    assert_eq!(report.pairs.eligible, 2);
    assert_eq!(report.pairs.matched, 2);
    assert_eq!(report.disagreement.agree, 2);
    assert_eq!(report.disagreement.differ, 0);
    assert_eq!(report.paired_objective.both_clean, 2);
    for arm in &report.arms {
        assert_eq!(arm.totals.cases, 2);
        assert_eq!(arm.totals.authoritative_forbidden_total(), 0);
        assert!(arm.totals.candidate_defects.values().all(|n| *n == 0));
        assert!(arm.totals.authoritative_defects.values().all(|n| *n == 0));
    }
    assert!(matches!(report.preference, PreferenceSection::NotCollected));
}

#[test]
fn a_wrong_deadline_on_one_arm_is_that_arms_defect_and_a_separate_disagreement() {
    let fixture = "design-explicit-reminder";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            scripted("conforming/design-explicit-reminder"),
            scripted("adversarial/wrong-instant"),
        )],
    ));

    assert_partition(&report);
    let (a, b) = (&report.arms[0], &report.arms[1]);
    assert!(a.totals.candidate_defects.values().all(|n| *n == 0));
    assert_eq!(b.totals.candidate_defects[&DefectKind::FalseDeadline], 1);
    // The guard did not persist the unsupported instant, so no authoritative false deadline.
    assert_eq!(
        b.totals.authoritative_defects[&DefectKind::FalseDeadline],
        0
    );
    assert_eq!(b.totals.authoritative_forbidden_total(), 0);

    let case = &report.cases[0];
    assert_eq!(
        case.disagreement,
        Disagreement::Differ {
            facets: vec![DisagreementFacet::Reminder]
        }
    );
    assert_eq!(
        report.disagreement.by_facet[&DisagreementFacet::Reminder],
        1
    );
    // The guard drops the instant for both arms, so neither is objectively clean even though
    // only B proposed a wrong deadline: objective cleanliness is not disagreement.
    assert_eq!(report.paired_objective.neither_clean, 1);
    assert_eq!(report.paired_objective.only_b_clean, 0);
}

#[test]
fn disagreement_without_a_defect_is_not_an_objective_difference() {
    // Both are valid, defect-free proposals for the fixture, yet they differ in item type.
    let fixture = "design-broad-intention";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            scripted("conforming/design-broad-intention"),
            ArmOutcome::completed(object(json!({
                "operation": { "kind": "annotate" },
                "item_type": "broad_intention",
                "source_spans": [{ "start": 0, "end": 33 }]
            }))),
        )],
    ));
    let case = &report.cases[0];
    assert!(matches!(
        case.disagreement,
        Disagreement::Differ { ref facets } if facets == &[DisagreementFacet::ItemType]
    ));
    let defects = |arm: usize| {
        report.arms[arm]
            .totals
            .candidate_defects
            .values()
            .sum::<u64>()
    };
    // The oracle decides correctness; disagreement alone does not make either arm wrong.
    assert_eq!(report.disagreement.differ, 1);
    assert_eq!(
        report.paired_objective.both_clean
            + report.paired_objective.neither_clean
            + report.paired_objective.only_a_clean
            + report.paired_objective.only_b_clean,
        1
    );
    assert_eq!(defects(0) > 0, !case.objective.unwrap().a_clean);
    assert_eq!(defects(1) > 0, !case.objective.unwrap().b_clean);
}

#[test]
fn false_actions_missed_intent_and_abstentions_are_separate_counts() {
    // The oracle has an idea with no action: A calls it an action, B abstains.
    let fixture = "design-broad-intention";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            ArmOutcome::completed(object(json!({
                "operation": { "kind": "annotate" },
                "item_type": "action",
                "source_spans": [{ "start": 0, "end": 33 }]
            }))),
            ArmOutcome::completed(object(json!({
                "operation": { "kind": "annotate" },
                "abstention": "UncertainTarget"
            }))),
        )],
    ));
    let (a, b) = (&report.arms[0], &report.arms[1]);
    assert_eq!(a.totals.candidate_defects[&DefectKind::FalseAction], 1);
    assert_eq!(a.totals.abstention[&AbstentionOutcome::Unexpected], 0);
    assert_eq!(b.totals.candidate_defects[&DefectKind::FalseAction], 0);
    assert_eq!(b.totals.abstention[&AbstentionOutcome::Unexpected], 1);
    assert_eq!(report.paired_objective.neither_clean, 1);
}

#[test]
fn an_arm_without_a_response_is_never_a_passing_abstention() {
    // The oracle abstains here, so a correct abstention is possible and must be earned.
    let fixture = "negation-do-not-remind";
    let kinds = [
        NoResponseKind::Failed,
        NoResponseKind::TimedOut,
        NoResponseKind::Cancelled,
        NoResponseKind::Unknown,
        NoResponseKind::Skipped,
        NoResponseKind::Missing,
    ];
    for kind in kinds {
        let report = score(&input(
            &[fixture],
            vec![pair(
                fixture,
                scripted("conforming/negation-do-not-remind"),
                none(kind, "provider_gave_nothing"),
            )],
        ));
        assert_partition(&report);
        let (a, b) = (&report.arms[0], &report.arms[1]);
        assert_eq!(a.totals.abstention[&AbstentionOutcome::Correct], 1);
        assert_eq!(b.totals.cases, 0, "{kind:?} was scored");
        assert!(
            b.totals.abstention.values().all(|n| *n == 0),
            "{kind:?} became an abstention outcome"
        );
        assert_eq!(report.pairs.matched, 0);
        assert_eq!(report.pairs.failed + report.pairs.missing, 1);
        assert_eq!(report.pairs.failed == 1, kind.is_failure());
        assert_eq!(report.cases[0].disagreement, Disagreement::NotComparable);
        assert!(report.cases[0].objective.is_none());
        assert_eq!(report.paired_objective.matched_pairs, 0);
        assert!(b.sides.reasons.values().sum::<u64>() == 1);
    }
}

#[test]
fn invalid_output_is_counted_apart_and_is_not_a_correct_abstention() {
    let fixture = "negation-do-not-remind";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            scripted("conforming/negation-do-not-remind"),
            ArmOutcome::completed(object(json!({
                "abstention": "Negated",
                "operation": { "kind": "annotate" },
                "item_id": "11111111-1111-4111-8111-111111111111"
            }))),
        )],
    ));
    assert_partition(&report);
    let b = &report.arms[1];
    assert_eq!(report.pairs.matched, 1);
    assert_eq!(b.invalid_outputs.mapping_rejected, 1);
    assert_eq!(b.totals.abstention[&AbstentionOutcome::Correct], 0);
    assert!(matches!(b.totals.producer.get("mapping_rejected"), Some(1)));
    assert_eq!(report.arms[0].invalid_outputs.mapping_rejected, 0);
    assert_eq!(report.cases[0].disagreement, Disagreement::NotComparable);
    assert!(!report.cases[0].objective.unwrap().b_clean);
    assert_eq!(report.paired_objective.only_a_clean, 1);
}

#[test]
fn a_rejected_update_request_is_a_candidate_false_completion_with_no_authoritative_change() {
    let fixture = "design-unsupported-spoken-update";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            scripted("conforming/design-unsupported-spoken-update"),
            scripted("adversarial/update-existing-item"),
        )],
    ));
    let b = &report.arms[1];
    assert_eq!(b.totals.candidate_defects[&DefectKind::FalseCompletion], 1);
    assert_eq!(
        b.totals.authoritative_defects[&DefectKind::FalseCompletion],
        0
    );
    assert_eq!(b.totals.authoritative_forbidden_total(), 0);
    assert_eq!(b.totals.candidate_forbidden[&MutationClass::Operation], 1);
    assert_eq!(b.invalid_outputs.mapping_rejected, 1);
    match &report.cases[0].sides[1].scored.as_ref().unwrap().producer {
        ProducerOutcome::MappingRejected {
            requested_operation,
            ..
        } => assert!(requested_operation.is_some()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn eligible_matched_failed_and_missing_counts_partition_with_reasons() {
    let eligible = [
        "design-explicit-reminder",
        "design-undated-action",
        "design-broad-intention",
        "design-session-topic",
        "negation-do-not-remind",
    ];
    let report = score(&input(
        &eligible,
        vec![
            pair(
                "design-explicit-reminder",
                scripted("conforming/design-explicit-reminder"),
                scripted("conforming/design-explicit-reminder"),
            ),
            pair(
                "design-undated-action",
                scripted("conforming/design-undated-action"),
                none(NoResponseKind::TimedOut, "request_timed_out"),
            ),
            pair(
                "design-broad-intention",
                none(NoResponseKind::Skipped, "not_authorized"),
                scripted("conforming/design-broad-intention"),
            ),
            pair(
                "design-session-topic",
                none(NoResponseKind::Unknown, "reservation_without_outcome"),
                none(NoResponseKind::Failed, "provider_error"),
            ),
            // negation-do-not-remind has no outcome record at all.
        ],
    ));
    assert_partition(&report);
    assert_eq!(report.pairs.eligible, 5);
    assert_eq!(report.pairs.matched, 1);
    assert_eq!(report.pairs.failed, 2);
    assert_eq!(report.pairs.missing, 2);
    assert_eq!(report.pairs.no_record, 1);
    assert_eq!(report.corpus.eligible, 5);
    assert!(report.corpus.fixtures > 5);

    let (a, b) = (&report.arms[0], &report.arms[1]);
    assert_eq!(a.sides.eligible, 5);
    assert_eq!(a.sides.completed, 2);
    assert_eq!(a.sides.skipped, 1);
    assert_eq!(a.sides.unknown, 1);
    assert_eq!(a.sides.missing, 1);
    assert_eq!(b.sides.timed_out, 1);
    assert_eq!(b.sides.failed, 1);
    assert_eq!(b.sides.missing, 1);
    assert_eq!(a.sides.reasons["skipped/not_authorized"], 1);
    assert_eq!(a.sides.reasons["missing/no_record"], 1);
    assert_eq!(b.sides.reasons["timed_out/request_timed_out"], 1);
    assert_eq!(report.disagreement.matched_pairs, 1);
    assert_eq!(report.paired_objective.matched_pairs, 1);
}

#[test]
fn each_arm_is_scored_in_a_scratch_store_of_its_own() {
    let corpus = corpus();
    let fixture = corpus.fixture("already-completed-action").expect("fixture");
    let context = fixture.case_context().expect("context");
    let reply = match scripted("conforming/already-completed-action") {
        ArmOutcome::Completed { output } => output,
        other => panic!("{other:?}"),
    };

    let mut arm_a = ScratchCase::open("comparison/test/a", &context).expect("scratch a");
    let mut arm_b = ScratchCase::open("comparison/test/b", &context).expect("scratch b");
    arm_a
        .connection_for_negative_control()
        .execute(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
             utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, \
             created_at) VALUES ('stray-capture', 'planted', '2026-01-01T00:00:00Z', 'UTC', 0, \
             'en-US', 'gregorian', 'personal', 'route-1', 0, '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("plant a row only arm A's guard could have written");
    arm_a
        .connection_for_negative_control()
        .execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES ('stray-item', 'stray-capture', 0, 'active', 'saved_local', \
             'not_configured', 'unprocessed', 'not_applicable', '2026-01-01T00:00:00Z', \
             '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("plant item");

    let scored_a = score_in_scratch(fixture, "a".into(), &mut arm_a, &reply).expect("a");
    let scored_b = score_in_scratch(fixture, "b".into(), &mut arm_b, &reply).expect("b");

    assert_eq!(
        scored_a.report.authoritative_state.foreign_records["items"],
        1
    );
    assert!(scored_a
        .report
        .authoritative_forbidden
        .iter()
        .any(|hit| hit.class == MutationClass::Operation));
    assert!(scored_b
        .report
        .authoritative_state
        .foreign_records
        .is_empty());
    assert!(scored_b.report.authoritative_forbidden.is_empty());
}

#[test]
fn model_chosen_text_does_not_reach_the_report() {
    let fixture = "design-session-topic";
    let report = score(&input(
        &[fixture],
        vec![pair(
            fixture,
            ArmOutcome::completed(object(json!({
                "operation": { "kind": "annotate" },
                "session_topic_proposal": {
                    "topic": "https://leak.example/secret-topic",
                    "source_span": { "start": 17, "end": 24 }
                }
            }))),
            ArmOutcome::completed(object(json!({
                "operation": { "kind": "annotate" },
                "https://leak.example/field": 1
            }))),
        )],
    ));
    let json = report.to_json();
    assert!(!json.contains("leak.example"), "{json}");
    assert!(!json.contains("secret-topic"));
    let a = &report.arms[0];
    assert_eq!(a.totals.candidate_defects[&DefectKind::UnsupportedClaim], 1);
    assert_eq!(report.arms[1].invalid_outputs.mapping_rejected, 1);
}

#[test]
fn the_report_is_deterministic_and_labels_its_execution_honestly() {
    let build = || {
        input(
            &["design-explicit-reminder"],
            vec![pair(
                "design-explicit-reminder",
                scripted("conforming/design-explicit-reminder"),
                scripted("adversarial/wrong-instant"),
            )],
        )
    };
    let first = score(&build()).to_json();
    let second = score(&build()).to_json();
    assert_eq!(first, second);

    let value: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(value["execution"], "fake");
    assert_eq!(value["preference"]["status"], "not_collected");
    assert_eq!(value["corpus"]["provenance"], "synthetic");
    assert!(first.contains("say nothing about live provider quality"));
}

#[test]
fn invalid_comparison_setup_is_rejected_rather_than_reported() {
    let corpus = corpus();
    let reply = || scripted("conforming/design-undated-action");
    let fixture = "design-undated-action";
    let reject =
        |input: ComparisonInput| score_comparison(&corpus, &input).expect_err("setup is rejected");

    let mut deterministic = input(&[fixture], vec![]);
    deterministic.execution = ExecutionKind::Deterministic;
    assert!(matches!(reject(deterministic), EvaluationError::Setup(_)));
    assert!(matches!(
        reject(input(&[], vec![])),
        EvaluationError::Setup(_)
    ));
    assert!(matches!(
        reject(input(&["no-such-fixture"], vec![])),
        EvaluationError::Setup(_)
    ));
    assert!(matches!(
        reject(input(&[fixture, fixture], vec![])),
        EvaluationError::Setup(_)
    ));
    assert!(matches!(
        reject(input(
            &[fixture],
            vec![pair("design-session-topic", reply(), reply())]
        )),
        EvaluationError::Setup(_)
    ));
    assert!(matches!(
        reject(input(
            &[fixture],
            vec![
                pair(fixture, reply(), reply()),
                pair(fixture, reply(), reply())
            ]
        )),
        EvaluationError::Setup(_)
    ));
}

#[test]
fn reasons_and_labels_cannot_carry_urls_or_free_text() {
    assert!(ReasonCode::new("timed_out").is_ok());
    for bad in [
        "",
        "Timed Out",
        "https://host.example/x",
        "has-dash",
        "UPPER",
        &"x".repeat(ReasonCode::MAX_LEN + 1),
    ] {
        assert!(ReasonCode::new(bad).is_err(), "{bad:?}");
    }
    assert!(ArmOutcome::no_response(NoResponseKind::Failed, "sk-secret value").is_err());
    assert!(Label::new("profile-a.v1").is_ok());
    for bad in ["", "a b", "https://host.example", "a/b", "key=value"] {
        assert!(Label::new(bad).is_err(), "{bad:?}");
    }
}
