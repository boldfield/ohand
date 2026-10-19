//! Reads a run journal without opening it for writing and resolves what each side of each case
//! last did. Every refusal is a fixed message: nothing from the journal is echoed back.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ohand_core::providers::contracts::DiagnosticMetadata;
use serde::Deserialize;

use super::ReportError;
use crate::journal::JournalReader;
use crate::records::{Attempt, AttemptState, Experiment, UsageMetadata};

/// The executor's record version inside `Attempt::provider_output`.
const SUPPORTED_EXECUTOR_RECORD: u32 = 1;

/// The status and error type of a provider rejection, as the executor journaled them.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct RejectionRecord {
    #[serde(default)]
    pub error_type: Option<String>,
}

/// The parts of the executor's envelope a report reads. Unknown fields are ignored so a later
/// executor can add some without breaking reports of older journals.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Envelope {
    executor_record: u32,
    pub dispatched: bool,
    pub run: u32,
    #[serde(default)]
    pub pair_fingerprint: Option<String>,
    #[serde(default)]
    pub retry_of: Option<String>,
    #[serde(default)]
    pub rerun_of: Option<String>,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub rejection: Option<RejectionRecord>,
    #[serde(default)]
    pub output: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub metadata: Option<DiagnosticMetadata>,
}

/// An attempt as last recorded: a later record with the same id supersedes the reservation.
#[derive(Debug, Clone)]
pub(super) struct AttemptRecord {
    pub id: String,
    pub case_index: usize,
    /// 0 for arm a, 1 for arm b.
    pub arm: usize,
    pub state: AttemptState,
    pub envelope: Envelope,
    pub elapsed_ms: Option<u64>,
    pub usage: Option<UsageMetadata>,
}

#[derive(Debug, Clone)]
pub(super) struct JournalCase {
    pub case_id: String,
    pub content: String,
}

#[derive(Debug)]
pub(super) struct Journal {
    pub experiment: Experiment,
    pub cases: Vec<JournalCase>,
    pub attempts: Vec<AttemptRecord>,
    pub record_count: usize,
    pub torn_tail: bool,
    pub ambiguous_started_attempts: usize,
}

/// What a side last did, resolved the way the executor resolves it.
pub(super) enum Side<'a> {
    /// The latest attempt that was really dispatched.
    Dispatched(&'a AttemptRecord),
    /// Nothing was dispatched; the latest record that says why.
    Skipped(&'a AttemptRecord),
    Absent,
}

impl Journal {
    pub fn side(&self, case_index: usize, arm: usize) -> Side<'_> {
        let mut dispatched = None;
        let mut skipped = None;
        for attempt in &self.attempts {
            if attempt.case_index != case_index || attempt.arm != arm {
                continue;
            }
            if attempt.envelope.dispatched {
                dispatched = Some(attempt);
            } else {
                skipped = Some(attempt);
            }
        }
        match (dispatched, skipped) {
            (Some(attempt), _) => Side::Dispatched(attempt),
            (None, Some(attempt)) => Side::Skipped(attempt),
            (None, None) => Side::Absent,
        }
    }

    /// Every dispatched attempt on one side, retries and reruns included.
    pub fn dispatched_attempts(&self, arm: usize) -> impl Iterator<Item = &AttemptRecord> {
        self.attempts
            .iter()
            .filter(move |attempt| attempt.arm == arm && attempt.envelope.dispatched)
    }
}

fn refused(message: &str) -> ReportError {
    ReportError(message.to_string())
}

fn arm_index(key: &str) -> Option<usize> {
    match key {
        "a" => Some(0),
        "b" => Some(1),
        _ => None,
    }
}

pub(super) fn load(path: &Path) -> Result<Journal, ReportError> {
    if !path.is_file() {
        return Err(refused("the journal does not exist or is not a file"));
    }
    let reader = JournalReader::open(path).map_err(|_| {
        refused("the journal could not be read, or holds a record that fails validation")
    })?;
    let experiments = reader.experiments();
    let experiment = match experiments.as_slice() {
        [only] => (*only).clone(),
        [] => return Err(refused("the journal holds no experiment")),
        _ => return Err(refused("the journal holds more than one experiment")),
    };

    let mut cases = Vec::new();
    let mut index_by_record: HashMap<&str, usize> = HashMap::new();
    let mut seen_case_ids = HashSet::new();
    for case in reader.cases() {
        if case.experiment_id != experiment.id {
            return Err(refused("the journal holds a case of another experiment"));
        }
        if !seen_case_ids.insert(case.case_id.as_str()) {
            return Err(refused("the journal records a case more than once"));
        }
        index_by_record.insert(case.id.as_str(), cases.len());
        cases.push(JournalCase {
            case_id: case.case_id.clone(),
            content: case.content.clone(),
        });
    }

    let mut attempts: Vec<AttemptRecord> = Vec::new();
    for attempt in reader.attempts() {
        let record = attempt_record(attempt, &index_by_record)?;
        match attempts.iter_mut().find(|known| known.id == record.id) {
            Some(known) => *known = record,
            None => attempts.push(record),
        }
    }

    let recovery = reader.recovery();
    Ok(Journal {
        experiment,
        cases,
        attempts,
        record_count: reader.records().len(),
        torn_tail: recovery.truncation_reason.is_some(),
        ambiguous_started_attempts: recovery.ambiguous_started_attempts,
    })
}

fn attempt_record(
    attempt: &Attempt,
    index_by_record: &HashMap<&str, usize>,
) -> Result<AttemptRecord, ReportError> {
    let case_index = *index_by_record
        .get(attempt.case_id.as_str())
        .ok_or_else(|| refused("the journal holds an attempt for a case it does not record"))?;
    let arm = arm_index(&attempt.arm)
        .ok_or_else(|| refused("the journal holds an attempt for an unknown arm"))?;
    let envelope: Envelope = attempt
        .provider_output
        .clone()
        .and_then(|value| serde_json::from_value(value).ok())
        .filter(|envelope: &Envelope| envelope.executor_record == SUPPORTED_EXECUTOR_RECORD)
        .ok_or_else(|| refused("the journal holds an attempt this executor did not write"))?;
    Ok(AttemptRecord {
        id: attempt.id.clone(),
        case_index,
        arm,
        state: attempt.state,
        envelope,
        elapsed_ms: attempt.elapsed_ms,
        usage: attempt.usage_tokens.clone(),
    })
}
