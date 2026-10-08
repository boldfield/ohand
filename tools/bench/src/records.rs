//! Versioned experiment, case, and attempt records for synthetic benchmarking.
//!
//! Records pin the corpus revision, source context, instruction version, and profile/model
//! selections to ensure reproducibility and explicit comparison boundaries. Unknown metadata
//! marks unavailable or unsupported values rather than defaulting them.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::guard::{is_secret_or_endpoint, json_is_secret_or_endpoint};

fn check_required_field(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    check_optional_field(name, value)
}

fn check_optional_field(name: &str, value: &str) -> Result<(), String> {
    if is_secret_or_endpoint(value) {
        return Err(format!(
            "{name} must not contain credentials, private endpoints, or database inputs"
        ));
    }
    Ok(())
}

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
    /// Returns error if any required identifier is empty or contains credentials/secrets.
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

        let experiment = Experiment {
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
        };
        experiment.validate()?;
        Ok(experiment)
    }

    /// Check every persisted string: required identifiers are non-empty and no field carries
    /// credentials, private endpoints, or database inputs.
    pub fn validate(&self) -> Result<(), String> {
        check_required_field("id", &self.id)?;
        check_required_field("corpus_version", &self.corpus_version)?;
        check_required_field("source_context", &self.source_context)?;
        check_required_field("instruction_version", &self.instruction_version)?;
        check_required_field("profile_a_id", &self.profile_a_id)?;
        check_required_field("profile_a_version", &self.profile_a_version)?;
        check_required_field("profile_b_id", &self.profile_b_id)?;
        check_required_field("profile_b_version", &self.profile_b_version)?;
        check_required_field("build_revision", &self.build_revision)?;
        for field in &self.unknown_metadata.unknown_fields {
            check_required_field("unknown_metadata", field)?;
        }
        Ok(())
    }

    /// Set explicit unknown metadata fields.
    pub fn set_unknown_fields(&mut self, fields: Vec<String>) -> Result<(), String> {
        for field in &fields {
            check_required_field("unknown_metadata", field)?;
        }
        self.unknown_metadata.unknown_fields = fields;
        Ok(())
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

        let case = Case {
            id: Uuid::new_v4().to_string(),
            experiment_id,
            case_id,
            content,
            schema_version: 1,
        };
        case.validate()?;
        Ok(case)
    }

    /// Check required identifiers and that no field carries credentials, private endpoints,
    /// or database inputs.
    pub fn validate(&self) -> Result<(), String> {
        check_required_field("id", &self.id)?;
        check_required_field("experiment_id", &self.experiment_id)?;
        check_required_field("case_id", &self.case_id)?;
        check_required_field("content", &self.content)
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

        let attempt = Attempt {
            id: Uuid::new_v4().to_string(),
            case_id,
            arm,
            state: AttemptState::Started,
            elapsed_ms: None,
            usage_tokens: None,
            failure_reason: None,
            provider_output: None,
            schema_version: 1,
        };
        attempt.validate()?;
        Ok(attempt)
    }

    /// Check required identifiers, the arm, and that no field carries credentials, private
    /// endpoints, or database inputs.
    pub fn validate(&self) -> Result<(), String> {
        check_required_field("id", &self.id)?;
        check_required_field("case_id", &self.case_id)?;
        if self.arm != "a" && self.arm != "b" {
            return Err("arm must be 'a' or 'b'".to_string());
        }
        if let Some(reason) = &self.failure_reason {
            check_optional_field("failure_reason", reason)?;
        }
        if let Some(output) = &self.provider_output {
            if json_is_secret_or_endpoint(output) {
                return Err("provider_output must not contain credentials, private endpoints, or database inputs".to_string());
            }
        }
        Ok(())
    }

    /// Mark this attempt as completed with elapsed time.
    /// Returns error if output contains credentials or secret patterns.
    pub fn complete(
        &mut self,
        elapsed_ms: u64,
        output: Option<serde_json::Value>,
    ) -> Result<(), String> {
        if let Some(out) = &output {
            if json_is_secret_or_endpoint(out) {
                return Err("provider_output must not contain credentials, private endpoints, or database inputs".to_string());
            }
        }
        self.state = AttemptState::Completed;
        self.elapsed_ms = Some(elapsed_ms);
        self.provider_output = output;
        Ok(())
    }

    /// Mark this attempt as failed with a reason.
    /// Returns error if reason contains credentials or secret patterns.
    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), String> {
        let reason_str = reason.into();
        check_optional_field("failure_reason", &reason_str)?;
        self.state = AttemptState::Failed;
        self.failure_reason = Some(reason_str);
        Ok(())
    }

    /// Mark this attempt state as unknown with a reason.
    /// Returns error if reason contains credentials or secret patterns.
    pub fn mark_unknown(&mut self, reason: impl Into<String>) -> Result<(), String> {
        let reason_str = reason.into();
        check_optional_field("failure_reason", &reason_str)?;
        self.state = AttemptState::Unknown;
        self.failure_reason = Some(reason_str);
        Ok(())
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
        let base_args = (
            "v1.0",
            "2026-10-08T12:00:00Z",
            "instr",
            "pa",
            "1",
            "pb",
            "1",
            "build",
        );

        let result = Experiment::new(
            "",
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("corpus_version"));

        let result = Experiment::new(
            base_args.0,
            "",
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("source_context"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            "",
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("instruction_version"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            base_args.2,
            "",
            base_args.4,
            base_args.5,
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("profile_a_id"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            base_args.2,
            base_args.3,
            "",
            base_args.5,
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("profile_a_version"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            "",
            base_args.6,
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("profile_b_id"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            "",
            base_args.7,
        );
        assert!(result.is_err() && result.unwrap_err().contains("profile_b_version"));

        let result = Experiment::new(
            base_args.0,
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
            "",
        );
        assert!(result.is_err() && result.unwrap_err().contains("build_revision"));
    }

    #[test]
    fn test_experiment_rejects_credentials_in_source_context() {
        let base_args = ("v1.0", "instr", "pa", "1", "pb", "1", "build");

        let result = Experiment::new(
            "v1",
            "http://user:pass@example.com",
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "/var/mobile/ohand/production.sqlite",
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "https://user:token@api.internal/path",
            base_args.1,
            base_args.2,
            base_args.3,
            base_args.4,
            base_args.5,
            base_args.6,
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));
    }

    #[test]
    fn test_experiment_rejects_credentials_in_profile_fields() {
        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "https://10.0.0.1:8080/api",
            "1",
            "pb",
            "1",
            "build",
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "pa",
            "https://api.internal/token",
            "pb",
            "1",
            "build",
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "pa",
            "1",
            "http://user:pass@api.internal",
            "1",
            "build",
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "pa",
            "1",
            "pb",
            "Bearer sk-secret123",
            "build",
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let result = Experiment::new(
            "v1",
            "ctx",
            "instr",
            "pa",
            "1",
            "pb",
            "1",
            "/var/mobile/ohand/production.sqlite",
        );
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));
    }

    #[test]
    fn test_experiment_unknown_metadata_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let mut exp = Experiment::new(
            "v1.0",
            "2026-10-08T12:00:00Z",
            "instr-v1",
            "profile-a",
            "1.0",
            "profile-b",
            "1.0",
            "build-123",
        )?;

        assert_eq!(exp.unknown_metadata.unknown_fields.len(), 0);

        exp.set_unknown_fields(vec!["field1".to_string(), "field2".to_string()])?;
        assert_eq!(exp.unknown_metadata.unknown_fields.len(), 2);

        let json = serde_json::to_string(&exp)?;
        let deserialized: Experiment = serde_json::from_str(&json)?;
        assert_eq!(
            deserialized.unknown_metadata.unknown_fields,
            exp.unknown_metadata.unknown_fields
        );

        Ok(())
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

        assert!(attempt
            .complete(1500, Some(serde_json::json!({"result": "ok"})))
            .is_ok());
        assert_eq!(attempt.state, AttemptState::Completed);
        assert_eq!(attempt.elapsed_ms, Some(1500));

        let mut attempt2 = Attempt::new("case-2", "b").unwrap();
        assert!(attempt2.fail("connection timeout").is_ok());
        assert_eq!(attempt2.state, AttemptState::Failed);
        assert!(attempt2.failure_reason.is_some());
    }

    #[test]
    fn test_attempt_rejects_invalid_arm() {
        let result = Attempt::new("case-1", "c");
        assert!(result.is_err());
    }

    #[test]
    fn test_attempt_rejects_credentials_in_failure_reason() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("error at http://user:token@api.example.com");
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));

        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("failed at /var/mobile/production.db");
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));
    }

    #[test]
    fn test_attempt_rejects_credentials_in_unknown_reason() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.mark_unknown("unknown: password=secret123");
        assert!(result.is_err() && result.unwrap_err().contains("credentials"));
    }

    #[test]
    fn test_attempt_accepts_normal_failure_reason() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("timeout after 30s");
        assert!(result.is_ok());
        assert_eq!(
            attempt.failure_reason,
            Some("timeout after 30s".to_string())
        );
    }

    #[test]
    fn test_attempt_accepts_token_count_failure() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("max output tokens exceeded");
        assert!(result.is_ok());
        assert_eq!(
            attempt.failure_reason,
            Some("max output tokens exceeded".to_string())
        );
    }

    #[test]
    fn test_attempt_accepts_normal_api_mention() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("failed to connect to remote API");
        assert!(result.is_ok());
        assert_eq!(
            attempt.failure_reason,
            Some("failed to connect to remote API".to_string())
        );
    }

    #[test]
    fn test_attempt_accepts_word_key_in_normal_context() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("monkey keystone not found");
        assert!(result.is_ok());
    }

    #[test]
    fn test_secret_heuristic_rejects_bearer_token() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("error: Bearer sk-secret-token");
        assert!(result.is_err());
    }

    #[test]
    fn test_secret_heuristic_rejects_api_key_value() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("config: api_key=sk-abc123");
        assert!(result.is_err());
    }

    #[test]
    fn test_secret_heuristic_rejects_url_userinfo() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("tried https://user:password@api.example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_secret_heuristic_rejects_production_db() {
        let mut attempt = Attempt::new("case-1", "a").unwrap();
        let result = attempt.fail("backed up /var/mobile/ohand/production.sqlite");
        assert!(result.is_err());
    }

    #[test]
    fn test_record_round_trip_serde() -> Result<(), Box<dyn std::error::Error>> {
        let exp = Experiment::new(
            "v1.0",
            "2026-10-08T12:00:00Z",
            "instr-v1",
            "profile-a",
            "1.0",
            "profile-b",
            "1.0",
            "build-123",
        )?;

        let case = Case::new(&exp.id, "case-001", "test content")?;
        let mut attempt = Attempt::new(&case.id, "a")?;
        attempt.complete(1500, Some(serde_json::json!({"result": "ok"})))?;

        let exp_json = serde_json::to_string(&exp)?;
        let case_json = serde_json::to_string(&case)?;
        let attempt_json = serde_json::to_string(&attempt)?;

        let exp_loaded: Experiment = serde_json::from_str(&exp_json)?;
        let case_loaded: Case = serde_json::from_str(&case_json)?;
        let attempt_loaded: Attempt = serde_json::from_str(&attempt_json)?;

        assert_eq!(exp_loaded, exp);
        assert_eq!(case_loaded, case);
        assert_eq!(attempt_loaded, attempt);

        Ok(())
    }
}
