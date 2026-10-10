use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

use super::arm::{ArmError, ArmLabel};
use super::pair::RequestMismatch;
use crate::interpretation::instructions::{content_version, OUTPUT_CONTRACT_VERSION};
use crate::providers::contracts::{
    DiagnosticRequestError, RequestValidationError, RequestedSettings, TextBasis,
};
use crate::time::{TimeContext, TimeResolver};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ComparisonError {
    #[error("{0} must be a UUID")]
    InvalidIdentifier(&'static str),
    #[error("source text must not be empty")]
    EmptyText,
    #[error("route id must not be empty")]
    EmptyRoute,
    #[error("text basis revision must equal the source revision")]
    RevisionMismatch,
    #[error("instructions do not match the declared instruction version")]
    InstructionVersionMismatch,
    #[error("time context is not coherent")]
    InvalidTimeContext,
    #[error("arms must be pinned to different profile versions")]
    SameProfile,
    #[error("arm {arm:?} cannot be compared: {error}")]
    Arm { arm: ArmLabel, error: ArmError },
    #[error("invalid request: {0}")]
    Request(#[from] RequestValidationError),
    #[error("invalid diagnostic request: {0}")]
    Diagnostic(#[from] DiagnosticRequestError),
    #[error("derived requests are not comparable: {0:?}")]
    NotComparable(Vec<RequestMismatch>),
}

/// The one input both arms are derived from. Everything model-visible is fixed here, once:
/// the source text and revision, its original time context, the rendered instructions and the
/// requested settings. The route is core-only and is used to authorize each arm.
#[derive(Debug, Clone)]
pub struct FrozenComparisonSource {
    pub(super) capture_id: String,
    pub(super) source_revision: u64,
    pub(super) text_basis: TextBasis,
    pub(super) text: String,
    pub(super) request_version: String,
    pub(super) time_context: TimeContext,
    pub(super) route_id: String,
    pub(super) instructions: String,
    pub(super) instruction_version: String,
    pub(super) settings: RequestedSettings,
}

impl FrozenComparisonSource {
    /// `instruction_version` is the version the frozen primary request names; the instructions
    /// must be the text that version digests, so a label can never stand in for other text.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capture_id: impl Into<String>,
        source_revision: u64,
        text_basis: TextBasis,
        text: impl Into<String>,
        request_version: impl Into<String>,
        time_context: TimeContext,
        route_id: impl Into<String>,
        instructions: impl Into<String>,
        instruction_version: impl Into<String>,
        settings: RequestedSettings,
    ) -> Result<FrozenComparisonSource, ComparisonError> {
        let capture_id = capture_id.into();
        let request_version = request_version.into();
        let text = text.into();
        let route_id = route_id.into();
        let instructions = instructions.into();
        let instruction_version = instruction_version.into();

        if Uuid::parse_str(&capture_id).is_err() {
            return Err(ComparisonError::InvalidIdentifier("capture_id"));
        }
        if Uuid::parse_str(&request_version).is_err() {
            return Err(ComparisonError::InvalidIdentifier("request_version"));
        }
        let basis_revision = match &text_basis {
            TextBasis::Original { item_revision } => *item_revision,
            TextBasis::Correction {
                correction_record_id,
                item_revision,
            } => {
                if Uuid::parse_str(correction_record_id).is_err() {
                    return Err(ComparisonError::InvalidIdentifier("correction_record_id"));
                }
                *item_revision
            }
        };
        if basis_revision != source_revision {
            return Err(ComparisonError::RevisionMismatch);
        }
        if text.trim().is_empty() {
            return Err(ComparisonError::EmptyText);
        }
        if route_id.trim().is_empty() {
            return Err(ComparisonError::EmptyRoute);
        }
        if content_version(&instructions) != instruction_version {
            return Err(ComparisonError::InstructionVersionMismatch);
        }
        TimeResolver::validate_context(&time_context)
            .map_err(|_| ComparisonError::InvalidTimeContext)?;

        Ok(FrozenComparisonSource {
            capture_id,
            source_revision,
            text_basis,
            text,
            request_version,
            time_context,
            route_id,
            instructions,
            instruction_version,
            settings,
        })
    }

    pub fn route_id(&self) -> &str {
        &self.route_id
    }

    pub fn source_revision(&self) -> u64 {
        self.source_revision
    }

    pub fn instruction_version(&self) -> &str {
        &self.instruction_version
    }

    /// The normalized context both arms receive. It names no profile, model, route or other
    /// arm, so it is identical for either arm by construction.
    pub(super) fn render_context(&self) -> String {
        json!({
            "instruction_version": self.instruction_version,
            "output_contract_version": OUTPUT_CONTRACT_VERSION,
            "request": {
                "capture_id": self.capture_id,
                "source_revision": self.source_revision,
                "text_basis": self.text_basis,
            },
            "time_context": self.time_context,
            "source": {
                "trust": "untrusted_data",
                "character_count": self.text.chars().count(),
                "text": self.text,
            },
        })
        .to_string()
    }
}
