//! Proposal schemas and provenance validation.
//!
//! Represents note/action/idea annotations, reminder proposals and updates as validated,
//! source-linked candidates rather than unrestricted state writes. Each candidate
//! references a capture ID/revision, source spans, and processing version.
//!
//! Every type here rejects unknown fields at every nesting level, so model output cannot carry
//! item scope, session read scope, route, permission or any other authorization: those are
//! explicit policy owned elsewhere. Schema validity is never proof of semantic intent;
//! [`Proposal::validate`] performs the semantic checks and, even when it passes, the result is
//! only a candidate until the separate application logic accepts it.

use crate::domain::items::{ItemState, SUPPORTED_PROPOSAL_SCHEMA_VERSION};
pub use crate::providers::contracts::TextBasis;
use crate::providers::contracts::{InterpretationOutput, InterpretationRequest};
use crate::store::events::ItemType;
use chrono::DateTime;
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize};
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// Why a proposal was rejected. Rejected output mutates nothing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProposalError {
    #[error("proposal is not valid JSON for this schema: {0}")]
    Malformed(String),
    #[error("unsupported proposal schema version {received}, expected {supported}")]
    UnsupportedSchemaVersion { received: i32, supported: i32 },
    #[error("{field} must be a UUID")]
    InvalidIdentifier { field: &'static str },
    #[error("source_revision must be non-negative, got {0}")]
    NegativeRevision(i32),
    #[error("text basis revision {basis_revision} must equal source_revision {source_revision}")]
    BasisRevisionMismatch {
        basis_revision: u64,
        source_revision: i32,
    },
    #[error("proposal {field} does not match the trusted request context")]
    ProvenanceMismatch { field: &'static str },
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("source span [{start}, {end}) must be non-empty")]
    EmptySpan { start: usize, end: usize },
    #[error("source span [{start}, {end}) is out of bounds for text with {char_count} characters")]
    SpanOutOfBounds {
        start: usize,
        end: usize,
        char_count: usize,
    },
    #[error("{facet} proposal requires source evidence")]
    MissingEvidence { facet: &'static str },
    #[error("reminder {field} is required unless the time quality is ambiguous")]
    MissingReminderResolution { field: &'static str },
    #[error("an ambiguous reminder time must not carry a resolved instant")]
    AmbiguousReminderHasInstant,
    #[error("reminder instant is not valid RFC 3339")]
    InvalidInstant,
    #[error("reminder timezone is not a valid IANA timezone")]
    InvalidTimezone,
    #[error("abstention cannot coexist with a {facet} proposal")]
    AbstentionWithContent { facet: &'static str },
    #[error("proposal must carry at least one facet or an explicit abstention")]
    NoContent,
    #[error("{operation} is not supported in M1 and must be an explicit abstention")]
    UnsupportedOperationNotAbstained { operation: &'static str },
    #[error("{operation} must abstain with the unsupported-operation reason")]
    UnsupportedOperationWrongReason { operation: &'static str },
    #[error("update target must be the proposal's own item")]
    UpdateTargetMismatch,
}

fn parse_uuid(value: &str, field: &'static str) -> Result<(), ProposalError> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| ProposalError::InvalidIdentifier { field })
}

/// Strict wire form of the provider text basis. The provider type does not reject unknown
/// fields, but model output is untrusted at every nesting level, so proposals decode through
/// this mirror and convert to the established [`TextBasis`], which keeps the item revision and
/// the correction record identity.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TextBasisWire {
    Original {
        item_revision: u64,
    },
    Correction {
        correction_record_id: String,
        item_revision: u64,
    },
}

fn deserialize_text_basis<'de, D>(deserializer: D) -> Result<TextBasis, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match TextBasisWire::deserialize(deserializer)? {
        TextBasisWire::Original { item_revision } => TextBasis::Original { item_revision },
        TextBasisWire::Correction {
            correction_record_id,
            item_revision,
        } => TextBasis::Correction {
            correction_record_id,
            item_revision,
        },
    })
}

fn basis_revision(text_basis: &TextBasis) -> u64 {
    match text_basis {
        TextBasis::Original { item_revision } | TextBasis::Correction { item_revision, .. } => {
            *item_revision
        }
    }
}

/// The operation a proposal asks for. Only [`Operation::Annotate`], which attaches derived
/// facets to the item created by the capture, is supported in M1. Everything else is an
/// unsupported spoken change and must be an explicit abstention; UI corrections and completion
/// do not pass through proposals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Annotate the item created by the capture with derived facets.
    Annotate {},
    /// Create another item from model output (unsupported: captures create items).
    Create {},
    /// Change an existing item (unsupported by spoken request in M1).
    Update { item_id: String },
}

impl Operation {
    fn unsupported_name(&self) -> Option<&'static str> {
        match self {
            Operation::Annotate {} => None,
            Operation::Create {} => Some("create"),
            Operation::Update { .. } => Some("update"),
        }
    }
}

/// Explicit resolution quality for a proposed reminder time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeResolutionQuality {
    /// A clear, unambiguous time from the source.
    Explicit,
    /// Inferred or partially stated (e.g. "next Monday" when the day is ambiguous).
    Inferred,
    /// Ambiguous or incomplete (e.g. "maybe Friday" or "next week"); never guessed.
    Ambiguous,
}

impl TimeResolutionQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeResolutionQuality::Explicit => "explicit",
            TimeResolutionQuality::Inferred => "inferred",
            TimeResolutionQuality::Ambiguous => "ambiguous",
        }
    }
}

impl FromStr for TimeResolutionQuality {
    type Err = ProposalError;

    fn from_str(s: &str) -> Result<Self, ProposalError> {
        match s {
            "explicit" => Ok(TimeResolutionQuality::Explicit),
            "inferred" => Ok(TimeResolutionQuality::Inferred),
            "ambiguous" => Ok(TimeResolutionQuality::Ambiguous),
            other => Err(ProposalError::Malformed(format!(
                "unknown resolution quality: {other}"
            ))),
        }
    }
}

/// Proposed reminder time. An ambiguous time carries no resolved instant and is never guessed;
/// explicit and inferred times carry both an RFC 3339 instant and an IANA timezone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReminderProposal {
    /// Resolved absolute instant (RFC 3339). Must be absent when quality is ambiguous.
    pub instant: Option<String>,
    /// Timezone identifier for display (IANA). Optional only when quality is ambiguous.
    pub timezone_id: Option<String>,
    /// Quality of the resolution.
    pub quality: TimeResolutionQuality,
    /// Character span of the time phrase in the text basis. Required.
    pub source_span: Option<SourceSpan>,
}

/// Reason for abstaining from a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AbstentionReason {
    /// Unable to determine the target or value from the source.
    UncertainTarget,
    /// The source contradicts or negates the field (e.g., "don't remind me").
    Negated,
    /// The source is too ambiguous or incomplete to resolve.
    Ambiguous,
    /// The operation is not supported in this schema version.
    UnsupportedOperation,
    /// Other reason (free text).
    Other(String),
}

/// Proposed session-topic: an independent derived facet with source evidence. A user-assigned
/// topic always wins over it; see [`resolve_session_topic`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTopicProposal {
    /// The proposed session-topic string.
    pub topic: String,
    /// Character span of the evidence in the text basis. Required.
    pub source_span: Option<SourceSpan>,
}

/// The session-topic in effect after applying the item state's precedence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedSessionTopic {
    /// Set or corrected by the user (a D04 `session_topic` correction); survives reprocessing.
    UserCorrected { topic: String },
    /// Stated by the user at capture; outranks any model proposal.
    StatedAtCapture { topic: String },
    /// Derived from source evidence by a validated interpretation proposal.
    Derived { topic: String, evidence: SourceSpan },
}

/// Resolve the session-topic using the D04 precedence: user correction, then a topic stated at
/// capture, then a model proposal. `item_state` is the authoritative projection; the proposal is
/// fully validated against `text` and must belong to the same item and capture before it is
/// considered, even when a higher-precedence topic wins. Neither the proposal nor the result can
/// change item scope, session read scope or any permission. `Ok(None)` means no topic.
pub fn resolve_session_topic(
    item_state: &ItemState,
    stated_at_capture: Option<&str>,
    proposal: &Proposal,
    text: &str,
) -> Result<Option<ResolvedSessionTopic>, ProposalError> {
    proposal.validate(text)?;
    if proposal.item_id != item_state.item_id {
        return Err(ProposalError::ProvenanceMismatch { field: "item_id" });
    }
    if proposal.capture_id != item_state.capture_id {
        return Err(ProposalError::ProvenanceMismatch {
            field: "capture_id",
        });
    }
    let non_blank = |topic: &str| Some(topic.trim().to_string()).filter(|t| !t.is_empty());
    if item_state.provenance.session_topic_corrected {
        return Ok(item_state
            .session_topic
            .as_deref()
            .and_then(non_blank)
            .map(|topic| ResolvedSessionTopic::UserCorrected { topic }));
    }
    if let Some(topic) = stated_at_capture.and_then(non_blank) {
        return Ok(Some(ResolvedSessionTopic::StatedAtCapture { topic }));
    }
    Ok(proposal
        .session_topic_proposal
        .as_ref()
        .and_then(|proposed| {
            let evidence = proposed.source_span?;
            Some(ResolvedSessionTopic::Derived {
                topic: proposed.topic.trim().to_string(),
                evidence,
            })
        }))
}

/// Character offset span into a text basis.
/// Offsets are Unicode scalar values (character count), not bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

impl SourceSpan {
    pub fn new(start: usize, end: usize) -> Self {
        SourceSpan { start, end }
    }

    /// The span must be non-empty and lie within the text, counted in characters.
    pub fn is_valid(&self, text: &str) -> Result<(), ProposalError> {
        if self.start >= self.end {
            return Err(ProposalError::EmptySpan {
                start: self.start,
                end: self.end,
            });
        }
        let char_count = text.chars().count();
        if self.end > char_count {
            return Err(ProposalError::SpanOutOfBounds {
                start: self.start,
                end: self.end,
                char_count,
            });
        }
        Ok(())
    }
}

/// A validated interpretation proposal: source-linked candidate output from an interpretation job.
/// Does not constitute a mutation until applied through the separate proposal-application logic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Immutable proposal identifier (UUID).
    pub proposal_id: String,
    /// Item ID this proposal applies to (UUID).
    pub item_id: String,
    /// Capture ID (UUID) for the audit trail.
    pub capture_id: String,
    /// Item revision when this proposal was created (compare-and-set, non-negative).
    pub source_revision: i32,
    /// Schema version; unknown versions are rejected without mutation.
    pub schema_version: i32,
    /// Immutable text basis with the item revision at which it was current.
    #[serde(deserialize_with = "deserialize_text_basis")]
    pub text_basis: TextBasis,
    /// Versioned interpretation request that produced this proposal (UUID).
    pub request_version: String,
    /// The operation being proposed. Required; only annotation is supported in M1.
    pub operation: Operation,
    /// Optional note, action, or idea annotation. Requires `source_spans`.
    pub item_type: Option<ItemType>,
    /// Optional proposed reminder with quality and source span.
    pub reminder_proposal: Option<ReminderProposal>,
    /// Optional proposed session-topic with source span.
    pub session_topic_proposal: Option<SessionTopicProposal>,
    /// Evidence spans for the item type facet.
    pub source_spans: Option<Vec<SourceSpan>>,
    /// Explicit abstention; excludes every proposed facet.
    pub abstention: Option<AbstentionReason>,
}

impl Proposal {
    /// Create a new annotate proposal with no facets; use the `with_*` setters to add them.
    pub fn new(
        proposal_id: String,
        item_id: String,
        capture_id: String,
        source_revision: i32,
        schema_version: i32,
        text_basis: TextBasis,
        request_version: String,
    ) -> Self {
        Proposal {
            proposal_id,
            item_id,
            capture_id,
            source_revision,
            schema_version,
            text_basis,
            request_version,
            operation: Operation::Annotate {},
            item_type: None,
            reminder_proposal: None,
            session_topic_proposal: None,
            source_spans: None,
            abstention: None,
        }
    }

    /// The checked boundary for provider output. Decodes the untrusted proposal object, rejects
    /// unknown fields at every level, requires its provenance to equal the trusted request
    /// (capture, revision, text basis, request version) and the expected item, then validates
    /// semantics against the request's text. Rejection mutates nothing.
    pub fn from_output(
        request: &InterpretationRequest,
        output: &InterpretationOutput,
        expected_item_id: &str,
    ) -> Result<Self, ProposalError> {
        if output.request_version != request.request_version() {
            return Err(ProposalError::ProvenanceMismatch {
                field: "output request_version",
            });
        }
        let proposal: Proposal =
            serde_json::from_value(serde_json::Value::Object(output.proposal.clone()))
                .map_err(|error| ProposalError::Malformed(error.to_string()))?;
        proposal.check_provenance(request, expected_item_id)?;
        proposal.validate(request.text())?;
        Ok(proposal)
    }

    fn check_provenance(
        &self,
        request: &InterpretationRequest,
        expected_item_id: &str,
    ) -> Result<(), ProposalError> {
        let mismatches = [
            ("item_id", self.item_id != expected_item_id),
            ("capture_id", self.capture_id != request.capture_id()),
            (
                "request_version",
                self.request_version != request.request_version(),
            ),
            (
                "source_revision",
                u64::try_from(self.source_revision).ok() != Some(request.source_revision()),
            ),
            ("text_basis", self.text_basis != *request.text_basis()),
        ];
        match mismatches.into_iter().find(|(_, differs)| *differs) {
            Some((field, _)) => Err(ProposalError::ProvenanceMismatch { field }),
            None => Ok(()),
        }
    }

    pub fn with_operation(mut self, operation: Operation) -> Self {
        self.operation = operation;
        self
    }

    pub fn with_item_type(mut self, item_type: Option<ItemType>) -> Self {
        self.item_type = item_type;
        self
    }

    pub fn with_reminder_proposal(mut self, reminder_proposal: Option<ReminderProposal>) -> Self {
        self.reminder_proposal = reminder_proposal;
        self
    }

    pub fn with_session_topic_proposal(mut self, topic: Option<SessionTopicProposal>) -> Self {
        self.session_topic_proposal = topic;
        self
    }

    pub fn with_source_spans(mut self, spans: Option<Vec<SourceSpan>>) -> Self {
        self.source_spans = spans;
        self
    }

    pub fn with_abstention(mut self, reason: Option<AbstentionReason>) -> Self {
        self.abstention = reason;
        self
    }

    /// Unknown schema versions are rejected without mutation.
    pub fn validate_schema_version(&self) -> Result<(), ProposalError> {
        if self.schema_version != SUPPORTED_PROPOSAL_SCHEMA_VERSION {
            return Err(ProposalError::UnsupportedSchemaVersion {
                received: self.schema_version,
                supported: SUPPORTED_PROPOSAL_SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// Identifiers must be UUIDs, the revision non-negative and the text basis current at that
    /// revision. Agreement with the trusted request is checked by [`Proposal::from_output`];
    /// freshness against stored state is checked when the proposal is applied.
    pub fn validate_identifiers(&self) -> Result<(), ProposalError> {
        parse_uuid(&self.proposal_id, "proposal_id")?;
        parse_uuid(&self.item_id, "item_id")?;
        parse_uuid(&self.capture_id, "capture_id")?;
        parse_uuid(&self.request_version, "request_version")?;
        let source_revision = u64::try_from(self.source_revision)
            .map_err(|_| ProposalError::NegativeRevision(self.source_revision))?;
        if let TextBasis::Correction {
            correction_record_id,
            ..
        } = &self.text_basis
        {
            parse_uuid(correction_record_id, "correction_record_id")?;
        }
        let basis_revision = basis_revision(&self.text_basis);
        if basis_revision != source_revision {
            return Err(ProposalError::BasisRevisionMismatch {
                basis_revision,
                source_revision: self.source_revision,
            });
        }
        Ok(())
    }

    /// Unsupported operations (create, update of existing items) must be explicit abstentions
    /// carrying the unsupported-operation reason and no facets. An update may only name the
    /// proposal's own item.
    pub fn validate_operation(&self) -> Result<(), ProposalError> {
        if let Operation::Update { item_id } = &self.operation {
            parse_uuid(item_id, "update item_id")?;
            if *item_id != self.item_id {
                return Err(ProposalError::UpdateTargetMismatch);
            }
        }
        if let Some(operation) = self.operation.unsupported_name() {
            match &self.abstention {
                None => {
                    return Err(ProposalError::UnsupportedOperationNotAbstained { operation });
                }
                Some(AbstentionReason::UnsupportedOperation) => {}
                Some(_) => {
                    return Err(ProposalError::UnsupportedOperationWrongReason { operation });
                }
            }
        }
        Ok(())
    }

    /// Item type evidence spans must lie within the basis text.
    pub fn validate_source_spans(&self, text: &str) -> Result<(), ProposalError> {
        for span in self.source_spans.iter().flatten() {
            span.is_valid(text)?;
        }
        Ok(())
    }

    fn validate_item_type_evidence(&self) -> Result<(), ProposalError> {
        if self.item_type.is_some() && self.source_spans.as_deref().unwrap_or_default().is_empty() {
            return Err(ProposalError::MissingEvidence { facet: "item type" });
        }
        Ok(())
    }

    /// A resolved reminder instant must be RFC 3339 and its timezone IANA; an ambiguous time
    /// carries no instant. Every reminder needs an in-bounds source span.
    pub fn validate_reminder_proposal(&self, text: &str) -> Result<(), ProposalError> {
        let Some(reminder) = &self.reminder_proposal else {
            return Ok(());
        };
        match (reminder.quality, &reminder.instant) {
            (TimeResolutionQuality::Ambiguous, Some(_)) => {
                return Err(ProposalError::AmbiguousReminderHasInstant);
            }
            (TimeResolutionQuality::Ambiguous, None) => {}
            (_, None) => {
                return Err(ProposalError::MissingReminderResolution { field: "instant" });
            }
            (_, Some(instant)) => {
                DateTime::parse_from_rfc3339(instant).map_err(|_| ProposalError::InvalidInstant)?;
                if reminder.timezone_id.is_none() {
                    return Err(ProposalError::MissingReminderResolution { field: "timezone" });
                }
            }
        }
        if let Some(timezone) = &reminder.timezone_id {
            timezone
                .parse::<Tz>()
                .map_err(|_| ProposalError::InvalidTimezone)?;
        }
        reminder
            .source_span
            .ok_or(ProposalError::MissingEvidence { facet: "reminder" })?
            .is_valid(text)
    }

    /// The topic must be non-empty and carry an in-bounds source span.
    pub fn validate_session_topic_proposal(&self, text: &str) -> Result<(), ProposalError> {
        let Some(topic) = &self.session_topic_proposal else {
            return Ok(());
        };
        if topic.topic.trim().is_empty() {
            return Err(ProposalError::EmptyField {
                field: "session topic",
            });
        }
        topic
            .source_span
            .ok_or(ProposalError::MissingEvidence {
                facet: "session topic",
            })?
            .is_valid(text)
    }

    /// An abstention excludes every proposed facet.
    pub fn validate_abstention_exclusivity(&self) -> Result<(), ProposalError> {
        let Some(reason) = &self.abstention else {
            return Ok(());
        };
        if let AbstentionReason::Other(text) = reason {
            if text.trim().is_empty() {
                return Err(ProposalError::EmptyField {
                    field: "abstention reason",
                });
            }
        }
        let present_facet = if self.item_type.is_some() {
            Some("item type")
        } else if self.reminder_proposal.is_some() {
            Some("reminder")
        } else if self.session_topic_proposal.is_some() {
            Some("session topic")
        } else {
            None
        };
        match present_facet {
            Some(facet) => Err(ProposalError::AbstentionWithContent { facet }),
            None => Ok(()),
        }
    }

    /// A proposal must propose something or explicitly abstain.
    pub fn validate_at_least_one_field(&self) -> Result<(), ProposalError> {
        if self.abstention.is_none()
            && self.item_type.is_none()
            && self.reminder_proposal.is_none()
            && self.session_topic_proposal.is_none()
        {
            return Err(ProposalError::NoContent);
        }
        Ok(())
    }

    /// Validate all fields for semantic correctness against the basis text. This does not check
    /// database state (capture/item existence, revision freshness) or whether the proposal may
    /// be applied; passing is not proof of semantic intent.
    pub fn validate(&self, text: &str) -> Result<(), ProposalError> {
        self.validate_schema_version()?;
        self.validate_identifiers()?;
        self.validate_operation()?;
        self.validate_abstention_exclusivity()?;
        self.validate_at_least_one_field()?;
        self.validate_source_spans(text)?;
        self.validate_item_type_evidence()?;
        self.validate_reminder_proposal(text)?;
        self.validate_session_topic_proposal(text)
    }
}
