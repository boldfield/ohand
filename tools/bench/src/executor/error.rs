use thiserror::Error;

use crate::journal::JournalError;

/// What differs between the journaled experiment and the one being run. Only field names and
/// case identifiers appear, never values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationChange {
    /// These experiment fields differ from the journaled experiment.
    Experiment { fields: Vec<&'static str> },
    /// The content of an already journaled case differs.
    CaseContent { case_id: String },
    /// The derived requests for an already attempted case differ (instructions, context,
    /// settings, request version or profile pins).
    Request { case_id: String },
}

#[derive(Debug, Error, PartialEq)]
pub enum ExecutorError {
    #[error("journal error: {0}")]
    Journal(#[from] JournalError),
    #[error("limits are invalid: {0}")]
    InvalidLimits(&'static str),
    #[error("invalid case: {0}")]
    InvalidCase(String),
    #[error("case id {0} appears more than once in the plan")]
    DuplicateCase(String),
    #[error("the experiment configuration changed: {0:?}")]
    ConfigurationChanged(ConfigurationChange),
    #[error("the journal holds records this executor did not write")]
    ForeignRecord,
    #[error("the journal holds more than one experiment, or records without one")]
    InconsistentJournal,
    #[error("arm binding does not match the experiment's pinned profile: {0}")]
    BindingMismatch(&'static str),
    #[error("the plan's instruction version does not match the experiment")]
    InstructionVersionMismatch,
    #[error("rerun target {case_id} is not in the plan")]
    UnknownRerunTarget { case_id: String },
    #[error("rerun target {case_id} has no dispatched attempt to rerun")]
    NothingToRerun { case_id: String },
}
