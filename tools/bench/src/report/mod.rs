//! Readable reports of a paired benchmark journal.
//!
//! A report is built from one run journal, read without opening it for writing, and the
//! synthetic corpus it was run on. Objective correctness comes from the XB08 scorer unchanged;
//! this module adds the journal's operational facts (requests, retries, latency, provider-reported
//! tokens, an optional cost estimate) and an explicit statement of what the two arms do and do
//! not have in common.
//!
//! The report is diagnostic. It carries no winner and no score, activates nothing, and never
//! prints model output, request text, credentials or endpoints: per-case rows hold counts and
//! fixed vocabulary only, and every string that came from the journal is flattened and withheld
//! if it looks like a secret or an endpoint.

mod comparability;
mod load;
mod rates;
mod render;
mod stats;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::Path;

use evaluation::comparison::{
    score_comparison, ArmOutcome, ArmSummary, ComparisonInput, CorpusSummary, Disagreement,
    DisagreementSummary, Label, NoResponseKind, PairCounts, PairObjective, PairedObjective,
    PairedOutcome, Pairing, PreferenceSection, ReasonCode, SideStatus, NOTICES,
};
use evaluation::corpus::{Category, Corpus};
use evaluation::report::{ExecutionKind, ProducerOutcome};
use serde::Serialize;

use crate::records::AttemptState;
use comparability::Comparability;
use load::{AttemptRecord, Journal, Side};
pub use rates::CostRates;
pub use stats::{ArmOperations, CostSummary, LatencySummary, RequestCounts, UsageSummary};

pub const BENCH_REPORT_FORMAT_VERSION: u32 = 1;

/// Case rows beyond this many are counted in `cases_omitted` rather than listed.
pub const MAX_CASE_ROWS: usize = 100;
const MAX_FREE_TEXT: usize = 96;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ReportError(pub String);

/// Text from the journal, flattened to one line with control and bidirectional-formatting
/// characters replaced, or withheld when it looks like a secret or an endpoint.
pub(crate) fn safe(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|character| {
            let invisible = character.is_control()
                || matches!(
                    character,
                    '\u{200B}'..='\u{200F}'
                        | '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{FEFF}'
                );
            if invisible {
                ' '
            } else {
                character
            }
        })
        .take(MAX_FREE_TEXT)
        .collect();
    if crate::guard::is_secret_or_endpoint(&flat) {
        "[withheld]".to_string()
    } else {
        flat
    }
}

pub(crate) fn name_of<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    /// Always null: this report never chooses a model.
    pub winner: Option<String>,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub arm: &'static str,
    pub profile_id: String,
    pub profile_version: Label,
    /// From the provider-reported provenance of the arm's completed attempts, when any.
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExperimentSummary {
    pub id: Label,
    pub execution: ExecutionKind,
    pub corpus_version: String,
    pub instruction_version: String,
    pub build_revision: String,
    pub source_context: String,
    pub profiles: [ProfileSummary; 2],
    pub unknown_metadata_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JournalSummary {
    pub records: u64,
    pub runs: u32,
    /// The journal ends in a record that could not be read completely. The report ignores it and
    /// leaves the file untouched.
    pub torn_tail: bool,
    pub ambiguous_started_attempts: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SideRow {
    pub status: SideStatus,
    pub reason: Option<ReasonCode>,
    /// `proposed`, `abstained`, `mapping_rejected` ...; completed responses only.
    pub producer: Option<&'static str>,
    /// Candidate defect kinds, once each.
    pub defects: Vec<String>,
    pub abstention: Option<String>,
    pub forbidden_hits: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseRow {
    pub fixture_id: String,
    pub category: Category,
    pub pairing: Pairing,
    pub disagreement: Disagreement,
    pub objective: Option<PairObjective>,
    pub sides: [SideRow; 2],
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub report_format_version: u32,
    /// Always true: nothing in this report decides or activates anything.
    pub diagnostic_only: bool,
    pub selection: Selection,
    pub experiment: ExperimentSummary,
    pub journal: JournalSummary,
    pub corpus: CorpusSummary,
    pub arms: [ArmSummary; 2],
    pub pairs: PairCounts,
    pub disagreement: DisagreementSummary,
    pub paired_objective: PairedObjective,
    pub preference: PreferenceSection,
    pub operations: [ArmOperations; 2],
    pub comparability: Comparability,
    pub cases: Vec<CaseRow>,
    pub cases_omitted: u64,
    pub notices: Vec<&'static str>,
}

impl BenchReport {
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("report serializes");
        text.push('\n');
        text
    }

    pub fn to_text(&self) -> String {
        render::render_text(self)
    }
}

fn refused(message: &str) -> ReportError {
    ReportError(message.to_string())
}

fn execution_of(source_context: &str) -> Result<ExecutionKind, ReportError> {
    let mode = source_context
        .split_whitespace()
        .find_map(|token| token.strip_prefix("mode-"));
    match mode {
        Some("fake") => Ok(ExecutionKind::Fake),
        Some("live") => Ok(ExecutionKind::Live),
        _ => Err(refused(
            "the journal's experiment does not say whether it ran in fake or live mode",
        )),
    }
}

fn reason_of(code: Option<&str>, fallback: &str) -> ReasonCode {
    code.and_then(|code| ReasonCode::new(code).ok())
        .unwrap_or_else(|| ReasonCode::new(fallback).expect("fixed reason code is valid"))
}

fn arm_outcome(side: Side<'_>) -> Option<ArmOutcome> {
    let no_response =
        |kind: NoResponseKind, reason: ReasonCode| ArmOutcome::NoResponse { kind, reason };
    Some(match side {
        Side::Absent => return None,
        Side::Dispatched(attempt) => match attempt.state {
            AttemptState::Completed => match &attempt.envelope.output {
                Some(output) => ArmOutcome::completed(output.clone()),
                None => no_response(NoResponseKind::Failed, reason_of(None, "output_missing")),
            },
            AttemptState::Failed => {
                let class = attempt.envelope.class.as_deref();
                let kind = match class {
                    Some("timed_out") => NoResponseKind::TimedOut,
                    Some("cancelled") => NoResponseKind::Cancelled,
                    _ => NoResponseKind::Failed,
                };
                no_response(kind, reason_of(class, "unclassified"))
            }
            AttemptState::Started | AttemptState::Unknown => {
                no_response(NoResponseKind::Unknown, reason_of(None, "outcome_unknown"))
            }
        },
        Side::Skipped(attempt) => {
            let class = attempt.envelope.class.as_deref();
            let kind = if class == Some("cancelled") {
                NoResponseKind::Cancelled
            } else {
                NoResponseKind::Skipped
            };
            no_response(kind, reason_of(class, "unclassified"))
        }
    })
}

fn producer_name(producer: &ProducerOutcome) -> &'static str {
    match producer {
        ProducerOutcome::Proposed => "proposed",
        ProducerOutcome::Abstained => "abstained",
        ProducerOutcome::NotHandled => "not_handled",
        ProducerOutcome::ProviderFailure { .. } => "provider_failure",
        ProducerOutcome::MappingRejected { .. } => "mapping_rejected",
    }
}

fn side_row(side: &evaluation::comparison::SideReport) -> SideRow {
    let mut row = SideRow {
        status: side.status,
        reason: side.reason.clone(),
        producer: None,
        defects: Vec::new(),
        abstention: None,
        forbidden_hits: 0,
    };
    if let Some(scored) = &side.scored {
        row.producer = Some(producer_name(&scored.producer));
        let mut kinds: Vec<_> = scored
            .candidate_defects
            .iter()
            .map(|defect| defect.kind)
            .collect();
        kinds.sort();
        kinds.dedup();
        row.defects = kinds.iter().map(name_of).collect();
        row.abstention = scored.abstention.as_ref().map(name_of);
        row.forbidden_hits =
            (scored.candidate_forbidden.len() + scored.authoritative_forbidden.len()) as u64;
    }
    row
}

fn check_against_corpus(corpus: &Corpus, journal: &Journal) -> Result<(), ReportError> {
    if journal.experiment.corpus_version != corpus.version {
        return Err(refused(
            "the corpus is not the version the journal's experiment was run on",
        ));
    }
    if journal.cases.is_empty() {
        return Err(refused(
            "the journal records no cases, so there is nothing to report",
        ));
    }
    for case in &journal.cases {
        let fixture = corpus
            .fixture(&case.case_id)
            .ok_or_else(|| refused("the journal records a case the corpus does not contain"))?;
        let context = fixture
            .case_context()
            .map_err(|_| refused("a corpus fixture could not be read"))?;
        if context.basis_text() != case.content {
            return Err(refused(
                "a journaled case differs from the corpus fixture of the same id",
            ));
        }
    }
    Ok(())
}

fn label_of(text: &str, what: &str) -> Result<Label, ReportError> {
    Label::new(text).map_err(|_| ReportError(format!("the journal's {what} is not a plain label")))
}

/// Builds the report. Reads `journal_path` and never writes to it.
pub fn build_report(
    corpus: &Corpus,
    journal_path: &Path,
    rates: Option<&CostRates>,
) -> Result<BenchReport, ReportError> {
    let journal = load::load(journal_path)?;
    check_against_corpus(corpus, &journal)?;
    let experiment = &journal.experiment;
    let execution = execution_of(&experiment.source_context)?;
    let profile_versions = [
        label_of(&experiment.profile_a_version, "profile version")?,
        label_of(&experiment.profile_b_version, "profile version")?,
    ];

    let mut outcomes = Vec::new();
    for (case_index, case) in journal.cases.iter().enumerate() {
        let arm_a = arm_outcome(journal.side(case_index, 0));
        let arm_b = arm_outcome(journal.side(case_index, 1));
        if arm_a.is_none() && arm_b.is_none() {
            continue;
        }
        let missing = |outcome: Option<ArmOutcome>| {
            outcome.unwrap_or_else(|| ArmOutcome::NoResponse {
                kind: NoResponseKind::Missing,
                reason: reason_of(None, "no_attempt_recorded"),
            })
        };
        outcomes.push(PairedOutcome {
            fixture_id: case.case_id.clone(),
            arm_a: missing(arm_a),
            arm_b: missing(arm_b),
        });
    }
    let input = ComparisonInput {
        experiment_id: label_of(&experiment.id, "experiment id")?,
        execution,
        arm_profiles: profile_versions.clone(),
        eligible_fixture_ids: journal
            .cases
            .iter()
            .map(|case| case.case_id.clone())
            .collect(),
        outcomes,
    };
    let scored = score_comparison(corpus, &input)
        .map_err(|_| refused("the journal could not be scored against the corpus"))?;

    let matched_cases: HashSet<usize> = scored
        .cases
        .iter()
        .enumerate()
        .filter(|(_, case)| case.pairing == Pairing::Matched)
        .map(|(index, _)| index)
        .collect();
    let operations = [
        stats::arm_operations(
            &journal,
            0,
            &matched_cases,
            profile_versions[0].as_str(),
            rates,
        ),
        stats::arm_operations(
            &journal,
            1,
            &matched_cases,
            profile_versions[1].as_str(),
            rates,
        ),
    ];
    let comparability = comparability::assess(&journal, &scored, execution);

    let rows: Vec<CaseRow> = scored
        .cases
        .iter()
        .map(|case| CaseRow {
            fixture_id: safe(&case.fixture_id),
            category: case.category,
            pairing: case.pairing,
            disagreement: case.disagreement.clone(),
            objective: case.objective,
            sides: [side_row(&case.sides[0]), side_row(&case.sides[1])],
        })
        .collect();
    let cases_omitted = rows.len().saturating_sub(MAX_CASE_ROWS) as u64;
    let rows = rows.into_iter().take(MAX_CASE_ROWS).collect();

    let protocol_of = |arm: usize| comparability.arms[arm].protocol.clone();
    let profile = |arm: usize, id: &str| ProfileSummary {
        arm: if arm == 0 { "a" } else { "b" },
        profile_id: safe(id),
        profile_version: profile_versions[arm].clone(),
        protocol: protocol_of(arm),
    };
    let summary = ExperimentSummary {
        id: input.experiment_id.clone(),
        execution,
        corpus_version: safe(&experiment.corpus_version),
        instruction_version: safe(&experiment.instruction_version),
        build_revision: safe(&experiment.build_revision),
        source_context: safe(&experiment.source_context),
        profiles: [
            profile(0, &experiment.profile_a_id),
            profile(1, &experiment.profile_b_id),
        ],
        unknown_metadata_fields: experiment
            .unknown_metadata
            .unknown_fields
            .iter()
            .map(|field| safe(field))
            .collect(),
    };
    let runs = journal
        .attempts
        .iter()
        .map(|attempt: &AttemptRecord| attempt.envelope.run)
        .max()
        .unwrap_or(0);

    Ok(BenchReport {
        report_format_version: BENCH_REPORT_FORMAT_VERSION,
        diagnostic_only: true,
        selection: Selection {
            winner: None,
            note: "No winner is chosen and nothing is activated: this report is diagnostic evidence for a person to read.",
        },
        experiment: summary,
        journal: JournalSummary {
            records: journal.record_count as u64,
            runs,
            torn_tail: journal.torn_tail,
            ambiguous_started_attempts: journal.ambiguous_started_attempts as u64,
        },
        corpus: scored.corpus,
        arms: scored.arms,
        pairs: scored.pairs,
        disagreement: scored.disagreement,
        paired_objective: scored.paired_objective,
        preference: scored.preference,
        operations,
        comparability,
        cases: rows,
        cases_omitted,
        notices: NOTICES.to_vec(),
    })
}
