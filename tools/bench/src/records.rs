//! Versioned experiment, case, and attempt records for synthetic benchmarking.
//!
//! Records pin the corpus revision, source context, instruction version, and profile/model
//! selections to ensure reproducibility and explicit comparison boundaries. Unknown metadata
//! marks unavailable or unsupported values rather than defaulting them.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Versioned benchmark experiment pinning corpus, instructions, and profile selections.
///
/// An experiment specifies:
/// - The exact synthetic corpus revision (source identification)
/// - Instruction version used for all arms
/// - Time context (captured from the original request time, never replayed date)
/// - Exactly two authorized provider profiles with their versions
/// - Metadata about the experiment itself (build version, run id)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Experiment {
    /// Unique identifier for this experiment run.
    pub id: String,
    /// Experiment schema version for format evolution.
    pub schema_version: u32,
    /// Synthetic corpus revision/commit hash or version identifier.
    pub corpus_version: String,
    /// Source context: the original timestamp/timezone of the captured content.
    pub source_context: String,
    /// Instruction/prompt version used in all arms.
    pub instruction_version: String,
    /// Primary provider profile id and version.
    pub profile_a_id: String,
    pub profile_a_version: String,
    /// Challenger provider profile id and version.
    pub profile_b_id: String,
    pub profile_b_version: String,
    /// Build/app revision for diagnostic purposes.
    pub build_revision: String,
    /// Metadata not yet determined or unsupported.
    pub unknown_metadata: UnknownMetadata,
}

/// Explicit unknown metadata markers for optional fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnknownMetadata {
    /// Fields that are unknown or intentionally omitted.
    pub unknown_fields: Vec<String>,
}

impl Experiment {
    /// Create a new experiment with all required identifiers.
    /// Returns error if any required identifier is empty.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        corpus_version: impl Into<String>,
        source_context: impl Into<String>,
        instruction_version: impl Into<String>,
        profile_a_id: impl Into<String>,
        profile_a_version: impl Into<String>,
        profile_b_id: impl Into<String>,
        profile_b_version: impl Into<String>,
        build_revision: impl Into<String>,
    ) -> Result<Self, String> {
        let corpus_version = corpus_version.into();
        let source_context = source_context.into();
        let instruction_version = instruction_version.into();
        let profile_a_id = profile_a_id.into();
        let profile_a_version = profile_a_version.into();
        let profile_b_id = profile_b_id.into();
        let profile_b_version = profile_b_version.into();
        let build_revision = build_revision.into();

        if corpus_version.is_empty() {
            return Err("corpus_version must not be empty".to_string());
        }
        if source_context.is_empty() {
            return Err("source_context must not be empty".to_string());
        }
        if instruction_version.is_empty() {
            return Err("instruction_version must not be empty".to_string());
        }
        if profile_a_id.is_empty() {
            return Err("profile_a_id must not be empty".to_string());
        }
        if profile_a_version.is_empty() {
            return Err("profile_a_version must not be empty".to_string());
        }
        if profile_b_id.is_empty() {
            return Err("profile_b_id must not be empty".to_string());
        }
        if profile_b_version.is_empty() {
            return Err("profile_b_version must not be empty".to_string());
        }
        if build_revision.is_empty() {
            return Err("build_revision must not be empty".to_string());
        }

        Ok(Experiment {
            id: Uuid::new_v4().to_string(),
            schema_version: 1,
            corpus_version,
            source_context,
            instruction_version,
            profile_a_id,
            profile_a_version,
            profile_b_id,
            profile_b_version,
            build_revision,
            unknown_metadata: UnknownMetadata {
                unknown_fields: Vec::new(),
            },
        })
    }
}

/// A synthetic case within an experiment.
///
/// Cases represent individual test inputs from the synthetic corpus. Each case is
/// identified by the corpus and an offset/case_id within that corpus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Case {
    /// Unique identifier for this case.
    pub id: String,
    /// Experiment this case belongs to.
    pub experiment_id: String,
    /// Identifier within the corpus.
    pub case_id: String,
    /// Synthetic case content (safe to serialize; no production captures).
    pub content: String,
    /// Case schema version.
    pub schema_version: u32,
}

impl Case {
    /// Create a new case with validation.
    pub fn new(
        experiment_id: impl Into<String>,
        case_id: impl Into<String>,
        content: impl Into<String>,
    ) -> Result<Self, String> {
        let experiment_id = experiment_id.into();
        let case_id = case_id.into();
        let content = content.into();

        if experiment_id.is_empty() {
            return Err("experiment_id must not be empty".to_string());
        }
        if case_id.is_empty() {
            return Err("case_id must not be empty".to_string());
        }
        if content.is_empty() {
            return Err("content must not be empty".to_string());
        }

        Ok(Case {
            id: Uuid::new_v4().to_string(),
            experiment_id,
            case_id,
            content,
            schema_version: 1,
        })
    }
}

/// The outcome state of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// Attempt is currently in progress.
    Started,
    /// Attempt completed successfully with result available.
    Completed,
    /// Attempt failed with an error.
    Failed,
    /// Attempt state is ambiguous (e.g., sent but response unknown).
    Unknown,
}

/// A single attempt to run a case through one arm of an experiment.
///
/// Attempts track the outcome and metadata of running a case, including wall-clock time,
/// provider-reported usage (if available), and failure reasons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attempt {
    /// Unique identifier for this attempt.
    pub id: String,
    /// Case being attempted.
    pub case_id: String,
    /// Which arm: "a" or "b".
    pub arm: String,
    /// Current state of the attempt.
    pub state: AttemptState,
    /// Elapsed milliseconds for this attempt (known only after completion).
    pub elapsed_ms: Option<u64>,
    /// Provider-reported token usage if available and disclosed.
    pub usage_tokens: Option<UsageMetadata>,
    /// Human-readable failure reason if state is Failed or Unknown.
    pub failure_reason: Option<String>,
    /// Parsed provider output if state is Completed.
    pub provider_output: Option<serde_json::Value>,
    /// Attempt schema version.
    pub schema_version: u32,
}

/// Provider usage metadata (tokens, cost estimation support).
///
/// Cost is never included; cost rates are unknown and volatile. Token counts are
/// reported per-provider availability and must never be inferred when unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageMetadata {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl Attempt {
    /// Create a new attempt in the Started state.
    pub fn new(case_id: impl Into<String>, arm: impl Into<String>) -> Result<Self, String> {
        let case_id = case_id.into();
        let arm = arm.into();

        if case_id.is_empty() {
            return Err("case_id must not be empty".to_string());
        }
        if arm != "a" && arm != "b" {
            return Err("arm must be 'a' or 'b'".to_string());
        }

        Ok(Attempt {
            id: Uuid::new_v4().to_string(),
            case_id,
            arm,
            state: AttemptState::Started,
            elapsed_ms: None,
            usage_tokens: None,
            failure_reason: None,
            provider_output: None,
            schema_version: 1,
        })
    }

    /// Mark this attempt as completed with elapsed time.
    pub fn complete(&mut self, elapsed_ms: u64, output: Option<serde_json::Value>) {
        self.state = AttemptState::Completed;
        self.elapsed_ms = Some(elapsed_ms);
        self.provider_output = output;
    }

    /// Mark this attempt as failed with a reason.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.state = AttemptState::Failed;
        self.failure_reason = Some(reason.into());
    }

    /// Mark this attempt state as unknown with a reason.
    pub fn mark_unknown(&mut self, reason: impl Into<String>) {
        self.state = AttemptState::Unknown;
        self.failure_reason = Some(reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_experiment_creation() {
        let exp = Experiment::new(
            "v1.0",
            "2026-10-08T12:00:00Z",
            "instr-v1",
            "profile-a",
            "1.0",
            "profile-b",
            "1.0",
            "build-123",
        );
        assert!(exp.is_ok());
        let exp = exp.unwrap();
        assert_eq!(exp.schema_version, 1);
        assert_eq!(exp.corpus_version, "v1.0");
    }

    #[test]
    fn test_experiment_rejects_empty_identifiers() {
        let result = Experiment::new(
            "",
            "2026-10-08T12:00:00Z",
            "instr",
            "pa",
            "1",
            "pb",
            "1",
            "b",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("corpus_version"));
    }

    #[test]
    fn test_case_creation() {
        let case = Case::new("exp-1", "case-001", "test content");
        assert!(case.is_ok());
        let case = case.unwrap();
        assert_eq!(case.case_id, "case-001");
        assert_eq!(case.content, "test content");
    }

    #[test]
    fn test_case_rejects_empty_fields() {
        let result = Case::new("", "case-1", "content");
        assert!(result.is_err());
        let result = Case::new("exp-1", "", "content");
        assert!(result.is_err());
        let result = Case::new("exp-1", "case-1", "");
        assert!(result.is_err());
    }

    #[test]
    fn test_attempt_lifecycle() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        assert_eq!(attempt.state, AttemptState::Started);
        assert_eq!(attempt.arm, "a");

        attempt.complete(1500, Some(serde_json::json!({"result": "ok"})));
        assert_eq!(attempt.state, AttemptState::Completed);
        assert_eq!(attempt.elapsed_ms, Some(1500));

        let mut attempt2 = Attempt::new("case-2", "b").unwrap();
        attempt2.fail("connection timeout");
        assert_eq!(attempt2.state, AttemptState::Failed);
        assert!(attempt2.failure_reason.is_some());
    }

    #[test]
    fn test_attempt_rejects_invalid_arm() {
        let result = Attempt::new("case-1", "c");
        assert!(result.is_err());
    }
}
