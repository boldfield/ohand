//! Versioned interpretation instructions, request rendering and response mapping.
//!
//! This module owns what an interpretation adapter tells the model and how the model's reply
//! becomes an I01 [`Proposal`](crate::interpretation::contracts::Proposal):
//!
//! - [`M1_INSTRUCTION_TEXT`] is pinned by the content digest [`M1_INSTRUCTION_VERSION`]; the
//!   request's `instruction_version` must be that digest.
//! - [`InterpretationMapping`] binds one trusted [`InterpretationRequest`], the item it
//!   annotates and the proposal identifier. [`InterpretationMapping::render`] produces the
//!   provider-neutral prompt: the instructions plus a JSON document in which the captured text
//!   is a single escaped string next to explicit capture, profile and time context.
//!   Adapters send it; they never add instructions of their own.
//! - [`InterpretationMapping::map_output`] accepts only the facet keys the contract allows,
//!   rejects provider-supplied provenance and any other key (scope, disclosure, route, ...),
//!   adds trusted provenance from the request and runs the result through the checked I01
//!   boundary, so unsupported updates and invalid facets are rejected without effect.
//!
//! Nothing here can confer a permission: the output contract has no authorization field, and
//! the mapping cannot be made to produce one.

mod mapping;
mod prompt;
mod text;

pub use mapping::InterpretationMapping;
pub use prompt::RenderedPrompt;
pub use text::{content_version, output_schema, M1_INSTRUCTION_TEXT, M1_INSTRUCTION_VERSION};

use crate::interpretation::contracts::ProposalError;
use thiserror::Error;

/// Version of the provider output contract, equal to the proposal schema version the mapping
/// produces.
pub const OUTPUT_CONTRACT_VERSION: i32 = crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;

/// Why a request could not be bound to the instructions or a provider reply could not be
/// mapped. Every error leaves state untouched.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstructionError {
    #[error("instruction version {requested} is not published; expected {expected}")]
    UnknownInstructionVersion { requested: String, expected: String },
    #[error("{field} must be a UUID")]
    InvalidIdentifier { field: &'static str },
    #[error("time context field {field} is invalid: {reason}")]
    InvalidTimeContext { field: &'static str, reason: String },
    #[error("source revision {0} is outside the proposal revision range")]
    SourceRevisionOutOfRange(u64),
    #[error("provider output must not supply trusted provenance field {field}")]
    ProviderSuppliedProvenance { field: String },
    #[error("provider output contains {field}, which the output contract does not allow")]
    UnknownOutputField { field: String },
    #[error(transparent)]
    Proposal(#[from] ProposalError),
}
