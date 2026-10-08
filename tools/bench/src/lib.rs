//! Developer-only synthetic benchmark runner for paired model comparison.
//!
//! This crate defines experiment records, run journals, and validation for synthetic
//! benchmarking. It reserves sibling modules for execution, transport, and reporting.
//!
//! All records use synthetic fixtures only. No credentials, private endpoints, or
//! production capture data enter journal records.

mod journal;
mod records;

pub use journal::{JournalError, JournalReader, JournalWriter, TruncationRecovery};
pub use records::{Attempt, Case, Experiment};
