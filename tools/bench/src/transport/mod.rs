//! Bounded, credential-safe host HTTP transport for the developer-only benchmark runner.
//!
//! [`HostTransport`] is the one secure HTTP effect; [`bindings`] adapts it to the provider
//! transport traits that landed in core. Secret bytes are resolved here, out of band, at
//! dispatch time, and never leave this module: not in arguments, journals, output or errors.

mod bindings;
mod credential;
mod dns;
mod error;
mod exchange;
mod host;
mod policy;
mod redact;

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;

pub use credential::{CredentialError, CredentialResolver, EnvCredentialResolver, SecretValue};
pub use error::HostTransportError;
pub use host::{
    AttachmentRule, ClockDeadline, CredentialUse, HostTransport, HostTransportBuilder,
    SecureRequest, SecureResponse,
};
pub use policy::DestinationPolicy;
