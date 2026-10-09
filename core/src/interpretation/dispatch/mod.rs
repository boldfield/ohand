//! Interpretation dispatcher (I06).
//!
//! [`InterpretationDispatcher::run_job`] takes one claimed interpretation job from a raw saved
//! capture to the I05 apply boundary:
//!
//! 1. The source is read from durable state (current effective text, capture time context,
//!    immutable route). A job whose item is gone, inactive or revised is retired; an item with no
//!    text yet backs off.
//! 2. The offline fast path (F03) runs first. It needs no provider and no remote authorization,
//!    because nothing leaves the device. A recognized command is applied directly through the
//!    local apply path, even while the job's route grant is missing. A
//!    fast-path abstention or non-match never suppresses the provider.
//! 3. Otherwise the job is authorized (V02/V03) before any provider is chosen. A denial backs the
//!    job off with reason `unauthorized` so the item reports that it awaits configuration;
//!    nothing is sent and nothing is applied.
//! 4. For a job pinned to a stored profile, the adapter registered for the profile's protocol is
//!    selected and called through the shared `dispatch` harness with a request built only from the
//!    trusted source. The reply is mapped through the checked proposal boundary and applied.
//!
//! Every outcome that is not a usable result goes through the I05 failure rules or the queue's
//! backoff, so a model failure never touches the captured source or the item's prior state, and
//! processing that cannot happen is reported through the job (`failed` or backed off with its
//! reason) rather than as an interpretation result. Capture saving never waits on this module: it
//! runs only for an already-saved item and only for a job someone has claimed.
//!
//! Retry policy for transient failures comes from the pinned profile's retry policy and is spent
//! only by provider calls that failed transiently (a durable per-job counter). While attempts
//! remain the capture stays safe and the item reports `retrying_after_transient`. The failure that
//! is the `max_attempts`-th ends the job `failed` with reason `retries_exhausted` through the I05
//! rules: the source and any prior result are kept and a first interpretation stays
//! `uninterpreted`. Cancellations, lease reclaims and configuration or source waits do not spend
//! the budget.
//!
//! The caller's `Job` is a claim receipt only. Before the source is read or anything is sent the
//! dispatcher reloads the durable row and requires the live lease (same attempt, running, not
//! expired) and agreement on item, revision, request and profile; a stale or altered job fails
//! with no provider call and no mutation.

mod registry;
mod source;

pub use registry::AdapterRegistry;

use crate::interpretation::apply::{
    apply_interpretation_proposal, apply_local_interpretation_proposal,
    record_interpretation_failure, record_interpretation_retries_exhausted, ApplyError,
    ApplyOutcome,
};
use crate::interpretation::contracts::Proposal;
use crate::interpretation::fast_path::recognize_with_session_topic;
use crate::interpretation::instructions::{InterpretationMapping, M1_INSTRUCTION_VERSION};
use crate::jobs::queue::{
    fail_job_with_backoff, fail_job_with_backoff_in_tx, get_job, Job, JobStatus,
};
use crate::privacy::routing::{authorize_job, Authorization, DenialReason, JOB_TYPE_INTERPRET};
use crate::providers::contracts::{
    dispatch, CancelToken, Clock as ProviderClock, DispatchLimits, ErrorClass, FailureKind,
    InterpretationRequest, ProviderFailure, ProviderProfile, RetryPolicy,
};
use crate::store::schema::Database;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use source::{load_profile, load_source, InterpretationSource, SourceState};
use thiserror::Error;
use uuid::Uuid;

/// Job failure reason recorded while authorization denies the job.
pub const UNAUTHORIZED_REASON: &str = "unauthorized";
/// Job failure reason recorded while the item has no text to interpret yet.
pub const SOURCE_UNAVAILABLE_REASON: &str = "source_unavailable";

const CONFIGURATION_BACKOFF_BASE_SECONDS: i64 = 60;
const CONFIGURATION_BACKOFF_MAX_SECONDS: i64 = 3_600;
const PROPOSAL_ID_DOMAIN: &[u8] = b"ohand.interpretation.dispatch.proposal.v1";

/// Where an applied result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretationRoute {
    /// The offline grammar; no provider was contacted.
    FastPath,
    /// The adapter selected by the pinned profile's protocol.
    Provider,
}

/// Why a job was put back in the queue instead of finishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// Authorization denies dispatch until routes, grants or the profile change.
    Denied(DenialReason),
    /// The provider call failed in a way that is expected to pass.
    Transient(FailureKind),
    /// The item has no text yet.
    SourceUnavailable,
}

/// Why a job was cancelled because it can no longer produce a usable result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireReason {
    ItemMissing,
    ItemNotActive,
    SourceRevised,
}

impl RetireReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RetireReason::ItemMissing => "item_missing",
            RetireReason::ItemNotActive => "item_not_active",
            RetireReason::SourceRevised => "source_revised",
        }
    }
}

/// What running one claimed job did.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// A result was applied (or an abstention recorded) and the job completed.
    Interpreted {
        route: InterpretationRoute,
        outcome: ApplyOutcome,
    },
    /// The job ended without a usable result under the I05 failure rules.
    Failed {
        failure: ProviderFailure,
        outcome: ApplyOutcome,
    },
    /// The job was re-queued; nothing about the item changed.
    BackedOff {
        reason: WaitReason,
        retry_at: DateTime<Utc>,
    },
    /// The job was cancelled; the item is gone, inactive or has moved to a newer revision.
    Retired(RetireReason),
}

/// The job could not be run at all. Nothing was changed.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("job is a {job_type:?} job, not an interpretation job")]
    NotAnInterpretationJob { job_type: String },
    #[error("job is {status}, not running")]
    JobNotRunning { status: &'static str },
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error("storage error: {0}")]
    Storage(#[from] anyhow::Error),
}

/// Routes claimed interpretation jobs through the fast path or the selected provider adapter.
pub struct InterpretationDispatcher<'a> {
    registry: &'a AdapterRegistry,
    provider_clock: &'a dyn ProviderClock,
    limits: DispatchLimits,
}

impl<'a> InterpretationDispatcher<'a> {
    /// `provider_clock` bounds provider calls (deadlines); it is independent of the wall-clock
    /// `now` that stamps durable records.
    pub fn new(
        registry: &'a AdapterRegistry,
        provider_clock: &'a dyn ProviderClock,
    ) -> InterpretationDispatcher<'a> {
        InterpretationDispatcher {
            registry,
            provider_clock,
            limits: DispatchLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: DispatchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Run the claimed, running interpretation job `job` (the value returned by the claim, whose
    /// `attempt_count` is the lease token). Cancelling `cancel` abandons an in-flight provider
    /// call and re-queues the job immediately.
    pub fn run_job(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        if claimed.job_type != JOB_TYPE_INTERPRET {
            return Err(DispatchError::NotAnInterpretationJob {
                job_type: claimed.job_type.clone(),
            });
        }
        if claimed.status != JobStatus::Running {
            return Err(DispatchError::JobNotRunning {
                status: claimed.status.as_str(),
            });
        }
        let stored = bind_running_job(db, claimed, now)?;
        let job = &stored;
        let Some(request_version) = job.request_version.as_deref() else {
            return self.record_failure(db, job, ProviderFailure::new(FailureKind::Rejected), now);
        };

        let source = match load_source(db.conn(), job)? {
            SourceState::Ready(source) => source,
            SourceState::Retired(reason) => return self.retire(db, job, reason),
            SourceState::NoText => {
                return self.back_off(
                    db,
                    job,
                    WaitReason::SourceUnavailable,
                    SOURCE_UNAVAILABLE_REASON,
                    configuration_backoff(),
                    now,
                )
            }
        };

        let proposal_id = proposal_id_for_job(job, request_version);
        let mut fast_path_abstention = None;
        if let Some(mut proposal) = recognize_with_session_topic(
            &source.text,
            &source.item_id,
            &source.capture_id,
            job.source_revision,
            source.text_basis.clone(),
            request_version,
            &source.time_context,
        ) {
            proposal.proposal_id = proposal_id.clone();
            if proposal.abstention.is_none() {
                return self.apply(db, job, &proposal, InterpretationRoute::FastPath, now);
            }
            fast_path_abstention = Some(proposal);
        }

        let authorization = match authorize_job(db.conn(), &job.job_id)? {
            crate::privacy::routing::AuthorizationDecision::Authorized(authorization) => {
                authorization
            }
            crate::privacy::routing::AuthorizationDecision::Denied(denial) => {
                return self.denied(db, job, denial, now)
            }
        };

        let unavailable = || ProviderFailure::new(FailureKind::CapabilityUnavailable);
        let Some(profile_version) = authorization.profile_version() else {
            return match fast_path_abstention {
                Some(abstention) => {
                    self.apply(db, job, &abstention, InterpretationRoute::FastPath, now)
                }
                None => self.record_failure(db, job, unavailable(), now),
            };
        };
        let Some(profile) = load_profile(db.conn(), profile_version)? else {
            return self.record_failure(db, job, unavailable(), now);
        };
        if !authorization_matches(&authorization, &profile) {
            return self.record_failure(db, job, unavailable(), now);
        }
        let Some(adapter) = self.registry.adapter_for(profile.protocol()) else {
            return self.record_failure(db, job, unavailable(), now);
        };

        let rejected = || ProviderFailure::new(FailureKind::Rejected);
        let Ok(request) = build_request(
            &source,
            request_version,
            job.source_revision,
            &profile,
            &authorization,
        ) else {
            return self.record_failure(db, job, rejected(), now);
        };
        let Ok(mapping) = InterpretationMapping::new(&request, &source.item_id, &proposal_id)
        else {
            return self.record_failure(db, job, rejected(), now);
        };

        match dispatch(
            adapter,
            &profile,
            &request,
            self.provider_clock,
            cancel,
            &self.limits,
        ) {
            Ok(output) => match mapping.map_output(&output) {
                Ok(proposal) => self.apply(db, job, &proposal, InterpretationRoute::Provider, now),
                Err(_) => self.record_failure(
                    db,
                    job,
                    ProviderFailure::new(FailureKind::InvalidOutput),
                    now,
                ),
            },
            Err(failure) => match failure.class {
                ErrorClass::Transient => {
                    let policy = profile.retry_policy();
                    if !record_transient_failure(db, job, policy, now, &failure)? {
                        return self.exhausted(db, job, failure, now);
                    }
                    let retry_at = get_job(db, &job.job_id)?
                        .and_then(|stored| stored.next_attempt_at)
                        .unwrap_or(now);
                    Ok(DispatchOutcome::BackedOff {
                        reason: WaitReason::Transient(failure.kind),
                        retry_at,
                    })
                }
                ErrorClass::Cancelled => self.back_off(
                    db,
                    job,
                    WaitReason::Transient(failure.kind),
                    failure_kind_name(failure.kind),
                    Backoff {
                        base_seconds: 0,
                        max_seconds: 0,
                    },
                    now,
                ),
                _ => self.record_failure(db, job, failure, now),
            },
        }
    }

    fn apply(
        &self,
        db: &mut Database,
        job: &Job,
        proposal: &Proposal,
        route: InterpretationRoute,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        let applied = match route {
            InterpretationRoute::FastPath => apply_local_interpretation_proposal(
                db,
                &job.job_id,
                job.attempt_count,
                proposal,
                now,
            ),
            InterpretationRoute::Provider => {
                apply_interpretation_proposal(db, &job.job_id, job.attempt_count, proposal, now)
            }
        };
        match applied {
            Ok(outcome) => Ok(DispatchOutcome::Interpreted { route, outcome }),
            Err(error) => self.settle_apply_error(db, job, error, now),
        }
    }

    fn record_failure(
        &self,
        db: &mut Database,
        job: &Job,
        failure: ProviderFailure,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        match record_interpretation_failure(db, &job.job_id, job.attempt_count, &failure, now) {
            Ok(outcome) => Ok(DispatchOutcome::Failed { failure, outcome }),
            Err(error) => self.settle_apply_error(db, job, error, now),
        }
    }

    fn exhausted(
        &self,
        db: &mut Database,
        job: &Job,
        failure: ProviderFailure,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        match record_interpretation_retries_exhausted(db, &job.job_id, job.attempt_count, now) {
            Ok(outcome) => Ok(DispatchOutcome::Failed { failure, outcome }),
            Err(error) => self.settle_apply_error(db, job, error, now),
        }
    }

    /// A rejection at the apply boundary records nothing. Authorization that changed mid-flight
    /// and a source that changed mid-flight are ordinary outcomes; anything else is a defect or a
    /// lost lease and is reported to the caller.
    fn settle_apply_error(
        &self,
        db: &mut Database,
        job: &Job,
        error: ApplyError,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        match error {
            ApplyError::Unauthorized(denial) => self.denied(db, job, denial, now),
            ApplyError::ItemNotFound(_) => self.retire(db, job, RetireReason::ItemMissing),
            ApplyError::ItemNotActive(_) => self.retire(db, job, RetireReason::ItemNotActive),
            ApplyError::StaleRevision { .. } | ApplyError::TextBasisStale => {
                self.retire(db, job, RetireReason::SourceRevised)
            }
            other => Err(other.into()),
        }
    }

    fn denied(
        &self,
        db: &mut Database,
        job: &Job,
        denial: DenialReason,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.back_off(
            db,
            job,
            WaitReason::Denied(denial),
            UNAUTHORIZED_REASON,
            configuration_backoff(),
            now,
        )
    }

    fn back_off(
        &self,
        db: &mut Database,
        job: &Job,
        reason: WaitReason,
        failure_reason: &str,
        backoff: Backoff,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        fail_job_with_backoff(
            db,
            &job.job_id,
            failure_reason.to_string(),
            backoff.base_seconds,
            backoff.max_seconds,
            now,
            job.attempt_count,
        )?;
        let retry_at = get_job(db, &job.job_id)?
            .and_then(|stored| stored.next_attempt_at)
            .unwrap_or(now);
        Ok(DispatchOutcome::BackedOff { reason, retry_at })
    }

    fn retire(
        &self,
        db: &mut Database,
        job: &Job,
        reason: RetireReason,
    ) -> Result<DispatchOutcome, DispatchError> {
        let affected = db
            .conn()
            .execute(
                "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL
                  WHERE job_id = ? AND status = ? AND attempt_count = ?",
                rusqlite::params![
                    JobStatus::Cancelled.as_str(),
                    reason.as_str(),
                    &job.job_id,
                    JobStatus::Running.as_str(),
                    job.attempt_count
                ],
            )
            .map_err(anyhow::Error::from)?;
        if affected == 0 {
            return Err(DispatchError::Storage(anyhow::anyhow!(
                "job {} lease is no longer held",
                job.job_id
            )));
        }
        Ok(DispatchOutcome::Retired(reason))
    }
}

struct Backoff {
    base_seconds: i64,
    max_seconds: i64,
}

fn configuration_backoff() -> Backoff {
    Backoff {
        base_seconds: CONFIGURATION_BACKOFF_BASE_SECONDS,
        max_seconds: CONFIGURATION_BACKOFF_MAX_SECONDS,
    }
}

/// Reload the durable job and fence the caller's copy against it before anything is read or sent.
/// The caller's `Job` is only a claim receipt: the lease must still be the live one (same attempt,
/// still running, not expired) and every field that selects what is disclosed and to whom must
/// agree with the stored row. The stored row is what the dispatcher then works from.
fn bind_running_job(
    db: &Database,
    claimed: &Job,
    now: DateTime<Utc>,
) -> Result<Job, DispatchError> {
    let stored = get_job(db, &claimed.job_id)?
        .ok_or_else(|| ApplyError::JobNotFound(claimed.job_id.clone()))?;
    if stored.job_type != JOB_TYPE_INTERPRET {
        return Err(DispatchError::NotAnInterpretationJob {
            job_type: stored.job_type,
        });
    }
    if stored.status != JobStatus::Running {
        return Err(ApplyError::JobNotRunning {
            status: stored.status.as_str(),
        }
        .into());
    }
    if stored.attempt_count != claimed.attempt_count {
        return Err(ApplyError::StaleLease {
            presented: claimed.attempt_count,
            current: stored.attempt_count,
        }
        .into());
    }
    let lease_expired = match stored.lease_expires_at {
        Some(expires_at) => expires_at <= now,
        None => true,
    };
    if lease_expired {
        return Err(ApplyError::StaleLease {
            presented: claimed.attempt_count,
            current: stored.attempt_count,
        }
        .into());
    }
    let mismatched_field = if stored.item_id != claimed.item_id {
        Some("item_id")
    } else if stored.source_revision != claimed.source_revision {
        Some("source_revision")
    } else if stored.request_version != claimed.request_version {
        Some("request_version")
    } else if stored.profile_version != claimed.profile_version {
        Some("profile_version")
    } else {
        None
    };
    if let Some(field) = mismatched_field {
        return Err(ApplyError::JobBindingMismatch { field }.into());
    }
    Ok(stored)
}

/// Count one transient provider failure against the job's retry budget and, while attempts
/// remain, re-queue it with the profile's backoff, in one transaction fenced by the lease. Returns
/// `false` when this failure used the last allowed attempt; nothing is re-queued then and the
/// caller ends the job through the I05 failure rules.
///
/// Only failed provider calls are counted: cancellations, lease reclaims and configuration or
/// source waits all raise the job's `attempt_count` but never spend the budget.
fn record_transient_failure(
    db: &mut Database,
    job: &Job,
    policy: &RetryPolicy,
    now: DateTime<Utc>,
    failure: &ProviderFailure,
) -> Result<bool, DispatchError> {
    let tx = db.immediate_transaction()?;
    let affected = tx
        .execute(
            "UPDATE jobs SET transient_failure_count = transient_failure_count + 1
              WHERE job_id = ? AND status = ? AND attempt_count = ?",
            rusqlite::params![&job.job_id, JobStatus::Running.as_str(), job.attempt_count],
        )
        .map_err(anyhow::Error::from)?;
    if affected == 0 {
        return Err(DispatchError::Apply(ApplyError::StaleLease {
            presented: job.attempt_count,
            current: tx
                .query_row(
                    "SELECT attempt_count FROM jobs WHERE job_id = ?",
                    [&job.job_id],
                    |row| row.get(0),
                )
                .map_err(anyhow::Error::from)?,
        }));
    }
    let failures: i64 = tx
        .query_row(
            "SELECT transient_failure_count FROM jobs WHERE job_id = ?",
            [&job.job_id],
            |row| row.get(0),
        )
        .map_err(anyhow::Error::from)?;
    if u64::try_from(failures).unwrap_or(0) >= u64::from(policy.max_attempts) {
        tx.commit().map_err(anyhow::Error::from)?;
        return Ok(false);
    }
    let backoff = transient_backoff(policy);
    fail_job_with_backoff_in_tx(
        &tx,
        &job.job_id,
        failure_kind_name(failure.kind).to_string(),
        backoff.base_seconds,
        backoff.max_seconds,
        now,
        job.attempt_count,
    )?;
    tx.commit().map_err(anyhow::Error::from)?;
    Ok(true)
}

fn transient_backoff(policy: &RetryPolicy) -> Backoff {
    let seconds =
        |milliseconds: u64| i64::try_from(milliseconds.div_ceil(1000)).unwrap_or(i64::MAX);
    Backoff {
        base_seconds: seconds(policy.initial_backoff_ms),
        max_seconds: seconds(policy.max_backoff_ms),
    }
}

fn failure_kind_name(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Timeout => "timeout",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Unavailable => "unavailable",
        FailureKind::RateLimited => "rate_limited",
        FailureKind::Unauthorized => "unauthorized",
        FailureKind::InvalidOutput => "invalid_output",
        FailureKind::OutputTooLarge => "output_too_large",
        FailureKind::InputTooLarge => "input_too_large",
        FailureKind::CapabilityUnavailable => "capability_unavailable",
        FailureKind::ProfileMismatch => "profile_mismatch",
        FailureKind::Rejected => "rejected",
    }
}

/// The authorization and the rebuilt profile must describe the same destination: every origin the
/// authorization approved is one the profile declares, and the versions agree.
fn authorization_matches(authorization: &Authorization, profile: &ProviderProfile) -> bool {
    authorization.profile_version() == Some(profile.profile_version())
        && authorization.destinations().iter().all(|destination| {
            profile
                .authorized_destinations()
                .iter()
                .any(|declared| declared == destination)
        })
}

fn build_request(
    source: &InterpretationSource,
    request_version: &str,
    source_revision: i32,
    profile: &ProviderProfile,
    authorization: &Authorization,
) -> Result<InterpretationRequest, crate::providers::contracts::RequestValidationError> {
    InterpretationRequest::new(
        &source.capture_id,
        u64::try_from(source_revision).unwrap_or_default(),
        source.text_basis.clone(),
        &source.text,
        request_version,
        M1_INSTRUCTION_VERSION,
        profile,
        authorization.route_id(),
        source.time_context.clone(),
    )
}

/// A UUID derived from the job, so a retry of the same job proposes the same identifier and can
/// never record two proposals for one request.
fn proposal_id_for_job(job: &Job, request_version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PROPOSAL_ID_DOMAIN);
    hasher.update(job.job_id.as_bytes());
    hasher.update([0]);
    hasher.update(request_version.as_bytes());
    hasher.update([0]);
    hasher.update(job.source_revision.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}
