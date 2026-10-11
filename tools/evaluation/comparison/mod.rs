//! Paired scoring of two model arms with E01's semantic oracle.
//!
//! Each completed arm response goes through the production instruction mapping and application
//! guard in a scratch store of its own, and is compared with the fixture's expectations by the
//! same functions the single-run evaluation uses. Arms without a response are counted, with
//! reasons, and are never scored or treated as abstentions. The scorer takes data only: it makes
//! no provider call, opens no real store and applies nothing authoritative.

mod arm;
mod input;
mod pair;
mod report;
#[cfg(test)]
mod tests;

use ohand_core::interpretation::instructions::M1_INSTRUCTION_VERSION;
use std::collections::{BTreeMap, BTreeSet};

use crate::corpus::Corpus;
use crate::report::{tally_with_category, ExecutionKind, ProducerOutcome};
use crate::scratch::GuardOutcome;
use crate::EvaluationError;

pub use input::{ArmOutcome, ComparisonInput, Label, NoResponseKind, PairedOutcome, ReasonCode};
pub use pair::{Disagreement, DisagreementFacet};
pub use report::{
    ArmSummary, ComparisonReport, CorpusSummary, DisagreementSummary, InvalidOutputs, PairCounts,
    PairObjective, PairReport, PairedObjective, Pairing, PreferenceSection, SideCounts, SideReport,
    SideStatus, COMPARISON_REPORT_FORMAT_VERSION, NOTICES,
};

use arm::{score_arm, Candidate};
use input::{arm_key, ARMS};
use report::new_arm_summary;

/// Score every eligible pair. An outcome for a fixture that is not eligible, a repeated
/// outcome, or an unknown fixture is a setup error rather than something silently dropped.
pub fn score_comparison(
    corpus: &Corpus,
    input: &ComparisonInput,
) -> Result<ComparisonReport, EvaluationError> {
    if input.execution == ExecutionKind::Deterministic {
        return Err(EvaluationError::Setup(
            "a comparison scores model arms; the deterministic fast path is not one".into(),
        ));
    }
    if input.eligible_fixture_ids.is_empty() {
        return Err(EvaluationError::Setup(
            "a comparison needs at least one eligible fixture".into(),
        ));
    }
    let mut eligible = BTreeSet::new();
    for fixture_id in &input.eligible_fixture_ids {
        if corpus.fixture(fixture_id).is_none() {
            return Err(EvaluationError::Setup(format!(
                "eligible fixture {fixture_id:?} is not in the corpus"
            )));
        }
        if !eligible.insert(fixture_id.as_str()) {
            return Err(EvaluationError::Setup(format!(
                "eligible fixture {fixture_id:?} is listed twice"
            )));
        }
    }
    let mut outcomes: BTreeMap<&str, &PairedOutcome> = BTreeMap::new();
    for outcome in &input.outcomes {
        if !eligible.contains(outcome.fixture_id.as_str()) {
            return Err(EvaluationError::Setup(format!(
                "outcome for fixture {:?}, which is not eligible",
                outcome.fixture_id
            )));
        }
        if outcomes
            .insert(outcome.fixture_id.as_str(), outcome)
            .is_some()
        {
            return Err(EvaluationError::Setup(format!(
                "two outcomes for fixture {:?}",
                outcome.fixture_id
            )));
        }
    }

    let mut arms = [
        new_arm_summary(ARMS[0], input.arm_profiles[0].clone()),
        new_arm_summary(ARMS[1], input.arm_profiles[1].clone()),
    ];
    let mut pairs = PairCounts::default();
    let mut disagreement = report::DisagreementSummary::default();
    let mut paired_objective = PairedObjective::default();
    let mut cases = Vec::new();

    // Report in the caller's eligible order so the output is deterministic.
    for fixture_id in &input.eligible_fixture_ids {
        let fixture = corpus
            .fixture(fixture_id)
            .expect("eligible fixtures were checked against the corpus");
        let recorded = outcomes.get(fixture_id.as_str()).copied();
        pairs.eligible += 1;
        if recorded.is_none() {
            pairs.no_record += 1;
        }

        let mut sides = Vec::with_capacity(2);
        let mut candidates: [Option<Candidate>; 2] = [None, None];
        let mut any_failure = false;
        let mut any_without_response = false;
        for (index, arm) in ARMS.into_iter().enumerate() {
            let outcome = recorded.map(|recorded| recorded.arm(arm));
            let summary = &mut arms[index];
            let side = match outcome {
                Some(ArmOutcome::Completed { output }) => {
                    let store_label = format!(
                        "comparison/{}/{}/{}",
                        input.experiment_id.as_str(),
                        fixture.id,
                        arm_key(arm)
                    );
                    let scored = score_arm(
                        fixture,
                        &store_label,
                        format!("{}/{}", fixture.id, arm_key(arm)),
                        output,
                    )?;
                    summary.sides.record(SideStatus::Completed, None);
                    tally_with_category(
                        &mut summary.totals,
                        &mut summary.by_category,
                        &scored.report,
                        &fixture.expected,
                    );
                    if matches!(
                        scored.report.producer,
                        ProducerOutcome::MappingRejected { .. }
                    ) {
                        summary.invalid_outputs.mapping_rejected += 1;
                    }
                    if matches!(scored.report.guard, Some(GuardOutcome::Rejected { .. })) {
                        summary.invalid_outputs.guard_rejected += 1;
                    }
                    candidates[index] = scored.candidate;
                    SideReport {
                        arm: arm_key(arm),
                        status: SideStatus::Completed,
                        reason: None,
                        scored: Some(scored.report),
                    }
                }
                Some(ArmOutcome::NoResponse { kind, reason }) => {
                    any_without_response = true;
                    any_failure |= kind.is_failure();
                    summary.sides.record((*kind).into(), Some(reason));
                    SideReport {
                        arm: arm_key(arm),
                        status: (*kind).into(),
                        reason: Some(reason.clone()),
                        scored: None,
                    }
                }
                None => {
                    any_without_response = true;
                    let reason = ReasonCode::new("no_record")?;
                    summary.sides.record(SideStatus::Missing, Some(&reason));
                    SideReport {
                        arm: arm_key(arm),
                        status: SideStatus::Missing,
                        reason: Some(reason),
                        scored: None,
                    }
                }
            };
            sides.push(side);
        }

        let pairing = if !any_without_response {
            Pairing::Matched
        } else if any_failure {
            Pairing::Failed
        } else {
            Pairing::Missing
        };
        match pairing {
            Pairing::Matched => pairs.matched += 1,
            Pairing::Failed => pairs.failed += 1,
            Pairing::Missing => pairs.missing += 1,
        }

        let verdict = if pairing == Pairing::Matched {
            pair::disagreement(candidates[0].as_ref(), candidates[1].as_ref())
        } else {
            Disagreement::NotComparable
        };
        let objective = (pairing == Pairing::Matched).then(|| {
            let clean =
                |side: &SideReport| side.scored.as_ref().is_some_and(pair::objectively_clean);
            PairObjective {
                a_clean: clean(&sides[0]),
                b_clean: clean(&sides[1]),
            }
        });
        if pairing == Pairing::Matched {
            disagreement.matched_pairs += 1;
            match &verdict {
                Disagreement::Agree => {
                    disagreement.comparable += 1;
                    disagreement.agree += 1;
                }
                Disagreement::Differ { facets } => {
                    disagreement.comparable += 1;
                    disagreement.differ += 1;
                    for facet in facets {
                        *disagreement.by_facet.entry(*facet).or_default() += 1;
                    }
                }
                Disagreement::NotComparable => disagreement.not_comparable += 1,
            }
            paired_objective.matched_pairs += 1;
            let objective = objective.expect("a matched pair has an objective view");
            match (objective.a_clean, objective.b_clean) {
                (true, true) => paired_objective.both_clean += 1,
                (true, false) => paired_objective.only_a_clean += 1,
                (false, true) => paired_objective.only_b_clean += 1,
                (false, false) => paired_objective.neither_clean += 1,
            }
        }

        let [side_a, side_b]: [SideReport; 2] = sides
            .try_into()
            .expect("exactly two arms are reported per pair");
        cases.push(PairReport {
            fixture_id: fixture.id.clone(),
            category: fixture.category,
            pairing,
            sides: [side_a, side_b],
            disagreement: verdict,
            objective,
        });
    }

    Ok(ComparisonReport {
        report_format_version: COMPARISON_REPORT_FORMAT_VERSION,
        instruction_version: M1_INSTRUCTION_VERSION.to_string(),
        experiment_id: input.experiment_id.clone(),
        execution: input.execution,
        corpus: CorpusSummary {
            version: corpus.version.clone(),
            fixtures: corpus.fixtures.len() as u64,
            eligible: pairs.eligible,
            provenance: "synthetic",
        },
        arms,
        pairs,
        disagreement,
        paired_objective,
        preference: PreferenceSection::NotCollected,
        cases,
        notices: NOTICES.to_vec(),
    })
}
