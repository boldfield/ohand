//! Bounded, resumable execution of paired comparison cases.
//!
//! The executor runs each synthetic case through the two comparison arms one request at a time,
//! in a recorded, counterbalanced order, using the normalized diagnostic dispatch of core. It
//! derives both arms from one frozen case, so the arms share model-visible content but never
//! each other's responses. Every network attempt is authorized against the arm's own current
//! decision and reserved in the journal before it is dispatched, and consumes the run's
//! request allowance, retries included.
//!
//! The journal is the only thing the executor writes. It holds no store, reminder or apply
//! handle, so a diagnostic result is data in the journal and nothing more.
//!
//! A reservation that was written but never completed is an unknown outcome: the request may
//! have been sent and paid for. Resuming preserves it, and only an explicit rerun with its own
//! allowance dispatches that side again, recording the attempt it follows. Results recorded
//! under a different experiment or request configuration are never merged into a run.

mod envelope;
mod error;
mod plan;
mod report;
mod run;
mod state;
#[cfg(test)]
mod tests;

pub use error::{ConfigurationChange, ExecutorError};
pub use plan::{
    ArmAuthorizer, ArmBinding, AuthorizationQuery, ExecutorLimits, PlannedCase, RunControls,
    RunRequest, SideKey, MAX_ATTEMPTS_PER_SIDE, MAX_CONCURRENT_REQUESTS, MAX_REQUEST_CAP,
};
pub use report::{CaseReport, ConfigurationStatus, RunReport, SideKind, SideReport};
pub use run::Executor;
