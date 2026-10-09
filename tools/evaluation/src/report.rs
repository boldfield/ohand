//! The evaluation report: typed, deterministic and free of any single accuracy figure.
//!
//! Every number is a count with the denominator it is counted against next to it. Two runs over
//! the same corpus and responses produce byte-identical JSON (no timestamps, no paths, ordered
//! maps).

use serde::Serialize;
use std::collections::BTreeMap;

use crate::corpus::Category;
use crate::responses::Role;
use crate::score::{AbstentionOutcome, Defect, DefectKind, ForbiddenHit, MutationClass, Stage};
use crate::scratch::{AuthoritativeState, GuardOutcome};

pub const REPORT_FORMAT_VERSION: u32 = 1;
pub const CORPUS_PATH: &str = "fixtures/intent/contrastive-fixtures.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    /// The offline fast path, which needs no provider.
    Deterministic,
    /// Synthetic authored provider replies; proves the pipeline, says nothing about any model.
    Fake,
    /// Replies captured from a specific authorized live run, carrying `live_run` provenance.
    Recorded,
    /// A call to a live backend. This tool never makes one.
    Live,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunStatus {
    Executed,
    /// Not executed, therefore not scored. Never counts as passed.
    Unavailable {
        reason: String,
    },
}

/// What the interpreter side handed to the application guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProducerOutcome {
    /// A proposal with facets.
    Proposed,
    /// A proposal that abstains.
    Abstained,
    /// The fast path recognized nothing, so nothing reached the guard.
    NotHandled,
    /// The provider call failed before any usable reply.
    ProviderFailure { kind: String },
    /// The reply could not be mapped to a proposal; recorded as an invalid-output failure.
    MappingRejected { error: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
    pub case_id: String,
    pub fixture_id: String,
    pub category: Category,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub producer: ProducerOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<GuardOutcome>,
    pub candidate_defects: Vec<Defect>,
    pub authoritative_defects: Vec<Defect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstention: Option<AbstentionOutcome>,
    pub candidate_forbidden: Vec<ForbiddenHit>,
    pub authoritative_forbidden: Vec<ForbiddenHit>,
    pub authoritative_state: AuthoritativeState,
}

/// Counts and their denominators. There is deliberately no ratio and no score.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub cases: u64,
    pub oracle_abstains: u64,
    pub oracle_has_action: u64,
    pub oracle_has_resolved_instant: u64,
    pub oracle_has_no_reminder: u64,
    pub producer: BTreeMap<String, u64>,
    pub guard: BTreeMap<String, u64>,
    pub not_handled_where_oracle_has_facets: u64,
    pub candidate_defects: BTreeMap<DefectKind, u64>,
    pub authoritative_defects: BTreeMap<DefectKind, u64>,
    pub abstention: BTreeMap<AbstentionOutcome, u64>,
    pub candidate_forbidden: BTreeMap<MutationClass, u64>,
    pub authoritative_forbidden: BTreeMap<MutationClass, u64>,
}

impl Totals {
    pub fn empty() -> Totals {
        Totals {
            candidate_defects: DefectKind::ALL.iter().map(|kind| (*kind, 0)).collect(),
            authoritative_defects: DefectKind::ALL.iter().map(|kind| (*kind, 0)).collect(),
            abstention: AbstentionOutcome::ALL
                .iter()
                .map(|kind| (*kind, 0))
                .collect(),
            candidate_forbidden: MutationClass::ALL.iter().map(|class| (*class, 0)).collect(),
            authoritative_forbidden: MutationClass::ALL.iter().map(|class| (*class, 0)).collect(),
            ..Totals::default()
        }
    }

    pub fn authoritative_forbidden_total(&self) -> u64 {
        self.authoritative_forbidden.values().sum()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub execution: ExecutionKind,
    #[serde(flatten)]
    pub status: RunStatus,
    pub totals: Totals,
    pub cases: Vec<CaseReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusInfo {
    pub path: &'static str,
    pub version: String,
    pub fixtures: u64,
    pub provenance: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesInfo {
    pub version: String,
    pub scenarios: u64,
    pub synthetic_authored: u64,
    pub live_run: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub name: &'static str,
    pub passed: bool,
    /// Forbidden authoritative mutations summed over every executed run.
    pub forbidden_authoritative_mutations: u64,
    pub by_execution: BTreeMap<ExecutionKind, u64>,
    pub failures: Vec<String>,
    /// Execution kinds that were not run. They are not scored and never counted as passed.
    pub unavailable: Vec<ExecutionKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub report_format_version: u32,
    pub instruction_version: String,
    pub corpus: CorpusInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses: Option<ResponsesInfo>,
    pub runs: Vec<RunReport>,
    pub gate: Gate,
    pub notices: Vec<&'static str>,
}

pub const NOTICES: [&str; 4] = [
    "No accuracy score is computed: every figure is a count with its denominator, and agreement with the oracle is never summed into a rate.",
    "A fake run uses synthetic authored replies. It shows the pipeline's handling of those replies and says nothing about the quality of any live model.",
    "A recorded run is classified as such only when its scenarios carry complete live_run provenance; otherwise the scenario runs as fake.",
    "A live run is unavailable in this tool and is never scored as passed.",
];

impl Report {
    pub fn run(&self, execution: ExecutionKind) -> Option<&RunReport> {
        self.runs.iter().find(|run| run.execution == execution)
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("report serializes");
        text.push('\n');
        text
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "evaluation report (format {})\ncorpus {} ({} synthetic fixtures)\ninstructions {}\n",
            self.report_format_version,
            self.corpus.version,
            self.corpus.fixtures,
            self.instruction_version
        ));
        if let Some(responses) = &self.responses {
            out.push_str(&format!(
                "responses {} ({} scenarios: {} synthetic authored, {} live-run)\n",
                responses.version,
                responses.scenarios,
                responses.synthetic_authored,
                responses.live_run
            ));
        }
        for run in &self.runs {
            out.push('\n');
            out.push_str(&format!("[{:?}] ", run.execution).to_lowercase());
            match &run.status {
                RunStatus::Unavailable { reason } => {
                    out.push_str(&format!("unavailable, not scored: {reason}\n"));
                    continue;
                }
                RunStatus::Executed => out.push_str(&format!("{} cases\n", run.totals.cases)),
            }
            let totals = &run.totals;
            out.push_str(&format!(
                "  oracle: {} abstain, {} action, {} resolved deadline, {} no reminder\n",
                totals.oracle_abstains,
                totals.oracle_has_action,
                totals.oracle_has_resolved_instant,
                totals.oracle_has_no_reminder
            ));
            out.push_str(&format!("  producer: {}\n", join_counts(&totals.producer)));
            out.push_str(&format!("  guard: {}\n", join_counts(&totals.guard)));
            out.push_str(&format!(
                "  not handled where the oracle has facets: {}\n",
                totals.not_handled_where_oracle_has_facets
            ));
            out.push_str(&format!(
                "  candidate defects: {}\n",
                join_counts(&named(&totals.candidate_defects))
            ));
            out.push_str(&format!(
                "  authoritative defects: {}\n",
                join_counts(&named(&totals.authoritative_defects))
            ));
            out.push_str(&format!(
                "  abstentions: {}\n",
                join_counts(&named(&totals.abstention))
            ));
            out.push_str(&format!(
                "  forbidden, candidate stage: {}\n",
                join_counts(&named(&totals.candidate_forbidden))
            ));
            out.push_str(&format!(
                "  forbidden, authoritative: {}\n",
                join_counts(&named(&totals.authoritative_forbidden))
            ));
            for case in &run.cases {
                for hit in &case.authoritative_forbidden {
                    out.push_str(&format!(
                        "  FORBIDDEN {} {:?}: {}\n",
                        case.case_id, hit.class, hit.rule
                    ));
                }
            }
        }
        out.push_str(&format!(
            "\ngate {}: {} forbidden authoritative mutations\n",
            self.gate.name, self.gate.forbidden_authoritative_mutations
        ));
        out.push_str(if self.gate.passed {
            "gate result: pass\n"
        } else {
            "gate result: FAIL\n"
        });
        for failure in &self.gate.failures {
            out.push_str(&format!("  - {failure}\n"));
        }
        for execution in &self.gate.unavailable {
            out.push_str(&format!(
                "  not scored (unavailable): {}\n",
                format!("{execution:?}").to_lowercase()
            ));
        }
        for notice in &self.notices {
            out.push_str(&format!("note: {notice}\n"));
        }
        out
    }
}

fn named<K: Serialize + Ord + Copy>(map: &BTreeMap<K, u64>) -> BTreeMap<String, u64> {
    map.iter()
        .map(|(key, count)| {
            let name = serde_json::to_value(key)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            (name, *count)
        })
        .collect()
}

fn join_counts(map: &BTreeMap<String, u64>) -> String {
    if map.is_empty() {
        return "none".to_string();
    }
    map.iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fold one case into the run totals.
pub fn tally(totals: &mut Totals, case: &CaseReport, oracle: &crate::corpus::Expected) {
    use ohand_core::store::events::ItemType;
    totals.cases += 1;
    if oracle.abstention.is_some() {
        totals.oracle_abstains += 1;
    }
    if oracle.item_type == Some(ItemType::Action) {
        totals.oracle_has_action += 1;
    }
    if oracle
        .reminder_proposal
        .as_ref()
        .is_some_and(|reminder| reminder.instant.is_some())
    {
        totals.oracle_has_resolved_instant += 1;
    }
    if oracle.reminder_proposal.is_none() {
        totals.oracle_has_no_reminder += 1;
    }
    let producer_name = match &case.producer {
        ProducerOutcome::Proposed => "proposed",
        ProducerOutcome::Abstained => "abstained",
        ProducerOutcome::NotHandled => "not_handled",
        ProducerOutcome::ProviderFailure { .. } => "provider_failure",
        ProducerOutcome::MappingRejected { .. } => "mapping_rejected",
    };
    *totals
        .producer
        .entry(producer_name.to_string())
        .or_default() += 1;
    if case.producer == ProducerOutcome::NotHandled
        && (oracle.item_type.is_some()
            || oracle.reminder_proposal.is_some()
            || oracle.session_topic_proposal.is_some())
    {
        totals.not_handled_where_oracle_has_facets += 1;
    }
    if let Some(guard) = &case.guard {
        let guard_name = match guard {
            GuardOutcome::Applied { reminder } => format!("applied/{reminder}"),
            GuardOutcome::Abstained => "abstained".to_string(),
            GuardOutcome::FailedPermanently => "failed_permanently".to_string(),
            GuardOutcome::RetryLater => "retry_later".to_string(),
            GuardOutcome::Duplicate => "duplicate".to_string(),
            GuardOutcome::Rejected { .. } => "rejected".to_string(),
        };
        *totals.guard.entry(guard_name).or_default() += 1;
    }
    for defect in &case.candidate_defects {
        debug_assert_eq!(defect.stage, Stage::Candidate);
        *totals.candidate_defects.entry(defect.kind).or_default() += 1;
    }
    for defect in &case.authoritative_defects {
        debug_assert_eq!(defect.stage, Stage::Authoritative);
        *totals.authoritative_defects.entry(defect.kind).or_default() += 1;
    }
    if let Some(outcome) = case.abstention {
        *totals.abstention.entry(outcome).or_default() += 1;
    }
    for hit in &case.candidate_forbidden {
        *totals.candidate_forbidden.entry(hit.class).or_default() += 1;
    }
    for hit in &case.authoritative_forbidden {
        *totals.authoritative_forbidden.entry(hit.class).or_default() += 1;
    }
}
