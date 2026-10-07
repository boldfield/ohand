use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::profile::ProviderProfile;
use crate::time::TimeContext;

/// Immutable identification of the exact text a request carries. Source spans in the
/// response refer to this text, never to the original when a correction was supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextBasis {
    Original {
        item_revision: u64,
    },
    Correction {
        correction_record_id: String,
        item_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestValidationError {
    #[error("{0} must be a UUID")]
    InvalidIdentifier(&'static str),
    #[error("request text must not be empty")]
    EmptyText,
    #[error("instruction version must not be empty")]
    EmptyInstructionVersion,
    #[error("route id must not be empty")]
    EmptyRoute,
    #[error("profile id must not be empty")]
    EmptyProfileId,
    #[error("text basis revision must equal the source revision")]
    RevisionMismatch,
}

/// One interpretation request. It carries everything needed to prove which immutable profile,
/// instruction set and source basis were dispatched, and nothing from earlier requests:
/// provider-side conversation history is never canonical memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "InterpretationRequestRecord")]
pub struct InterpretationRequest {
    capture_id: String,
    source_revision: u64,
    text_basis: TextBasis,
    text: String,
    request_version: String,
    instruction_version: String,
    profile_id: String,
    profile_version: String,
    /// Used only for authorization inside core; never disclosed to the interpreter.
    #[serde(skip_serializing, default)]
    route_id: String,
    time_context: TimeContext,
}

/// Unvalidated wire form; deserialization always passes through `InterpretationRequest::validate`.
/// `route_id` is never serialized, so a record without one is rejected rather than defaulted.
#[derive(Deserialize)]
struct InterpretationRequestRecord {
    capture_id: String,
    source_revision: u64,
    text_basis: TextBasis,
    text: String,
    request_version: String,
    instruction_version: String,
    profile_id: String,
    profile_version: String,
    #[serde(default)]
    route_id: String,
    time_context: TimeContext,
}

impl TryFrom<InterpretationRequestRecord> for InterpretationRequest {
    type Error = RequestValidationError;

    fn try_from(record: InterpretationRequestRecord) -> Result<Self, Self::Error> {
        let request = InterpretationRequest {
            capture_id: record.capture_id,
            source_revision: record.source_revision,
            text_basis: record.text_basis,
            text: record.text,
            request_version: record.request_version,
            instruction_version: record.instruction_version,
            profile_id: record.profile_id,
            profile_version: record.profile_version,
            route_id: record.route_id,
            time_context: record.time_context,
        };
        request.validate()?;
        Ok(request)
    }
}

impl InterpretationRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        capture_id: impl Into<String>,
        source_revision: u64,
        text_basis: TextBasis,
        text: impl Into<String>,
        request_version: impl Into<String>,
        instruction_version: impl Into<String>,
        profile: &ProviderProfile,
        route_id: impl Into<String>,
        time_context: TimeContext,
    ) -> Result<InterpretationRequest, RequestValidationError> {
        let request = InterpretationRequest {
            capture_id: capture_id.into(),
            source_revision,
            text_basis,
            text: text.into(),
            request_version: request_version.into(),
            instruction_version: instruction_version.into(),
            profile_id: profile.profile_id().to_string(),
            profile_version: profile.profile_version().to_string(),
            route_id: route_id.into(),
            time_context,
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), RequestValidationError> {
        use RequestValidationError as E;
        if Uuid::parse_str(&self.capture_id).is_err() {
            return Err(E::InvalidIdentifier("capture_id"));
        }
        if Uuid::parse_str(&self.request_version).is_err() {
            return Err(E::InvalidIdentifier("request_version"));
        }
        if Uuid::parse_str(&self.profile_version).is_err() {
            return Err(E::InvalidIdentifier("profile_version"));
        }
        if self.profile_id.trim().is_empty() {
            return Err(E::EmptyProfileId);
        }
        let basis_revision = match &self.text_basis {
            TextBasis::Original { item_revision } => *item_revision,
            TextBasis::Correction {
                correction_record_id,
                item_revision,
            } => {
                if Uuid::parse_str(correction_record_id).is_err() {
                    return Err(E::InvalidIdentifier("correction_record_id"));
                }
                *item_revision
            }
        };
        if basis_revision != self.source_revision {
            return Err(E::RevisionMismatch);
        }
        if self.text.trim().is_empty() {
            return Err(E::EmptyText);
        }
        if self.instruction_version.trim().is_empty() {
            return Err(E::EmptyInstructionVersion);
        }
        if self.route_id.trim().is_empty() {
            return Err(E::EmptyRoute);
        }
        Ok(())
    }

    pub fn capture_id(&self) -> &str {
        &self.capture_id
    }
    pub fn source_revision(&self) -> u64 {
        self.source_revision
    }
    pub fn text_basis(&self) -> &TextBasis {
        &self.text_basis
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn request_version(&self) -> &str {
        &self.request_version
    }
    pub fn instruction_version(&self) -> &str {
        &self.instruction_version
    }
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
    pub fn profile_version(&self) -> &str {
        &self.profile_version
    }
    pub fn route_id(&self) -> &str {
        &self.route_id
    }
    pub fn time_context(&self) -> &TimeContext {
        &self.time_context
    }

    pub fn is_pinned_to(&self, profile: &ProviderProfile) -> bool {
        self.profile_id == profile.profile_id() && self.profile_version == profile.profile_version()
    }
}
