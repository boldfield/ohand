use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

/// Profile schema version understood by this build. Unknown versions are rejected.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

const MAX_TIMEOUT_SECONDS: u32 = 300;
const MAX_RETRY_ATTEMPTS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAi,
    Anthropic,
    SelfHosted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    TextInterpretation,
    Transcription,
    Embeddings,
    SpeechGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    None,
    JsonObject,
    JsonSchema,
}

/// Opaque reference to a secret held by the native credential service. Never the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new(reference: impl Into<String>) -> CredentialRef {
        CredentialRef(reference.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityMetadata {
    pub capability: ProviderCapability,
    pub support: CapabilitySupport,
    /// Adapter test or probe reference that established `Supported`.
    pub evidence: Option<String>,
    /// Why the capability is unavailable; shown to the user for unsupported/unverified states.
    pub reason: Option<String>,
    pub input_size_limit: Option<usize>,
    pub structured_output: StructuredOutputMode,
}

impl CapabilityMetadata {
    pub fn supported(capability: ProviderCapability, evidence: impl Into<String>) -> Self {
        CapabilityMetadata {
            capability,
            support: CapabilitySupport::Supported,
            evidence: Some(evidence.into()),
            reason: None,
            input_size_limit: None,
            structured_output: StructuredOutputMode::None,
        }
    }

    pub fn unsupported(capability: ProviderCapability, reason: impl Into<String>) -> Self {
        CapabilityMetadata {
            capability,
            support: CapabilitySupport::Unsupported,
            evidence: None,
            reason: Some(reason.into()),
            input_size_limit: None,
            structured_output: StructuredOutputMode::None,
        }
    }

    pub fn unverified(capability: ProviderCapability, reason: impl Into<String>) -> Self {
        CapabilityMetadata {
            support: CapabilitySupport::Unverified,
            ..Self::unsupported(capability, reason)
        }
    }

    pub fn with_input_size_limit(mut self, limit: usize) -> Self {
        self.input_size_limit = Some(limit);
        self
    }

    pub fn with_structured_output(mut self, mode: StructuredOutputMode) -> Self {
        self.structured_output = mode;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileValidationError {
    #[error("unsupported profile schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("profile_version must be a UUID")]
    InvalidProfileVersion,
    #[error("profile_id must not be empty")]
    EmptyProfileId,
    #[error("model must not be empty")]
    EmptyModel,
    #[error("timeout_seconds must be between 1 and {MAX_TIMEOUT_SECONDS}")]
    InvalidTimeout,
    #[error("retry policy is inconsistent")]
    InvalidRetryPolicy,
    #[error("hosted protocols must not declare an endpoint")]
    UnexpectedEndpoint,
    #[error("self-hosted profiles require an endpoint")]
    MissingEndpoint,
    #[error("endpoint must be an https URL without credentials")]
    InvalidEndpoint,
    #[error("endpoint origin is not among the authorized destinations")]
    EndpointNotAuthorized,
    #[error("credential reference is missing or empty")]
    MissingCredentialRef,
    #[error("at least one authorized destination is required")]
    NoAuthorizedDestinations,
    #[error("authorized destination must be a bare https origin")]
    InvalidAuthorizedDestination,
    #[error("capability key does not match its metadata")]
    CapabilityKeyMismatch,
    #[error("text_interpretation capability must be declared")]
    MissingTextInterpretation,
    #[error("text_interpretation is unsupported by this profile")]
    TextInterpretationUnsupported,
    #[error("supported capability requires evidence")]
    SupportedWithoutEvidence,
    #[error("unsupported or unverified capability requires a reason")]
    UnavailableWithoutReason,
    #[error("only self-hosted profiles may be unverified")]
    UnverifiedHostedCapability,
    #[error("text_interpretation requires a positive input size limit")]
    MissingInputSizeLimit,
    #[error("capability {0:?} is not usable in M1")]
    CapabilityNotUsableInM1(ProviderCapability),
}

/// Unvalidated profile contents; the only way to obtain a [`ProviderProfile`] is to validate one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub schema_version: u32,
    pub profile_version: String,
    pub profile_id: String,
    pub protocol: ProviderProtocol,
    pub endpoint: Option<String>,
    pub model: String,
    pub credential_ref: Option<CredentialRef>,
    pub timeout_seconds: u32,
    pub retry_policy: RetryPolicy,
    pub authorized_destinations: Vec<String>,
    pub capabilities: BTreeMap<ProviderCapability, CapabilityMetadata>,
    pub created_at: DateTime<Utc>,
}

/// Immutable, validated provider profile. Any change goes through a builder and mints a new
/// `profile_version`, so queued work pinned to a version can never be silently rerouted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ProfileRecord", into = "ProfileRecord")]
pub struct ProviderProfile {
    record: ProfileRecord,
}

impl From<ProviderProfile> for ProfileRecord {
    fn from(profile: ProviderProfile) -> ProfileRecord {
        profile.record
    }
}

impl TryFrom<ProfileRecord> for ProviderProfile {
    type Error = ProfileValidationError;

    fn try_from(record: ProfileRecord) -> Result<Self, Self::Error> {
        validate_record(&record)?;
        Ok(ProviderProfile { record })
    }
}

impl ProviderProfile {
    pub fn schema_version(&self) -> u32 {
        self.record.schema_version
    }
    pub fn profile_version(&self) -> &str {
        &self.record.profile_version
    }
    pub fn profile_id(&self) -> &str {
        &self.record.profile_id
    }
    pub fn protocol(&self) -> ProviderProtocol {
        self.record.protocol
    }
    pub fn endpoint(&self) -> Option<&str> {
        self.record.endpoint.as_deref()
    }
    pub fn model(&self) -> &str {
        &self.record.model
    }
    pub fn credential_ref(&self) -> &CredentialRef {
        self.record
            .credential_ref
            .as_ref()
            .expect("validated profile has a credential reference")
    }
    pub fn timeout_seconds(&self) -> u32 {
        self.record.timeout_seconds
    }
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.record.retry_policy
    }
    pub fn authorized_destinations(&self) -> &[String] {
        &self.record.authorized_destinations
    }
    pub fn capabilities(&self) -> &BTreeMap<ProviderCapability, CapabilityMetadata> {
        &self.record.capabilities
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.record.created_at
    }

    pub fn capability(&self, capability: ProviderCapability) -> Option<&CapabilityMetadata> {
        self.record.capabilities.get(&capability)
    }

    /// A builder prefilled with this profile's contents; `build()` mints a new profile version.
    pub fn to_builder(&self) -> ProviderProfileBuilder {
        ProviderProfileBuilder {
            record: self.record.clone(),
        }
    }
}

pub struct ProviderProfileBuilder {
    record: ProfileRecord,
}

impl ProviderProfileBuilder {
    pub fn new(
        profile_id: impl Into<String>,
        protocol: ProviderProtocol,
        model: impl Into<String>,
    ) -> Self {
        ProviderProfileBuilder {
            record: ProfileRecord {
                schema_version: PROFILE_SCHEMA_VERSION,
                profile_version: String::new(),
                profile_id: profile_id.into(),
                protocol,
                endpoint: None,
                model: model.into(),
                credential_ref: None,
                timeout_seconds: 30,
                retry_policy: RetryPolicy::default(),
                authorized_destinations: Vec::new(),
                capabilities: BTreeMap::new(),
                created_at: Utc::now(),
            },
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.record.model = model.into();
        self
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.record.endpoint = Some(endpoint.into());
        self
    }

    pub fn credential_ref(mut self, reference: impl Into<String>) -> Self {
        self.record.credential_ref = Some(CredentialRef::new(reference));
        self
    }

    pub fn timeout_seconds(mut self, seconds: u32) -> Self {
        self.record.timeout_seconds = seconds;
        self
    }

    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.record.retry_policy = policy;
        self
    }

    pub fn authorized_destination(mut self, origin: impl Into<String>) -> Self {
        self.record.authorized_destinations.push(origin.into());
        self
    }

    pub fn clear_authorized_destinations(mut self) -> Self {
        self.record.authorized_destinations.clear();
        self
    }

    pub fn capability(mut self, metadata: CapabilityMetadata) -> Self {
        self.record
            .capabilities
            .insert(metadata.capability, metadata);
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.record.created_at = created_at;
        self
    }

    /// Mint a fresh immutable profile version and validate the result.
    pub fn build(mut self) -> Result<ProviderProfile, ProfileValidationError> {
        self.record.schema_version = PROFILE_SCHEMA_VERSION;
        self.record.profile_version = Uuid::new_v4().to_string();
        ProviderProfile::try_from(self.record)
    }
}

/// Returns the origin (`https://host[:port]`) of an https URL, or `None` if malformed.
fn https_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    if rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(format!("https://{}", authority.to_ascii_lowercase()))
}

fn validate_record(record: &ProfileRecord) -> Result<(), ProfileValidationError> {
    use ProfileValidationError as E;

    if record.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(E::UnsupportedSchemaVersion(record.schema_version));
    }
    if Uuid::parse_str(&record.profile_version).is_err() {
        return Err(E::InvalidProfileVersion);
    }
    if record.profile_id.trim().is_empty() {
        return Err(E::EmptyProfileId);
    }
    if record.model.trim().is_empty() {
        return Err(E::EmptyModel);
    }
    if record.timeout_seconds == 0 || record.timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(E::InvalidTimeout);
    }
    let retry = &record.retry_policy;
    if retry.max_attempts == 0
        || retry.max_attempts > MAX_RETRY_ATTEMPTS
        || retry.initial_backoff_ms == 0
        || retry.initial_backoff_ms > retry.max_backoff_ms
    {
        return Err(E::InvalidRetryPolicy);
    }

    match &record.credential_ref {
        Some(reference) if !reference.as_str().trim().is_empty() => {}
        _ => return Err(E::MissingCredentialRef),
    }

    if record.authorized_destinations.is_empty() {
        return Err(E::NoAuthorizedDestinations);
    }
    let mut destination_origins = Vec::new();
    for destination in &record.authorized_destinations {
        match https_origin(destination) {
            Some(origin) if origin.eq_ignore_ascii_case(destination) => {
                destination_origins.push(origin)
            }
            _ => return Err(E::InvalidAuthorizedDestination),
        }
    }

    match (record.protocol, &record.endpoint) {
        (ProviderProtocol::SelfHosted, None) => return Err(E::MissingEndpoint),
        (ProviderProtocol::SelfHosted, Some(endpoint)) => {
            let origin = https_origin(endpoint).ok_or(E::InvalidEndpoint)?;
            if !destination_origins.contains(&origin) {
                return Err(E::EndpointNotAuthorized);
            }
        }
        (_, Some(_)) => return Err(E::UnexpectedEndpoint),
        (_, None) => {}
    }

    for (key, metadata) in &record.capabilities {
        if *key != metadata.capability {
            return Err(E::CapabilityKeyMismatch);
        }
        let has_text =
            |value: &Option<String>| value.as_deref().is_some_and(|v| !v.trim().is_empty());
        match metadata.support {
            CapabilitySupport::Supported => {
                if !has_text(&metadata.evidence) {
                    return Err(E::SupportedWithoutEvidence);
                }
                if *key != ProviderCapability::TextInterpretation {
                    return Err(E::CapabilityNotUsableInM1(*key));
                }
            }
            CapabilitySupport::Unsupported | CapabilitySupport::Unverified => {
                if !has_text(&metadata.reason) {
                    return Err(E::UnavailableWithoutReason);
                }
                if metadata.support == CapabilitySupport::Unverified
                    && record.protocol != ProviderProtocol::SelfHosted
                {
                    return Err(E::UnverifiedHostedCapability);
                }
            }
        }
    }

    let text = record
        .capabilities
        .get(&ProviderCapability::TextInterpretation)
        .ok_or(E::MissingTextInterpretation)?;
    if text.support == CapabilitySupport::Unsupported {
        return Err(E::TextInterpretationUnsupported);
    }
    if text.input_size_limit.is_none_or(|limit| limit == 0) {
        return Err(E::MissingInputSizeLimit);
    }
    Ok(())
}
