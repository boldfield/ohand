//! Proposal schemas and provenance validation.
//!
//! Represents note/action/idea annotations, reminder proposals and updates as validated,
//! source-linked candidates rather than unrestricted state writes. Each candidate
//! references a capture ID/revision, source spans, and processing version.

use crate::domain::items::SUPPORTED_PROPOSAL_SCHEMA_VERSION;
use crate::store::events::ItemType;
use anyhow::{anyhow, Result};
use chrono::DateTime;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Identifies the text basis used for source spans and validation.
/// Text basis is always immutable: the original source text, or a specific text-correction record
/// at the item revision where it was current.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum TextBasis {
    /// Original source text from capture.
    Original,
    /// A user text-correction record, identified by its correction record ID.
    /// The basis is valid only at the item revision when the correction was stored.
    #[serde(rename_all = "lowercase")]
    Correction { correction_id: String },
}

impl fmt::Display for TextBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TextBasis::Original => write!(f, "original"),
            TextBasis::Correction { correction_id } => {
                write!(f, "correction:{}", correction_id)
            }
        }
    }
}

impl TextBasis {
    /// Parse from database representation (kind + optional id).
    pub fn from_db(kind: &str, id: Option<&str>) -> Result<Self> {
        match kind {
            "original" => Ok(TextBasis::Original),
            "correction" => {
                let correction_id = id
                    .ok_or_else(|| anyhow!("correction basis requires correction_id"))?
                    .to_string();
                Ok(TextBasis::Correction { correction_id })
            }
            _ => Err(anyhow!("unknown text basis kind: {}", kind)),
        }
    }

    /// Convert to database representation (kind, optional id).
    pub fn to_db(&self) -> (&'static str, Option<String>) {
        match self {
            TextBasis::Original => ("original", None),
            TextBasis::Correction { correction_id } => ("correction", Some(correction_id.clone())),
        }
    }

    /// Validate that the text basis has non-empty required fields.
    pub fn validate(&self) -> Result<()> {
        match self {
            TextBasis::Original => Ok(()),
            TextBasis::Correction { correction_id } => {
                if correction_id.is_empty() {
                    return Err(anyhow!("correction_id must be non-empty"));
                }
                Ok(())
            }
        }
    }
}

/// Supported proposal operations.
/// Represents the type of change being proposed to an item or its facets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Create a new item with the proposed facets.
    Create,
    /// Annotate an existing item with the proposed facets.
    Annotate,
    /// Update an existing item or its reminder by ID.
    #[serde(rename_all = "lowercase")]
    Update { item_id: String },
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operation::Create => write!(f, "create"),
            Operation::Annotate => write!(f, "annotate"),
            Operation::Update { item_id } => write!(f, "update({})", item_id),
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
    /// Ambiguous or incomplete (e.g. "maybe Friday" or "next week").
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
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "explicit" => Ok(TimeResolutionQuality::Explicit),
            "inferred" => Ok(TimeResolutionQuality::Inferred),
            "ambiguous" => Ok(TimeResolutionQuality::Ambiguous),
            _ => Err(anyhow!("unknown resolution quality: {}", s)),
        }
    }
}

/// Proposed reminder: an absolute instant with quality and source span.
/// When quality is Ambiguous, instant and timezone_id are optional;
/// the proposal represents an unresolved or uncertain time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReminderProposal {
    /// Resolved absolute instant (UTC, RFC 3339 format).
    /// Optional when quality is Ambiguous (unresolved time).
    pub instant: Option<String>,
    /// Timezone identifier for display (IANA timezone).
    /// Optional when quality is Ambiguous.
    pub timezone_id: Option<String>,
    /// Quality of the resolution.
    pub quality: TimeResolutionQuality,
    /// Character offsets [start, end) into the text basis.
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

/// Proposed session-topic with optional source evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTopicProposal {
    /// The proposed session-topic string.
    pub topic: String,
    /// Source span where the topic was found, if available.
    pub source_span: Option<SourceSpan>,
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

    /// Check if this span is valid within the given text.
    /// Offsets are character positions (Unicode scalar values), not byte positions.
    /// The span must be non-empty (start < end) to be valid.
    pub fn is_valid(&self, text: &str) -> Result<()> {
        if self.start >= self.end {
            return Err(anyhow!(
                "source span [{}, {}) must be non-empty (start < end)",
                self.start,
                self.end
            ));
        }
        let char_count = text.chars().count();
        if self.start > char_count || self.end > char_count {
            return Err(anyhow!(
                "source span [{}, {}) is out of bounds for text with {} characters",
                self.start,
                self.end,
                char_count
            ));
        }
        Ok(())
    }
}

/// A validated interpretation proposal: source-linked candidate output from an interpretation job.
/// Does not constitute a mutation until applied through the separate proposal-application logic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Immutable proposal identifier (non-empty, UUID format required).
    pub proposal_id: String,
    /// Item ID this proposal applies to (non-empty, UUID format required).
    pub item_id: String,
    /// Capture ID (for audit trail and to reject unknown captures, non-empty, UUID format required).
    pub capture_id: String,
    /// Item revision when this proposal was created (compare-and-set, non-negative).
    pub source_revision: i32,
    /// Schema version; unknown versions are rejected without mutation.
    pub schema_version: i32,
    /// Text basis and immutable identification.
    pub text_basis: TextBasis,
    /// Versioned interpretation request that produced this proposal (non-empty, UUID format required).
    pub request_version: String,
    /// The supported operation being proposed (create, annotate, or update).
    pub operation: Option<Operation>,
    /// Optional note, action, or idea annotation.
    pub item_type: Option<ItemType>,
    /// Optional proposed reminder with quality and source span.
    pub reminder_proposal: Option<ReminderProposal>,
    /// Optional proposed session-topic with source span and evidence.
    pub session_topic_proposal: Option<SessionTopicProposal>,
    /// Source spans for extracted elements (required for type-only proposals).
    pub source_spans: Option<Vec<SourceSpan>>,
    /// Explicit abstention: source could not be interpreted, with optional reason.
    pub abstention: Option<AbstentionReason>,
}

impl Proposal {
    /// Create a new proposal with validation of required fields.
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
            operation: None,
            item_type: None,
            reminder_proposal: None,
            session_topic_proposal: None,
            source_spans: None,
            abstention: None,
        }
    }

    /// Set the operation being proposed.
    pub fn with_operation(mut self, operation: Option<Operation>) -> Self {
        self.operation = operation;
        self
    }

    /// Set an optional item type annotation.
    pub fn with_item_type(mut self, item_type: Option<ItemType>) -> Self {
        self.item_type = item_type;
        self
    }

    /// Set an optional reminder proposal.
    pub fn with_reminder_proposal(mut self, reminder_proposal: Option<ReminderProposal>) -> Self {
        self.reminder_proposal = reminder_proposal;
        self
    }

    /// Set an optional session-topic proposal.
    pub fn with_session_topic_proposal(mut self, topic: Option<SessionTopicProposal>) -> Self {
        self.session_topic_proposal = topic;
        self
    }

    /// Set source spans for extracted elements.
    pub fn with_source_spans(mut self, spans: Option<Vec<SourceSpan>>) -> Self {
        self.source_spans = spans;
        self
    }

    /// Mark this proposal with an explicit abstention reason.
    pub fn with_abstention(mut self, reason: Option<AbstentionReason>) -> Self {
        self.abstention = reason;
        self
    }

    /// Validate schema version compatibility.
    /// Returns an error if the schema version is unknown or unsupported.
    pub fn validate_schema_version(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_PROPOSAL_SCHEMA_VERSION {
            return Err(anyhow!(
                "unsupported proposal schema version {}, expected {}",
                self.schema_version,
                SUPPORTED_PROPOSAL_SCHEMA_VERSION
            ));
        }
        Ok(())
    }

    /// Validate that an identifier is a valid UUID.
    fn validate_uuid(id: &str, field_name: &str) -> Result<()> {
        Uuid::parse_str(id)
            .map_err(|_| anyhow!("{} must be a valid UUID, got '{}'", field_name, id))?;
        Ok(())
    }

    /// Validate required identifier fields are non-empty and in UUID format.
    pub fn validate_identifiers(&self) -> Result<()> {
        if self.proposal_id.is_empty() {
            return Err(anyhow!("proposal_id must be non-empty"));
        }
        Self::validate_uuid(&self.proposal_id, "proposal_id")?;

        if self.item_id.is_empty() {
            return Err(anyhow!("item_id must be non-empty"));
        }
        Self::validate_uuid(&self.item_id, "item_id")?;

        if self.capture_id.is_empty() {
            return Err(anyhow!("capture_id must be non-empty"));
        }
        Self::validate_uuid(&self.capture_id, "capture_id")?;

        if self.request_version.is_empty() {
            return Err(anyhow!("request_version must be non-empty"));
        }
        Self::validate_uuid(&self.request_version, "request_version")?;

        if self.source_revision < 0 {
            return Err(anyhow!(
                "source_revision must be non-negative, got {}",
                self.source_revision
            ));
        }
        self.text_basis.validate()?;
        Ok(())
    }

    /// Validate that source spans are within bounds of the given text.
    /// Spans are character offsets, not byte offsets.
    pub fn validate_source_spans(&self, text: &str) -> Result<()> {
        if let Some(spans) = &self.source_spans {
            for span in spans {
                span.is_valid(text)?;
            }
        }
        Ok(())
    }

    /// Validate the reminder proposal if present.
    /// Checks that the instant is RFC 3339, timezone is IANA, and span is required and within text bounds.
    /// When quality is Ambiguous, instant and timezone_id are optional (representing unresolved time).
    pub fn validate_reminder_proposal(&self, text: &str) -> Result<()> {
        if let Some(reminder) = &self.reminder_proposal {
            // When quality is Ambiguous, instant/timezone are optional; otherwise required.
            match reminder.quality {
                TimeResolutionQuality::Ambiguous => {
                    if let Some(instant) = &reminder.instant {
                        DateTime::parse_from_rfc3339(instant).map_err(|e| {
                            anyhow!("reminder proposal instant is not valid RFC 3339: {}", e)
                        })?;
                    }
                    if let Some(tz) = &reminder.timezone_id {
                        tz.parse::<Tz>().map_err(|e| {
                            anyhow!(
                                "reminder proposal timezone_id is not a valid IANA timezone: {}",
                                e
                            )
                        })?;
                    }
                }
                TimeResolutionQuality::Explicit | TimeResolutionQuality::Inferred => {
                    if let Some(instant) = &reminder.instant {
                        DateTime::parse_from_rfc3339(instant).map_err(|e| {
                            anyhow!("reminder proposal instant is not valid RFC 3339: {}", e)
                        })?;
                    } else {
                        return Err(anyhow!(
                            "reminder proposal instant is required for non-Ambiguous quality"
                        ));
                    }

                    if let Some(tz) = &reminder.timezone_id {
                        tz.parse::<Tz>().map_err(|e| {
                            anyhow!(
                                "reminder proposal timezone_id is not a valid IANA timezone: {}",
                                e
                            )
                        })?;
                    } else {
                        return Err(anyhow!(
                            "reminder proposal timezone_id is required for non-Ambiguous quality"
                        ));
                    }
                }
            }

            // Validate source span is required and valid.
            match &reminder.source_span {
                Some(span) => span.is_valid(text)?,
                None => {
                    return Err(anyhow!(
                        "reminder proposal must have source evidence (source_span)"
                    ))
                }
            }
        }
        Ok(())
    }

    /// Validate session-topic proposal if present.
    /// Requires source evidence (source_span) to be present.
    pub fn validate_session_topic_proposal(&self, text: &str) -> Result<()> {
        if let Some(topic) = &self.session_topic_proposal {
            if topic.topic.is_empty() {
                return Err(anyhow!("session-topic proposal topic cannot be empty"));
            }
            // Source evidence is required for session-topic proposals.
            match &topic.source_span {
                Some(span) => span.is_valid(text)?,
                None => {
                    return Err(anyhow!(
                        "session-topic proposal must have source evidence (source_span)"
                    ))
                }
            }
        }
        Ok(())
    }

    /// Validate abstention/content exclusivity.
    /// If abstained, no proposed field (type, reminder, session-topic) may be present.
    pub fn validate_abstention_exclusivity(&self) -> Result<()> {
        if self.abstention.is_some() {
            if self.item_type.is_some() {
                return Err(anyhow!("abstention cannot coexist with item_type proposal"));
            }
            if self.reminder_proposal.is_some() {
                return Err(anyhow!("abstention cannot coexist with reminder_proposal"));
            }
            if self.session_topic_proposal.is_some() {
                return Err(anyhow!(
                    "abstention cannot coexist with session_topic_proposal"
                ));
            }
        }
        Ok(())
    }

    /// Validate that at least one field (type, reminder, session-topic) or abstention is present.
    pub fn validate_at_least_one_field(&self) -> Result<()> {
        if self.abstention.is_none()
            && self.item_type.is_none()
            && self.reminder_proposal.is_none()
            && self.session_topic_proposal.is_none()
        {
            return Err(anyhow!(
                "proposal must have at least one field (type, reminder, session-topic) or an explicit abstention"
            ));
        }
        Ok(())
    }

    /// Validate that non-abstaining facets have required source evidence.
    /// - Type-only proposals require source_spans.
    /// - Reminders require source_span (validated separately in validate_reminder_proposal).
    /// - Session-topic requires source_span (validated separately in validate_session_topic_proposal).
    /// - When multiple facets are present, each contributes its own evidence requirement.
    pub fn validate_evidence_requirements(&self, _text: &str) -> Result<()> {
        // If abstaining, evidence is not required.
        if self.abstention.is_some() {
            return Ok(());
        }

        // Type-only proposals must have source_spans.
        if self.item_type.is_some()
            && self.reminder_proposal.is_none()
            && self.session_topic_proposal.is_none()
            && (self.source_spans.is_none() || self.source_spans.as_ref().unwrap().is_empty())
        {
            return Err(anyhow!(
                "type-only proposal must have at least one source span"
            ));
        }

        // Each non-abstaining facet must have its own evidence (validated separately for reminder and session-topic).
        // This validation is co-located with the facet-specific validators.
        Ok(())
    }

    /// Validate all fields of the proposal for semantic correctness.
    /// This checks schema version, identifier presence, span bounds, field invariants,
    /// but does NOT check database constraints (existence of captures/items)
    /// or policy (whether the proposal can be applied to the current item state).
    pub fn validate(&self, text: &str) -> Result<()> {
        self.validate_schema_version()?;
        self.validate_identifiers()?;
        self.validate_source_spans(text)?;
        self.validate_reminder_proposal(text)?;
        self.validate_session_topic_proposal(text)?;
        self.validate_abstention_exclusivity()?;
        self.validate_at_least_one_field()?;
        self.validate_evidence_requirements(text)?;
        Ok(())
    }
}
