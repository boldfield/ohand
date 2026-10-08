//! Developer-only synthetic benchmark runner for paired model comparison.
//!
//! This crate defines experiment records, run journals, and validation for synthetic
//! benchmarking. It reserves sibling modules for execution, transport, and reporting.
//!
//! All records use synthetic fixtures only. No credentials, private endpoints, or
//! production capture data enter journal records.
//!
//! # Reserved modules
//! - `executor`: Paired execution of cases through comparison arms
//! - `transport`: Credential-safe HTTP and provider communication
//! - `cli`: Command-line interface for benchmark orchestration
//! - `report`: Structured output and summary reporting

mod journal;
mod records;

pub use journal::{JournalError, JournalReader, JournalRecord, JournalWriter, TruncationRecovery};
pub use records::{
    is_secret_or_endpoint, Attempt, AttemptState, Case, Experiment, UnknownMetadata, UsageMetadata,
};
