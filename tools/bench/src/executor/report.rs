use ohand_core::providers::contracts::DiagnosticMetadata;
use ohand_core::review::compare::{ArmLabel, Comparability};

/// How one side of one case stands. A side is never silently counted as an abstention or a
/// success: everything other than `Completed` carries a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideKind {
    Completed,
    /// The provider call ended with an error.
    Failed,
    /// The call timed out; whether the provider finished it is unknown.
    TimedOut,
    /// The call was cancelled, or the run was cancelled before the side was sent.
    Cancelled,
    /// A request was reserved but its outcome was never recorded, for example after a crash.
    /// It may have been sent and paid for, and is never dispatched again without a rerun.
    Unknown,
    /// Nothing was sent in the latest run that considered the side.
    Skipped,
    /// The side has no record at all.
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SideReport {
    pub arm: ArmLabel,
    pub kind: SideKind,
    pub class: Option<String>,
    pub reason: Option<String>,
    /// The attempt whose outcome this is; `None` for skipped or missing sides.
    pub attempt_id: Option<String>,
    /// Requests dispatched for this side across every run of the journal.
    pub dispatched_attempts: u32,
    /// Requests this run reserved for the side.
    pub dispatched_this_run: u32,
    /// The attempt this one explicitly reran, when it was a rerun.
    pub rerun_of: Option<String>,
    pub output: Option<serde_json::Map<String, serde_json::Value>>,
    pub metadata: Option<DiagnosticMetadata>,
}

/// Whether the case's requests were derived and checked against what the journal holds.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigurationStatus {
    /// Both arms were derived this run and match any journaled attempts.
    Verified,
    /// No pair could be derived this run, so journaled results are preserved but were not
    /// checked against the current configuration.
    Unverified { reason: String },
}

#[derive(Debug, Clone)]
pub struct CaseReport {
    pub case_id: String,
    /// The recorded order, first arm first. `sides` follows it.
    pub arm_order: [ArmLabel; 2],
    pub sides: [SideReport; 2],
    pub configuration: ConfigurationStatus,
    /// Known configuration differences and unknown defaults; `None` when no pair was derived.
    pub comparability: Option<Comparability>,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub experiment_id: String,
    pub run: u32,
    pub request_cap: u32,
    /// Requests this run reserved. Never exceeds `request_cap`.
    pub reserved_requests: u32,
    pub budget_exhausted: bool,
    pub cancelled: bool,
    pub cases: Vec<CaseReport>,
}
