//! Execution of the corpus through the pipeline, and assembly of the report.

use ohand_core::interpretation::contracts::Proposal;
use ohand_core::interpretation::fast_path::recognize_with_session_topic;
use ohand_core::interpretation::instructions::{InterpretationMapping, M1_INSTRUCTION_VERSION};
use ohand_core::providers::contracts::fake::{FakeProvider, FakeStep};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, FailureKind, InterpretationRequest,
    ManualClock, ProviderCapability, ProviderFailure, ProviderProfile, ProviderProfileBuilder,
    ProviderProtocol, StructuredOutputMode,
};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::corpus::{CaseContext, Corpus, Fixture};
use crate::report::{
    tally_with_category, CaseReport, CorpusInfo, ExecutionKind, Gate, ProducerOutcome, Report,
    ResponsesInfo, RunReport, RunStatus, Totals, CORPUS_PATH, NOTICES, REPORT_FORMAT_VERSION,
};
use crate::responses::{Behavior, Provenance, Responses, Scenario};
use crate::score::{
    abstention_outcome, defects_against, forbidden_hits, invariant_hits, FacetView,
    RequestedOperation, Stage,
};
use crate::scratch::{AuthoritativeState, GuardOutcome, JobBinding, ScratchCase, Submission};
use crate::EvaluationError;

pub const GATE_NAME: &str = "zero-forbidden-authoritative-mutations";
const ROUTE_NAME: &str = "evaluation-route";
const PROFILE_ID: &str = "evaluation-synthetic-profile";
const LIVE_UNAVAILABLE_REASON: &str =
    "this tool makes no live provider call; live quality is not inferred from fake or recorded runs";

/// Run everything and assemble the report. `responses` is optional: without it only the
/// deterministic run is executed and the fake and recorded runs are reported as unavailable.
pub fn evaluate(corpus: &Corpus, responses: Option<&Responses>) -> Result<Report, EvaluationError> {
    let mut runs = vec![deterministic_run(corpus)?];

    let (fake, recorded) = match responses {
        Some(responses) => {
            let mut fake_cases = Vec::new();
            let mut recorded_cases = Vec::new();
            for scenario in &responses.scenarios {
                let case = scenario_case(corpus, scenario)?;
                match scenario.provenance {
                    Provenance::SyntheticAuthored => fake_cases.push((scenario, case)),
                    Provenance::LiveRun { .. } => recorded_cases.push((scenario, case)),
                }
            }
            (
                scenario_run(corpus, ExecutionKind::Fake, fake_cases),
                scenario_run(corpus, ExecutionKind::Recorded, recorded_cases),
            )
        }
        None => (None, None),
    };
    runs.push(fake.unwrap_or_else(|| {
        unavailable_run(
            ExecutionKind::Fake,
            "no recorded-response file was supplied, or it holds no synthetic scenarios",
        )
    }));
    runs.push(recorded.unwrap_or_else(|| {
        unavailable_run(
            ExecutionKind::Recorded,
            "no scenario carries live_run provenance, so there is nothing recorded to replay",
        )
    }));
    runs.push(unavailable_run(
        ExecutionKind::Live,
        LIVE_UNAVAILABLE_REASON,
    ));

    let gate = gate_for(&runs);
    Ok(Report {
        report_format_version: REPORT_FORMAT_VERSION,
        instruction_version: M1_INSTRUCTION_VERSION.to_string(),
        corpus: CorpusInfo {
            path: CORPUS_PATH,
            version: corpus.version.clone(),
            fixtures: corpus.fixtures.len() as u64,
            provenance: "synthetic",
        },
        responses: responses.map(|responses| ResponsesInfo {
            version: responses.version.clone(),
            scenarios: responses.scenarios.len() as u64,
            synthetic_authored: responses
                .scenarios
                .iter()
                .filter(|scenario| matches!(scenario.provenance, Provenance::SyntheticAuthored))
                .count() as u64,
            live_run: responses
                .scenarios
                .iter()
                .filter(|scenario| matches!(scenario.provenance, Provenance::LiveRun { .. }))
                .count() as u64,
        }),
        runs,
        gate,
        notices: NOTICES.to_vec(),
    })
}

fn unavailable_run(execution: ExecutionKind, reason: &str) -> RunReport {
    RunReport {
        execution,
        status: RunStatus::Unavailable {
            reason: reason.to_string(),
        },
        totals: Totals::empty(),
        by_category: BTreeMap::new(),
        cases: Vec::new(),
    }
}

fn gate_for(runs: &[RunReport]) -> Gate {
    let mut failures = Vec::new();
    let mut by_execution = BTreeMap::new();
    let mut unavailable = Vec::new();
    let mut total = 0;
    for run in runs {
        match &run.status {
            RunStatus::Unavailable { .. } => unavailable.push(run.execution),
            RunStatus::Executed => {
                let count = run.totals.authoritative_forbidden_total();
                by_execution.insert(run.execution, count);
                total += count;
                if count > 0 {
                    failures.push(format!(
                        "{count} forbidden authoritative mutation(s) in the {} run",
                        format!("{:?}", run.execution).to_lowercase()
                    ));
                }
                if run.totals.cases == 0 {
                    failures.push("an executed run evaluated no cases".to_string());
                }
            }
        }
    }
    if !by_execution.contains_key(&ExecutionKind::Deterministic) {
        failures.push("the deterministic run did not execute".to_string());
    }
    Gate {
        name: GATE_NAME,
        passed: failures.is_empty(),
        forbidden_authoritative_mutations: total,
        by_execution,
        failures,
        unavailable,
    }
}

fn deterministic_run(corpus: &Corpus) -> Result<RunReport, EvaluationError> {
    let mut totals = Totals::empty();
    let mut by_category = BTreeMap::new();
    let mut cases = Vec::new();
    for fixture in &corpus.fixtures {
        let context = fixture.case_context()?;
        let mut scratch = ScratchCase::open(&format!("deterministic/{}", fixture.id), &context)?;
        let binding = scratch.binding().clone();
        let proposal = recognize_with_session_topic(
            &binding.basis_text,
            &binding.item_id,
            &binding.capture_id,
            binding.source_revision,
            binding.text_basis.clone(),
            &binding.request_version,
            &binding.time_context,
        )
        .map(|mut proposal| {
            proposal.proposal_id = binding.proposal_id.clone();
            proposal
        });
        let producer_submission = match proposal {
            Some(proposal) => Produced::Proposal(Box::new(proposal)),
            None => Produced::NotHandled,
        };
        let case = finish_case(
            fixture,
            fixture.id.clone(),
            None,
            &mut scratch,
            producer_submission,
        )?;
        tally_with_category(&mut totals, &mut by_category, &case, &fixture.expected);
        cases.push(case);
    }
    Ok(RunReport {
        execution: ExecutionKind::Deterministic,
        status: RunStatus::Executed,
        totals,
        by_category,
        cases,
    })
}

fn scenario_run(
    corpus: &Corpus,
    execution: ExecutionKind,
    cases: Vec<(&Scenario, CaseReport)>,
) -> Option<RunReport> {
    if cases.is_empty() {
        return None;
    }
    let mut totals = Totals::empty();
    let mut by_category = BTreeMap::new();
    let mut reports = Vec::new();
    for (scenario, case) in cases {
        let fixture = corpus
            .fixture(&scenario.fixture_id)
            .expect("fixture ids are validated when responses load");
        tally_with_category(&mut totals, &mut by_category, &case, &fixture.expected);
        reports.push(case);
    }
    Some(RunReport {
        execution,
        status: RunStatus::Executed,
        totals,
        by_category,
        cases: reports,
    })
}

fn scenario_case(corpus: &Corpus, scenario: &Scenario) -> Result<CaseReport, EvaluationError> {
    let fixture = corpus
        .fixture(&scenario.fixture_id)
        .expect("fixture ids are validated when responses load");
    let context = fixture.case_context()?;
    let mut scratch = ScratchCase::open(&format!("scenario/{}", scenario.scenario_id), &context)?;
    let produced = produce_from_scenario(scenario, &context, scratch.binding())?;
    finish_case(
        fixture,
        scenario.scenario_id.clone(),
        Some(scenario),
        &mut scratch,
        produced,
    )
}

enum Produced {
    NotHandled,
    Proposal(Box<Proposal>),
    ProviderFailure(ProviderFailure),
    MappingRejected {
        error: String,
        requested_operation: Option<RequestedOperation>,
    },
}

fn profile() -> Result<ProviderProfile, EvaluationError> {
    ProviderProfileBuilder::new(PROFILE_ID, ProviderProtocol::SelfHosted, "synthetic-model")
        .endpoint("https://evaluation.example.test/v1/interpret")
        .credential_ref("evaluation-credential-reference")
        .timeout_seconds(30)
        .authorized_destination("https://evaluation.example.test")
        .capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "evaluation:scripted",
            )
            .with_input_size_limit(1_000_000)
            .with_structured_output(StructuredOutputMode::JsonObject),
        )
        .build()
        .map_err(|error| EvaluationError::Setup(error.to_string()))
}

/// Send the scenario's scripted reply through the real dispatch and instruction mapping.
fn produce_from_scenario(
    scenario: &Scenario,
    _context: &CaseContext,
    binding: &JobBinding,
) -> Result<Produced, EvaluationError> {
    let profile = profile()?;
    let request = InterpretationRequest::new(
        binding.capture_id.clone(),
        u64::try_from(binding.source_revision)
            .map_err(|error| EvaluationError::Setup(error.to_string()))?,
        binding.text_basis.clone(),
        binding.basis_text.clone(),
        binding.request_version.clone(),
        M1_INSTRUCTION_VERSION,
        &profile,
        ROUTE_NAME,
        binding.time_context.clone(),
    )
    .map_err(|error| EvaluationError::Setup(error.to_string()))?;

    let step = match &scenario.behavior {
        Behavior::Respond { body } => FakeStep::respond(
            serde_json::to_vec(body).map_err(|error| EvaluationError::Setup(error.to_string()))?,
        ),
        Behavior::RespondRaw { body } => FakeStep::respond(body.as_bytes().to_vec()),
        Behavior::TransportError { error } => FakeStep::fail(*error),
    };
    let clock = Arc::new(ManualClock::new());
    let adapter = FakeProvider::new(clock.clone(), [step]);
    let output = match dispatch(
        &adapter,
        &profile,
        &request,
        clock.as_ref(),
        &CancelToken::new(),
        &DispatchLimits::default(),
    ) {
        Ok(output) => output,
        Err(failure) => return Ok(Produced::ProviderFailure(failure)),
    };
    let mapping = InterpretationMapping::new(
        &request,
        binding.item_id.clone(),
        binding.proposal_id.clone(),
    )
    .map_err(|error| EvaluationError::Setup(error.to_string()))?;
    Ok(match mapping.map_output(&output) {
        Ok(proposal) => Produced::Proposal(Box::new(proposal)),
        Err(error) => Produced::MappingRejected {
            error: error.to_string(),
            requested_operation: requested_operation_of(&output.proposal),
        },
    })
}

/// What a rejected reply asked for, read from the untrusted reply itself: the mapping refuses
/// update and create operations, but the attempt is still a finding to report.
fn requested_operation_of(
    reply: &serde_json::Map<String, serde_json::Value>,
) -> Option<RequestedOperation> {
    match reply.get("operation")?.get("kind")?.as_str()? {
        "update" => Some(RequestedOperation::Update),
        "create" => Some(RequestedOperation::Create),
        _ => None,
    }
}

fn finish_case(
    fixture: &Fixture,
    case_id: String,
    scenario: Option<&Scenario>,
    scratch: &mut ScratchCase,
    produced: Produced,
) -> Result<CaseReport, EvaluationError> {
    let (producer, submission, candidate_view, candidate_abstention) = match produced {
        Produced::NotHandled => (ProducerOutcome::NotHandled, None, None, None),
        Produced::Proposal(proposal) => {
            let producer = if proposal.abstention.is_some() {
                ProducerOutcome::Abstained
            } else {
                ProducerOutcome::Proposed
            };
            let view = FacetView::of_proposal(&proposal);
            let abstention = proposal.abstention.clone();
            (
                producer,
                Some(Submission::Proposal(proposal)),
                Some(view),
                abstention,
            )
        }
        Produced::ProviderFailure(failure) => (
            ProducerOutcome::ProviderFailure {
                kind: format!("{:?}", failure.kind),
            },
            Some(Submission::Failure(failure)),
            None,
            None,
        ),
        Produced::MappingRejected {
            error,
            requested_operation,
        } => (
            ProducerOutcome::MappingRejected {
                error,
                requested_operation,
            },
            Some(Submission::Failure(ProviderFailure::new(
                FailureKind::InvalidOutput,
            ))),
            requested_operation.map(|requested_operation| FacetView {
                requested_operation: Some(requested_operation),
                ..FacetView::default()
            }),
            None,
        ),
    };

    let (guard, state): (Option<GuardOutcome>, AuthoritativeState) = match &submission {
        Some(submission) => {
            let (outcome, state) = scratch.submit(submission)?;
            (Some(outcome), state)
        }
        None => (None, scratch.untouched_state()?),
    };

    let abstained = candidate_abstention.is_some();
    let empty = FacetView::default();
    let candidate = candidate_view.as_ref().unwrap_or(&empty);
    let candidate_defects = defects_against(
        &fixture.expected,
        candidate,
        Stage::Candidate,
        None,
        abstained || producer != ProducerOutcome::Proposed,
    );
    let authoritative_view = match candidate_view.as_ref() {
        Some(candidate_view) => FacetView::of_state(&state).with_evidence_of(candidate_view),
        None => FacetView::of_state(&state),
    };
    let authoritative_defects = defects_against(
        &fixture.expected,
        &authoritative_view,
        Stage::Authoritative,
        candidate_view.as_ref(),
        false,
    );
    let abstention = match &producer {
        ProducerOutcome::Proposed | ProducerOutcome::Abstained => {
            abstention_outcome(&fixture.expected, candidate_abstention.as_ref())
        }
        _ => None,
    };
    let candidate_forbidden = forbidden_hits(&fixture.forbidden, candidate, Stage::Candidate);
    let mut authoritative_forbidden = forbidden_hits(
        &fixture.forbidden,
        &authoritative_view,
        Stage::Authoritative,
    );
    for hit in invariant_hits(&state) {
        if !authoritative_forbidden.contains(&hit) {
            authoritative_forbidden.push(hit);
        }
    }

    Ok(CaseReport {
        case_id,
        fixture_id: fixture.id.clone(),
        category: fixture.category,
        role: scenario.map(|scenario| scenario.role),
        description: scenario.map(|scenario| scenario.description.clone()),
        producer,
        guard,
        candidate_defects,
        authoritative_defects,
        abstention,
        candidate_forbidden,
        authoritative_forbidden,
        authoritative_state: state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::MutationClass;
    use std::path::PathBuf;

    fn corpus() -> Corpus {
        Corpus::load(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(CORPUS_PATH),
        )
        .expect("the synthetic corpus loads")
    }

    fn plant_second_item(scratch: &ScratchCase, label: &str) {
        let connection = scratch.connection_for_negative_control();
        connection
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, \
                 created_at) VALUES (?, 'planted', '2026-01-01T00:00:00Z', 'UTC', 0, 'en-US', \
                 'gregorian', 'personal', 'route-1', 0, '2026-01-01T00:00:00Z')",
                [format!("capture-{label}")],
            )
            .expect("plant capture");
        connection
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
                 'not_applicable', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [format!("item-{label}"), format!("capture-{label}")],
            )
            .expect("plant item");
    }

    fn run_with(fixture: &Fixture, plant: bool) -> (CaseReport, Totals) {
        let context = fixture.case_context().expect("context");
        let mut scratch = ScratchCase::open("negative-control", &context).expect("scratch");
        if plant {
            plant_second_item(&scratch, "stray");
        }
        let case = finish_case(
            fixture,
            "control".into(),
            None,
            &mut scratch,
            Produced::NotHandled,
        )
        .expect("case");
        let mut totals = Totals::empty();
        let mut by_category = BTreeMap::new();
        tally_with_category(&mut totals, &mut by_category, &case, &fixture.expected);
        (case, totals)
    }

    #[test]
    fn an_extra_item_in_durable_state_is_an_authoritative_operation_hit_that_fails_the_gate() {
        let corpus = corpus();
        let fixture = corpus.fixture("already-completed-action").expect("fixture");

        let (clean, clean_totals) = run_with(fixture, false);
        assert!(clean.authoritative_state.foreign_records.is_empty());
        assert_eq!(clean_totals.authoritative_forbidden_total(), 0);

        let (planted, totals) = run_with(fixture, true);
        assert_eq!(planted.authoritative_state.foreign_records["items"], 1);
        assert_eq!(planted.authoritative_state.foreign_records["captures"], 1);
        assert!(planted
            .authoritative_forbidden
            .iter()
            .any(|hit| hit.class == MutationClass::Operation));
        assert_eq!(totals.authoritative_forbidden[&MutationClass::Operation], 1);

        let gate = gate_for(&[RunReport {
            execution: ExecutionKind::Deterministic,
            status: RunStatus::Executed,
            totals,
            by_category: BTreeMap::new(),
            cases: vec![planted],
        }]);
        assert!(!gate.passed);
        assert_eq!(gate.forbidden_authoritative_mutations, 1);
    }
}
