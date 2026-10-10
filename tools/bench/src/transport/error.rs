//! Transport failures. Variants are unit-like with fixed messages so no error report can carry
//! a URL, header, credential reference, secret or response body.

use ohand_core::providers::contracts::TransportError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HostTransportError {
    #[error("destination is not approved")]
    DestinationNotApproved,
    #[error("credential is not approved for this destination")]
    CredentialNotApproved,
    #[error("request is invalid")]
    InvalidRequest,
    #[error("credential is unavailable")]
    CredentialUnavailable,
    #[error("server certificate was refused")]
    TlsVerificationFailed,
    #[error("destination could not be reached")]
    Unreachable,
    #[error("request timed out")]
    Timeout,
    #[error("request was cancelled")]
    Cancelled,
    #[error("transport could not be configured")]
    Setup,
}

impl From<HostTransportError> for TransportError {
    fn from(error: HostTransportError) -> TransportError {
        match error {
            HostTransportError::DestinationNotApproved
            | HostTransportError::CredentialNotApproved
            | HostTransportError::InvalidRequest
            | HostTransportError::Setup => TransportError::Rejected,
            HostTransportError::CredentialUnavailable => TransportError::Unauthorized,
            HostTransportError::TlsVerificationFailed | HostTransportError::Unreachable => {
                TransportError::Unavailable
            }
            HostTransportError::Timeout => TransportError::Timeout,
            HostTransportError::Cancelled => TransportError::Cancelled,
        }
    }
}
