//! Destination and capability authorization for processing jobs.
//!
//! Every job that could send payload off the device is authorized here before dispatch. The
//! decision is derived entirely from durable state: the job's stored profile pin, the capture's
//! immutable `route_id`, the stored `routes` ceiling and the per-capability `route_authorizations`
//! grants. Callers supply only a job id and the capability they intend to exercise; route,
//! destinations and profile are never accepted from the caller, and nothing a model, classifier or
//! capture text produced (item scope, item type, proposals, capture text) is ever read.
//!
//! A fresh install has no routes or grants, so every job pinned to a remote profile is denied.
//! Only a job with no profile pin (on-device processing) is authorized without a grant.

use crate::providers::contracts::ProviderProtocol;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::fmt;

/// What a job asks a destination to do. Each capability needs its own stored grant; `Review`
/// (an independent reviewer pass) is deliberately not implied by any interpretation grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessingCapability {
    TextInterpretation,
    Transcription,
    Embeddings,
    SpeechGeneration,
    Review,
}

impl ProcessingCapability {
    /// Stable key stored in `route_authorizations.capability`.
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessingCapability::TextInterpretation => "text_interpretation",
            ProcessingCapability::Transcription => "transcription",
            ProcessingCapability::Embeddings => "embeddings",
            ProcessingCapability::SpeechGeneration => "speech_generation",
            ProcessingCapability::Review => "review",
        }
    }
}

/// Where the payload would go, derived from the pinned profile's protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationClass {
    Cloud,
    PrivateServer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Disposition {
    Local,
    Remote {
        class: DestinationClass,
        destinations: Vec<String>,
    },
}

/// Proof that a job may be dispatched. It can only be produced by [`authorize_job`]; dispatch
/// code must send only to [`Authorization::destinations`] using [`Authorization::profile_version`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    job_id: String,
    capability: ProcessingCapability,
    route_id: String,
    profile_version: Option<String>,
    disposition: Disposition,
}

impl Authorization {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn capability(&self) -> ProcessingCapability {
        self.capability
    }

    /// The capture's stored route, for cross-checking against any route carried by a request.
    pub fn route_id(&self) -> &str {
        &self.route_id
    }

    /// The stored profile pin the job must be dispatched with; `None` for on-device processing.
    pub fn profile_version(&self) -> Option<&str> {
        self.profile_version.as_deref()
    }

    /// True when no payload leaves the device.
    pub fn is_local(&self) -> bool {
        matches!(self.disposition, Disposition::Local)
    }

    pub fn destination_class(&self) -> Option<DestinationClass> {
        match &self.disposition {
            Disposition::Local => None,
            Disposition::Remote { class, .. } => Some(*class),
        }
    }

    /// Bare https origins the payload may be sent to; empty for local processing.
    pub fn destinations(&self) -> &[String] {
        match &self.disposition {
            Disposition::Local => &[],
            Disposition::Remote { destinations, .. } => destinations,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialReason {
    JobNotFound,
    ProfileUnavailable,
    UnknownProviderType,
    MalformedProfile,
    RouteNotConfigured,
    MalformedPolicy,
    DestinationNotInRoute,
    CapabilityNotAuthorized,
    DestinationNotAuthorizedForCapability,
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            DenialReason::JobNotFound => "job, its item or its capture does not exist",
            DenialReason::ProfileUnavailable => "the job's pinned profile version is unavailable",
            DenialReason::UnknownProviderType => "the pinned profile has an unknown provider type",
            DenialReason::MalformedProfile => "the pinned profile has no valid destination",
            DenialReason::RouteNotConfigured => "the capture's route has no stored configuration",
            DenialReason::MalformedPolicy => "stored route policy is malformed",
            DenialReason::DestinationNotInRoute => "destination is not permitted by the route",
            DenialReason::CapabilityNotAuthorized => {
                "no stored authorization for this capability on the route"
            }
            DenialReason::DestinationNotAuthorizedForCapability => {
                "destination is not authorized for this capability"
            }
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Authorized(Authorization),
    Denied(DenialReason),
}

impl AuthorizationDecision {
    pub fn authorization(&self) -> Option<&Authorization> {
        match self {
            AuthorizationDecision::Authorized(authorization) => Some(authorization),
            AuthorizationDecision::Denied(_) => None,
        }
    }

    pub fn denial(&self) -> Option<DenialReason> {
        match self {
            AuthorizationDecision::Authorized(_) => None,
            AuthorizationDecision::Denied(reason) => Some(*reason),
        }
    }
}

/// Authorize `job_id` to exercise `capability`. Denials are returned as
/// [`AuthorizationDecision::Denied`]; `Err` means storage could not be read, which callers must
/// also treat as "do not dispatch".
pub fn authorize_job(
    conn: &Connection,
    job_id: &str,
    capability: ProcessingCapability,
) -> Result<AuthorizationDecision> {
    use AuthorizationDecision::Denied;

    let Some((route_id, profile_version)) = load_job_binding(conn, job_id)? else {
        return Ok(Denied(DenialReason::JobNotFound));
    };

    let make = |profile_version: Option<String>, disposition: Disposition| {
        AuthorizationDecision::Authorized(Authorization {
            job_id: job_id.to_string(),
            capability,
            route_id: route_id.clone(),
            profile_version,
            disposition,
        })
    };

    let Some(profile_version) = profile_version else {
        return Ok(make(None, Disposition::Local));
    };

    let Some(profile) = load_profile(conn, &profile_version)? else {
        return Ok(Denied(DenialReason::ProfileUnavailable));
    };
    let Some(protocol) = serde_json::from_value::<ProviderProtocol>(serde_json::Value::String(
        profile.provider_type.clone(),
    ))
    .ok() else {
        return Ok(Denied(DenialReason::UnknownProviderType));
    };
    let class = match protocol {
        ProviderProtocol::SelfHosted => DestinationClass::PrivateServer,
        ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => DestinationClass::Cloud,
    };
    let destinations = match contacted_destinations(protocol, &profile) {
        Some(destinations) if !destinations.is_empty() => destinations,
        _ => return Ok(Denied(DenialReason::MalformedProfile)),
    };

    let route_destinations: Option<String> = conn
        .query_row(
            "SELECT processing_destinations FROM routes WHERE route_id = ?",
            [route_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .context("reading route")?;
    let Some(route_destinations) = route_destinations else {
        return Ok(Denied(DenialReason::RouteNotConfigured));
    };
    let Some(route_destinations) = parse_origin_list(&route_destinations) else {
        return Ok(Denied(DenialReason::MalformedPolicy));
    };
    if !is_subset(&destinations, &route_destinations) {
        return Ok(Denied(DenialReason::DestinationNotInRoute));
    }

    let grant: Option<String> = conn
        .query_row(
            "SELECT authorized_destinations FROM route_authorizations \
             WHERE route_id = ? AND capability = ?",
            [route_id.as_str(), capability.as_str()],
            |row| row.get(0),
        )
        .optional()
        .context("reading route authorization")?;
    let Some(grant) = grant else {
        return Ok(Denied(DenialReason::CapabilityNotAuthorized));
    };
    let Some(granted_destinations) = parse_origin_list(&grant) else {
        return Ok(Denied(DenialReason::MalformedPolicy));
    };
    if !is_subset(&destinations, &granted_destinations) {
        return Ok(Denied(DenialReason::DestinationNotAuthorizedForCapability));
    }

    Ok(make(
        Some(profile_version),
        Disposition::Remote {
            class,
            destinations,
        },
    ))
}

/// The capture's immutable route and the job's profile pin, joined through the item.
fn load_job_binding(conn: &Connection, job_id: &str) -> Result<Option<(String, Option<String>)>> {
    conn.query_row(
        "SELECT captures.route_id, jobs.profile_version \
         FROM jobs \
         JOIN items ON items.item_id = jobs.item_id \
         JOIN captures ON captures.capture_id = items.capture_id \
         WHERE jobs.job_id = ?",
        [job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .context("reading job binding")
}

struct StoredProfile {
    provider_type: String,
    endpoint: Option<String>,
    authorized_destinations: String,
}

/// Origins a dispatch with this profile can contact. Hosted protocols contact their declared
/// vendor origins; self-hosted profiles contact only their endpoint origin.
fn contacted_destinations(
    protocol: ProviderProtocol,
    profile: &StoredProfile,
) -> Option<Vec<String>> {
    match protocol {
        ProviderProtocol::SelfHosted => {
            let authority = profile
                .endpoint
                .as_deref()?
                .strip_prefix("https://")?
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("");
            let origin = normalize_origin(&format!("https://{}", authority.to_ascii_lowercase()))?;
            Some(vec![origin])
        }
        ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => {
            parse_origin_list(&profile.authorized_destinations)
        }
    }
}

fn load_profile(conn: &Connection, profile_version: &str) -> Result<Option<StoredProfile>> {
    conn.query_row(
        "SELECT provider_type, endpoint, authorized_destinations \
         FROM provider_profiles WHERE profile_version = ?",
        [profile_version],
        |row| {
            Ok(StoredProfile {
                provider_type: row.get(0)?,
                endpoint: row.get(1)?,
                authorized_destinations: row.get(2)?,
            })
        },
    )
    .optional()
    .context("reading provider profile")
}

/// Returns the canonical bare `https://host[:port]` origin, or `None` for anything else
/// (wildcards, paths, credentials, uppercase, other schemes).
fn normalize_origin(candidate: &str) -> Option<String> {
    let authority = candidate.strip_prefix("https://")?;
    let is_valid = !authority.is_empty()
        && authority.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
        && !authority.starts_with(['.', '-', ':'])
        && !authority.contains("..");
    is_valid.then(|| candidate.to_string())
}

/// Parses a stored JSON array of bare origins; any invalid entry invalidates the whole list.
fn parse_origin_list(stored: &str) -> Option<Vec<String>> {
    let entries: Vec<String> = serde_json::from_str(stored).ok()?;
    entries
        .iter()
        .map(|entry| normalize_origin(entry))
        .collect()
}

fn is_subset(required: &[String], allowed: &[String]) -> bool {
    required
        .iter()
        .all(|destination| allowed.contains(destination))
}
