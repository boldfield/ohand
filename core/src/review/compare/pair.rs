use crate::privacy::routing::Authorization;
use crate::providers::contracts::ProviderCapability;
use crate::providers::contracts::{
    DiagnosticRequest, DiagnosticSetting, InterpretationRequest, ProviderProfile, ProviderProtocol,
    StructuredOutputMode,
};

use super::arm::{check_arm, ArmInput, ArmLabel};
use super::source::{ComparisonError, FrozenComparisonSource};

const ALL_SETTINGS: [DiagnosticSetting; 3] = [
    DiagnosticSetting::Temperature,
    DiagnosticSetting::MaxOutputTokens,
    DiagnosticSetting::Seed,
];

/// A way two diagnostic requests differ in something the model sees (or in what was asked of
/// it), which would make a difference in outcome attributable to more than model identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMismatch {
    CaptureId,
    SourceRevision,
    TextBasis,
    Text,
    RequestVersion,
    Route,
    TimeContext,
    InstructionVersion,
    Instructions,
    Context,
    Settings,
}

/// Compare the model-visible content of two already-built diagnostic requests. The live path
/// uses this to check a challenger request against the frozen primary request; profile pins
/// are deliberately not compared.
pub fn check_comparable(
    first: &DiagnosticRequest,
    second: &DiagnosticRequest,
) -> Result<(), Vec<RequestMismatch>> {
    let (a, b) = (first.request(), second.request());
    let (time_a, time_b) = (a.time_context(), b.time_context());
    let checks = [
        (a.capture_id() == b.capture_id(), RequestMismatch::CaptureId),
        (
            a.source_revision() == b.source_revision(),
            RequestMismatch::SourceRevision,
        ),
        (a.text_basis() == b.text_basis(), RequestMismatch::TextBasis),
        (a.text() == b.text(), RequestMismatch::Text),
        (
            a.request_version() == b.request_version(),
            RequestMismatch::RequestVersion,
        ),
        (a.route_id() == b.route_id(), RequestMismatch::Route),
        (
            time_a.timezone == time_b.timezone
                && time_a.locale == time_b.locale
                && time_a.reference_time == time_b.reference_time
                && time_a.utc_offset_at_capture == time_b.utc_offset_at_capture
                && time_a.calendar == time_b.calendar,
            RequestMismatch::TimeContext,
        ),
        (
            a.instruction_version() == b.instruction_version(),
            RequestMismatch::InstructionVersion,
        ),
        (
            first.instructions() == second.instructions(),
            RequestMismatch::Instructions,
        ),
        (
            first.context() == second.context(),
            RequestMismatch::Context,
        ),
        (
            first.settings() == second.settings(),
            RequestMismatch::Settings,
        ),
    ];
    let mismatches: Vec<RequestMismatch> = checks
        .into_iter()
        .filter(|(equal, _)| !equal)
        .map(|(_, mismatch)| mismatch)
        .collect();
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches)
    }
}

/// A configured difference between the arms other than the profile identities. Model and
/// profile names are free-form operator strings and are never reproduced here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationDifference {
    /// The arms use different models. This is the variable under study, not a confounder.
    Model,
    Protocol {
        first: ProviderProtocol,
        second: ProviderProtocol,
    },
    TimeoutSeconds {
        first: u32,
        second: u32,
    },
    RetryMaxAttempts {
        first: u32,
        second: u32,
    },
    StructuredOutput {
        first: StructuredOutputMode,
        second: StructuredOutputMode,
    },
    InputSizeLimit {
        first: Option<usize>,
        second: Option<usize>,
    },
}

impl ConfigurationDifference {
    fn is_confounder(&self) -> bool {
        !matches!(self, ConfigurationDifference::Model)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonLimitation {
    /// Agreement between the arms is not evidence that either is correct.
    AgreementIsNotAccuracy,
    /// Some settings were left to provider defaults the experiment cannot see.
    UnknownProviderDefaults,
    /// The arms differ in more than model identity.
    ConfigurationDiffers,
}

/// What can and cannot be concluded from running this pair, known before any request is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparability {
    pub differences: Vec<ConfigurationDifference>,
    /// Settings neither arm was asked for explicitly: each provider applies its own default,
    /// which is unknown here and may differ.
    pub unknown_defaults: Vec<DiagnosticSetting>,
    /// Always starts with [`ComparisonLimitation::AgreementIsNotAccuracy`].
    pub limitations: Vec<ComparisonLimitation>,
}

impl Comparability {
    /// True only when the sole known difference is the model and every setting was requested
    /// explicitly. It is not proof that model identity was the only variable: effective
    /// settings still have to be confirmed by the response metadata of each call.
    pub fn no_known_confounders(&self) -> bool {
        self.unknown_defaults.is_empty()
            && !self
                .differences
                .iter()
                .any(ConfigurationDifference::is_confounder)
    }

    fn derive(
        first: &ProviderProfile,
        second: &ProviderProfile,
        unknown_defaults: Vec<DiagnosticSetting>,
    ) -> Comparability {
        let mut differences = Vec::new();
        if first.model() != second.model() {
            differences.push(ConfigurationDifference::Model);
        }
        if first.protocol() != second.protocol() {
            differences.push(ConfigurationDifference::Protocol {
                first: first.protocol(),
                second: second.protocol(),
            });
        }
        if first.timeout_seconds() != second.timeout_seconds() {
            differences.push(ConfigurationDifference::TimeoutSeconds {
                first: first.timeout_seconds(),
                second: second.timeout_seconds(),
            });
        }
        let (first_retries, second_retries) = (
            first.retry_policy().max_attempts,
            second.retry_policy().max_attempts,
        );
        if first_retries != second_retries {
            differences.push(ConfigurationDifference::RetryMaxAttempts {
                first: first_retries,
                second: second_retries,
            });
        }
        let first_text = first.capability(ProviderCapability::TextInterpretation);
        let second_text = second.capability(ProviderCapability::TextInterpretation);
        let first_mode = first_text.map(|metadata| metadata.structured_output);
        let second_mode = second_text.map(|metadata| metadata.structured_output);
        if let (Some(first_mode), Some(second_mode)) = (first_mode, second_mode) {
            if first_mode != second_mode {
                differences.push(ConfigurationDifference::StructuredOutput {
                    first: first_mode,
                    second: second_mode,
                });
            }
        }
        let first_limit = first_text.and_then(|metadata| metadata.input_size_limit);
        let second_limit = second_text.and_then(|metadata| metadata.input_size_limit);
        if first_limit != second_limit {
            differences.push(ConfigurationDifference::InputSizeLimit {
                first: first_limit,
                second: second_limit,
            });
        }

        let mut limitations = vec![ComparisonLimitation::AgreementIsNotAccuracy];
        if !unknown_defaults.is_empty() {
            limitations.push(ComparisonLimitation::UnknownProviderDefaults);
        }
        if differences
            .iter()
            .any(ConfigurationDifference::is_confounder)
        {
            limitations.push(ComparisonLimitation::ConfigurationDiffers);
        }
        Comparability {
            differences,
            unknown_defaults,
            limitations,
        }
    }
}

/// One derived arm: its profile-pinned request and the authorization it must be dispatched
/// under. There is no store or apply handle here, and the request has no field for another
/// arm's output.
#[derive(Debug, Clone)]
pub struct PairedArm {
    request: DiagnosticRequest,
    authorization: Authorization,
}

impl PairedArm {
    pub fn request(&self) -> &DiagnosticRequest {
        &self.request
    }

    /// Send only to these destinations; re-authorize immediately before every dispatch.
    pub fn authorization(&self) -> &Authorization {
        &self.authorization
    }
}

#[derive(Debug, Clone)]
pub struct PairedRequests {
    pub first: PairedArm,
    pub second: PairedArm,
    pub comparability: Comparability,
}

fn arm_request(
    source: &FrozenComparisonSource,
    profile: &ProviderProfile,
    context: &str,
) -> Result<DiagnosticRequest, ComparisonError> {
    let request = InterpretationRequest::new(
        source.capture_id.clone(),
        source.source_revision,
        source.text_basis.clone(),
        source.text.clone(),
        source.request_version.clone(),
        source.instruction_version.clone(),
        profile,
        source.route_id.clone(),
        source.time_context.clone(),
    )?;
    Ok(DiagnosticRequest::new(
        request,
        source.instructions.clone(),
        context,
        source.settings.clone(),
    )?)
}

/// Derive two profile-pinned requests from one frozen source. Fails as a whole if either arm
/// lacks a current, independent text-interpretation authorization for its own profile, is
/// unverified, cannot honor the requested settings, or if the arms are not distinct profiles.
pub fn derive_pair(
    source: &FrozenComparisonSource,
    first: ArmInput<'_>,
    second: ArmInput<'_>,
) -> Result<PairedRequests, ComparisonError> {
    if first.profile.profile_version() == second.profile.profile_version() {
        return Err(ComparisonError::SameProfile);
    }
    check_arm(source, &first).map_err(|error| ComparisonError::Arm {
        arm: ArmLabel::A,
        error,
    })?;
    check_arm(source, &second).map_err(|error| ComparisonError::Arm {
        arm: ArmLabel::B,
        error,
    })?;

    let context = source.render_context();
    let first_request = arm_request(source, first.profile, &context)?;
    let second_request = arm_request(source, second.profile, &context)?;
    check_comparable(&first_request, &second_request).map_err(ComparisonError::NotComparable)?;

    let unknown_defaults = ALL_SETTINGS
        .into_iter()
        .filter(|setting| source.settings.value_for(*setting).is_none())
        .collect();
    let authorization_of = |input: &ArmInput<'_>| {
        input
            .authorization
            .authorization()
            .expect("check_arm accepted only authorized decisions")
            .clone()
    };
    Ok(PairedRequests {
        comparability: Comparability::derive(first.profile, second.profile, unknown_defaults),
        first: PairedArm {
            request: first_request,
            authorization: authorization_of(&first),
        },
        second: PairedArm {
            request: second_request,
            authorization: authorization_of(&second),
        },
    })
}
