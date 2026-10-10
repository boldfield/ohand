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
//! - `transport`: Bounded, credential-safe host HTTP effect and its provider-trait bindings
//! - `cli`: Command-line interface for benchmark orchestration
//! - `report`: Structured output and summary reporting

mod guard;
mod journal;
mod records;

pub mod transport;

// Reserved modules for future implementation
pub mod cli;
pub mod executor;
pub mod report;

pub use guard::{is_secret_or_endpoint, json_is_secret_or_endpoint};
pub use journal::{JournalError, JournalReader, JournalRecord, JournalWriter, TruncationRecovery};
pub use records::{Attempt, AttemptState, Case, Experiment, UnknownMetadata, UsageMetadata};
