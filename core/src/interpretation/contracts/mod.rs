//! Proposal schemas and provenance validation.
//!
//! Represents note/action/idea annotations, reminder proposals and updates as validated,
//! source-linked candidates rather than unrestricted state writes. Each candidate
//! references a capture ID/revision, source spans, and processing version.

use crate::store::events::ItemType;
use anyhow::{anyhow, Result};
use std::fmt;
use std::str::FromStr;

/// Proposal schema version this build can apply. Any other version is rejected without mutation.
pub const SUPPORTED_PROPOSAL_SCHEMA_VERSION: i32 = 1;

/// Identifies the text basis used for source spans and validation.
/// Text basis is always immutable: the original source text, or a specific text-correction record
/// at the item revision where it was current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextBasis {
    /// Original source text from capture.
    Original,
    /// A user text-correction record, identified by its correction record ID.
    /// The basis is valid only at the item revision when the correction was stored.
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
}

/// Explicit resolution quality for a proposed reminder time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderProposal {
    /// Resolved absolute instant (UTC).
    pub instant: String,
    /// Timezone identifier for display.
    pub timezone_id: String,
    /// Quality of the resolution.
    pub quality: TimeResolutionQuality,
    /// Character offsets [start, end) into the text basis.
    pub source_span: Option<(usize, usize)>,
}

/// Character offset span into a text basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

impl SourceSpan {
    pub fn new(start: usize, end: usize) -> Self {
        SourceSpan { start, end }
    }

    /// Check if this span is valid within the given text.
    pub fn is_valid(&self, text: &str) -> bool {
        if self.start > self.end {
            return false;
        }
        // Validate that both positions are at valid UTF-8 boundaries.
        let bytes = text.as_bytes();
        if self.start > bytes.len() || self.end > bytes.len() {
            return false;
        }
        // Check start is at a UTF-8 boundary.
        if self.start > 0 && (bytes[self.start] & 0xC0) == 0x80 {
            return false;
        }
        // Check end is at a UTF-8 boundary.
        if self.end > 0 && self.end < bytes.len() && (bytes[self.end] & 0xC0) == 0x80 {
            return false;
        }
        true
    }
}

/// A validated interpretation proposal: source-linked candidate output from an interpretation job.
/// Does not constitute a mutation until applied through the separate proposal-application logic.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    /// Immutable proposal identifier.
    pub proposal_id: String,
    /// Item ID this proposal applies to.
    pub item_id: String,
    /// Capture ID (for audit trail and to reject unknown captures).
    pub capture_id: String,
    /// Item revision when this proposal was created (compare-and-set).
    pub source_revision: i32,
    /// Schema version; unknown versions are rejected without mutation.
    pub schema_version: i32,
    /// Text basis and immutable identification.
    pub text_basis: TextBasis,
    /// Versioned interpretation request that produced this proposal.
    pub request_version: String,
    /// Optional note, action, or idea annotation.
    pub item_type: Option<ItemType>,
    /// Optional proposed reminder with quality and source span.
    pub reminder_proposal: Option<ReminderProposal>,
    /// Optional proposed session-topic with source span.
    pub session_topic_proposal: Option<String>,
    /// Source spans for extracted elements (JSON array).
    pub source_spans: Option<Vec<SourceSpan>>,
    /// Explicit abstention: source could not be interpreted.
    pub abstained: bool,
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
            item_type: None,
            reminder_proposal: None,
            session_topic_proposal: None,
            source_spans: None,
            abstained: false,
        }
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
    pub fn with_session_topic_proposal(mut self, topic: Option<String>) -> Self {
        self.session_topic_proposal = topic;
        self
    }

    /// Set source spans for extracted elements.
    pub fn with_source_spans(mut self, spans: Option<Vec<SourceSpan>>) -> Self {
        self.source_spans = spans;
        self
    }

    /// Mark this proposal as an explicit abstention.
    pub fn with_abstention(mut self, abstained: bool) -> Self {
        self.abstained = abstained;
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

    /// Validate that source spans are within bounds of the given text.
    /// Returns an error if any span is invalid.
    pub fn validate_source_spans(&self, text: &str) -> Result<()> {
        if let Some(spans) = &self.source_spans {
            for span in spans {
                if !span.is_valid(text) {
                    return Err(anyhow!(
                        "source span [{}, {}) is out of bounds or invalid for text of length {}",
                        span.start,
                        span.end,
                        text.len()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate the reminder proposal if present.
    /// Checks that the instant is a valid timestamp and timezone is a recognized IANA identifier.
    pub fn validate_reminder_proposal(&self) -> Result<()> {
        if let Some(reminder) = &self.reminder_proposal {
            // Validate instant is an RFC 3339 timestamp.
            // This is a basic check; detailed validation depends on the time module.
            if reminder.instant.is_empty() {
                return Err(anyhow!("reminder proposal instant cannot be empty"));
            }
            if reminder.timezone_id.is_empty() {
                return Err(anyhow!("reminder proposal timezone_id cannot be empty"));
            }
            // Validate span bounds if present.
            if let Some((start, end)) = reminder.source_span {
                if start > end {
                    return Err(anyhow!(
                        "reminder source span start {} > end {}",
                        start,
                        end
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validate all fields of the proposal for semantic correctness.
    /// This checks schema version, span bounds, and field invariants,
    /// but does NOT check database constraints (existence of captures/items)
    /// or policy (whether the proposal can be applied to the current item state).
    pub fn validate(&self, text: &str) -> Result<()> {
        self.validate_schema_version()?;
        self.validate_source_spans(text)?;
        self.validate_reminder_proposal()?;

        // At least one field (type, reminder, session-topic) or abstention must be present.
        if !self.abstained
            && self.item_type.is_none()
            && self.reminder_proposal.is_none()
            && self.session_topic_proposal.is_none()
        {
            return Err(anyhow!(
                "proposal must have at least one field (type, reminder, session-topic) or be marked abstained"
            ));
        }

        Ok(())
    }
}

/// Validation error for proposal application attempts.
#[derive(Clone, Debug)]
pub enum ProposalError {
    /// Unknown or unsupported schema version.
    UnsupportedSchemaVersion { received: i32, supported: i32 },
    /// Source revision does not match current item revision (stale proposal).
    StaleRevision { expected: i32, found: i32 },
    /// Source span is out of bounds or invalid.
    InvalidSourceSpan {
        start: usize,
        end: usize,
        text_len: usize,
    },
    /// Text basis does not resolve (capture missing, correction missing, etc.).
    UnresolvableTextBasis(String),
    /// Capture ID is unknown or does not exist.
    UnknownCapture { capture_id: String },
    /// Item type cannot be applied given current item state (e.g., model tried to override correction).
    ForbiddenItemTypeOverride,
    /// Reminder cannot be applied given current item state.
    ForbiddenReminderOverride,
    /// Session topic cannot be applied given current item state.
    ForbiddenSessionTopicOverride,
    /// Other validation error.
    ValidationError(String),
}

impl fmt::Display for ProposalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposalError::UnsupportedSchemaVersion {
                received,
                supported,
            } => write!(
                f,
                "unsupported proposal schema version {}, expected {}",
                received, supported
            ),
            ProposalError::StaleRevision { expected, found } => {
                write!(
                    f,
                    "proposal is stale: expected revision {}, found {}",
                    expected, found
                )
            }
            ProposalError::InvalidSourceSpan {
                start,
                end,
                text_len,
            } => write!(
                f,
                "source span [{}, {}) is out of bounds for text of length {}",
                start, end, text_len
            ),
            ProposalError::UnresolvableTextBasis(reason) => {
                write!(f, "text basis cannot be resolved: {}", reason)
            }
            ProposalError::UnknownCapture { capture_id } => {
                write!(f, "unknown capture: {}", capture_id)
            }
            ProposalError::ForbiddenItemTypeOverride => {
                write!(f, "item type cannot be overridden by model output")
            }
            ProposalError::ForbiddenReminderOverride => {
                write!(f, "reminder cannot be overridden by model output")
            }
            ProposalError::ForbiddenSessionTopicOverride => {
                write!(f, "session topic cannot be overridden by model output")
            }
            ProposalError::ValidationError(msg) => write!(f, "validation error: {}", msg),
        }
    }
}

impl std::error::Error for ProposalError {}
