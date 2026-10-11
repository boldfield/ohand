//! Developer-only synthetic benchmark runner for paired model comparison.
//!
//! This crate defines experiment records, run journals, and validation for synthetic
//! benchmarking. It composes execution, transport, and the command line, and reserves a module
//! for reporting.
//!
//! All records use synthetic fixtures only. No credentials, private endpoints, or
//! production capture data enter journal records.
//!
//! # Modules
//! - `executor`: Bounded, resumable paired execution of cases through comparison arms
//! - `transport`: Bounded, credential-safe host HTTP effect and its provider-trait bindings
//! - `cli`: The `bench plan` / `bench run` command composing corpus, profiles, authorization,
//!   executor and transport; see `docs/validation/model-benchmark.md`
//!
//! # Reserved modules
//! - `report`: Structured output and summary reporting

mod guard;
mod journal;
mod records;

pub mod cli;
pub mod executor;
pub mod transport;

// Reserved module for future implementation
pub mod report;

pub use guard::{is_secret_or_endpoint, json_is_secret_or_endpoint};
pub use journal::{JournalError, JournalReader, JournalRecord, JournalWriter, TruncationRecovery};
pub use records::{Attempt, AttemptState, Case, Experiment, UnknownMetadata, UsageMetadata};
