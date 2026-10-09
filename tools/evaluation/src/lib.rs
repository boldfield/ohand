//! Semantic evaluation of the interpretation pipeline against the synthetic intent corpus.
//!
//! Three execution kinds are reported and never merged: `deterministic` (the offline fast path),
//! `fake` (synthetic authored provider replies through the real dispatch and mapping) and
//! `recorded` (replies captured from an authorized live run). A fourth, `live`, is always
//! reported as unavailable here. Every candidate is applied with the production application
//! guard to a scratch store, and the report compares both the candidate and the durable state
//! with the fixture's oracle. See `docs/validation/evaluation-format.md`.

pub mod corpus;
pub mod report;
pub mod responses;
pub mod run;
pub mod score;
pub mod scratch;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("corpus: {0}")]
    Corpus(String),
    #[error("recorded responses: {0}")]
    Responses(String),
    #[error("i/o: {0}")]
    Io(String),
    #[error("scratch store: {0}")]
    Store(String),
    #[error("pipeline setup: {0}")]
    Setup(String),
}

impl From<anyhow::Error> for EvaluationError {
    fn from(error: anyhow::Error) -> Self {
        EvaluationError::Store(error.to_string())
    }
}
