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
//! that are safe to publish: no credential reference, endpoint or provider request identifier.
//! A diagnostic outcome is evidence only; it has no field that could confer authority.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
const MAX_PUBLIC_IDENTIFIER_CHARS: usize = 128;
const MAX_PUBLIC_IDENTIFIER_GROUPS: usize = 12;

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

/// Publishable provenance: profile identity and model names only. Configured identifiers are
/// published only when they have a plain model-name shape (see [`is_publishable_identifier`]);
/// `None` means the configured value was withheld, never that it was absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProvenance {
    pub protocol: ProviderProtocol,
    pub profile_id: Option<String>,
    /// Core-assigned UUID of the pinned profile revision.
    pub profile_version: String,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    /// The provider named a model that is not the pinned model or a dated revision of it;
    /// the name itself is withheld.
    pub reported_model_differs: bool,
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

/// Hyphen groups that mark credential, request-identifier or endpoint vocabulary. A value
/// containing one is withheld even when its shape is otherwise plain.
const UNPUBLISHABLE_GROUPS: &[&str] = &[
    "akia",
    "apikey",
    "auth",
    "bearer",
    "chatcmpl",
    "cred",
    "credential",
    "ghp",
    "gho",
    "http",
    "https",
    "id",
    "key",
    "localhost",
    "msg",
    "pass",
    "password",
    "pat",
    "pk",
    "req",
    "request",
    "rk",
    "secret",
    "sess",
    "session",
    "sk",
    "token",
    "trace",
    "www",
];

fn is_publishable_group(group: &str) -> bool {
    let letters = group.bytes().filter(u8::is_ascii_lowercase).count();
    let digits = group.bytes().filter(u8::is_ascii_digit).count();
    if group.is_empty() || UNPUBLISHABLE_GROUPS.contains(&group) {
        return false;
    }
    if let Some((major, minor)) = group.split_once('.') {
        // A dotted version such as `4.5`; dotted words are host names.
        return [major, minor].iter().all(|part| {
            (1..=4).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
        });
    }
    if letters + digits != group.len() {
        return false;
    }
    match (letters, digits) {
        (_, 0) => letters <= 12,
        (0, _) => digits <= 8,
        // Short mixed groups such as `4o`, `72b` or `v2`; longer ones are opaque tokens.
        _ => group.len() <= 4,
    }
}

/// Fail-closed check for configured or reported identifiers entering public metadata. Only
/// lowercase hyphenated model-name shapes that start with a letter pass: endpoints, host
/// names, IP addresses, paths, UUIDs, uppercase or underscored identifiers, opaque tokens and
/// credential or request-identifier vocabulary are all withheld.
fn is_publishable_identifier(value: &str) -> bool {
    if value.len() > MAX_PUBLIC_IDENTIFIER_CHARS
        || !value.starts_with(|first: char| first.is_ascii_lowercase())
    {
        return false;
    }
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() <= MAX_PUBLIC_IDENTIFIER_GROUPS && groups.into_iter().all(is_publishable_group)
}

/// Reported model names are provider-controlled, so a name is published only when it is
/// the pinned model or extends it with a date or short numeric revision suffix. Anything
/// else (an endpoint, credential, request id or different model) is never echoed.
fn reported_model_extends_pinned(pinned_model: &str, reported_model: &str) -> bool {
    if reported_model.len() > MAX_PUBLIC_IDENTIFIER_CHARS {
        return false;
    }
    if reported_model == pinned_model {
        return true;
    }
    let Some(suffix) = reported_model
        .strip_prefix(pinned_model)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let digit_groups: Vec<&str> = suffix.split('-').collect();
    let all_digits = digit_groups
        .iter()
        .all(|group| !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()));
    if !all_digits {
        return false;
    }
    let group_lengths: Vec<usize> = digit_groups.iter().map(|group| group.len()).collect();
    matches!(group_lengths.as_slice(), [1..=4] | [8] | [4, 2, 2])
}

fn publishable(value: &str) -> Option<String> {
    is_publishable_identifier(value).then(|| value.to_string())
}

fn build_metadata(
    profile: &ProviderProfile,
    requested: &RequestedSettings,
    observations: Option<&CallObservations>,
    elapsed_ms: u64,
) -> DiagnosticMetadata {
    let reported_model = observations.and_then(|observed| observed.reported_model.as_deref());
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
            profile_id: publishable(profile.profile_id()),
            profile_version: profile.profile_version().to_string(),
            requested_model: publishable(profile.model()),
            reported_model: reported_model
                .filter(|name| reported_model_extends_pinned(profile.model(), name))
                .and_then(publishable),
            reported_model_differs: reported_model
                .is_some_and(|name| !reported_model_extends_pinned(profile.model(), name)),
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
