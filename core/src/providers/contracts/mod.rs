//! Provider protocol contracts and normalization.
//!
//! Versioned non-secret provider profiles, the normalized interpretation request, and a
//! shared `dispatch` harness that wraps any [`ProviderAdapter`] with deadline, cancellation,
//! size-bound and output-validation enforcement. Adapters only perform transport and return
//! raw bytes or a [`TransportError`]; every outcome visible to callers is produced here.

mod dispatch;
mod failure;
pub mod fake;
mod profile;
mod request;

pub use dispatch::{
    dispatch, AdapterCall, CancelToken, Clock, DispatchLimits, InterpretationOutput, ManualClock,
    ProviderAdapter, SystemClock, TransportError, DEFAULT_MAX_RESPONSE_BYTES,
};
pub use failure::{ErrorClass, FailureKind, ProviderFailure};
pub use profile::{
    CapabilityMetadata, CapabilitySupport, CredentialRef, ProfileValidationError,
    ProviderCapability, ProviderProfile, ProviderProfileBuilder, ProviderProtocol, RetryPolicy,
    StructuredOutputMode, PROFILE_SCHEMA_VERSION,
};
pub use request::{InterpretationRequest, RequestValidationError, TextBasis};
