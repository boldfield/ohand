use serde::{Deserialize, Serialize};
use std::fmt;

/// Normalized error classes shared with every effect interface (m1-contracts.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Transient,
    Permanent,
    Unauthorized,
    Cancelled,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Timeout,
    Cancelled,
    Unavailable,
    RateLimited,
    Unauthorized,
    InvalidOutput,
    OutputTooLarge,
    InputTooLarge,
    CapabilityUnavailable,
    ProfileMismatch,
    Rejected,
}

impl FailureKind {
    pub fn class(self) -> ErrorClass {
        match self {
            FailureKind::Timeout | FailureKind::Unavailable | FailureKind::RateLimited => {
                ErrorClass::Transient
            }
            FailureKind::Cancelled => ErrorClass::Cancelled,
            FailureKind::Unauthorized => ErrorClass::Unauthorized,
            FailureKind::CapabilityUnavailable => ErrorClass::Unsupported,
            FailureKind::InvalidOutput
            | FailureKind::OutputTooLarge
            | FailureKind::InputTooLarge
            | FailureKind::ProfileMismatch
            | FailureKind::Rejected => ErrorClass::Permanent,
        }
    }

    fn message(self) -> &'static str {
        match self {
            FailureKind::Timeout => "provider call exceeded the profile timeout",
            FailureKind::Cancelled => "provider call was cancelled",
            FailureKind::Unavailable => "provider is unavailable",
            FailureKind::RateLimited => "provider rate limit reached",
            FailureKind::Unauthorized => "provider credential was rejected or is missing",
            FailureKind::InvalidOutput => "provider output was not a valid JSON object",
            FailureKind::OutputTooLarge => "provider output exceeded the response size bound",
            FailureKind::InputTooLarge => "request text exceeds the profile input size limit",
            FailureKind::CapabilityUnavailable => "profile does not support text interpretation",
            FailureKind::ProfileMismatch => "request is not pinned to the supplied profile version",
            FailureKind::Rejected => "provider permanently rejected the request",
        }
    }
}

/// Normalized failure. The message is fixed per kind: it never carries provider payloads,
/// content or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailure {
    pub kind: FailureKind,
    pub class: ErrorClass,
    pub retriable: bool,
    pub message: String,
}

impl ProviderFailure {
    pub fn new(kind: FailureKind) -> ProviderFailure {
        let class = kind.class();
        ProviderFailure {
            kind,
            class,
            retriable: class == ErrorClass::Transient,
            message: kind.message().to_string(),
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderFailure {}
