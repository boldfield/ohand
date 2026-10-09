//! Interpretation dispatcher (I06).
//!
//! [`InterpretationDispatcher::run_job`] takes one claimed interpretation job from a raw saved
//! capture to the I05 apply boundary:
//!
//! 1. The source is read from durable state (current effective text, capture time context,
//!    immutable route). A job whose item is gone, inactive or revised is retired; an item with no
//!    text yet backs off.
//! 2. The job is authorized (V02/V03) before anything else runs. A denial backs the job off with
//!    reason `unauthorized` so the item reports that it awaits configuration; nothing is sent and
//!    nothing is applied.
//! 3. The offline fast path (F03) runs first and needs no provider. A recognized command is
//!    applied directly. A fast-path abstention or non-match never suppresses the provider.
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
//! Retry policy for transient failures comes from the pinned profile's retry policy. Attempts are
//! not capped: the capture stays safe and the item reports `retrying_after_transient` until the
//! provider recovers, the profile is revoked or the item changes.

mod registry;
mod source;

pub use registry::AdapterRegistry;

use crate::interpretation::apply::{
    apply_interpretation_proposal, record_interpretation_failure, ApplyError, ApplyOutcome,
};
use crate::interpretation::contracts::Proposal;
use crate::interpretation::fast_path::recognize_with_session_topic;
use crate::interpretation::instructions::{InterpretationMapping, M1_INSTRUCTION_VERSION};
use crate::jobs::queue::{fail_job_with_backoff, get_job, Job, JobStatus};
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
        job: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        if job.job_type != JOB_TYPE_INTERPRET {
            return Err(DispatchError::NotAnInterpretationJob {
                job_type: job.job_type.clone(),
            });
        }
        if job.status != JobStatus::Running {
            return Err(DispatchError::JobNotRunning {
                status: job.status.as_str(),
            });
        }
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

        let authorization = match authorize_job(db.conn(), &job.job_id)? {
            crate::privacy::routing::AuthorizationDecision::Authorized(authorization) => {
                authorization
            }
            crate::privacy::routing::AuthorizationDecision::Denied(denial) => {
                return self.denied(db, job, denial, now)
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
                    let backoff = transient_backoff(profile.retry_policy());
                    self.back_off(
                        db,
                        job,
                        WaitReason::Transient(failure.kind),
                        failure_kind_name(failure.kind),
                        backoff,
                        now,
                    )
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
        match apply_interpretation_proposal(db, &job.job_id, job.attempt_count, proposal, now) {
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
