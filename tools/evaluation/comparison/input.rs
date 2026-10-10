//! What the comparison scorer accepts: one outcome per arm per case, as plain data.
//!
//! A completed arm carries the provider's raw proposal map and nothing else. Every other
//! outcome is a typed non-result with a closed-vocabulary reason, so a failed, timed-out,
//! cancelled, unknown, skipped or missing arm can never be mistaken for an abstention, and no
//! free text (an endpoint, a credential, model prose) enters the report through a reason.

use ohand_core::review::compare::ArmLabel;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::report::ExecutionKind;
use crate::EvaluationError;

pub(super) fn arm_key(arm: ArmLabel) -> &'static str {
    match arm {
        ArmLabel::A => "a",
        ArmLabel::B => "b",
    }
}

pub(super) const ARMS: [ArmLabel; 2] = [ArmLabel::A, ArmLabel::B];

/// A short lowercase snake_case code such as `timed_out` or `not_authorized`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ReasonCode(String);

impl ReasonCode {
    pub const MAX_LEN: usize = 48;

    pub fn new(code: &str) -> Result<ReasonCode, EvaluationError> {
        let valid = !code.is_empty()
            && code.len() <= Self::MAX_LEN
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid {
            return Err(EvaluationError::Setup(format!(
                "a reason code is 1 to {} characters of a-z, 0-9 and underscore",
                Self::MAX_LEN
            )));
        }
        Ok(ReasonCode(code.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An identifier for an experiment or a profile version: letters, digits, `.`, `_` and `-`
/// only, so a URL, a path or a credential cannot be carried as a label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Label(String);

impl Label {
    pub const MAX_LEN: usize = 64;

    pub fn new(label: &str) -> Result<Label, EvaluationError> {
        let valid = !label.is_empty()
            && label.len() <= Self::MAX_LEN
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            return Err(EvaluationError::Setup(format!(
                "a label is 1 to {} characters of letters, digits, '.', '_' and '-'",
                Self::MAX_LEN
            )));
        }
        Ok(Label(label.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why an arm has no response to score. None of these is an abstention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoResponseKind {
    /// The provider call ended with an error.
    Failed,
    /// The call timed out; whether the provider finished it is unknown.
    TimedOut,
    /// The call, or the run, was cancelled before a response was recorded.
    Cancelled,
    /// A request was reserved but its outcome was never recorded.
    Unknown,
    /// Nothing was sent for this arm.
    Skipped,
    /// The arm has no record at all.
    Missing,
}

impl NoResponseKind {
    /// Failures count against the pair as failed; the rest leave it missing.
    pub(super) fn is_failure(self) -> bool {
        matches!(self, NoResponseKind::Failed | NoResponseKind::TimedOut)
    }
}

/// One arm's result for one case.
#[derive(Debug, Clone, PartialEq)]
pub enum ArmOutcome {
    /// The provider returned this proposal map. It may still be invalid output.
    Completed { output: Map<String, Value> },
    NoResponse {
        kind: NoResponseKind,
        reason: ReasonCode,
    },
}

impl ArmOutcome {
    pub fn completed(output: Map<String, Value>) -> ArmOutcome {
        ArmOutcome::Completed { output }
    }

    pub fn no_response(kind: NoResponseKind, reason: &str) -> Result<ArmOutcome, EvaluationError> {
        Ok(ArmOutcome::NoResponse {
            kind,
            reason: ReasonCode::new(reason)?,
        })
    }
}

/// Both arms' results for one fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedOutcome {
    pub fixture_id: String,
    pub arm_a: ArmOutcome,
    pub arm_b: ArmOutcome,
}

impl PairedOutcome {
    pub(super) fn arm(&self, arm: ArmLabel) -> &ArmOutcome {
        match arm {
            ArmLabel::A => &self.arm_a,
            ArmLabel::B => &self.arm_b,
        }
    }
}

/// A comparison to score. `eligible_fixture_ids` is the denominator: a pair that is eligible
/// but has no outcome is reported missing, never dropped.
#[derive(Debug, Clone)]
pub struct ComparisonInput {
    pub experiment_id: Label,
    /// How the outcomes were produced. The scorer makes no provider call itself, and
    /// `Deterministic` is not a model arm.
    pub execution: ExecutionKind,
    /// The profile/model version label of each arm, A first.
    pub arm_profiles: [Label; 2],
    pub eligible_fixture_ids: Vec<String>,
    pub outcomes: Vec<PairedOutcome>,
}
