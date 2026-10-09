//! Additive diagnostic call contract.
//!
//! A diagnostic call sends explicit, already-rendered instructions and normalized context with
//! explicitly requested settings, and returns the validated output together with
//! [`DiagnosticMetadata`]. Nothing here changes the production [`dispatch`](super::dispatch):
//! an adapter that does not declare diagnostic support is refused before any transport happens,
//! so a diagnostic request can never fall back to an adapter's built-in prompt or defaults.
//!
//! Metadata never invents facts. A setting the provider did not confirm is `Unknown`, usage the
//! provider did not report is `Unavailable` (never zero), and provenance carries only values
//! the core assigned or derived: never a configured free-form string (profile id, model name,
//! credential reference, endpoint) nor a provider-controlled one (reported model name, request
//! identifier).
//! A diagnostic outcome is evidence only; it has no field that could confer authority.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::dispatch::{
    complete_call, preflight, CancelToken, Clock, DispatchLimits, InterpretationOutput,
    ProviderAdapter,
};
use super::failure::{ErrorClass, FailureKind, ProviderFailure};
use super::profile::{ProviderProfile, ProviderProtocol};
use super::request::InterpretationRequest;
use crate::interpretation::instructions::content_version;

pub const MAX_DIAGNOSTIC_INSTRUCTION_BYTES: usize = 64 * 1024;
pub const MAX_DIAGNOSTIC_CONTEXT_BYTES: usize = 256 * 1024;
pub const MAX_TEMPERATURE_MILLI: u32 = 2000;
/// Longest suffix (after the separating hyphen) that can still count as a pinned revision:
/// a `YYYY-MM-DD` date. Bounding the suffix rather than the whole name keeps an exact echo of a
/// long pinned model classified as [`ReportedModel::Pinned`].
const MAX_REVISION_SUFFIX_CHARS: usize = 10;

/// A model setting an experiment may request explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSetting {
    Temperature,
    MaxOutputTokens,
    Seed,
}

/// A concrete setting value. Temperature is in thousandths so values compare exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum SettingValue {
    TemperatureMilli(u32),
    MaxOutputTokens(u32),
    Seed(u64),
}

impl SettingValue {
    pub fn setting(&self) -> DiagnosticSetting {
        match self {
            SettingValue::TemperatureMilli(_) => DiagnosticSetting::Temperature,
            SettingValue::MaxOutputTokens(_) => DiagnosticSetting::MaxOutputTokens,
            SettingValue::Seed(_) => DiagnosticSetting::Seed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiagnosticRequestError {
    #[error("diagnostic instructions must not be empty")]
    EmptyInstructions,
    #[error("diagnostic instructions exceed {MAX_DIAGNOSTIC_INSTRUCTION_BYTES} bytes")]
    InstructionsTooLarge,
    #[error("diagnostic context must not be empty")]
    EmptyContext,
    #[error("diagnostic context exceeds {MAX_DIAGNOSTIC_CONTEXT_BYTES} bytes")]
    ContextTooLarge,
    #[error("instructions do not match the request's instruction version")]
    InstructionVersionMismatch,
    #[error("requested value for {0:?} is out of range")]
    InvalidSetting(DiagnosticSetting),
}

/// Settings an experiment asks for. An unset setting is left to the provider and reported as
/// such; it is never filled in by an adapter default that the experiment cannot see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RequestedSettings {
    temperature_milli: Option<u32>,
    max_output_tokens: Option<u32>,
    seed: Option<u64>,
}

impl RequestedSettings {
    pub fn new() -> RequestedSettings {
        RequestedSettings::default()
    }

    pub fn with_temperature_milli(mut self, milli: u32) -> Result<Self, DiagnosticRequestError> {
        if milli > MAX_TEMPERATURE_MILLI {
            return Err(DiagnosticRequestError::InvalidSetting(
                DiagnosticSetting::Temperature,
            ));
        }
        self.temperature_milli = Some(milli);
        Ok(self)
    }

    pub fn with_max_output_tokens(mut self, tokens: u32) -> Result<Self, DiagnosticRequestError> {
        if tokens == 0 {
            return Err(DiagnosticRequestError::InvalidSetting(
                DiagnosticSetting::MaxOutputTokens,
            ));
        }
        self.max_output_tokens = Some(tokens);
        Ok(self)
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Requested values in a stable order.
    pub fn values(&self) -> Vec<SettingValue> {
        let mut values = Vec::new();
        if let Some(milli) = self.temperature_milli {
            values.push(SettingValue::TemperatureMilli(milli));
        }
        if let Some(tokens) = self.max_output_tokens {
            values.push(SettingValue::MaxOutputTokens(tokens));
        }
        if let Some(seed) = self.seed {
            values.push(SettingValue::Seed(seed));
        }
        values
    }

    pub fn value_for(&self, setting: DiagnosticSetting) -> Option<SettingValue> {
        self.values()
            .into_iter()
            .find(|value| value.setting() == setting)
    }
}

/// One diagnostic request: the pinned interpretation request plus the exact instructions and
/// normalized context to send. It deliberately has no field for another model's answer.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticRequest {
    request: InterpretationRequest,
    instructions: String,
    context: String,
    settings: RequestedSettings,
}

impl DiagnosticRequest {
    /// `instructions` must be the text the request's `instruction_version` names (its content
    /// digest), so a version label can never stand in for different text.
    pub fn new(
        request: InterpretationRequest,
        instructions: impl Into<String>,
        context: impl Into<String>,
        settings: RequestedSettings,
    ) -> Result<DiagnosticRequest, DiagnosticRequestError> {
        let instructions = instructions.into();
        let context = context.into();
        if instructions.trim().is_empty() {
            return Err(DiagnosticRequestError::EmptyInstructions);
        }
        if instructions.len() > MAX_DIAGNOSTIC_INSTRUCTION_BYTES {
            return Err(DiagnosticRequestError::InstructionsTooLarge);
        }
        if context.trim().is_empty() {
            return Err(DiagnosticRequestError::EmptyContext);
        }
        if context.len() > MAX_DIAGNOSTIC_CONTEXT_BYTES {
            return Err(DiagnosticRequestError::ContextTooLarge);
        }
        if content_version(&instructions) != request.instruction_version() {
            return Err(DiagnosticRequestError::InstructionVersionMismatch);
        }
        Ok(DiagnosticRequest {
            request,
            instructions,
            context,
            settings,
        })
    }

    pub fn request(&self) -> &InterpretationRequest {
        &self.request
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn context(&self) -> &str {
        &self.context
    }
    pub fn settings(&self) -> &RequestedSettings {
        &self.settings
    }
}

/// What an adapter declares it can honor for diagnostic calls. The default is nothing, which
/// is what every adapter written before this contract reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticSupport {
    settings: Option<BTreeSet<DiagnosticSetting>>,
}

impl DiagnosticSupport {
    pub fn unsupported() -> DiagnosticSupport {
        DiagnosticSupport::default()
    }

    pub fn supporting(settings: impl IntoIterator<Item = DiagnosticSetting>) -> DiagnosticSupport {
        DiagnosticSupport {
            settings: Some(settings.into_iter().collect()),
        }
    }

    pub fn is_supported(&self) -> bool {
        self.settings.is_some()
    }

    pub fn supports(&self, setting: DiagnosticSetting) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.contains(&setting))
    }
}

/// Token counts the provider reported. A count the provider omitted is `None`, not zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum UsageAvailability {
    #[default]
    Unavailable,
    Reported {
        usage: TokenUsage,
    },
}

impl UsageAvailability {
    fn from_reported(usage: Option<TokenUsage>) -> UsageAvailability {
        match usage {
            Some(usage) if usage.input_tokens.is_some() || usage.output_tokens.is_some() => {
                UsageAvailability::Reported { usage }
            }
            _ => UsageAvailability::Unavailable,
        }
    }
}

/// Facts an adapter could extract from the same response that produced the output. Provider
/// request identifiers are deliberately not representable here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallObservations {
    pub effective_settings: Vec<SettingValue>,
    pub usage: Option<TokenUsage>,
    pub reported_model: Option<String>,
}

/// Raw diagnostic reply: the body plus whatever the provider disclosed, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticResponse {
    pub body: Vec<u8>,
    pub observations: Option<CallObservations>,
}

/// Everything an adapter receives for one diagnostic call.
pub struct DiagnosticAdapterCall<'a> {
    pub request: &'a DiagnosticRequest,
    pub profile: &'a ProviderProfile,
    pub deadline_ms: u64,
    pub max_response_bytes: usize,
    pub cancel: &'a CancelToken,
    pub clock: &'a dyn Clock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectiveSetting {
    Reported { value: SettingValue },
    Unknown,
}

/// One setting's requested and effective state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingOutcome {
    pub setting: DiagnosticSetting,
    pub requested: Option<SettingValue>,
    pub effective: EffectiveSetting,
}

/// How the model a provider named relates to the pinned model. The name itself is never
/// published: it is provider-controlled and could carry an endpoint, credential or request id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedModel {
    /// The provider did not name a model.
    NotReported,
    /// The provider named exactly the pinned model.
    Pinned,
    /// The provider named the pinned model extended by a date or short numeric revision.
    PinnedRevision,
    /// The provider named some other model.
    Different,
}

/// Publishable provenance. Configured profile ids and model names are free-form strings that
/// no validator can prove secret-free, so they are never published; a report that needs a
/// human-readable label resolves `profile_version` through its own private configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProvenance {
    pub protocol: ProviderProtocol,
    /// Core-assigned UUID of the pinned profile revision, in canonical hyphenated form.
    pub profile_version: String,
    pub reported_model: ReportedModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticMetadata {
    pub settings: Vec<SettingOutcome>,
    pub usage: UsageAvailability,
    pub provenance: ProviderProvenance,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticOutput {
    pub output: InterpretationOutput,
    pub metadata: DiagnosticMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticFailureKind {
    /// The adapter declared no diagnostic support; nothing was sent.
    DiagnosticsUnsupported,
    /// The adapter cannot honor these requested settings; nothing was sent.
    UnsupportedSettings {
        settings: Vec<DiagnosticSetting>,
    },
    Provider {
        failure: ProviderFailure,
    },
}

/// A failed diagnostic call. `usage` is `Unavailable` unless the provider actually reported it
/// before the call was rejected, cancelled or timed out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticFailure {
    pub kind: DiagnosticFailureKind,
    pub usage: UsageAvailability,
}

impl DiagnosticFailure {
    fn without_usage(kind: DiagnosticFailureKind) -> DiagnosticFailure {
        DiagnosticFailure {
            kind,
            usage: UsageAvailability::Unavailable,
        }
    }

    pub fn class(&self) -> ErrorClass {
        match &self.kind {
            DiagnosticFailureKind::DiagnosticsUnsupported
            | DiagnosticFailureKind::UnsupportedSettings { .. } => ErrorClass::Unsupported,
            DiagnosticFailureKind::Provider { failure } => failure.class,
        }
    }
}

impl std::fmt::Display for DiagnosticFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            DiagnosticFailureKind::DiagnosticsUnsupported => {
                formatter.write_str("adapter does not support diagnostic calls")
            }
            DiagnosticFailureKind::UnsupportedSettings { .. } => {
                formatter.write_str("adapter cannot honor the requested settings")
            }
            DiagnosticFailureKind::Provider { failure } => failure.fmt(formatter),
        }
    }
}

impl std::error::Error for DiagnosticFailure {}

fn classify_reported_model(pinned_model: &str, reported_model: Option<&str>) -> ReportedModel {
    let Some(reported_model) = reported_model else {
        return ReportedModel::NotReported;
    };
    // Exact equality is decided before any bound on the provider-controlled value: the pinned
    // model is operator configuration with no length limit, and an exact echo of it is `Pinned`
    // however long it is.
    if reported_model == pinned_model {
        return ReportedModel::Pinned;
    }
    let Some(suffix) = reported_model
        .strip_prefix(pinned_model)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return ReportedModel::Different;
    };
    if suffix.len() > MAX_REVISION_SUFFIX_CHARS {
        return ReportedModel::Different;
    }
    let digit_groups: Vec<&str> = suffix.split('-').collect();
    let all_digits = digit_groups
        .iter()
        .all(|group| !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()));
    let group_lengths: Vec<usize> = digit_groups.iter().map(|group| group.len()).collect();
    if all_digits && matches!(group_lengths.as_slice(), [1..=4] | [8] | [4, 2, 2]) {
        ReportedModel::PinnedRevision
    } else {
        ReportedModel::Different
    }
}

/// Profile versions are core-assigned UUIDs; publishing the parsed canonical form means no
/// other string shape can pass through this field.
fn canonical_profile_version(profile: &ProviderProfile) -> String {
    Uuid::parse_str(profile.profile_version())
        .map(|version| version.hyphenated().to_string())
        .unwrap_or_default()
}

fn build_metadata(
    profile: &ProviderProfile,
    requested: &RequestedSettings,
    observations: Option<&CallObservations>,
    elapsed_ms: u64,
) -> DiagnosticMetadata {
    let reported_values: &[SettingValue] = observations
        .map(|observed| observed.effective_settings.as_slice())
        .unwrap_or(&[]);
    let mut settings = Vec::new();
    for setting in [
        DiagnosticSetting::Temperature,
        DiagnosticSetting::MaxOutputTokens,
        DiagnosticSetting::Seed,
    ] {
        let requested_value = requested.value_for(setting);
        let effective_value = reported_values
            .iter()
            .find(|value| value.setting() == setting)
            .copied();
        if requested_value.is_none() && effective_value.is_none() {
            continue;
        }
        settings.push(SettingOutcome {
            setting,
            requested: requested_value,
            effective: match effective_value {
                Some(value) => EffectiveSetting::Reported { value },
                None => EffectiveSetting::Unknown,
            },
        });
    }
    DiagnosticMetadata {
        settings,
        usage: UsageAvailability::from_reported(observations.and_then(|observed| observed.usage)),
        provenance: ProviderProvenance {
            protocol: profile.protocol(),
            profile_version: canonical_profile_version(profile),
            reported_model: classify_reported_model(
                profile.model(),
                observations.and_then(|observed| observed.reported_model.as_deref()),
            ),
        },
        elapsed_ms,
    }
}

/// Run one diagnostic request through an adapter under the profile's contract. Profile
/// pinning, capability, size, deadline, cancellation and output bounds are the same as for
/// [`dispatch`](super::dispatch); in addition the adapter must declare diagnostic support and
/// every requested setting, otherwise the call fails before any transport happens.
pub fn dispatch_diagnostic(
    adapter: &dyn ProviderAdapter,
    profile: &ProviderProfile,
    request: &DiagnosticRequest,
    clock: &dyn Clock,
    cancel: &CancelToken,
    limits: &DispatchLimits,
) -> Result<DiagnosticOutput, DiagnosticFailure> {
    let provider_failure = |failure: ProviderFailure| {
        DiagnosticFailure::without_usage(DiagnosticFailureKind::Provider { failure })
    };
    preflight(profile, request.request()).map_err(provider_failure)?;

    let support = adapter.diagnostic_support();
    if !support.is_supported() {
        return Err(DiagnosticFailure::without_usage(
            DiagnosticFailureKind::DiagnosticsUnsupported,
        ));
    }
    let unsupported_settings: Vec<DiagnosticSetting> = request
        .settings()
        .values()
        .iter()
        .map(SettingValue::setting)
        .filter(|setting| !support.supports(*setting))
        .collect();
    if !unsupported_settings.is_empty() {
        return Err(DiagnosticFailure::without_usage(
            DiagnosticFailureKind::UnsupportedSettings {
                settings: unsupported_settings,
            },
        ));
    }
    if cancel.is_cancelled() {
        return Err(provider_failure(ProviderFailure::new(
            FailureKind::Cancelled,
        )));
    }

    let started_ms = clock.now_ms();
    let timeout_ms = u64::from(profile.timeout_seconds()) * 1000;
    let call = DiagnosticAdapterCall {
        request,
        profile,
        deadline_ms: started_ms + timeout_ms,
        max_response_bytes: limits.max_response_bytes,
        cancel,
        clock,
    };
    let (body, observations) = match adapter.invoke_diagnostic(&call) {
        Ok(response) => (Ok(response.body), response.observations),
        Err(error) => (Err(error), None),
    };
    let reported_usage =
        UsageAvailability::from_reported(observations.as_ref().and_then(|observed| observed.usage));
    match complete_call(
        request.request(),
        body,
        started_ms,
        timeout_ms,
        clock,
        cancel,
        limits,
    ) {
        Ok(output) => {
            let elapsed_ms = output.elapsed_ms;
            Ok(DiagnosticOutput {
                output,
                metadata: build_metadata(
                    profile,
                    request.settings(),
                    observations.as_ref(),
                    elapsed_ms,
                ),
            })
        }
        Err(failure) => Err(DiagnosticFailure {
            kind: DiagnosticFailureKind::Provider { failure },
            usage: reported_usage,
        }),
    }
}
