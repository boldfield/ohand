//! Stored provider profiles and the default profile per processing capability (V01b).
//!
//! This is the production write path for `provider_profiles`. Every create or edit mints a new
//! immutable row (same `profile_id`, new `profile_version`); an existing row is never updated
//! here, and revocation stays with `jobs::configuration`. Destinations are never accepted from
//! the caller: a hosted protocol contacts its vendor origin and a self-hosted profile contacts
//! the origin of its own endpoint, both derived here. Route grants for those destinations are a
//! separate, explicit act.
//!
//! A default is a pointer from a processing capability to one profile version. Changing it never
//! touches existing jobs, which keep the pin they were created with.
//!
//! Failures are [`ProfileFailure`] values with fixed codes that never echo caller input.

use crate::privacy::routing::ProcessingCapability;
use crate::providers::anthropic::ANTHROPIC_MESSAGES_ENDPOINT;
use crate::providers::contracts::CapabilityMetadata;
use crate::providers::contracts::{
    ProfileValidationError, ProviderCapability, ProviderProfile, ProviderProfileBuilder,
    ProviderProtocol, RetryPolicy, StructuredOutputMode,
};
use crate::providers::openai::OPENAI_CHAT_COMPLETIONS_ENDPOINT;
use crate::providers::self_hosted::declared_capabilities;
use crate::store::schema::Database;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Largest text, in bytes, a stored profile declares it accepts for interpretation.
pub const TEXT_INPUT_LIMIT_BYTES: usize = 262_144;
const MAX_PROFILE_ID_BYTES: usize = 128;
const MAX_MODEL_BYTES: usize = 256;
const MAX_CREDENTIAL_REF_BYTES: usize = 256;
const MAX_ENDPOINT_BYTES: usize = 2048;

/// Why a profile or default operation was refused. [`ProfileFailure::code`] is stable and fixed.
#[derive(Debug)]
pub enum ProfileFailure {
    UnknownProtocol,
    ProfileIdInvalid,
    EndpointRequired,
    EndpointInvalid,
    EndpointNotAllowed,
    ModelRequired,
    ModelInvalid,
    CapabilityUnsupported,
    CredentialRequired,
    CredentialInvalid,
    TimeoutOutOfBounds,
    RetryOutOfBounds,
    CredentialPendingRemoval,
    CapabilityNotConfigurable,
    ProfileNotFound,
    ProfileRevoked,
    Storage(anyhow::Error),
}

impl ProfileFailure {
    pub fn code(&self) -> &'static str {
        match self {
            ProfileFailure::UnknownProtocol => "unknown_protocol",
            ProfileFailure::ProfileIdInvalid => "profile_id_invalid",
            ProfileFailure::EndpointRequired => "endpoint_required",
            ProfileFailure::EndpointInvalid => "endpoint_invalid",
            ProfileFailure::EndpointNotAllowed => "endpoint_not_allowed",
            ProfileFailure::ModelRequired => "model_required",
            ProfileFailure::ModelInvalid => "model_invalid",
            ProfileFailure::CapabilityUnsupported => "capability_unsupported",
            ProfileFailure::CredentialRequired => "credential_required",
            ProfileFailure::CredentialInvalid => "credential_invalid",
            ProfileFailure::TimeoutOutOfBounds => "timeout_out_of_bounds",
            ProfileFailure::RetryOutOfBounds => "retry_out_of_bounds",
            ProfileFailure::CredentialPendingRemoval => "credential_pending_removal",
            ProfileFailure::CapabilityNotConfigurable => "capability_not_configurable",
            ProfileFailure::ProfileNotFound => "profile_not_found",
            ProfileFailure::ProfileRevoked => "profile_revoked",
            ProfileFailure::Storage(_) => "storage_error",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            ProfileFailure::UnknownProtocol => "the provider protocol is not recognized",
            ProfileFailure::ProfileIdInvalid => "the profile identifier is empty or malformed",
            ProfileFailure::EndpointRequired => "this protocol requires an endpoint",
            ProfileFailure::EndpointInvalid => "the endpoint is not an acceptable https URL",
            ProfileFailure::EndpointNotAllowed => "this protocol does not take an endpoint",
            ProfileFailure::ModelRequired => "the model is empty",
            ProfileFailure::ModelInvalid => "the model is malformed",
            ProfileFailure::CapabilityUnsupported => {
                "a requested capability is not supported by this protocol's adapter"
            }
            ProfileFailure::CredentialRequired => "the profile needs a credential reference",
            ProfileFailure::CredentialInvalid => "the credential reference is malformed",
            ProfileFailure::TimeoutOutOfBounds => "the timeout is outside the allowed bounds",
            ProfileFailure::RetryOutOfBounds => "the retry policy is outside the allowed bounds",
            ProfileFailure::CredentialPendingRemoval => {
                "the credential reference is pending removal by a reset"
            }
            ProfileFailure::CapabilityNotConfigurable => {
                "this capability has no configurable default profile"
            }
            ProfileFailure::ProfileNotFound => "no stored profile has that version",
            ProfileFailure::ProfileRevoked => "the profile version has been revoked",
            ProfileFailure::Storage(_) => "the core store failed",
        }
    }
}

impl From<anyhow::Error> for ProfileFailure {
    fn from(error: anyhow::Error) -> Self {
        ProfileFailure::Storage(error)
    }
}

impl From<rusqlite::Error> for ProfileFailure {
    fn from(error: rusqlite::Error) -> Self {
        ProfileFailure::Storage(anyhow::Error::new(error))
    }
}

/// Retry values as the caller states them. Signed so a negative number is an out-of-bounds value
/// rather than a malformed request.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryDraft {
    pub max_attempts: i64,
    pub initial_backoff_ms: i64,
    pub max_backoff_ms: i64,
}

/// What the caller states about a profile version. It carries no destinations, capability
/// metadata or evidence: the core derives those from the protocol.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDraft {
    /// Groups the versions of one profile. A new identifier starts a new profile; an existing one
    /// adds a version to it.
    pub profile_id: String,
    pub protocol: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    pub model: String,
    /// Opaque reference to a secret held by the native credential service. Never the secret.
    #[serde(default)]
    pub credential_ref: Option<String>,
    pub timeout_seconds: i64,
    /// `None` uses the contract's default policy.
    #[serde(default)]
    pub retry_policy: Option<RetryDraft>,
    /// Capability keys the profile should serve, for example `text_interpretation`.
    pub capabilities: Vec<String>,
}

/// The identity of a newly stored profile version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredProfileVersion {
    pub profile_id: String,
    pub profile_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilitySummary {
    pub capability: ProviderCapability,
    pub support: crate::providers::contracts::CapabilitySupport,
}

/// One stored version as listing reports it. It never carries the credential reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileVersionSummary {
    pub profile_version: String,
    pub protocol: String,
    pub endpoint: Option<String>,
    pub model: String,
    pub capabilities: Vec<CapabilitySummary>,
    pub credential_bound: bool,
    pub revoked: bool,
    /// Processing capabilities whose default is this version.
    pub default_for: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileSummary {
    pub profile_id: String,
    pub versions: Vec<ProfileVersionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultSummary {
    pub capability: String,
    pub profile_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileListing {
    pub profiles: Vec<ProfileSummary>,
    pub defaults: Vec<DefaultSummary>,
}

fn protocol_key(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAi => "open_ai",
        ProviderProtocol::Anthropic => "anthropic",
        ProviderProtocol::SelfHosted => "self_hosted",
    }
}

fn parse_protocol(text: &str) -> Option<ProviderProtocol> {
    serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
}

fn is_plain_text(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

fn origin_of_url(url: &str) -> Option<String> {
    let authority = url
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    Some(format!("https://{}", authority.to_ascii_lowercase()))
}

/// The origins a profile of this protocol contacts. Hosted protocols contact their vendor origin;
/// a self-hosted profile contacts only the origin of its own endpoint.
fn destination_for(protocol: ProviderProtocol, endpoint: Option<&str>) -> Option<String> {
    match protocol {
        ProviderProtocol::OpenAi => origin_of_url(OPENAI_CHAT_COMPLETIONS_ENDPOINT),
        ProviderProtocol::Anthropic => origin_of_url(ANTHROPIC_MESSAGES_ENDPOINT),
        ProviderProtocol::SelfHosted => origin_of_url(endpoint?),
    }
}

/// Capability metadata the protocol's adapter can honestly declare, or `None` when the caller
/// asked for a capability the adapter does not serve.
fn capability_metadata(
    protocol: ProviderProtocol,
    model: &str,
    requested: &[String],
) -> Option<Vec<CapabilityMetadata>> {
    let mut wanted = BTreeSet::new();
    for key in requested {
        let capability: ProviderCapability =
            serde_json::from_value(serde_json::Value::String(key.clone())).ok()?;
        wanted.insert(capability);
    }
    // Every adapter serves text interpretation and nothing else, and a profile must declare it.
    if wanted != BTreeSet::from([ProviderCapability::TextInterpretation]) {
        return None;
    }
    Some(match protocol {
        ProviderProtocol::OpenAi => vec![CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "protocol-fixtures:providers_openai",
        )
        .with_input_size_limit(TEXT_INPUT_LIMIT_BYTES)
        .with_structured_output(StructuredOutputMode::JsonObject)],
        ProviderProtocol::Anthropic => vec![CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "protocol-fixtures:providers_anthropic",
        )
        .with_input_size_limit(TEXT_INPUT_LIMIT_BYTES)
        .with_structured_output(StructuredOutputMode::JsonSchema)],
        ProviderProtocol::SelfHosted => declared_capabilities(model, TEXT_INPUT_LIMIT_BYTES),
    })
}

fn bounded_u32(value: i64) -> Option<u32> {
    u32::try_from(value).ok()
}

fn retry_policy_from(draft: &Option<RetryDraft>) -> Result<RetryPolicy, ProfileFailure> {
    let Some(retry) = draft else {
        return Ok(RetryPolicy::default());
    };
    let max_attempts = bounded_u32(retry.max_attempts).ok_or(ProfileFailure::RetryOutOfBounds)?;
    let initial =
        u64::try_from(retry.initial_backoff_ms).map_err(|_| ProfileFailure::RetryOutOfBounds)?;
    let maximum =
        u64::try_from(retry.max_backoff_ms).map_err(|_| ProfileFailure::RetryOutOfBounds)?;
    Ok(RetryPolicy {
        max_attempts,
        initial_backoff_ms: initial,
        max_backoff_ms: maximum,
    })
}

/// Validate `draft` against the protocol and the profile contract and build the profile, minting
/// a fresh `profile_version`.
fn build_profile(
    draft: &ProfileDraft,
    created_at: DateTime<Utc>,
) -> Result<ProviderProfile, ProfileFailure> {
    let protocol = parse_protocol(&draft.protocol).ok_or(ProfileFailure::UnknownProtocol)?;

    let profile_id = draft.profile_id.trim();
    if profile_id.is_empty()
        || profile_id.len() > MAX_PROFILE_ID_BYTES
        || !is_plain_text(profile_id)
    {
        return Err(ProfileFailure::ProfileIdInvalid);
    }

    match (protocol, draft.endpoint.as_deref()) {
        (ProviderProtocol::SelfHosted, None) => return Err(ProfileFailure::EndpointRequired),
        (ProviderProtocol::SelfHosted, Some(endpoint)) => {
            if endpoint.trim().is_empty() {
                return Err(ProfileFailure::EndpointRequired);
            }
            if endpoint.len() > MAX_ENDPOINT_BYTES || endpoint.contains(['?', '#']) {
                return Err(ProfileFailure::EndpointInvalid);
            }
        }
        (_, Some(_)) => return Err(ProfileFailure::EndpointNotAllowed),
        (_, None) => {}
    }

    let model = draft.model.trim();
    if model.is_empty() {
        return Err(ProfileFailure::ModelRequired);
    }
    if model.len() > MAX_MODEL_BYTES || !is_plain_text(model) {
        return Err(ProfileFailure::ModelInvalid);
    }

    let capabilities = capability_metadata(protocol, model, &draft.capabilities)
        .ok_or(ProfileFailure::CapabilityUnsupported)?;

    let credential = match draft.credential_ref.as_deref() {
        Some(reference) if !reference.trim().is_empty() => reference,
        _ => return Err(ProfileFailure::CredentialRequired),
    };
    if credential.len() > MAX_CREDENTIAL_REF_BYTES || !is_plain_text(credential) {
        return Err(ProfileFailure::CredentialInvalid);
    }

    let timeout_seconds =
        bounded_u32(draft.timeout_seconds).ok_or(ProfileFailure::TimeoutOutOfBounds)?;
    let retry_policy = retry_policy_from(&draft.retry_policy)?;

    let destination = destination_for(protocol, draft.endpoint.as_deref())
        .ok_or(ProfileFailure::EndpointInvalid)?;
    let mut builder = ProviderProfileBuilder::new(profile_id, protocol, model)
        .credential_ref(credential)
        .timeout_seconds(timeout_seconds)
        .retry_policy(retry_policy)
        .authorized_destination(destination)
        .created_at(created_at);
    if let Some(endpoint) = draft.endpoint.as_deref() {
        builder = builder.endpoint(endpoint);
    }
    for metadata in capabilities {
        builder = builder.capability(metadata);
    }
    builder.build().map_err(|error| match error {
        ProfileValidationError::InvalidTimeout => ProfileFailure::TimeoutOutOfBounds,
        ProfileValidationError::InvalidRetryPolicy => ProfileFailure::RetryOutOfBounds,
        ProfileValidationError::EmptyModel => ProfileFailure::ModelRequired,
        ProfileValidationError::MissingCredentialRef => ProfileFailure::CredentialRequired,
        ProfileValidationError::MissingEndpoint => ProfileFailure::EndpointRequired,
        ProfileValidationError::UnexpectedEndpoint => ProfileFailure::EndpointNotAllowed,
        ProfileValidationError::InvalidEndpoint
        | ProfileValidationError::EndpointNotAuthorized
        | ProfileValidationError::InvalidAuthorizedDestination
        | ProfileValidationError::NoAuthorizedDestinations => ProfileFailure::EndpointInvalid,
        ProfileValidationError::EmptyProfileId => ProfileFailure::ProfileIdInvalid,
        _ => ProfileFailure::CapabilityUnsupported,
    })
}

/// Store `draft` as a new immutable profile version in the caller's transaction. Earlier
/// versions of the same `profile_id` are left exactly as they were.
pub fn create_profile_version_in_tx(
    transaction: &Transaction<'_>,
    draft: &ProfileDraft,
    now: DateTime<Utc>,
) -> Result<StoredProfileVersion, ProfileFailure> {
    let profile = build_profile(draft, now)?;
    let record = serde_json::to_value(&profile).map_err(anyhow::Error::new)?;
    let text_of = |key: &str| record[key].to_string();
    let credential = profile.credential_ref().as_str();
    let inserted = transaction.execute(
        "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
         model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, \
         capabilities, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            profile.profile_version(),
            profile.profile_id(),
            protocol_key(profile.protocol()),
            profile.endpoint(),
            profile.model(),
            credential,
            profile.timeout_seconds(),
            text_of("retry_policy"),
            text_of("authorized_destinations"),
            text_of("capabilities"),
            record["created_at"].as_str().unwrap_or_default(),
        ],
    );
    match inserted {
        Ok(_) => Ok(StoredProfileVersion {
            profile_id: profile.profile_id().to_string(),
            profile_version: profile.profile_version().to_string(),
        }),
        Err(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            // The v10 trigger aborts with a constraint failure; the typed claim row says why.
            let claimed = transaction
                .query_row(
                    "SELECT 1 FROM reset_credential_claims WHERE credential_ref = ?",
                    [credential],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            Err(if claimed {
                ProfileFailure::CredentialPendingRemoval
            } else {
                ProfileFailure::Storage(anyhow::Error::new(rusqlite::Error::SqliteFailure(
                    failure, None,
                )))
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Store `draft` as a new immutable profile version in its own transaction.
pub fn create_profile_version(
    database: &mut Database,
    draft: &ProfileDraft,
    now: DateTime<Utc>,
) -> Result<StoredProfileVersion, ProfileFailure> {
    let transaction = database.immediate_transaction()?;
    let stored = create_profile_version_in_tx(&transaction, draft, now)?;
    transaction.commit().context("committing profile")?;
    Ok(stored)
}

/// Capabilities that can have a default profile. Transcription is on-device only.
pub fn parse_default_capability(key: &str) -> Result<ProcessingCapability, ProfileFailure> {
    match key {
        "text_interpretation" => Ok(ProcessingCapability::TextInterpretation),
        "review" => Ok(ProcessingCapability::Review),
        _ => Err(ProfileFailure::CapabilityNotConfigurable),
    }
}

/// Point `capability`'s default at `profile_version`. A missing or revoked version is refused.
pub fn set_default_profile(
    database: &mut Database,
    capability: ProcessingCapability,
    profile_version: &str,
    now: DateTime<Utc>,
) -> Result<(), ProfileFailure> {
    let transaction = database.immediate_transaction()?;
    let revoked_at: Option<Option<String>> = transaction
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| row.get(0),
        )
        .optional()?;
    match revoked_at {
        None => return Err(ProfileFailure::ProfileNotFound),
        Some(Some(_)) => return Err(ProfileFailure::ProfileRevoked),
        Some(None) => {}
    }
    transaction.execute(
        "INSERT INTO default_provider_profiles (capability, profile_version, updated_at) \
         VALUES (?, ?, ?) ON CONFLICT(capability) DO UPDATE SET \
         profile_version = excluded.profile_version, updated_at = excluded.updated_at",
        rusqlite::params![capability.as_str(), profile_version, now.to_rfc3339()],
    )?;
    transaction.commit().context("committing default")?;
    Ok(())
}

/// Remove `capability`'s default. Allowed when none is set.
pub fn clear_default_profile(
    database: &mut Database,
    capability: ProcessingCapability,
) -> Result<(), ProfileFailure> {
    database.conn().execute(
        "DELETE FROM default_provider_profiles WHERE capability = ?",
        [capability.as_str()],
    )?;
    Ok(())
}

/// The profile version currently set as `capability`'s default, whether or not it is still usable.
pub fn default_profile_version(
    conn: &Connection,
    capability: ProcessingCapability,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT profile_version FROM default_provider_profiles WHERE capability = ?",
        [capability.as_str()],
        |row| row.get(0),
    )
    .optional()
    .context("reading default profile")
}

type ListedRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
);

/// Every stored profile with its versions in creation order, plus the defaults. Credential
/// references are reported only as bound or not.
pub fn list_profiles(conn: &Connection) -> Result<ProfileListing> {
    let mut defaults_statement = conn
        .prepare(
            "SELECT capability, profile_version FROM default_provider_profiles ORDER BY capability",
        )
        .context("preparing defaults")?;
    let defaults = defaults_statement
        .query_map([], |row| {
            Ok(DefaultSummary {
                capability: row.get(0)?,
                profile_version: row.get(1)?,
            })
        })
        .context("reading defaults")?
        .collect::<Result<Vec<_>, _>>()
        .context("collecting defaults")?;

    let mut statement = conn
        .prepare(
            "SELECT profile_id, profile_version, provider_type, endpoint, model, credential_ref, \
             capabilities, revoked_at, created_at FROM provider_profiles ORDER BY rowid",
        )
        .context("preparing profiles")?;
    let rows: Vec<ListedRow> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        })
        .context("reading profiles")?
        .collect::<Result<_, _>>()
        .context("collecting profiles")?;

    let mut profiles: Vec<ProfileSummary> = Vec::new();
    for (
        profile_id,
        profile_version,
        protocol,
        endpoint,
        model,
        credential_ref,
        capabilities,
        revoked_at,
        created_at,
    ) in rows
    {
        let default_for = defaults
            .iter()
            .filter(|default| default.profile_version == profile_version)
            .map(|default| default.capability.clone())
            .collect();
        let version = ProfileVersionSummary {
            profile_version,
            protocol,
            endpoint,
            model,
            capabilities: summarize_capabilities(&capabilities),
            credential_bound: credential_ref.is_some_and(|reference| !reference.is_empty()),
            revoked: revoked_at.is_some(),
            default_for,
            created_at,
        };
        match profiles.iter_mut().find(|p| p.profile_id == profile_id) {
            Some(profile) => profile.versions.push(version),
            None => profiles.push(ProfileSummary {
                profile_id,
                versions: vec![version],
            }),
        }
    }
    Ok(ProfileListing { profiles, defaults })
}

fn summarize_capabilities(stored: &str) -> Vec<CapabilitySummary> {
    let Ok(map) = serde_json::from_str::<
        std::collections::BTreeMap<ProviderCapability, CapabilityMetadata>,
    >(stored) else {
        return Vec::new();
    };
    map.into_iter()
        .map(|(capability, metadata)| CapabilitySummary {
            capability,
            support: metadata.support,
        })
        .collect()
}
