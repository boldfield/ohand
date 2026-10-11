//! The paired report. Every figure is a count next to its denominator; there is no score, no
//! ranking and no winner. Objective correctness, disagreement and preference are separate
//! sections, and an arm without a response is counted as such, never as an abstention.

use ohand_core::review::compare::ArmLabel;
use serde::Serialize;
use std::collections::BTreeMap;

use super::input::{arm_key, Label, NoResponseKind, ReasonCode};
use super::pair::{Disagreement, DisagreementFacet};
use crate::corpus::Category;
use crate::report::{CaseReport, ExecutionKind, Totals};

pub const COMPARISON_REPORT_FORMAT_VERSION: u32 = 1;

pub const NOTICES: [&str; 5] = [
    "Each arm is scored against the synthetic fixture oracle in its own scratch store; no real store is opened and no provider is called by this scorer.",
    "An arm without a response (failed, timed out, cancelled, unknown, skipped or missing) is never an abstention and is not scored.",
    "Disagreement means two valid proposals differ; it is not accuracy, and agreement is not correctness.",
    "Objective correctness is reported per arm against the oracle; preference is collected elsewhere and is not part of this report.",
    "Results labeled fake or recorded say nothing about live provider quality.",
];

/// How a pair stands. The three are exclusive and add up to the eligible count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    /// Both arms returned a response.
    Matched,
    /// At least one arm failed or timed out.
    Failed,
    /// No arm failed, but at least one has no response (cancelled, unknown, skipped, missing).
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct SideReport {
    pub arm: &'static str,
    /// `completed` or one of the no-response kinds.
    pub status: SideStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ReasonCode>,
    /// Present only for a completed arm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scored: Option<CaseReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideStatus {
    Completed,
    Failed,
    TimedOut,
    Cancelled,
    Unknown,
    Skipped,
    Missing,
}

impl From<NoResponseKind> for SideStatus {
    fn from(kind: NoResponseKind) -> SideStatus {
        match kind {
            NoResponseKind::Failed => SideStatus::Failed,
            NoResponseKind::TimedOut => SideStatus::TimedOut,
            NoResponseKind::Cancelled => SideStatus::Cancelled,
            NoResponseKind::Unknown => SideStatus::Unknown,
            NoResponseKind::Skipped => SideStatus::Skipped,
            NoResponseKind::Missing => SideStatus::Missing,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PairReport {
    pub fixture_id: String,
    pub category: Category,
    pub pairing: Pairing,
    pub sides: [SideReport; 2],
    pub disagreement: Disagreement,
    /// Present for matched pairs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<PairObjective>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PairObjective {
    pub a_clean: bool,
    pub b_clean: bool,
}

/// Responses and non-responses of one arm, with the eligible count as their denominator.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SideCounts {
    pub eligible: u64,
    pub completed: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub cancelled: u64,
    pub unknown: u64,
    pub skipped: u64,
    pub missing: u64,
    /// Non-responses by `status/reason`.
    pub reasons: BTreeMap<String, u64>,
}

impl SideCounts {
    pub(super) fn record(&mut self, status: SideStatus, reason: Option<&ReasonCode>) {
        self.eligible += 1;
        let counter = match status {
            SideStatus::Completed => &mut self.completed,
            SideStatus::Failed => &mut self.failed,
            SideStatus::TimedOut => &mut self.timed_out,
            SideStatus::Cancelled => &mut self.cancelled,
            SideStatus::Unknown => &mut self.unknown,
            SideStatus::Skipped => &mut self.skipped,
            SideStatus::Missing => &mut self.missing,
        };
        *counter += 1;
        if let Some(reason) = reason {
            let status_name = serde_json::to_value(status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            *self
                .reasons
                .entry(format!("{status_name}/{}", reason.as_str()))
                .or_default() += 1;
        }
    }
}

/// Invalid outputs among an arm's responses, counted apart from abstentions.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct InvalidOutputs {
    /// Responses the instruction mapping refused.
    pub mapping_rejected: u64,
    /// Proposals the application guard refused.
    pub guard_rejected: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArmSummary {
    pub arm: &'static str,
    pub profile: Label,
    pub sides: SideCounts,
    pub invalid_outputs: InvalidOutputs,
    /// E01 totals over this arm's completed responses. `totals.cases` is their denominator.
    pub totals: Totals,
    pub by_category: BTreeMap<Category, Totals>,
}

/// Pair counts. `matched + failed + missing == eligible`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PairCounts {
    pub eligible: u64,
    pub matched: u64,
    pub failed: u64,
    pub missing: u64,
    /// Eligible pairs with no outcome record at all (a subset of `missing`).
    pub no_record: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DisagreementSummary {
    /// Denominator: matched pairs.
    pub matched_pairs: u64,
    pub comparable: u64,
    pub not_comparable: u64,
    pub agree: u64,
    pub differ: u64,
    pub by_facet: BTreeMap<DisagreementFacet, u64>,
}

/// Cross-tabulation of per-arm objective cleanliness over matched pairs, kept apart from
/// disagreement.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PairedObjective {
    /// Denominator: matched pairs.
    pub matched_pairs: u64,
    pub both_clean: u64,
    pub only_a_clean: u64,
    pub only_b_clean: u64,
    pub neither_clean: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PreferenceSection {
    /// Blinded preferences are recorded by a different component.
    NotCollected,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusSummary {
    pub version: String,
    pub fixtures: u64,
    pub eligible: u64,
    pub provenance: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonReport {
    pub report_format_version: u32,
    pub instruction_version: String,
    pub experiment_id: Label,
    pub execution: ExecutionKind,
    pub corpus: CorpusSummary,
    pub arms: [ArmSummary; 2],
    pub pairs: PairCounts,
    pub disagreement: DisagreementSummary,
    pub paired_objective: PairedObjective,
    pub preference: PreferenceSection,
    pub cases: Vec<PairReport>,
    pub notices: Vec<&'static str>,
}

impl ComparisonReport {
    pub fn arm(&self, arm: ArmLabel) -> &ArmSummary {
        &self.arms[match arm {
            ArmLabel::A => 0,
            ArmLabel::B => 1,
        }]
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("report serializes");
        text.push('\n');
        text
    }
}

pub(super) fn new_arm_summary(arm: ArmLabel, profile: Label) -> ArmSummary {
    ArmSummary {
        arm: arm_key(arm),
        profile,
        sides: SideCounts::default(),
        invalid_outputs: InvalidOutputs::default(),
        totals: Totals::empty(),
        by_category: BTreeMap::new(),
    }
}
