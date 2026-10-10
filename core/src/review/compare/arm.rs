use thiserror::Error;

use crate::privacy::routing::{AuthorizationDecision, DenialReason, ProcessingCapability};
use crate::providers::contracts::{
    CapabilitySupport, DiagnosticSetting, DiagnosticSupport, ProviderCapability, ProviderProfile,
    RequestedSettings, SettingValue,
};

use super::source::FrozenComparisonSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmLabel {
    A,
    B,
}

/// Everything one arm needs, each supplied separately so that nothing about one arm can be
/// satisfied by the other: its pinned profile, its own current authorization decision, and what
/// its adapter declares it can honor for diagnostic calls.
#[derive(Debug, Clone, Copy)]
pub struct ArmInput<'a> {
    pub profile: &'a ProviderProfile,
    pub authorization: &'a AuthorizationDecision,
    pub adapter_support: &'a DiagnosticSupport,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArmError {
    #[error("the pinned profile has been revoked")]
    ProfileRevoked,
    #[error("destination or capability is not authorized: {0}")]
    NotAuthorized(DenialReason),
    #[error("authorization is for on-device processing, not a remote destination")]
    LocalAuthorization,
    #[error("authorization is for {0:?}, not text interpretation")]
    WrongCapability(ProcessingCapability),
    #[error("authorization is for a different profile version")]
    AuthorizationProfileMismatch,
    #[error("authorization is for a different route than the source")]
    AuthorizationRouteMismatch,
    #[error("text interpretation is unverified for this profile")]
    ProfileUnverified,
    #[error("text interpretation is not supported by this profile")]
    TextInterpretationUnavailable,
    #[error("source text exceeds the profile's input size limit")]
    InputTooLarge,
    #[error("the adapter does not support diagnostic calls")]
    DiagnosticsUnsupported,
    #[error("the adapter cannot honor the requested settings {0:?}")]
    UnsupportedSettings(Vec<DiagnosticSetting>),
}

/// Checks that make one arm eligible. The authorization must be for this arm's own profile
/// version, so one arm's grant can never stand in for the other's.
pub(super) fn check_arm(
    source: &FrozenComparisonSource,
    input: &ArmInput<'_>,
) -> Result<(), ArmError> {
    let authorization = match input.authorization {
        AuthorizationDecision::Denied(DenialReason::ProfileRevoked) => {
            return Err(ArmError::ProfileRevoked)
        }
        AuthorizationDecision::Denied(reason) => return Err(ArmError::NotAuthorized(*reason)),
        AuthorizationDecision::Authorized(authorization) => authorization,
    };
    if authorization.is_local() {
        return Err(ArmError::LocalAuthorization);
    }
    if authorization.capability() != ProcessingCapability::TextInterpretation {
        return Err(ArmError::WrongCapability(authorization.capability()));
    }
    if authorization.profile_version() != Some(input.profile.profile_version()) {
        return Err(ArmError::AuthorizationProfileMismatch);
    }
    if authorization.route_id() != source.route_id {
        return Err(ArmError::AuthorizationRouteMismatch);
    }

    let text_capability = input
        .profile
        .capability(ProviderCapability::TextInterpretation)
        .ok_or(ArmError::TextInterpretationUnavailable)?;
    match text_capability.support {
        CapabilitySupport::Supported => {}
        CapabilitySupport::Unverified => return Err(ArmError::ProfileUnverified),
        CapabilitySupport::Unsupported => return Err(ArmError::TextInterpretationUnavailable),
    }
    if text_capability
        .input_size_limit
        .is_some_and(|limit| source.text.len() > limit)
    {
        return Err(ArmError::InputTooLarge);
    }

    if !input.adapter_support.is_supported() {
        return Err(ArmError::DiagnosticsUnsupported);
    }
    let unsupported = unsupported_settings(&source.settings, input.adapter_support);
    if !unsupported.is_empty() {
        return Err(ArmError::UnsupportedSettings(unsupported));
    }
    Ok(())
}

fn unsupported_settings(
    requested: &RequestedSettings,
    support: &DiagnosticSupport,
) -> Vec<DiagnosticSetting> {
    requested
        .values()
        .iter()
        .map(SettingValue::setting)
        .filter(|setting| !support.supports(*setting))
        .collect()
}
