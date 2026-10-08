//! Binding of one trusted request to the instructions, and mapping of the reply to a proposal.

use super::prompt::{render, RenderedPrompt};
use super::text::M1_INSTRUCTION_VERSION;
use super::InstructionError;
use crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use crate::interpretation::contracts::Proposal;
use crate::providers::contracts::{InterpretationOutput, InterpretationRequest};
use chrono::Offset;
use chrono::TimeZone;
use chrono_tz::Tz;
use serde_json::{json, Map, Value};
use uuid::Uuid;

/// Keys a provider may return. They are the facet fields of the I01 proposal.
const FACET_KEYS: [&str; 6] = [
    "operation",
    "item_type",
    "source_spans",
    "reminder_proposal",
    "session_topic_proposal",
    "abstention",
];

/// Keys the mapping fills in from trusted context. A provider that sends one is rejected
/// rather than overwritten, so a disagreement is visible instead of silently resolved.
const PROVENANCE_KEYS: [&str; 7] = [
    "proposal_id",
    "item_id",
    "capture_id",
    "source_revision",
    "schema_version",
    "text_basis",
    "request_version",
];

/// One interpretation request bound to the item it annotates and the proposal it will produce.
///
/// The proposal identifier is supplied by the caller, so mapping the same reply again (a retry
/// after a crash) yields the same proposal rather than a duplicate.
#[derive(Debug, Clone)]
pub struct InterpretationMapping {
    request: InterpretationRequest,
    item_id: String,
    proposal_id: String,
}

impl InterpretationMapping {
    /// Check that the request names the published instructions, carries a coherent time
    /// context and that both identifiers are UUIDs.
    pub fn new(
        request: &InterpretationRequest,
        item_id: impl Into<String>,
        proposal_id: impl Into<String>,
    ) -> Result<Self, InstructionError> {
        if request.instruction_version() != M1_INSTRUCTION_VERSION {
            return Err(InstructionError::UnknownInstructionVersion {
                requested: request.instruction_version().to_string(),
                expected: M1_INSTRUCTION_VERSION.to_string(),
            });
        }
        let item_id = item_id.into();
        let proposal_id = proposal_id.into();
        require_uuid(&item_id, "item_id")?;
        require_uuid(&proposal_id, "proposal_id")?;
        i32::try_from(request.source_revision())
            .map_err(|_| InstructionError::SourceRevisionOutOfRange(request.source_revision()))?;
        validate_time_context(request)?;
        Ok(InterpretationMapping {
            request: request.clone(),
            item_id,
            proposal_id,
        })
    }

    pub fn request(&self) -> &InterpretationRequest {
        &self.request
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }

    /// The provider-neutral prompt for this request.
    pub fn render(&self) -> RenderedPrompt {
        render(&self.request)
    }

    /// Map a provider reply to a validated proposal candidate.
    ///
    /// The reply may contain only [`FACET_KEYS`]. Trusted provenance comes from the bound
    /// request, never from the reply. The result is a candidate only: applying it is a separate
    /// step with its own freshness checks.
    pub fn map_output(&self, output: &InterpretationOutput) -> Result<Proposal, InstructionError> {
        let mut proposal = Map::new();
        for (key, value) in &output.proposal {
            if PROVENANCE_KEYS.contains(&key.as_str()) {
                return Err(InstructionError::ProviderSuppliedProvenance { field: key.clone() });
            }
            if !FACET_KEYS.contains(&key.as_str()) {
                return Err(InstructionError::UnknownOutputField { field: key.clone() });
            }
            proposal.insert(key.clone(), value.clone());
        }
        let source_revision = i32::try_from(self.request.source_revision()).map_err(|_| {
            InstructionError::SourceRevisionOutOfRange(self.request.source_revision())
        })?;
        let trusted: [(&str, Value); 7] = [
            ("proposal_id", json!(self.proposal_id)),
            ("item_id", json!(self.item_id)),
            ("capture_id", json!(self.request.capture_id())),
            ("source_revision", json!(source_revision)),
            ("schema_version", json!(SUPPORTED_PROPOSAL_SCHEMA_VERSION)),
            ("text_basis", json!(self.request.text_basis())),
            ("request_version", json!(self.request.request_version())),
        ];
        for (key, value) in trusted {
            proposal.insert(key.to_string(), value);
        }
        let enriched = InterpretationOutput {
            request_version: output.request_version.clone(),
            proposal,
            elapsed_ms: output.elapsed_ms,
        };
        Ok(Proposal::from_output(
            &self.request,
            &enriched,
            &self.item_id,
        )?)
    }
}

fn require_uuid(value: &str, field: &'static str) -> Result<(), InstructionError> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| InstructionError::InvalidIdentifier { field })
}

fn validate_time_context(request: &InterpretationRequest) -> Result<(), InstructionError> {
    let time = request.time_context();
    let invalid = |field: &'static str, reason: &str| InstructionError::InvalidTimeContext {
        field,
        reason: reason.to_string(),
    };
    if time.locale.trim().is_empty() {
        return Err(invalid("locale", "must not be empty"));
    }
    if time.calendar != "gregorian" {
        return Err(invalid("calendar", "only gregorian is supported in M1"));
    }
    let zone: Tz = time
        .timezone
        .parse()
        .map_err(|_| invalid("timezone", "not an IANA timezone"))?;
    let offset = zone
        .offset_from_utc_datetime(&time.reference_time.naive_utc())
        .fix()
        .local_minus_utc();
    if offset != time.utc_offset_at_capture {
        return Err(invalid(
            "utc_offset_at_capture",
            "does not match the timezone's offset at the reference time",
        ));
    }
    Ok(())
}
