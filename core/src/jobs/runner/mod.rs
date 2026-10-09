//! Core job dispatch loop (J02a).
//!
//! [`JobRunner::drain`] works through the jobs that are ready to run: queued jobs whose backoff
//! has passed and running jobs whose lease expired (lease recovery). Each job is claimed through
//! the J01 queue, which already retires work that can never run (unsupported schema version,
//! revoked or missing pinned profile, deleted or revised item), then handed to the
//! [`JobCapability`] registered for its type. Capabilities are injected: the runner owns no
//! provider, transcription or reminder behavior, and a job whose type has no registered
//! capability waits (`capability_unavailable`) instead of being failed or run by a guess.
//!
//! A capability applies its own result through the core apply operations, which complete the job
//! under the lease token. The runner never writes an interpretation or reminder result, never
//! picks a deadline or destination, and settles only what a capability leaves unsettled:
//!
//! - transient failures spend a durable per-job budget ([`RunnerConfig::max_attempts`]) with
//!   exponential backoff, and the failure that spends the last attempt ends the job `failed`;
//! - permanent failures end the job `failed` immediately;
//! - interruption (cancellation, checkpoint) re-queues the job at once and spends nothing.
//!
//! A result presented for a lease the job no longer holds (a duplicate or late delivery after
//! lease recovery) is rejected by the queue/apply fencing, recorded nothing, and reported as
//! [`JobResult::LeaseLost`].
//!
//! Work per drain is bounded by a job count, an optional elapsed-time budget measured on the
//! injected clock, and a [`CancelToken`] checked before every claim. Cancellation never abandons
//! a claimed job: it is either finished or put back durably before the drain returns.

mod interpretation;
mod settle;

pub use interpretation::InterpretationCapability;
pub use settle::{CAPABILITY_UNAVAILABLE_REASON, INTERRUPTED_REASON, RETRIES_EXHAUSTED_REASON};

use crate::jobs::queue::{claim_job_with_lease, Job};
use crate::providers::contracts::CancelToken;
use crate::store::schema::{Clock, Database};
use chrono::{DateTime, Duration, Utc};
use settle::{
    settle_permanent_failure, settle_transient_failure, settle_without_cost, still_holds_lease,
    TransientSettlement,
};
use thiserror::Error;

/// Failure reason recorded when a capability reports a transient fault without a reason of its
/// own, or breaks the settlement contract.
pub const INTERNAL_ERROR_REASON: &str = "internal_error";
/// Failure reason recorded when a capability returned success but left the job running.
pub const NOT_SETTLED_REASON: &str = "capability_did_not_settle";

/// What a capability did to the job it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Completed,
    Failed,
    BackedOff {
        retry_at: DateTime<Utc>,
    },
    /// Put back at a checkpoint (for example a cancelled provider call); nothing was spent.
    Interrupted,
    /// Cancelled because the item or source can no longer produce a usable result.
    Retired,
}

/// What a capability reports back for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityOutcome {
    /// The capability applied its result or settled the job itself through the core operations
    /// and the job no longer runs under this lease.
    Settled(Settlement),
    /// A fault expected to pass. The runner counts it against the retry limit. `reason` is a
    /// short machine-readable label stored on the job and must never contain captured content.
    TransientFailure { reason: String },
    /// A fault retrying cannot fix. `reason` follows the same rule.
    PermanentFailure { reason: String },
    /// The capability stopped at a checkpoint without settling the job.
    Interrupted,
}

/// A capability could not process the job.
#[derive(Debug, Error)]
pub enum CapabilityError {
    /// The result was rejected because the job no longer runs under this lease (duplicate or
    /// late delivery). Nothing was recorded.
    #[error("lease lost: {0}")]
    LeaseLost(String),
    /// A defect or storage fault. The job is re-queued against the retry limit.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Work for one job type, injected into the runner. Implementations must settle or report every
/// job they are given and must not extend what the job's pinned profile and route authorize.
pub trait JobCapability {
    fn job_type(&self) -> &str;

    /// Run `claimed`, whose `attempt_count` is the lease token, at `now`. A capability that runs
    /// long work observes `cancel` and returns at a checkpoint.
    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError>;
}

/// Limits and backoff for one runner.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Lease taken on each claim; a holder that goes quiet loses the job after this.
    pub lease_duration: Duration,
    /// Most jobs claimed by one drain.
    pub max_jobs_per_drain: usize,
    /// Optional elapsed time (on the injected clock) after which no further job is claimed.
    pub time_budget: Option<Duration>,
    /// Transient failures a job may spend before it is ended `failed`.
    pub max_attempts: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// How long a job waits when its type has no registered capability.
    pub capability_unavailable_delay: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            lease_duration: Duration::minutes(5),
            max_jobs_per_drain: 25,
            time_budget: Some(Duration::seconds(25)),
            max_attempts: 5,
            base_backoff: Duration::seconds(30),
            max_backoff: Duration::hours(1),
            capability_unavailable_delay: Duration::minutes(15),
        }
    }
}

/// The capabilities a runner may use, one per job type.
#[derive(Default)]
pub struct JobCapabilities<'a> {
    capabilities: Vec<Box<dyn JobCapability + 'a>>,
}

impl<'a> JobCapabilities<'a> {
    pub fn new() -> JobCapabilities<'a> {
        JobCapabilities::default()
    }

    /// Register `capability` for its job type, replacing any registered earlier.
    pub fn with(mut self, capability: impl JobCapability + 'a) -> JobCapabilities<'a> {
        self.capabilities
            .retain(|existing| existing.job_type() != capability.job_type());
        self.capabilities.push(Box::new(capability));
        self
    }

    fn for_job_type(&self, job_type: &str) -> Option<&(dyn JobCapability + 'a)> {
        self.capabilities
            .iter()
            .find(|capability| capability.job_type() == job_type)
            .map(|capability| capability.as_ref())
    }
}

/// What happened to one claimed job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobResult {
    Settled(Settlement),
    /// A transient failure was counted and the job re-queued.
    RetryScheduled {
        retry_at: DateTime<Utc>,
        failures: u32,
    },
    /// The failure that spent the last allowed attempt ended the job `failed`.
    RetriesExhausted,
    FailedPermanently {
        reason: String,
    },
    /// Put back at a checkpoint at once; nothing was spent.
    Interrupted,
    /// No capability is registered for the job's type; the job waits until `retry_at`.
    CapabilityUnavailable {
        retry_at: DateTime<Utc>,
    },
    /// The result was rejected: the job no longer runs under this lease. Nothing was recorded.
    LeaseLost,
    /// The job could not be settled; its lease will expire and the job will be recovered.
    Unsettled {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReport {
    pub job_id: String,
    pub job_type: String,
    /// The lease token of the claim that ran.
    pub attempt: i32,
    pub result: JobResult,
}

/// Why a drain returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Nothing else is ready.
    Idle,
    JobLimit,
    TimeBudget,
    Cancelled,
    /// The queue could not be read; nothing further was claimed.
    ClaimFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub jobs: Vec<JobReport>,
    pub stop: StopReason,
}

/// Drains ready jobs through injected capabilities.
pub struct JobRunner<'a> {
    capabilities: JobCapabilities<'a>,
    clock: &'a dyn Clock,
    config: RunnerConfig,
}

impl<'a> JobRunner<'a> {
    /// `clock` stamps every durable time the runner and its capabilities write, so tests drive
    /// lease expiry, backoff and the time budget with a fake clock.
    pub fn new(
        capabilities: JobCapabilities<'a>,
        clock: &'a dyn Clock,
        config: RunnerConfig,
    ) -> JobRunner<'a> {
        JobRunner {
            capabilities,
            clock,
            config,
        }
    }

    /// Run ready jobs until nothing is ready, a limit is reached or `cancel` fires.
    pub fn drain(&self, db: &mut Database, cancel: &CancelToken) -> DrainReport {
        let started_at = self.clock.now();
        let mut jobs = Vec::new();
        let stop = loop {
            if cancel.is_cancelled() {
                break StopReason::Cancelled;
            }
            if jobs.len() >= self.config.max_jobs_per_drain {
                break StopReason::JobLimit;
            }
            let claim_time = self.clock.now();
            if self
                .config
                .time_budget
                .is_some_and(|budget| claim_time - started_at >= budget)
            {
                break StopReason::TimeBudget;
            }
            match claim_job_with_lease(db, self.config.lease_duration, claim_time) {
                Ok(Some(claimed)) => jobs.push(self.run_claimed(db, &claimed, cancel)),
                Ok(None) => break StopReason::Idle,
                Err(error) => break StopReason::ClaimFailed(error.to_string()),
            }
        };
        DrainReport { jobs, stop }
    }

    fn run_claimed(&self, db: &mut Database, claimed: &Job, cancel: &CancelToken) -> JobReport {
        let result = self.run_and_settle(db, claimed, cancel);
        JobReport {
            job_id: claimed.job_id.clone(),
            job_type: claimed.job_type.clone(),
            attempt: claimed.attempt_count,
            result,
        }
    }

    fn run_and_settle(&self, db: &mut Database, claimed: &Job, cancel: &CancelToken) -> JobResult {
        let now = self.clock.now();
        let Some(capability) = self.capabilities.for_job_type(&claimed.job_type) else {
            return unsettled_on_error(
                settle_without_cost(
                    db,
                    &claimed.job_id,
                    claimed.attempt_count,
                    CAPABILITY_UNAVAILABLE_REASON,
                    self.config.capability_unavailable_delay,
                    now,
                )
                .map(|retry_at| JobResult::CapabilityUnavailable { retry_at }),
            );
        };
        if cancel.is_cancelled() {
            return self.interrupt(db, claimed, now);
        }

        match capability.run(db, claimed, cancel, now) {
            Ok(CapabilityOutcome::Settled(settlement)) => {
                match still_holds_lease(db, &claimed.job_id, claimed.attempt_count) {
                    Ok(false) => JobResult::Settled(settlement),
                    Ok(true) => self.count_transient(db, claimed, NOT_SETTLED_REASON),
                    Err(error) => JobResult::Unsettled {
                        message: error.to_string(),
                    },
                }
            }
            Ok(CapabilityOutcome::TransientFailure { reason }) => {
                self.count_transient(db, claimed, &reason)
            }
            Ok(CapabilityOutcome::PermanentFailure { reason }) => unsettled_on_error(
                settle_permanent_failure(db, &claimed.job_id, claimed.attempt_count, &reason)
                    .map(|()| JobResult::FailedPermanently { reason }),
            ),
            Ok(CapabilityOutcome::Interrupted) => self.interrupt(db, claimed, now),
            Err(CapabilityError::LeaseLost(_)) => JobResult::LeaseLost,
            Err(CapabilityError::Internal(_)) => {
                self.count_transient(db, claimed, INTERNAL_ERROR_REASON)
            }
        }
    }

    fn interrupt(&self, db: &mut Database, claimed: &Job, now: DateTime<Utc>) -> JobResult {
        unsettled_on_error(
            settle_without_cost(
                db,
                &claimed.job_id,
                claimed.attempt_count,
                INTERRUPTED_REASON,
                Duration::zero(),
                now,
            )
            .map(|_| JobResult::Interrupted),
        )
    }

    fn count_transient(&self, db: &mut Database, claimed: &Job, reason: &str) -> JobResult {
        unsettled_on_error(
            settle_transient_failure(
                db,
                &claimed.job_id,
                claimed.attempt_count,
                reason,
                self.config.max_attempts,
                self.config.base_backoff,
                self.config.max_backoff,
                self.clock.now(),
            )
            .map(|settlement| match settlement {
                TransientSettlement::Requeued { retry_at, failures } => {
                    JobResult::RetryScheduled { retry_at, failures }
                }
                TransientSettlement::Exhausted => JobResult::RetriesExhausted,
            }),
        )
    }
}

fn unsettled_on_error(result: anyhow::Result<JobResult>) -> JobResult {
    result.unwrap_or_else(|error| JobResult::Unsettled {
        message: error.to_string(),
    })
}
