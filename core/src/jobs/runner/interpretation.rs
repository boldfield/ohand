//! The interpretation dispatcher (I06) as a runner capability.

use super::{CapabilityError, CapabilityOutcome, JobCapability, Settlement};
use crate::interpretation::apply::ApplyError;
use crate::interpretation::dispatch::{
    DispatchError, DispatchOutcome, InterpretationDispatcher, WaitReason,
};
use crate::jobs::queue::Job;
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use crate::providers::contracts::{CancelToken, FailureKind};
use crate::store::schema::Database;
use chrono::{DateTime, Utc};

/// Runs `interpret` jobs through the offline fast path or the pinned profile's provider adapter.
/// The dispatcher settles the job itself (apply, failure rules, retry budget, backoff), so every
/// outcome is reported as already settled.
pub struct InterpretationCapability<'a> {
    dispatcher: InterpretationDispatcher<'a>,
}

impl<'a> InterpretationCapability<'a> {
    pub fn new(dispatcher: InterpretationDispatcher<'a>) -> InterpretationCapability<'a> {
        InterpretationCapability { dispatcher }
    }
}

impl JobCapability for InterpretationCapability<'_> {
    fn job_type(&self) -> &str {
        JOB_TYPE_INTERPRET
    }

    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError> {
        match self.dispatcher.run_job(db, claimed, cancel, now) {
            Ok(outcome) => Ok(CapabilityOutcome::Settled(settlement_of(&outcome))),
            Err(error) => Err(classify(error)),
        }
    }
}

fn settlement_of(outcome: &DispatchOutcome) -> Settlement {
    match outcome {
        DispatchOutcome::Interpreted { .. } => Settlement::Completed,
        DispatchOutcome::Failed { .. } | DispatchOutcome::RetriesExhausted { .. } => {
            Settlement::Failed
        }
        DispatchOutcome::BackedOff {
            reason: WaitReason::Transient(FailureKind::Cancelled),
            ..
        } => Settlement::Interrupted,
        DispatchOutcome::BackedOff { retry_at, .. } => Settlement::BackedOff {
            retry_at: *retry_at,
        },
        DispatchOutcome::Retired(_) => Settlement::Retired,
    }
}

/// A result for a lease the job no longer holds, or for a job that is no longer running, was
/// rejected at the boundary and recorded nothing: it is a duplicate or late delivery, not a fault.
fn classify(error: DispatchError) -> CapabilityError {
    match error {
        DispatchError::JobNotRunning { .. }
        | DispatchError::NotAnInterpretationJob { .. }
        | DispatchError::Apply(
            ApplyError::JobNotFound(_)
            | ApplyError::NotAnInterpretationJob { .. }
            | ApplyError::JobNotRunning { .. }
            | ApplyError::StaleLease { .. },
        ) => CapabilityError::LeaseLost(error.to_string()),
        other => CapabilityError::Internal(other.to_string()),
    }
}
