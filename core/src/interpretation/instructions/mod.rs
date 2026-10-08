//! Versioned interpretation instructions and provider-neutral mapping.
//!
//! Owns the actual instructions, request context and response mapping used by interpretation
//! adapters. Instructions and output contracts are versioned, source text is treated as
//! untrusted data, and capture/profile/time context is explicit.
//!
//! Each instruction version is immutable and can only be superseded by a new version. Adapters
//! never use multiple versions of instructions in a single dispatch; all interpretation flows
//! through a versioned request with an explicit `instruction_version` in the contract.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

/// Schema version for interpretation instructions. Incremented when the instruction
/// contract or output format changes in a backward-incompatible way.
pub const INSTRUCTION_SCHEMA_VERSION: i32 = 1;

/// Error when instruction version is unknown, malformed, or incompatible.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InstructionError {
    #[error("instruction version {0} is not recognized")]
    UnknownVersion(String),
    #[error("instruction version must be a valid UUID")]
    InvalidVersionFormat,
    #[error("instruction content is empty")]
    EmptyContent,
    #[error("requested schema version {requested} does not match supported version {supported}")]
    SchemaMismatch { requested: i32, supported: i32 },
}

/// Immutable versioned instruction set for interpretation. Each version is identified by a
/// UUID and carries explicit metadata about capabilities, expected output format and
/// supported proposal types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSet {
    /// Immutable instruction version identifier (UUID).
    pub version: String,
    /// Schema version this instruction set uses.
    pub schema_version: i32,
    /// The actual instruction text to be sent to the provider. Treated as untrusted input
    /// at the provider level; contract validation happens at response parse time.
    pub instruction_text: String,
    /// Structured metadata about what this instruction set expects and supports.
    pub metadata: InstructionMetadata,
}

impl InstructionSet {
    /// Create a new instruction set with the given text. The version is generated as a UUID.
    pub fn new(instruction_text: impl Into<String>) -> Result<Self, InstructionError> {
        let text = instruction_text.into();
        if text.trim().is_empty() {
            return Err(InstructionError::EmptyContent);
        }
        let version = Uuid::new_v4().to_string();
        Ok(InstructionSet {
            version,
            schema_version: INSTRUCTION_SCHEMA_VERSION,
            instruction_text: text,
            metadata: InstructionMetadata::default(),
        })
    }

    /// Validate that this instruction set can process the given schema version.
    pub fn supports_schema_version(&self, requested: i32) -> Result<(), InstructionError> {
        if self.schema_version != requested {
            return Err(InstructionError::SchemaMismatch {
                requested,
                supported: self.schema_version,
            });
        }
        Ok(())
    }
}

/// Metadata describing what an instruction set expects and supports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionMetadata {
    /// Whether this instruction set expects a session-topic proposal in the response.
    pub supports_session_topic: bool,
    /// Whether this instruction set expects reminder proposals in the response.
    pub supports_reminders: bool,
    /// Whether this instruction set expects item-type proposals in the response.
    pub supports_item_type: bool,
    /// Whether this instruction set explicitly supports abstention as a first-class response.
    pub supports_abstention: bool,
    /// Supported proposed operations (e.g., "annotate", "update", "create").
    pub supported_operations: Vec<String>,
}

impl InstructionMetadata {
    /// Create default M1 metadata: supports session topics, reminders, item types and abstention.
    pub fn default_m1() -> Self {
        InstructionMetadata {
            supports_session_topic: true,
            supports_reminders: true,
            supports_item_type: true,
            supports_abstention: true,
            supported_operations: vec!["annotate".to_string()],
        }
    }
}

/// Request context that must accompany every interpretation request. This captures the
/// execution environment and ensures idempotency, audit trail, and deterministic behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    /// Unique identifier for this interpretation request (UUID).
    pub request_id: String,
    /// Capture that this request comes from (UUID).
    pub capture_id: String,
    /// Item being interpreted (UUID).
    pub item_id: String,
    /// Revision of the source at request time (for compare-and-set semantics).
    pub source_revision: u64,
    /// Immutable instruction version being used (UUID).
    pub instruction_version: String,
    /// Provider profile version being used (UUID).
    pub profile_version: String,
    /// Privacy route being used (explicit authorization).
    pub route_id: String,
    /// Capture timestamp and timezone for deterministic time resolution.
    pub capture_instant: String, // RFC 3339
    pub device_timezone: String, // IANA timezone
}

impl RequestContext {
    pub fn validate(&self) -> Result<(), InstructionError> {
        if Uuid::parse_str(&self.request_id).is_err() {
            return Err(InstructionError::InvalidVersionFormat);
        }
        if Uuid::parse_str(&self.capture_id).is_err() {
            return Err(InstructionError::InvalidVersionFormat);
        }
        if Uuid::parse_str(&self.item_id).is_err() {
            return Err(InstructionError::InvalidVersionFormat);
        }
        if Uuid::parse_str(&self.instruction_version).is_err() {
            return Err(InstructionError::InvalidVersionFormat);
        }
        if Uuid::parse_str(&self.profile_version).is_err() {
            return Err(InstructionError::InvalidVersionFormat);
        }
        if self.route_id.trim().is_empty() {
            return Err(InstructionError::EmptyContent);
        }
        if self.capture_instant.trim().is_empty() {
            return Err(InstructionError::EmptyContent);
        }
        if self.device_timezone.trim().is_empty() {
            return Err(InstructionError::EmptyContent);
        }
        Ok(())
    }
}

/// Provider-neutral interpretation request mapping. This structure contains everything
/// required to construct a request to any provider adapter, in a form independent of
/// the provider's specific protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationMapping {
    /// The instruction set to use.
    pub instructions: InstructionSet,
    /// Request context with capture, item, and profile information.
    pub context: RequestContext,
    /// The source text to interpret. Treated as untrusted data throughout processing.
    pub source_text: String,
}

impl InterpretationMapping {
    pub fn validate(&self) -> Result<(), InstructionError> {
        self.instructions
            .supports_schema_version(INSTRUCTION_SCHEMA_VERSION)?;
        if self.source_text.trim().is_empty() {
            return Err(InstructionError::EmptyContent);
        }
        self.context.validate()?;
        Ok(())
    }
}

/// Golden fixtures for testing interpretation instructions and provider adapters.
/// Each fixture represents a specific input scenario with expected and forbidden outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenFixture {
    /// Unique fixture identifier.
    pub id: String,
    /// Category of the fixture (e.g., "design", "negation", "session-topic", "abstention").
    pub category: String,
    /// Input text to interpret.
    pub input: String,
    /// Provenance (e.g., "synthetic, from DESIGN.md").
    pub provenance: String,
    /// Expected outcomes (item_type, reminder, session_topic, or abstention).
    pub expected: ExpectedOutcome,
    /// Forbidden outcomes that must never occur.
    pub forbidden: ForbiddenOutcome,
    /// Whether this fixture is recoverable (source can be preserved if processing fails).
    pub recoverable: bool,
    /// Explanation of why this fixture is important.
    pub notes: String,
}

/// Expected outcome from interpreting a fixture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpectedOutcome {
    pub item_type: Option<String>,
    pub reminder_quality: Option<String>,
    pub session_topic: Option<String>,
    pub abstention: Option<String>,
}

/// Forbidden outcomes that must never occur.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForbiddenOutcome {
    pub item_types: Vec<String>,
    pub reminder_qualities: Vec<String>,
    pub session_topics: Vec<String>,
    pub operations: Vec<String>,
}

/// Registry of golden fixtures by category.
pub struct FixtureRegistry {
    fixtures: HashMap<String, Vec<GoldenFixture>>,
}

impl FixtureRegistry {
    pub fn new() -> Self {
        FixtureRegistry {
            fixtures: HashMap::new(),
        }
    }

    pub fn register(&mut self, fixture: GoldenFixture) {
        self.fixtures
            .entry(fixture.category.clone())
            .or_default()
            .push(fixture);
    }

    pub fn get_fixtures_by_category(&self, category: &str) -> Option<&[GoldenFixture]> {
        self.fixtures.get(category).map(|v| v.as_slice())
    }

    pub fn all_fixtures(&self) -> Vec<&GoldenFixture> {
        self.fixtures.values().flat_map(|v| v.iter()).collect()
    }
}

impl Default for FixtureRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instruction_set_creation() {
        let text = "Interpret this as a note or action";
        let set = InstructionSet::new(text).expect("valid instruction set");
        assert_eq!(set.schema_version, INSTRUCTION_SCHEMA_VERSION);
        assert_eq!(set.instruction_text, text);
        assert!(!set.version.is_empty());
        Uuid::parse_str(&set.version).expect("version is valid UUID");
    }

    #[test]
    fn test_instruction_set_empty_text_rejected() {
        let result = InstructionSet::new("");
        assert_eq!(result, Err(InstructionError::EmptyContent));

        let result = InstructionSet::new("   ");
        assert_eq!(result, Err(InstructionError::EmptyContent));
    }

    #[test]
    fn test_instruction_set_schema_validation() {
        let set = InstructionSet::new("test").expect("valid");
        assert!(set
            .supports_schema_version(INSTRUCTION_SCHEMA_VERSION)
            .is_ok());
        assert!(set.supports_schema_version(2).is_err());
    }

    #[test]
    fn test_request_context_validation() {
        let request_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        let item_id = Uuid::new_v4().to_string();
        let instruction_version = Uuid::new_v4().to_string();
        let profile_version = Uuid::new_v4().to_string();

        let ctx = RequestContext {
            request_id: request_id.clone(),
            capture_id: capture_id.clone(),
            item_id: item_id.clone(),
            source_revision: 1,
            instruction_version: instruction_version.clone(),
            profile_version: profile_version.clone(),
            route_id: "general".to_string(),
            capture_instant: "2026-10-08T14:00:00Z".to_string(),
            device_timezone: "America/New_York".to_string(),
        };

        assert!(ctx.validate().is_ok());
    }

    #[test]
    fn test_request_context_invalid_uuid() {
        let ctx = RequestContext {
            request_id: "not-a-uuid".to_string(),
            capture_id: Uuid::new_v4().to_string(),
            item_id: Uuid::new_v4().to_string(),
            source_revision: 1,
            instruction_version: Uuid::new_v4().to_string(),
            profile_version: Uuid::new_v4().to_string(),
            route_id: "general".to_string(),
            capture_instant: "2026-10-08T14:00:00Z".to_string(),
            device_timezone: "America/New_York".to_string(),
        };

        assert!(ctx.validate().is_err());
    }

    #[test]
    fn test_interpretation_mapping_validation() {
        let request_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        let item_id = Uuid::new_v4().to_string();
        let instruction_version = Uuid::new_v4().to_string();
        let profile_version = Uuid::new_v4().to_string();

        let instructions = InstructionSet::new("test instruction").expect("valid");
        let context = RequestContext {
            request_id,
            capture_id,
            item_id,
            source_revision: 1,
            instruction_version,
            profile_version,
            route_id: "general".to_string(),
            capture_instant: "2026-10-08T14:00:00Z".to_string(),
            device_timezone: "America/New_York".to_string(),
        };

        let mapping = InterpretationMapping {
            instructions,
            context,
            source_text: "I need to call the roofer".to_string(),
        };

        assert!(mapping.validate().is_ok());
    }

    #[test]
    fn test_interpretation_mapping_empty_text_rejected() {
        let request_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        let item_id = Uuid::new_v4().to_string();
        let instruction_version = Uuid::new_v4().to_string();
        let profile_version = Uuid::new_v4().to_string();

        let instructions = InstructionSet::new("test instruction").expect("valid");
        let context = RequestContext {
            request_id,
            capture_id,
            item_id,
            source_revision: 1,
            instruction_version,
            profile_version,
            route_id: "general".to_string(),
            capture_instant: "2026-10-08T14:00:00Z".to_string(),
            device_timezone: "America/New_York".to_string(),
        };

        let mapping = InterpretationMapping {
            instructions,
            context,
            source_text: "".to_string(),
        };

        assert!(mapping.validate().is_err());
    }

    #[test]
    fn test_fixture_registry() {
        let mut registry = FixtureRegistry::new();

        let fixture1 = GoldenFixture {
            id: "design-broad-intention".to_string(),
            category: "design".to_string(),
            input: "Maybe a roof garden would be nice".to_string(),
            provenance: "synthetic, from DESIGN.md".to_string(),
            expected: ExpectedOutcome {
                item_type: Some("idea".to_string()),
                ..Default::default()
            },
            forbidden: ForbiddenOutcome {
                item_types: vec!["action".to_string()],
                ..Default::default()
            },
            recoverable: true,
            notes: "Broad intention must not create an obligation".to_string(),
        };

        registry.register(fixture1.clone());

        assert_eq!(
            registry.get_fixtures_by_category("design").unwrap().len(),
            1
        );
        assert_eq!(registry.all_fixtures().len(), 1);
    }

    #[test]
    fn test_instruction_metadata_default_m1() {
        let meta = InstructionMetadata::default_m1();
        assert!(meta.supports_session_topic);
        assert!(meta.supports_reminders);
        assert!(meta.supports_item_type);
        assert!(meta.supports_abstention);
        assert_eq!(meta.supported_operations, vec!["annotate"]);
    }
}
