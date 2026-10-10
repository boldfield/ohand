//! Scoring of one completed arm with E01's oracle, in an isolated scratch store of its own.
//!
//! The reply goes through the production instruction mapping and the production application
//! guard, and is compared with the fixture's `expected` and `forbidden` by the same functions
//! E01 uses. Only the orchestration is repeated here, because E01 keeps it private.

use ohand_core::interpretation::contracts::{AbstentionReason, Proposal};
use ohand_core::interpretation::instructions::{
    InstructionError, InterpretationMapping, M1_INSTRUCTION_VERSION,
};
use ohand_core::providers::contracts::{
    CapabilityMetadata, FailureKind, InterpretationOutput, InterpretationRequest,
    ProviderCapability, ProviderFailure, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    StructuredOutputMode,
};
use serde_json::{Map, Value};
use std::mem::Discriminant;

use crate::corpus::Fixture;
use crate::report::{CaseReport, ProducerOutcome};
use crate::score::{
    abstention_outcome, defects_against, forbidden_hits, invariant_hits, FacetView,
    RequestedOperation, Stage,
};
use crate::scratch::{AuthoritativeState, GuardOutcome, JobBinding, ScratchCase, Submission};
use crate::EvaluationError;

const ROUTE_NAME: &str = "comparison-route";
const PROFILE_ID: &str = "comparison-synthetic-profile";

/// What a valid candidate asserted, kept for the pair-level disagreement check.
#[derive(Debug, Clone)]
pub(super) struct Candidate {
    pub view: FacetView,
    pub abstention: Option<Discriminant<AbstentionReason>>,
}

pub(super) struct ScoredArm {
    pub report: CaseReport,
    /// Present only for a reply that mapped to a valid proposal.
    pub candidate: Option<Candidate>,
}

/// Open a fresh scratch store for `store_label` and score the reply in it.
pub(super) fn score_arm(
    fixture: &Fixture,
    store_label: &str,
    case_id: String,
    output: &Map<String, Value>,
) -> Result<ScoredArm, EvaluationError> {
    let context = fixture.case_context()?;
    let mut scratch = ScratchCase::open(store_label, &context)?;
    score_in_scratch(fixture, case_id, &mut scratch, output)
}

fn profile() -> Result<ProviderProfile, EvaluationError> {
    ProviderProfileBuilder::new(PROFILE_ID, ProviderProtocol::SelfHosted, "synthetic-model")
        .endpoint("https://comparison.example.test/v1/interpret")
        .credential_ref("comparison-credential-reference")
        .timeout_seconds(30)
        .authorized_destination("https://comparison.example.test")
        .capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "comparison:recorded",
            )
            .with_input_size_limit(1_000_000)
            .with_structured_output(StructuredOutputMode::JsonObject),
        )
        .build()
        .map_err(|error| EvaluationError::Setup(error.to_string()))
}

enum Produced {
    Proposal(Box<Proposal>),
    MappingRejected {
        class: &'static str,
        requested_operation: Option<RequestedOperation>,
    },
}

fn produce(binding: &JobBinding, output: &Map<String, Value>) -> Result<Produced, EvaluationError> {
    let setup = |error: &dyn std::fmt::Display| EvaluationError::Setup(error.to_string());
    let profile = profile()?;
    let request = InterpretationRequest::new(
        binding.capture_id.clone(),
        u64::try_from(binding.source_revision).map_err(|error| setup(&error))?,
        binding.text_basis.clone(),
        binding.basis_text.clone(),
        binding.request_version.clone(),
        M1_INSTRUCTION_VERSION,
        &profile,
        ROUTE_NAME,
        binding.time_context.clone(),
    )
    .map_err(|error| setup(&error))?;
    let mapping = InterpretationMapping::new(
        &request,
        binding.item_id.clone(),
        binding.proposal_id.clone(),
    )
    .map_err(|error| setup(&error))?;
    let reply = InterpretationOutput {
        request_version: binding.request_version.clone(),
        proposal: output.clone(),
        elapsed_ms: 0,
    };
    Ok(match mapping.map_output(&reply) {
        Ok(proposal) => Produced::Proposal(Box::new(proposal)),
        Err(error) => Produced::MappingRejected {
            // The error text can quote model-chosen field names; only its class is reported.
            class: match error {
                InstructionError::ProviderSuppliedProvenance { .. } => {
                    "provider_supplied_provenance"
                }
                InstructionError::UnknownOutputField { .. } => "unknown_output_field",
                InstructionError::Proposal(_) => "proposal_contract_violation",
                _ => "mapping_rejected",
            },
            requested_operation: requested_operation_of(output),
        },
    })
}

/// What a rejected reply asked for, read from the untrusted reply itself.
fn requested_operation_of(reply: &Map<String, Value>) -> Option<RequestedOperation> {
    match reply.get("operation")?.get("kind")?.as_str()? {
        "update" => Some(RequestedOperation::Update),
        "create" => Some(RequestedOperation::Create),
        _ => None,
    }
}

pub(super) fn score_in_scratch(
    fixture: &Fixture,
    case_id: String,
    scratch: &mut ScratchCase,
    output: &Map<String, Value>,
) -> Result<ScoredArm, EvaluationError> {
    let produced = produce(scratch.binding(), output)?;
    let (producer, submission, candidate_view, candidate_abstention) = match produced {
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
                Submission::Proposal(proposal),
                Some(view),
                abstention,
            )
        }
        Produced::MappingRejected {
            class,
            requested_operation,
        } => (
            ProducerOutcome::MappingRejected {
                error: class.to_string(),
                requested_operation,
            },
            Submission::Failure(ProviderFailure::new(FailureKind::InvalidOutput)),
            requested_operation.map(|requested_operation| FacetView {
                requested_operation: Some(requested_operation),
                ..FacetView::default()
            }),
            None,
        ),
    };

    let (guard, state): (GuardOutcome, AuthoritativeState) = scratch.submit(&submission)?;

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
    // Only a reply that mapped to a proposal can abstain; an invalid reply never counts as one.
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

    let valid_candidate = matches!(
        producer,
        ProducerOutcome::Proposed | ProducerOutcome::Abstained
    )
    .then(|| Candidate {
        view: candidate_view.clone().unwrap_or_default(),
        abstention: candidate_abstention.as_ref().map(std::mem::discriminant),
    });

    let mut report = CaseReport {
        case_id,
        fixture_id: fixture.id.clone(),
        category: fixture.category,
        role: None,
        description: None,
        producer,
        guard: Some(guard),
        candidate_defects,
        authoritative_defects,
        abstention,
        candidate_forbidden,
        authoritative_forbidden,
        authoritative_state: state,
    };
    redact_model_text(&mut report);
    Ok(ScoredArm {
        report,
        candidate: valid_candidate,
    })
}

const REDACTED: &str = "[model text omitted]";

/// A comparison report may be kept next to run journals, which refuse model-chosen strings that
/// look like endpoints or credentials. Scoring has already used every value; what is reported
/// keeps the defect kinds, stages and counts but not text the model chose.
fn redact_model_text(report: &mut CaseReport) {
    for defect in report
        .candidate_defects
        .iter_mut()
        .chain(report.authoritative_defects.iter_mut())
    {
        if let Some(quote) = defect.detail.find('"') {
            defect.detail.truncate(quote);
            defect.detail.push_str(REDACTED);
        }
    }
    if let Some(GuardOutcome::Rejected { error }) = report.guard.as_mut() {
        *error = "rejected by the application guard".to_string();
    }
    if report.authoritative_state.session_topic.is_some() {
        report.authoritative_state.session_topic = Some(REDACTED.to_string());
    }
}
