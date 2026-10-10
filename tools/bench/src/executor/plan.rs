use ohand_core::privacy::routing::AuthorizationDecision;
use ohand_core::providers::contracts::{
    CancelToken, Clock, DispatchLimits, ProviderAdapter, ProviderProfile,
};
use ohand_core::review::compare::{ArmLabel, FrozenComparisonSource};

use super::error::ExecutorError;
use crate::records::Case;

/// Requests are sent strictly one at a time: arms run sequentially and no retry overlaps
/// another request. The executor owns the only dispatch loop, so this is a fixed bound rather
/// than a setting.
pub const MAX_CONCURRENT_REQUESTS: usize = 1;
/// Upper bound on attempts for one side of one case, the first request included.
pub const MAX_ATTEMPTS_PER_SIDE: u32 = 5;
/// Upper bound on the request allowance of one run.
pub const MAX_REQUEST_CAP: u32 = 10_000;

/// Hard bounds for one run. Both are enforced on the number of requests actually reserved, so
/// retries and reruns count and an unknown money cost cannot lift them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorLimits {
    attempts_per_side: u32,
    request_cap: u32,
}

impl ExecutorLimits {
    /// `attempts_per_side` includes the first request; `request_cap` is the allowance of this
    /// run alone. A cap of zero sends nothing.
    pub fn new(attempts_per_side: u32, request_cap: u32) -> Result<Self, ExecutorError> {
        if attempts_per_side == 0 || attempts_per_side > MAX_ATTEMPTS_PER_SIDE {
            return Err(ExecutorError::InvalidLimits(
                "attempts per side must be between 1 and the maximum",
            ));
        }
        if request_cap > MAX_REQUEST_CAP {
            return Err(ExecutorError::InvalidLimits(
                "request cap exceeds the maximum allowance",
            ));
        }
        Ok(ExecutorLimits {
            attempts_per_side,
            request_cap,
        })
    }

    pub fn attempts_per_side(&self) -> u32 {
        self.attempts_per_side
    }

    pub fn request_cap(&self) -> u32 {
        self.request_cap
    }
}

/// One synthetic case: its corpus identifier, its content and the frozen source both arms are
/// derived from.
#[derive(Debug, Clone)]
pub struct PlannedCase {
    pub(super) case_id: String,
    pub(super) content: String,
    pub(super) source: FrozenComparisonSource,
}

impl PlannedCase {
    /// `content` is the synthetic case text recorded in the journal; it must be the text the
    /// frozen source carries.
    pub fn new(
        case_id: impl Into<String>,
        content: impl Into<String>,
        source: FrozenComparisonSource,
    ) -> Result<PlannedCase, ExecutorError> {
        let case_id = case_id.into();
        let content = content.into();
        Case::new("planned", case_id.clone(), content.clone())
            .map_err(ExecutorError::InvalidCase)?;
        Ok(PlannedCase {
            case_id,
            content,
            source,
        })
    }

    pub fn case_id(&self) -> &str {
        &self.case_id
    }
}

/// What the executor asks an authorizer about. The answer must be a fresh decision for this
/// arm's own pinned profile; one arm's decision never stands in for the other's.
#[derive(Debug, Clone, Copy)]
pub struct AuthorizationQuery<'a> {
    pub arm: ArmLabel,
    pub case_id: &'a str,
    pub route_id: &'a str,
    pub profile_version: &'a str,
}

/// Source of current destination and capability decisions. It is asked immediately before every
/// request, and a revoked or changed decision stops the request.
pub trait ArmAuthorizer {
    fn authorize(&self, query: &AuthorizationQuery<'_>) -> AuthorizationDecision;
}

/// Everything one arm needs. There is no store or apply handle.
#[derive(Clone, Copy)]
pub struct ArmBinding<'a> {
    pub profile: &'a ProviderProfile,
    pub adapter: &'a dyn ProviderAdapter,
    pub authorizer: &'a dyn ArmAuthorizer,
}

/// Time, cancellation and dispatch bounds shared by every request of a run.
#[derive(Clone, Copy)]
pub struct RunControls<'a> {
    pub clock: &'a dyn Clock,
    pub cancel: &'a CancelToken,
    pub dispatch_limits: DispatchLimits,
}

/// One side of one case, named for an explicit rerun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideKey {
    pub case_id: String,
    pub arm: ArmLabel,
}

/// A run: the cases to cover, the two arms, and the explicit sides to send again.
pub struct RunRequest<'a> {
    pub cases: &'a [PlannedCase],
    pub arm_a: ArmBinding<'a>,
    pub arm_b: ArmBinding<'a>,
    pub controls: RunControls<'a>,
    pub limits: ExecutorLimits,
    /// Sides to dispatch again even though they were attempted before. Each rerun reserves new
    /// allowance from this run's cap and records the attempt it follows.
    pub reruns: &'a [SideKey],
}
