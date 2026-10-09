use chrono::{DateTime, Utc};
use std::sync::Arc;
use uuid::Uuid;

use super::*;
use crate::interpretation::instructions::{content_version, M1_INSTRUCTION_TEXT};
use crate::jobs::configuration::revoke_profile;
use crate::jobs::queue::enqueue_job;
use crate::privacy::routing::{
    authorize_job, AuthorizationDecision, DenialReason, ProcessingCapability, JOB_TYPE_INTERPRET,
    JOB_TYPE_SHADOW_REVIEW,
};
use crate::providers::contracts::{
    CapabilityMetadata, DiagnosticRequest, DiagnosticSetting, DiagnosticSupport,
    InterpretationRequest, ProviderCapability, ProviderProfile, ProviderProfileBuilder,
    ProviderProtocol, RequestedSettings, StructuredOutputMode, TextBasis,
};
use crate::store::schema::{Clock, Database};
use crate::time::TimeContext;

const ROUTE: &str = "route-1";
const OPENAI: &str = "https://api.openai.com";
const ANTHROPIC: &str = "https://api.anthropic.com";
const SELF_HOSTED: &str = "https://self-hosted.example.test";
const STAMP: &str = "2026-01-15T10:30:00Z";
const SOURCE_TEXT: &str = "Call the dentist tomorrow morning";

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        stamp()
    }
}

fn stamp() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(STAMP)
        .unwrap()
        .with_timezone(&Utc)
}

fn time_context() -> TimeContext {
    TimeContext {
        timezone: "UTC".to_string(),
        locale: "en".to_string(),
        reference_time: stamp(),
        utc_offset_at_capture: 0,
        calendar: "gregorian".to_string(),
    }
}

fn supported_text(limit: usize) -> CapabilityMetadata {
    CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "synthetic fixture")
        .with_input_size_limit(limit)
}

fn hosted_profile(
    id: &str,
    protocol: ProviderProtocol,
    model: &str,
    origin: &str,
) -> ProviderProfile {
    ProviderProfileBuilder::new(id, protocol, model)
        .credential_ref("credential-ref")
        .authorized_destination(origin)
        .capability(supported_text(1000))
        .build()
        .unwrap()
}

fn openai_profile(model: &str) -> ProviderProfile {
    hosted_profile("openai-arm", ProviderProtocol::OpenAi, model, OPENAI)
}

fn anthropic_profile(model: &str) -> ProviderProfile {
    hosted_profile(
        "anthropic-arm",
        ProviderProtocol::Anthropic,
        model,
        ANTHROPIC,
    )
}

fn unverified_self_hosted_profile() -> ProviderProfile {
    ProviderProfileBuilder::new("local-arm", ProviderProtocol::SelfHosted, "local-model")
        .credential_ref("credential-ref")
        .endpoint(SELF_HOSTED)
        .authorized_destination(SELF_HOSTED)
        .capability(
            CapabilityMetadata::unverified(
                ProviderCapability::TextInterpretation,
                "not yet probed",
            )
            .with_input_size_limit(1000),
        )
        .build()
        .unwrap()
}

struct Fixture {
    db: Database,
    path: String,
    item_id: String,
    capture_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Fixture {
    fn new() -> Fixture {
        let path = format!(
            "{}/test_compare_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let db = Database::open(&path, Arc::new(FixedClock)).unwrap();
        let fixture = Fixture {
            db,
            path,
            item_id: Uuid::new_v4().to_string(),
            capture_id: Uuid::new_v4().to_string(),
        };
        fixture.exec(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
             VALUES (?, 'default', 'personal', ?, ?)",
            rusqlite::params![
                ROUTE,
                format!("[\"{OPENAI}\",\"{ANTHROPIC}\",\"{SELF_HOSTED}\"]"),
                STAMP
            ],
        );
        fixture.exec(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
             utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
             VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
            rusqlite::params![fixture.capture_id, SOURCE_TEXT, STAMP, ROUTE, STAMP],
        );
        fixture.exec(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
             'not_applicable', ?, ?)",
            rusqlite::params![fixture.item_id, fixture.capture_id, STAMP, STAMP],
        );
        fixture
    }

    fn exec(&self, sql: &str, params: impl rusqlite::Params) {
        self.db.conn().execute(sql, params).unwrap();
    }

    fn grant(&self, capability: &str, destinations: &[&str]) {
        let list: Vec<String> = destinations
            .iter()
            .map(|origin| format!("\"{origin}\""))
            .collect();
        self.exec(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, \
             authorized_destinations, created_at) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                ROUTE,
                capability,
                format!("[{}]", list.join(",")),
                STAMP
            ],
        );
    }

    fn store_profile(&self, profile: &ProviderProfile) {
        let provider_type = match profile.protocol() {
            ProviderProtocol::OpenAi => "open_ai",
            ProviderProtocol::Anthropic => "anthropic",
            ProviderProtocol::SelfHosted => "self_hosted",
        };
        self.exec(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) \
             VALUES (?, ?, ?, ?, 'model', 30, '{}', ?, '{}', ?)",
            rusqlite::params![
                profile.profile_version(),
                profile.profile_id(),
                provider_type,
                profile.endpoint(),
                serde_json::to_string(profile.authorized_destinations()).unwrap(),
                STAMP
            ],
        );
    }

    /// Stores `profile`, queues a job of `job_type` pinned to it and returns the authorization
    /// decision derived from durable state.
    fn authorize(&mut self, profile: &ProviderProfile, job_type: &str) -> AuthorizationDecision {
        self.store_profile(profile);
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            self.item_id.clone(),
            job_type.to_string(),
            0,
            Some(profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            stamp(),
        )
        .unwrap();
        authorize_job(self.db.conn(), &job_id).unwrap()
    }

    fn authorize_interpretation(&mut self, profile: &ProviderProfile) -> AuthorizationDecision {
        self.authorize(profile, JOB_TYPE_INTERPRET)
    }

    /// A fixture with grants for OpenAI and Anthropic interpretation.
    fn granted() -> Fixture {
        let fixture = Fixture::new();
        fixture.grant("text_interpretation", &[OPENAI, ANTHROPIC, SELF_HOSTED]);
        fixture
    }
}

fn full_settings() -> RequestedSettings {
    RequestedSettings::new()
        .with_temperature_milli(0)
        .unwrap()
        .with_max_output_tokens(512)
        .unwrap()
        .with_seed(7)
}

fn source_with(
    fixture: &Fixture,
    instructions: &str,
    settings: RequestedSettings,
) -> FrozenComparisonSource {
    FrozenComparisonSource::new(
        fixture.capture_id.clone(),
        0,
        TextBasis::Original { item_revision: 0 },
        SOURCE_TEXT,
        Uuid::new_v4().to_string(),
        time_context(),
        ROUTE,
        instructions,
        content_version(instructions),
        settings,
    )
    .unwrap()
}

fn source(fixture: &Fixture) -> FrozenComparisonSource {
    source_with(fixture, M1_INSTRUCTION_TEXT, full_settings())
}

fn support_all() -> DiagnosticSupport {
    DiagnosticSupport::supporting([
        DiagnosticSetting::Temperature,
        DiagnosticSetting::MaxOutputTokens,
        DiagnosticSetting::Seed,
    ])
}

fn arm<'a>(
    profile: &'a ProviderProfile,
    authorization: &'a AuthorizationDecision,
    adapter_support: &'a DiagnosticSupport,
) -> ArmInput<'a> {
    ArmInput {
        profile,
        authorization,
        adapter_support,
    }
}

fn arm_error(result: Result<PairedRequests, ComparisonError>) -> (ArmLabel, ArmError) {
    match result {
        Err(ComparisonError::Arm { arm, error }) => (arm, error),
        other => panic!("expected an arm failure, got {other:?}"),
    }
}

#[test]
fn derives_two_pinned_requests_with_equivalent_model_visible_content() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();

    let pair = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    )
    .unwrap();

    let (first, second) = (pair.first.request(), pair.second.request());
    assert!(first.request().is_pinned_to(&first_profile));
    assert!(second.request().is_pinned_to(&second_profile));
    assert!(!first.request().is_pinned_to(&second_profile));
    assert_eq!(check_comparable(first, second), Ok(()));
    assert_eq!(first.context(), second.context());
    assert_eq!(first.instructions(), second.instructions());
    assert_eq!(first.settings(), second.settings());
    assert_eq!(
        first.request().request_version(),
        second.request().request_version()
    );
    assert!(first.context().contains(SOURCE_TEXT));
    assert!(first.context().contains("\"source_revision\":0"));
    assert!(first.context().contains("\"timezone\":\"UTC\""));

    // Neither arm's model-visible content names any profile or the route.
    for visible in [first.context(), first.instructions()] {
        for forbidden in [
            first_profile.profile_id(),
            first_profile.profile_version(),
            second_profile.profile_id(),
            second_profile.profile_version(),
            ROUTE,
        ] {
            assert!(!visible.contains(forbidden), "leaked {forbidden}");
        }
    }
    assert_eq!(pair.first.authorization().route_id(), ROUTE);
    assert_eq!(
        pair.first.authorization().profile_version(),
        Some(first_profile.profile_version())
    );
    assert_eq!(
        pair.second.authorization().profile_version(),
        Some(second_profile.profile_version())
    );
}

#[test]
fn rejects_mismatched_revision_instructions_and_context() {
    let fixture = Fixture::new();
    let build = |basis: TextBasis, revision: u64, instructions: &str, version: String| {
        FrozenComparisonSource::new(
            fixture.capture_id.clone(),
            revision,
            basis,
            SOURCE_TEXT,
            Uuid::new_v4().to_string(),
            time_context(),
            ROUTE,
            instructions,
            version,
            RequestedSettings::new(),
        )
    };
    let version = content_version(M1_INSTRUCTION_TEXT);

    assert_eq!(
        build(
            TextBasis::Original { item_revision: 3 },
            4,
            M1_INSTRUCTION_TEXT,
            version.clone()
        )
        .unwrap_err(),
        ComparisonError::RevisionMismatch
    );
    assert_eq!(
        build(
            TextBasis::Original { item_revision: 1 },
            1,
            "Different instructions than the declared version.",
            version.clone()
        )
        .unwrap_err(),
        ComparisonError::InstructionVersionMismatch
    );
    assert_eq!(
        build(
            TextBasis::Correction {
                correction_record_id: "not-a-uuid".to_string(),
                item_revision: 1
            },
            1,
            M1_INSTRUCTION_TEXT,
            version.clone()
        )
        .unwrap_err(),
        ComparisonError::InvalidIdentifier("correction_record_id")
    );

    let mut bad_time = time_context();
    bad_time.timezone = "Not/AZone".to_string();
    let invalid_time = FrozenComparisonSource::new(
        fixture.capture_id.clone(),
        0,
        TextBasis::Original { item_revision: 0 },
        SOURCE_TEXT,
        Uuid::new_v4().to_string(),
        bad_time,
        ROUTE,
        M1_INSTRUCTION_TEXT,
        version,
        RequestedSettings::new(),
    );
    assert_eq!(
        invalid_time.unwrap_err(),
        ComparisonError::InvalidTimeContext
    );
}

fn diagnostic_request(
    capture_id: &str,
    revision: u64,
    text: &str,
    instructions: &str,
    context: &str,
    settings: RequestedSettings,
    profile: &ProviderProfile,
) -> DiagnosticRequest {
    let request = InterpretationRequest::new(
        capture_id,
        revision,
        TextBasis::Original {
            item_revision: revision,
        },
        text,
        "2f0fe6a4-6f67-4d36-8a35-0a0d5e2f3c11",
        content_version(instructions),
        profile,
        ROUTE,
        time_context(),
    )
    .unwrap();
    DiagnosticRequest::new(request, instructions, context, settings).unwrap()
}

#[test]
fn comparability_check_names_every_differing_model_visible_input() {
    let profile = openai_profile("model-a");
    let capture = Uuid::new_v4().to_string();
    let base = diagnostic_request(
        &capture,
        1,
        "same text",
        M1_INSTRUCTION_TEXT,
        "context",
        RequestedSettings::new(),
        &profile,
    );
    let same = diagnostic_request(
        &capture,
        1,
        "same text",
        M1_INSTRUCTION_TEXT,
        "context",
        RequestedSettings::new(),
        &anthropic_profile("model-b"),
    );
    assert_eq!(check_comparable(&base, &same), Ok(()));

    let other = diagnostic_request(
        &Uuid::new_v4().to_string(),
        2,
        "other text",
        "Other instructions",
        "other context",
        RequestedSettings::new().with_seed(1),
        &profile,
    );
    let mismatches = check_comparable(&base, &other).unwrap_err();
    for expected in [
        RequestMismatch::CaptureId,
        RequestMismatch::SourceRevision,
        RequestMismatch::TextBasis,
        RequestMismatch::Text,
        RequestMismatch::InstructionVersion,
        RequestMismatch::Instructions,
        RequestMismatch::Context,
        RequestMismatch::Settings,
    ] {
        assert!(mismatches.contains(&expected), "missing {expected:?}");
    }
    // The profile pin is not a comparability input.
    assert!(!mismatches.contains(&RequestMismatch::Route));
}

#[test]
fn rejects_unsupported_settings_and_missing_diagnostic_support() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let full = support_all();
    let no_seed = DiagnosticSupport::supporting([
        DiagnosticSetting::Temperature,
        DiagnosticSetting::MaxOutputTokens,
    ]);
    let source = source(&fixture);

    let result = derive_pair(
        &source,
        arm(&first_profile, &first_auth, &full),
        arm(&second_profile, &second_auth, &no_seed),
    );
    assert_eq!(
        arm_error(result),
        (
            ArmLabel::B,
            ArmError::UnsupportedSettings(vec![DiagnosticSetting::Seed])
        )
    );

    let unsupported = DiagnosticSupport::unsupported();
    let result = derive_pair(
        &source,
        arm(&first_profile, &first_auth, &unsupported),
        arm(&second_profile, &second_auth, &full),
    );
    assert_eq!(
        arm_error(result),
        (ArmLabel::A, ArmError::DiagnosticsUnsupported)
    );
}

#[test]
fn rejects_revoked_profile_even_with_a_grant() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    assert!(first_auth.authorization().is_some());
    revoke_profile(&mut fixture.db, first_profile.profile_version(), stamp()).unwrap();
    let revoked = authorize_job(
        fixture.db.conn(),
        first_auth.authorization().unwrap().job_id(),
    )
    .unwrap();
    assert_eq!(revoked.denial(), Some(DenialReason::ProfileRevoked));
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();

    let result = derive_pair(
        &source(&fixture),
        arm(&first_profile, &revoked, &support),
        arm(&second_profile, &second_auth, &support),
    );
    assert_eq!(arm_error(result), (ArmLabel::A, ArmError::ProfileRevoked));
}

#[test]
fn each_arm_needs_its_own_destination_grant() {
    let mut fixture = Fixture::new();
    fixture.grant("text_interpretation", &[OPENAI]);
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    assert!(first_auth.authorization().is_some());
    let support = support_all();

    let result = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    );
    assert_eq!(
        arm_error(result),
        (
            ArmLabel::B,
            ArmError::NotAuthorized(DenialReason::DestinationNotAuthorizedForCapability)
        )
    );
}

#[test]
fn review_grant_does_not_authorize_an_interpretation_arm() {
    let mut fixture = Fixture::granted();
    fixture.grant("review", &[ANTHROPIC]);
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let review_auth = fixture.authorize(&second_profile, JOB_TYPE_SHADOW_REVIEW);
    assert!(review_auth.authorization().is_some());
    let support = support_all();

    let result = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &review_auth, &support),
    );
    assert_eq!(
        arm_error(result),
        (
            ArmLabel::B,
            ArmError::WrongCapability(ProcessingCapability::Review)
        )
    );
}

#[test]
fn authorization_for_another_profile_or_route_is_rejected() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let second_profile = anthropic_profile("model-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();

    // The first arm's grant cannot be offered for the second arm.
    let result = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &first_auth, &support),
    );
    assert_eq!(
        arm_error(result),
        (ArmLabel::B, ArmError::AuthorizationProfileMismatch)
    );

    let other_route = FrozenComparisonSource::new(
        fixture.capture_id.clone(),
        0,
        TextBasis::Original { item_revision: 0 },
        SOURCE_TEXT,
        Uuid::new_v4().to_string(),
        time_context(),
        "another-route",
        M1_INSTRUCTION_TEXT,
        content_version(M1_INSTRUCTION_TEXT),
        RequestedSettings::new(),
    )
    .unwrap();
    let result = derive_pair(
        &other_route,
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    );
    assert_eq!(
        arm_error(result),
        (ArmLabel::A, ArmError::AuthorizationRouteMismatch)
    );
}

#[test]
fn rejects_unverified_profile_and_identical_profile_arms() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let unverified = unverified_self_hosted_profile();
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let unverified_auth = fixture.authorize_interpretation(&unverified);
    assert!(unverified_auth.authorization().is_some());
    let support = support_all();
    let source = source(&fixture);

    let result = derive_pair(
        &source,
        arm(&first_profile, &first_auth, &support),
        arm(&unverified, &unverified_auth, &support),
    );
    assert_eq!(
        arm_error(result),
        (ArmLabel::B, ArmError::ProfileUnverified)
    );

    let result = derive_pair(
        &source,
        arm(&first_profile, &first_auth, &support),
        arm(&first_profile, &first_auth, &support),
    );
    assert!(matches!(result, Err(ComparisonError::SameProfile)));
}

#[test]
fn rejects_source_larger_than_a_profile_input_limit() {
    let mut fixture = Fixture::granted();
    let small = ProviderProfileBuilder::new("small", ProviderProtocol::OpenAi, "model-a")
        .credential_ref("credential-ref")
        .authorized_destination(OPENAI)
        .capability(supported_text(SOURCE_TEXT.len() - 1))
        .build()
        .unwrap();
    let second_profile = anthropic_profile("model-b");
    let small_auth = fixture.authorize_interpretation(&small);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();

    let result = derive_pair(
        &source(&fixture),
        arm(&small, &small_auth, &support),
        arm(&second_profile, &second_auth, &support),
    );
    assert_eq!(arm_error(result), (ArmLabel::A, ArmError::InputTooLarge));
}

#[test]
fn reports_differences_and_unknown_defaults_without_claiming_accuracy() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("model-a");
    let second_profile =
        hosted_profile("second-openai", ProviderProtocol::OpenAi, "model-b", OPENAI);
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();

    // Same protocol and configuration, every setting requested: only the model differs.
    let isolated = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    )
    .unwrap()
    .comparability;
    assert_eq!(isolated.differences, vec![ConfigurationDifference::Model]);
    assert!(isolated.unknown_defaults.is_empty());
    assert!(isolated.no_known_confounders());
    assert_eq!(
        isolated.limitations,
        vec![ComparisonLimitation::AgreementIsNotAccuracy]
    );

    // Unrequested settings are unknown provider defaults, not equal values.
    let only_temperature = RequestedSettings::new().with_temperature_milli(0).unwrap();
    let defaults = derive_pair(
        &source_with(&fixture, M1_INSTRUCTION_TEXT, only_temperature),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    )
    .unwrap()
    .comparability;
    assert_eq!(
        defaults.unknown_defaults,
        vec![DiagnosticSetting::MaxOutputTokens, DiagnosticSetting::Seed]
    );
    assert!(!defaults.no_known_confounders());
    assert!(defaults
        .limitations
        .contains(&ComparisonLimitation::UnknownProviderDefaults));

    // Different protocol, timeout and structured-output mode are known confounders.
    let slow_json = ProviderProfileBuilder::new("slow", ProviderProtocol::Anthropic, "model-c")
        .credential_ref("credential-ref")
        .authorized_destination(ANTHROPIC)
        .timeout_seconds(90)
        .capability(supported_text(500).with_structured_output(StructuredOutputMode::JsonSchema))
        .build()
        .unwrap();
    let slow_auth = fixture.authorize_interpretation(&slow_json);
    let confounded = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&slow_json, &slow_auth, &support),
    )
    .unwrap()
    .comparability;
    assert!(!confounded.no_known_confounders());
    assert!(confounded
        .limitations
        .contains(&ComparisonLimitation::ConfigurationDiffers));
    assert_eq!(
        confounded.limitations[0],
        ComparisonLimitation::AgreementIsNotAccuracy
    );
    for expected in [
        ConfigurationDifference::Protocol {
            first: ProviderProtocol::OpenAi,
            second: ProviderProtocol::Anthropic,
        },
        ConfigurationDifference::TimeoutSeconds {
            first: 30,
            second: 90,
        },
        ConfigurationDifference::StructuredOutput {
            first: StructuredOutputMode::None,
            second: StructuredOutputMode::JsonSchema,
        },
        ConfigurationDifference::InputSizeLimit {
            first: Some(1000),
            second: Some(500),
        },
    ] {
        assert!(
            confounded.differences.contains(&expected),
            "missing {expected:?}"
        );
    }
}

#[test]
fn comparability_never_reproduces_configured_names() {
    let mut fixture = Fixture::granted();
    let first_profile = openai_profile("secret-model-name-a");
    let second_profile = anthropic_profile("secret-model-name-b");
    let first_auth = fixture.authorize_interpretation(&first_profile);
    let second_auth = fixture.authorize_interpretation(&second_profile);
    let support = support_all();
    let pair = derive_pair(
        &source(&fixture),
        arm(&first_profile, &first_auth, &support),
        arm(&second_profile, &second_auth, &support),
    )
    .unwrap();
    let report = format!("{:?}", pair.comparability);
    assert!(!report.contains("secret-model-name"));
    assert!(!report.contains(first_profile.profile_id()));
}
