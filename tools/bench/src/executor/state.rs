use std::collections::HashMap;

use ohand_core::review::compare::ArmLabel;

use super::envelope::Envelope;
use super::error::ExecutorError;
use crate::journal::JournalReader;
use crate::records::{AttemptState, Case, Experiment};

pub(super) fn arm_key(arm: ArmLabel) -> &'static str {
    match arm {
        ArmLabel::A => "a",
        ArmLabel::B => "b",
    }
}

fn parse_arm(key: &str) -> Option<ArmLabel> {
    match key {
        "a" => Some(ArmLabel::A),
        "b" => Some(ArmLabel::B),
        _ => None,
    }
}

/// An attempt as last recorded: a later record with the same id supersedes the reservation.
#[derive(Debug, Clone)]
pub(super) struct AttemptEntry {
    pub id: String,
    pub case_record_id: String,
    pub arm: ArmLabel,
    pub state: AttemptState,
    pub envelope: Envelope,
}

#[derive(Debug, Clone)]
pub(super) struct CaseEntry {
    pub record_id: String,
    pub content: String,
}

/// In-memory view of the journal, kept in step with what the writer has appended.
#[derive(Debug)]
pub(super) struct JournalState {
    cases: HashMap<String, CaseEntry>,
    attempts: Vec<AttemptEntry>,
    pub runs: u32,
    pub reservations: u32,
}

impl JournalState {
    pub fn empty() -> JournalState {
        JournalState {
            cases: HashMap::new(),
            attempts: Vec::new(),
            runs: 0,
            reservations: 0,
        }
    }

    /// Build the view from a recovered journal. `experiment` is the one record the journal holds.
    pub fn load(
        reader: &JournalReader,
        experiment: &Experiment,
    ) -> Result<JournalState, ExecutorError> {
        let mut state = JournalState::empty();
        let mut record_to_case: HashMap<&str, &str> = HashMap::new();
        for case in reader.cases() {
            if case.experiment_id != experiment.id
                || state
                    .cases
                    .insert(
                        case.case_id.clone(),
                        CaseEntry {
                            record_id: case.id.clone(),
                            content: case.content.clone(),
                        },
                    )
                    .is_some()
            {
                return Err(ExecutorError::InconsistentJournal);
            }
            record_to_case.insert(&case.id, &case.case_id);
        }
        for attempt in reader.attempts() {
            if !record_to_case.contains_key(attempt.case_id.as_str()) {
                return Err(ExecutorError::InconsistentJournal);
            }
            let envelope = Envelope::from_value(attempt.provider_output.as_ref())?;
            let arm = parse_arm(&attempt.arm).ok_or(ExecutorError::ForeignRecord)?;
            state.upsert_attempt(AttemptEntry {
                id: attempt.id.clone(),
                case_record_id: attempt.case_id.clone(),
                arm,
                state: attempt.state,
                envelope,
            });
        }
        Ok(state)
    }

    pub fn case(&self, case_id: &str) -> Option<&CaseEntry> {
        self.cases.get(case_id)
    }

    pub fn insert_case(&mut self, case: &Case) {
        self.cases.insert(
            case.case_id.clone(),
            CaseEntry {
                record_id: case.id.clone(),
                content: case.content.clone(),
            },
        );
    }

    pub fn upsert_attempt(&mut self, entry: AttemptEntry) {
        self.runs = self.runs.max(entry.envelope.run);
        if let Some(reservation) = entry.envelope.reservation {
            self.reservations = self.reservations.max(reservation);
        }
        match self
            .attempts
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            Some(existing) => *existing = entry,
            None => self.attempts.push(entry),
        }
    }

    fn side_attempts<'s>(&'s self, case_record_id: &str, arm: ArmLabel) -> Vec<&'s AttemptEntry> {
        self.attempts
            .iter()
            .filter(|entry| entry.case_record_id == case_record_id && entry.arm == arm)
            .collect()
    }

    pub fn dispatched_count(&self, case_record_id: &str, arm: ArmLabel) -> u32 {
        self.side_attempts(case_record_id, arm)
            .iter()
            .filter(|entry| entry.envelope.dispatched)
            .count() as u32
    }

    /// Requests the given run reserved for this side.
    pub fn dispatched_in_run(&self, case_record_id: &str, arm: ArmLabel, run: u32) -> u32 {
        self.side_attempts(case_record_id, arm)
            .iter()
            .filter(|entry| entry.envelope.dispatched && entry.envelope.run == run)
            .count() as u32
    }

    pub fn latest_dispatched(&self, case_record_id: &str, arm: ArmLabel) -> Option<&AttemptEntry> {
        self.side_attempts(case_record_id, arm)
            .into_iter()
            .rfind(|entry| entry.envelope.dispatched)
    }

    pub fn latest_skip(&self, case_record_id: &str, arm: ArmLabel) -> Option<&AttemptEntry> {
        self.side_attempts(case_record_id, arm)
            .into_iter()
            .rfind(|entry| !entry.envelope.dispatched)
    }

    /// The arm order first recorded for this case, if any attempt or skip was journaled.
    pub fn recorded_order(&self, case_record_id: &str) -> Option<[ArmLabel; 2]> {
        self.attempts
            .iter()
            .filter(|entry| entry.case_record_id == case_record_id)
            .find_map(|entry| match entry.envelope.arm_order.as_slice() {
                [first, second] => {
                    let (first, second) = (parse_arm(first)?, parse_arm(second)?);
                    (first != second).then_some([first, second])
                }
                _ => None,
            })
    }

    /// Any fingerprint journaled for this case that differs from `current`.
    pub fn conflicting_fingerprint(&self, case_record_id: &str, current: &str) -> bool {
        self.attempts
            .iter()
            .filter(|entry| entry.case_record_id == case_record_id)
            .filter_map(|entry| entry.envelope.pair_fingerprint.as_deref())
            .any(|journaled| journaled != current)
    }
}
