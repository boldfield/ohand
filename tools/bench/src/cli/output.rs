//! Text rendering of the plan and of run reports, and the read-only view of an existing journal.
//!
//! Output carries identifiers, counts and fixed vocabulary only: never model output, request
//! text, credential references or endpoints. Free text from lower layers passes through
//! [`sanitize`], which withholds anything the journal guard would refuse to store.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use ohand_core::review::compare::ArmLabel;

use super::args::{protocol_label, Mode};
use super::profiles::ArmProfile;
use crate::executor::{PlannedCase, RunReport, SideKey, SideKind, SideReport};
use crate::guard::is_secret_or_endpoint;
use crate::journal::JournalReader;
use crate::records::Experiment;

const MAX_FREE_TEXT: usize = 160;

pub fn sanitize(text: &str) -> String {
    sanitize_to(text, MAX_FREE_TEXT)
}

pub fn sanitize_to(text: &str, limit: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(limit)
        .collect();
    if is_secret_or_endpoint(&flat) {
        "[withheld]".to_string()
    } else {
        flat
    }
}

pub fn arm_name(arm: ArmLabel) -> &'static str {
    match arm {
        ArmLabel::A => "a",
        ArmLabel::B => "b",
    }
}

pub fn mode_note(mode: Mode) -> &'static str {
    match mode {
        Mode::Fake => {
            "fake: scripted synthetic replies, no network, no credentials; a pipeline check, not \
             evidence about any provider"
        }
        Mode::Live => {
            "live: synthetic fixtures sent to the selected hosted providers; explicit network \
             execution"
        }
        Mode::Recorded => "recorded: reserved label, not available",
    }
}

/// What an existing journal already holds, read without opening it for writing.
pub struct PriorJournal {
    pub experiment: Option<Experiment>,
    /// Number of experiment records; anything but one, with other records present, makes the
    /// executor refuse the journal as inconsistent.
    pub experiment_count: usize,
    /// Whether the journal holds any case or attempt record.
    pub has_case_or_attempt: bool,
    /// Why the executor would refuse the journal's records when loading it, if it would.
    pub inconsistency: Option<String>,
    /// Journaled case text by corpus case id.
    pub content_by_case: HashMap<String, String>,
    /// Phase of the latest dispatched attempt per side: `completed`, `failed` or `reserved`.
    dispatched: HashMap<(String, &'static str), String>,
    orders: HashMap<String, [ArmLabel; 2]>,
}

fn arm_of(key: &str) -> Option<ArmLabel> {
    match key {
        "a" => Some(ArmLabel::A),
        "b" => Some(ArmLabel::B),
        _ => None,
    }
}

impl PriorJournal {
    pub fn read(path: &Path) -> Result<Option<PriorJournal>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let reader = JournalReader::open(path).map_err(|error| sanitize(&error.to_string()))?;
        let experiments = reader.experiments();
        let case_ids: HashMap<&str, &str> = reader
            .cases()
            .into_iter()
            .map(|case| (case.id.as_str(), case.case_id.as_str()))
            .collect();
        let content_by_case = reader
            .cases()
            .into_iter()
            .map(|case| (case.case_id.clone(), case.content.clone()))
            .collect();
        let has_case_or_attempt = !reader.cases().is_empty() || !reader.attempts().is_empty();
        let inconsistency = match experiments.as_slice() {
            [only] => load_inconsistency(&reader, only),
            _ => None,
        };
        let mut dispatched = HashMap::new();
        let mut orders = HashMap::new();
        for attempt in reader.attempts() {
            let Some(case_id) = case_ids.get(attempt.case_id.as_str()) else {
                continue;
            };
            let Some(arm) = arm_of(&attempt.arm) else {
                continue;
            };
            let Some(envelope) = attempt.provider_output.as_ref() else {
                continue;
            };
            if let Some([first, second]) = envelope["arm_order"].as_array().map(Vec::as_slice) {
                if let (Some(first), Some(second)) = (
                    first.as_str().and_then(arm_of),
                    second.as_str().and_then(arm_of),
                ) {
                    orders
                        .entry((*case_id).to_string())
                        .or_insert([first, second]);
                }
            }
            if envelope["dispatched"].as_bool() == Some(true) {
                let phase = envelope["phase"].as_str().unwrap_or("reserved").to_string();
                dispatched.insert(((*case_id).to_string(), arm_name(arm)), phase);
            }
        }
        Ok(Some(PriorJournal {
            experiment: if let [only] = experiments.as_slice() {
                Some((*only).clone())
            } else {
                None
            },
            experiment_count: experiments.len(),
            has_case_or_attempt,
            inconsistency,
            content_by_case,
            dispatched,
            orders,
        }))
    }

    pub fn has_dispatched(&self, case_id: &str, arm: ArmLabel) -> bool {
        self.side(case_id, arm).is_some()
    }

    fn side(&self, case_id: &str, arm: ArmLabel) -> Option<&str> {
        self.dispatched
            .get(&(case_id.to_string(), arm_name(arm)))
            .map(String::as_str)
    }
}

/// The first rule of the executor's journal load that `reader` breaks, in the order the executor
/// applies them: each case belongs to `experiment` and is journaled once, then each attempt names
/// a journaled case, carries an executor record of the supported version and a known arm. The
/// executor refuses such a journal only after opening it for writing, so the CLI checks first.
fn load_inconsistency(reader: &JournalReader, experiment: &Experiment) -> Option<String> {
    let mut case_ids = std::collections::HashSet::new();
    let mut record_ids = std::collections::HashSet::new();
    for case in reader.cases() {
        let case_id = sanitize(&case.case_id);
        if case.experiment_id != experiment.id {
            return Some(format!(
                "the journal's case {case_id} belongs to another experiment"
            ));
        }
        if !case_ids.insert(case.case_id.as_str()) {
            return Some(format!("the journal records case {case_id} more than once"));
        }
        record_ids.insert(case.id.as_str());
    }
    for attempt in reader.attempts() {
        if !record_ids.contains(attempt.case_id.as_str()) {
            return Some("the journal holds an attempt for a case it does not record".to_string());
        }
        let supported = attempt
            .provider_output
            .as_ref()
            .and_then(|envelope| envelope["executor_record"].as_u64())
            == Some(EXECUTOR_RECORD_VERSION);
        if !supported {
            return Some("the journal holds an attempt this executor did not write".to_string());
        }
        if arm_of(&attempt.arm).is_none() {
            return Some("the journal holds an attempt for an unknown arm".to_string());
        }
    }
    None
}

/// The executor's record version inside `Attempt::provider_output`.
const EXECUTOR_RECORD_VERSION: u64 = 1;

/// Experiment fields that differ, by name; the run would be refused when any do.
pub fn configuration_differences(stored: &Experiment, planned: &Experiment) -> Vec<&'static str> {
    [
        (
            "schema_version",
            stored.schema_version == planned.schema_version,
        ),
        (
            "corpus_version",
            stored.corpus_version == planned.corpus_version,
        ),
        (
            "source_context",
            stored.source_context == planned.source_context,
        ),
        (
            "instruction_version",
            stored.instruction_version == planned.instruction_version,
        ),
        ("profile_a_id", stored.profile_a_id == planned.profile_a_id),
        (
            "profile_a_version",
            stored.profile_a_version == planned.profile_a_version,
        ),
        ("profile_b_id", stored.profile_b_id == planned.profile_b_id),
        (
            "profile_b_version",
            stored.profile_b_version == planned.profile_b_version,
        ),
        (
            "build_revision",
            stored.build_revision == planned.build_revision,
        ),
        (
            "unknown_metadata",
            stored.unknown_metadata == planned.unknown_metadata,
        ),
    ]
    .into_iter()
    .filter(|(_, same)| !same)
    .map(|(field, _)| field)
    .collect()
}

pub struct PlanView<'a> {
    pub mode: Mode,
    pub experiment: &'a Experiment,
    pub corpus_size: usize,
    pub arm_a: &'a ArmProfile,
    pub arm_b: &'a ArmProfile,
    pub cases: &'a [PlannedCase],
    pub attempts_per_side: u32,
    pub request_cap: u32,
    pub reruns: &'a [SideKey],
    pub prior: Option<&'a PriorJournal>,
}

fn counterbalanced(position: usize) -> [ArmLabel; 2] {
    if position.is_multiple_of(2) {
        [ArmLabel::A, ArmLabel::B]
    } else {
        [ArmLabel::B, ArmLabel::A]
    }
}

/// Sides this run would send: those with no dispatched attempt and the explicit reruns.
pub fn sides_to_send(view: &PlanView<'_>) -> u32 {
    if !view.arm_a.blockers.is_empty() || !view.arm_b.blockers.is_empty() {
        // A pair needs both arms eligible; otherwise both sides are recorded as skipped.
        return 0;
    }
    let mut count = 0;
    for planned in view.cases {
        for arm in [ArmLabel::A, ArmLabel::B] {
            let prior = view.prior.and_then(|p| p.side(planned.case_id(), arm));
            let rerun = view
                .reruns
                .iter()
                .any(|key| key.case_id == planned.case_id() && key.arm == arm);
            if prior.is_none() || rerun {
                count += 1;
            }
        }
    }
    count
}

fn profile_line(label: &str, arm: &ArmProfile, profile_version: &str) -> String {
    format!(
        "profile {label}: {} / {}  version {profile_version}",
        protocol_label(arm.spec.protocol),
        arm.spec.model
    )
}

pub fn render_plan(view: &PlanView<'_>) -> String {
    let experiment = view.experiment;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "bench plan (dry run: nothing is sent, no credential is read, the journal is not written)"
    );
    let _ = writeln!(text, "mode: {}", mode_note(view.mode));
    let _ = writeln!(
        text,
        "corpus: {} ({} fixtures, {} selected)",
        experiment.corpus_version,
        view.corpus_size,
        view.cases.len()
    );
    let _ = writeln!(text, "instructions: {}", experiment.instruction_version);
    let _ = writeln!(text, "build revision: {}", experiment.build_revision);
    let _ = writeln!(
        text,
        "{}",
        profile_line("a", view.arm_a, &experiment.profile_a_version)
    );
    let _ = writeln!(
        text,
        "{}",
        profile_line("b", view.arm_b, &experiment.profile_b_version)
    );
    for (label, arm) in [("a", view.arm_a), ("b", view.arm_b)] {
        for blocker in &arm.blockers {
            let _ = writeln!(
                text,
                "  not eligible, profile {label}: {}",
                sanitize(blocker)
            );
        }
    }
    if !view.arm_a.blockers.is_empty() || !view.arm_b.blockers.is_empty() {
        let _ = writeln!(
            text,
            "  a run records every case as not eligible and sends nothing, for either arm"
        );
    }
    let _ = writeln!(
        text,
        "attempts per side: {}   request cap: {}",
        view.attempts_per_side, view.request_cap
    );

    let to_send = sides_to_send(view);
    let _ = writeln!(
        text,
        "planned requests: {to_send} (at most {} with retries)",
        to_send * view.attempts_per_side
    );
    if view.request_cap < to_send * view.attempts_per_side {
        let _ = writeln!(
            text,
            "note: the cap is below the maximum; sides beyond it stay unsent and a later run \
             continues them"
        );
    }
    match view.prior {
        None => {
            let _ = writeln!(text, "journal: none yet; a run creates it");
        }
        Some(prior) => match &prior.experiment {
            None => {
                let _ = writeln!(text, "journal: exists but holds no single experiment");
            }
            // `plan` refuses a journal a run would refuse before rendering anything.
            Some(_) => {
                let _ = writeln!(
                    text,
                    "journal: exists; configuration matches, a run resumes it"
                );
            }
        },
    }

    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "{:<28} {:<6} {:<22} {:<22}",
        "case", "order", "a", "b"
    );
    for (position, planned) in view.cases.iter().enumerate() {
        let order = view
            .prior
            .and_then(|prior| prior.orders.get(planned.case_id()).copied())
            .unwrap_or_else(|| counterbalanced(position));
        let blocked = !view.arm_a.blockers.is_empty() || !view.arm_b.blockers.is_empty();
        let describe = |arm: ArmLabel| -> String {
            let prior = view.prior.and_then(|p| p.side(planned.case_id(), arm));
            let rerun = view
                .reruns
                .iter()
                .any(|key| key.case_id == planned.case_id() && key.arm == arm);
            match (prior, rerun) {
                (None, _) | (Some(_), true) if blocked => "not eligible".to_string(),
                (None, _) => "to send".to_string(),
                (Some(_), true) => "rerun (new allowance)".to_string(),
                (Some("completed"), false) => "kept: completed".to_string(),
                (Some("failed"), false) => "kept: failed".to_string(),
                (Some(_), false) => "kept: outcome unknown".to_string(),
            }
        };
        let _ = writeln!(
            text,
            "{:<28} {:<6} {:<22} {:<22}",
            sanitize(planned.case_id()),
            format!("{},{}", arm_name(order[0]), arm_name(order[1])),
            describe(ArmLabel::A),
            describe(ArmLabel::B)
        );
    }
    text
}

fn side_label(side: &SideReport) -> String {
    let kind = match side.kind {
        SideKind::Completed => "completed",
        SideKind::Failed => "failed",
        SideKind::TimedOut => "timed_out",
        SideKind::Cancelled => "cancelled",
        SideKind::Unknown => "unknown",
        SideKind::Skipped => "skipped",
        SideKind::Missing => "missing",
    };
    match &side.class {
        Some(class) if side.kind != SideKind::Completed => {
            format!("{kind} ({})", sanitize(class))
        }
        _ => kind.to_string(),
    }
}

pub struct ReportView<'a> {
    pub mode: Mode,
    pub experiment: &'a Experiment,
    pub arm_a: &'a ArmProfile,
    pub arm_b: &'a ArmProfile,
    pub report: &'a RunReport,
}

/// True when every side of every case completed.
pub fn all_completed(report: &RunReport) -> bool {
    report
        .cases
        .iter()
        .all(|case| case.sides.iter().all(|s| s.kind == SideKind::Completed))
}

pub fn render_report(view: &ReportView<'_>) -> String {
    let report = view.report;
    let experiment = view.experiment;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "bench run {} of experiment {}",
        report.run, report.experiment_id
    );
    let _ = writeln!(text, "mode: {}", mode_note(view.mode));
    let _ = writeln!(text, "corpus: {}", experiment.corpus_version);
    let _ = writeln!(text, "instructions: {}", experiment.instruction_version);
    let _ = writeln!(text, "build revision: {}", experiment.build_revision);
    let _ = writeln!(
        text,
        "{}",
        profile_line("a", view.arm_a, &experiment.profile_a_version)
    );
    let _ = writeln!(
        text,
        "{}",
        profile_line("b", view.arm_b, &experiment.profile_b_version)
    );
    let _ = writeln!(
        text,
        "requests reserved this run: {} of cap {}   budget exhausted: {}   cancelled: {}",
        report.reserved_requests,
        report.request_cap,
        yes_no(report.budget_exhausted),
        yes_no(report.cancelled)
    );

    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "{:<28} {:<6} {:<26} {:<26}",
        "case", "order", "a", "b"
    );
    let mut totals: Vec<(String, u32)> = Vec::new();
    let mut notes = Vec::new();
    for case in &report.cases {
        let side_of = |arm: ArmLabel| case.sides.iter().find(|side| side.arm == arm);
        let label_of = |arm: ArmLabel| side_of(arm).map(side_label).unwrap_or_default();
        let _ = writeln!(
            text,
            "{:<28} {:<6} {:<26} {:<26}",
            sanitize(&case.case_id),
            format!(
                "{},{}",
                arm_name(case.arm_order[0]),
                arm_name(case.arm_order[1])
            ),
            label_of(ArmLabel::A),
            label_of(ArmLabel::B)
        );
        for side in &case.sides {
            let kind = side_label(side);
            let base = kind.split(' ').next().unwrap_or("").to_string();
            match totals.iter_mut().find(|(name, _)| *name == base) {
                Some((_, count)) => *count += 1,
                None => totals.push((base, 1)),
            }
            if let Some(reason) = &side.reason {
                notes.push(format!(
                    "{} {}: {}",
                    sanitize(&case.case_id),
                    arm_name(side.arm),
                    sanitize(reason)
                ));
            }
        }
    }
    let _ = writeln!(text);
    let summary: Vec<String> = totals
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    let _ = writeln!(text, "sides: {}", summary.join(", "));
    if !notes.is_empty() {
        let _ = writeln!(text, "notes:");
        for note in notes {
            let _ = writeln!(text, "  {note}");
        }
    }
    let _ = writeln!(
        text,
        "diagnostic only: nothing was applied to any store; results are in the journal"
    );
    if view.mode == Mode::Fake {
        let _ = writeln!(
            text,
            "fake mode results validate the pipeline only, not the quality of any provider"
        );
    }
    text
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
