use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::failure::{FailureKind, ProviderFailure};
use super::profile::{CapabilitySupport, ProviderCapability, ProviderProfile};
use super::request::InterpretationRequest;

pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Monotonic millisecond clock, injectable so deadline behavior is deterministic in tests.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Manually advanced clock for deterministic tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    now_ms: AtomicU64,
}

impl ManualClock {
    pub fn new() -> ManualClock {
        ManualClock::default()
    }

    pub fn advance_ms(&self, milliseconds: u64) {
        self.now_ms.fetch_add(milliseconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// Shared cancellation control; clones observe the same state.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Transport-level failure an adapter may report. Adapters never produce normalized outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportError {
    Timeout,
    Cancelled,
    Unavailable,
    RateLimited,
    Unauthorized,
    Rejected,
    InvalidOutput,
}

impl From<TransportError> for FailureKind {
    fn from(error: TransportError) -> FailureKind {
        match error {
            TransportError::Timeout => FailureKind::Timeout,
            TransportError::Cancelled => FailureKind::Cancelled,
            TransportError::Unavailable => FailureKind::Unavailable,
            TransportError::RateLimited => FailureKind::RateLimited,
            TransportError::Unauthorized => FailureKind::Unauthorized,
            TransportError::Rejected => FailureKind::Rejected,
            TransportError::InvalidOutput => FailureKind::InvalidOutput,
        }
    }
}

/// Everything an adapter receives for one call. There is deliberately no field for earlier
/// requests or responses: each call is self-contained.
pub struct AdapterCall<'a> {
    pub request: &'a InterpretationRequest,
    pub profile: &'a ProviderProfile,
    /// Absolute deadline on the dispatch clock; real transports should pass the remaining
    /// time to the HTTP layer. `dispatch` enforces it regardless.
    pub deadline_ms: u64,
    /// Bound real transports should apply while reading the body.
    pub max_response_bytes: usize,
    pub cancel: &'a CancelToken,
    pub clock: &'a dyn Clock,
}

/// A protocol adapter: transport only. It returns the raw response body or a transport error;
/// deadline, cancellation, size and output validation are applied by [`dispatch`].
pub trait ProviderAdapter {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchLimits {
    pub max_response_bytes: usize,
}

impl Default for DispatchLimits {
    fn default() -> Self {
        DispatchLimits {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

/// Validated provider output. Semantic validation of the proposal belongs to interpretation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterpretationOutput {
    pub request_version: String,
    pub proposal: serde_json::Map<String, serde_json::Value>,
    pub elapsed_ms: u64,
}

fn fail(kind: FailureKind) -> ProviderFailure {
    ProviderFailure::new(kind)
}

/// Run one interpretation request through an adapter under the profile's contract.
pub fn dispatch(
    adapter: &dyn ProviderAdapter,
    profile: &ProviderProfile,
    request: &InterpretationRequest,
    clock: &dyn Clock,
    cancel: &CancelToken,
    limits: &DispatchLimits,
) -> Result<InterpretationOutput, ProviderFailure> {
    if !request.is_pinned_to(profile) {
        return Err(fail(FailureKind::ProfileMismatch));
    }
    let text_capability = profile
        .capability(ProviderCapability::TextInterpretation)
        .filter(|metadata| metadata.support == CapabilitySupport::Supported)
        .ok_or_else(|| fail(FailureKind::CapabilityUnavailable))?;
    if text_capability
        .input_size_limit
        .is_some_and(|limit| request.text().len() > limit)
    {
        return Err(fail(FailureKind::InputTooLarge));
    }
    if cancel.is_cancelled() {
        return Err(fail(FailureKind::Cancelled));
    }

    let started_ms = clock.now_ms();
    let timeout_ms = u64::from(profile.timeout_seconds()) * 1000;
    let call = AdapterCall {
        request,
        profile,
        deadline_ms: started_ms + timeout_ms,
        max_response_bytes: limits.max_response_bytes,
        cancel,
        clock,
    };
    let result = adapter.invoke(&call);

    // A late or cancelled result is discarded even if the adapter produced output.
    if cancel.is_cancelled() {
        return Err(fail(FailureKind::Cancelled));
    }
    let elapsed_ms = clock.now_ms().saturating_sub(started_ms);
    if elapsed_ms > timeout_ms {
        return Err(fail(FailureKind::Timeout));
    }

    let raw_output = result.map_err(|error| fail(error.into()))?;
    if raw_output.len() > limits.max_response_bytes {
        return Err(fail(FailureKind::OutputTooLarge));
    }
    match serde_json::from_slice::<serde_json::Value>(&raw_output) {
        Ok(serde_json::Value::Object(proposal)) => Ok(InterpretationOutput {
            request_version: request.request_version().to_string(),
            proposal,
            elapsed_ms,
        }),
        _ => Err(fail(FailureKind::InvalidOutput)),
    }
}
