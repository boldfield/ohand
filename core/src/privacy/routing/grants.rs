//! Explicit, per-capability route processing grants.
//!
//! This is the only production write path for route processing policy. A grant names a route, a
//! capability and a stored profile version; the destinations come from that profile
//! ([`super::contacted_destinations`]) and the caller can never supply them. Granting widens the
//! route's `processing_destinations` ceiling and the capability's `route_authorizations` row in
//! one transaction. Revoking removes the capability's row and drops every destination from the
//! ceiling that no remaining grant on the route uses, so queued jobs fail
//! [`super::authorize_job`] with its existing denial and never fall back to another destination.
//!
//! Nothing here reads a classifier, a model output, capture text or any other inferred signal.
//! Failures are fixed codes that never echo the request.

use super::{
    contacted_destinations, grant_covers, load_profile, parse_origin_list, ProcessingCapability,
    StoredProfile,
};
use crate::providers::contracts::ProviderProtocol;
use crate::store::schema::Database;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Why a grant was refused. Each variant has a fixed [`GrantError::code`]; `Storage` is the only
/// one that carries a cause, which callers must not forward to a user.
#[derive(Debug)]
pub enum GrantError {
    /// The profile version does not exist.
    ProfileUnavailable,
    /// The profile version has been revoked.
    ProfileRevoked,
    /// The profile has an unknown provider type or no valid destination.
    ProfileMalformed,
    /// The route does not exist.
    UnknownRoute,
    /// The capability runs on device only (transcription) and can never be granted a destination.
    CapabilityNotGrantable,
    /// The route's stored policy cannot be read, so it is not widened.
    PolicyMalformed,
    Storage(anyhow::Error),
}

impl GrantError {
    /// Stable machine code.
    pub fn code(&self) -> &'static str {
        match self {
            GrantError::ProfileUnavailable => "profile_unavailable",
            GrantError::ProfileRevoked => "profile_revoked",
            GrantError::ProfileMalformed => "profile_malformed",
            GrantError::UnknownRoute => "route_unknown",
            GrantError::CapabilityNotGrantable => "capability_not_grantable",
            GrantError::PolicyMalformed => "route_policy_malformed",
            GrantError::Storage(_) => "storage_error",
        }
    }
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GrantError::ProfileUnavailable => "the profile version does not exist",
            GrantError::ProfileRevoked => "the profile version has been revoked",
            GrantError::ProfileMalformed => "the profile has no valid destination",
            GrantError::UnknownRoute => "the route does not exist",
            GrantError::CapabilityNotGrantable => "the capability runs on device only",
            GrantError::PolicyMalformed => "stored route policy is malformed",
            GrantError::Storage(_) => "the core store failed",
        })
    }
}

impl std::error::Error for GrantError {}

impl From<anyhow::Error> for GrantError {
    fn from(error: anyhow::Error) -> Self {
        GrantError::Storage(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantOutcome {
    /// Stored policy changed.
    Granted,
    /// The route and capability already allowed every destination; nothing was written.
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The capability's grant was removed.
    Revoked,
    /// The capability had no grant on the route; nothing was written.
    Unchanged,
}

/// Whether the route's destinations for a granted capability are known to be cloud or private
/// servers. `Unknown` marks a destination that no stored profile version contacts, or a stored
/// grant that cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListedDestinationClass {
    Cloud,
    PrivateServer,
    Unknown,
}

impl ListedDestinationClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ListedDestinationClass::Cloud => "cloud",
            ListedDestinationClass::PrivateServer => "private_server",
            ListedDestinationClass::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedCapability {
    pub capability: ProcessingCapability,
    pub destination_classes: Vec<ListedDestinationClass>,
    /// Unrevoked profile versions whose every origin is within this grant, ordered by version identifier. Empty when
    /// the stored grant cannot be read.
    pub covered_profile_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePolicy {
    pub route_id: String,
    pub route_name: String,
    pub scope: String,
    pub granted_capabilities: Vec<GrantedCapability>,
}

/// Capability for a stored `route_authorizations.capability` key.
pub fn capability_from_key(key: &str) -> Option<ProcessingCapability> {
    [
        ProcessingCapability::TextInterpretation,
        ProcessingCapability::Transcription,
        ProcessingCapability::Review,
    ]
    .into_iter()
    .find(|capability| capability.as_str() == key)
}

/// Allows `capability` on `route_id` to send to the destinations of `profile_version`. Adds them
/// to the route ceiling and to the capability's grant in one immediate transaction; repeating the
/// grant writes nothing. `created_at` stamps a new grant row.
pub fn grant_route_capability(
    database: &mut Database,
    route_id: &str,
    capability: ProcessingCapability,
    profile_version: &str,
    created_at: &str,
) -> Result<GrantOutcome, GrantError> {
    if capability == ProcessingCapability::Transcription {
        return Err(GrantError::CapabilityNotGrantable);
    }
    let transaction = database.immediate_transaction()?;

    let revoked_at: Option<Option<String>> = transaction
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [profile_version],
            |row| row.get(0),
        )
        .optional()
        .context("reading profile revocation")?;
    match revoked_at {
        None => return Err(GrantError::ProfileUnavailable),
        Some(Some(_)) => return Err(GrantError::ProfileRevoked),
        Some(None) => {}
    }
    let destinations =
        profile_destinations(&transaction, profile_version)?.ok_or(GrantError::ProfileMalformed)?;

    let stored_ceiling: Option<String> = transaction
        .query_row(
            "SELECT processing_destinations FROM routes WHERE route_id = ?",
            [route_id],
            |row| row.get(0),
        )
        .optional()
        .context("reading route")?;
    let stored_ceiling = stored_ceiling.ok_or(GrantError::UnknownRoute)?;
    let ceiling = parse_origin_list(&stored_ceiling).ok_or(GrantError::PolicyMalformed)?;

    let stored_grant: Option<String> = transaction
        .query_row(
            "SELECT authorized_destinations FROM route_authorizations \
             WHERE route_id = ? AND capability = ?",
            [route_id, capability.as_str()],
            |row| row.get(0),
        )
        .optional()
        .context("reading route authorization")?;
    let grant = match &stored_grant {
        Some(stored) => parse_origin_list(stored).ok_or(GrantError::PolicyMalformed)?,
        None => Vec::new(),
    };

    let widened_ceiling = union_preserving_order(&ceiling, &destinations);
    let widened_grant = union_preserving_order(&grant, &destinations);
    let ceiling_changed = widened_ceiling.len() != ceiling.len();
    let grant_changed = stored_grant.is_none() || widened_grant.len() != grant.len();
    if !ceiling_changed && !grant_changed {
        return Ok(GrantOutcome::Unchanged);
    }

    if ceiling_changed {
        transaction
            .execute(
                "UPDATE routes SET processing_destinations = ? WHERE route_id = ?",
                rusqlite::params![encode_origin_list(&widened_ceiling)?, route_id],
            )
            .context("widening route ceiling")?;
    }
    if stored_grant.is_none() {
        transaction
            .execute(
                "INSERT INTO route_authorizations \
                 (auth_id, route_id, capability, authorized_destinations, created_at) \
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    route_id,
                    capability.as_str(),
                    encode_origin_list(&widened_grant)?,
                    created_at
                ],
            )
            .context("recording route authorization")?;
    } else if grant_changed {
        transaction
            .execute(
                "UPDATE route_authorizations SET authorized_destinations = ? \
                 WHERE route_id = ? AND capability = ?",
                rusqlite::params![
                    encode_origin_list(&widened_grant)?,
                    route_id,
                    capability.as_str()
                ],
            )
            .context("widening route authorization")?;
    }
    transaction.commit().context("committing route grant")?;
    Ok(GrantOutcome::Granted)
}

/// Removes `capability`'s grant on `route_id`, then narrows the route ceiling to the destinations
/// that the remaining grants on the route use; every other ceiling entry leaves. An unreadable
/// ceiling is not rewritten (jobs on it are already denied as malformed policy), but the grant
/// is still removed.
pub fn revoke_route_capability(
    database: &mut Database,
    route_id: &str,
    capability: ProcessingCapability,
) -> Result<RevokeOutcome, GrantError> {
    let transaction = database.immediate_transaction()?;

    let stored_ceiling: Option<String> = transaction
        .query_row(
            "SELECT processing_destinations FROM routes WHERE route_id = ?",
            [route_id],
            |row| row.get(0),
        )
        .optional()
        .context("reading route")?;
    let stored_ceiling = stored_ceiling.ok_or(GrantError::UnknownRoute)?;

    let grant_exists: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM route_authorizations WHERE route_id = ? AND capability = ?",
            [route_id, capability.as_str()],
            |row| row.get(0),
        )
        .optional()
        .context("reading route authorization")?;
    if grant_exists.is_none() {
        return Ok(RevokeOutcome::Unchanged);
    }

    transaction
        .execute(
            "DELETE FROM route_authorizations WHERE route_id = ? AND capability = ?",
            [route_id, capability.as_str()],
        )
        .context("removing route authorization")?;

    if let Some(ceiling) = parse_origin_list(&stored_ceiling) {
        let still_used = destinations_of_remaining_grants(&transaction, route_id)?;
        let pruned: Vec<String> = ceiling
            .iter()
            .filter(|origin| still_used.contains(*origin))
            .cloned()
            .collect();
        if pruned.len() != ceiling.len() {
            transaction
                .execute(
                    "UPDATE routes SET processing_destinations = ? WHERE route_id = ?",
                    rusqlite::params![encode_origin_list(&pruned)?, route_id],
                )
                .context("narrowing route ceiling")?;
        }
    }
    transaction.commit().context("committing route revoke")?;
    Ok(RevokeOutcome::Revoked)
}

/// Every route with its name, scope and granted capabilities (each with the classes of the
/// destinations it may use), ordered by route id then capability key. Read only.
pub fn list_route_policy(conn: &Connection) -> Result<Vec<RoutePolicy>> {
    let classes_by_origin = destination_classes_by_origin(conn)?;
    let live_versions = live_version_destinations(conn)?;

    let mut grants_by_route: BTreeMap<String, Vec<(String, GrantedCapability)>> = BTreeMap::new();
    let mut statement = conn
        .prepare(
            "SELECT route_id, capability, authorized_destinations FROM route_authorizations \
             ORDER BY route_id, capability",
        )
        .context("preparing grant listing")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("listing grants")?;
    for row in rows {
        let (route_id, key, stored_destinations) = row.context("reading grant")?;
        let Some(capability) = capability_from_key(&key) else {
            continue;
        };
        let granted_origins = parse_origin_list(&stored_destinations);
        let covered_profile_versions: Vec<String> = match &granted_origins {
            Some(granted) => live_versions
                .iter()
                .filter(|(_, destinations)| grant_covers(destinations, granted))
                .map(|(version, _)| version.clone())
                .collect(),
            None => Vec::new(),
        };
        let classes: BTreeSet<ListedDestinationClass> = match granted_origins {
            Some(origins) if !origins.is_empty() => origins
                .iter()
                .flat_map(|origin| match classes_by_origin.get(origin) {
                    Some(classes) => classes.iter().copied().collect::<Vec<_>>(),
                    None => vec![ListedDestinationClass::Unknown],
                })
                .collect(),
            _ => BTreeSet::from([ListedDestinationClass::Unknown]),
        };
        grants_by_route.entry(route_id).or_default().push((
            key,
            GrantedCapability {
                capability,
                destination_classes: classes.into_iter().collect(),
                covered_profile_versions,
            },
        ));
    }

    let mut statement = conn
        .prepare("SELECT route_id, route_name, scope FROM routes ORDER BY route_id")
        .context("preparing route listing")?;
    let routes = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("listing routes")?;
    let mut listing = Vec::new();
    for route in routes {
        let (route_id, route_name, scope) = route.context("reading route")?;
        let granted_capabilities = grants_by_route
            .remove(&route_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, granted)| granted)
            .collect();
        listing.push(RoutePolicy {
            route_id,
            route_name,
            scope,
            granted_capabilities,
        });
    }
    Ok(listing)
}

fn protocol_of(provider_type: &str) -> Option<ProviderProtocol> {
    serde_json::from_value::<ProviderProtocol>(serde_json::Value::String(provider_type.to_string()))
        .ok()
}

/// The origins a dispatch with this profile version contacts; `None` when the profile's provider
/// type is unknown or it contacts no valid origin.
fn profile_destinations(conn: &Connection, profile_version: &str) -> Result<Option<Vec<String>>> {
    let profile = load_profile(conn, profile_version)?;
    let Some(profile) = profile else {
        return Ok(None);
    };
    Ok(protocol_of(&profile.provider_type)
        .and_then(|protocol| contacted_destinations(protocol, &profile))
        .filter(|destinations| !destinations.is_empty()))
}

/// Each unrevoked profile version with the origins a dispatch with it contacts, ordered by version identifier.
/// Versions with an unknown provider type or no valid origin are left out: they can never be
/// authorized, so no grant covers them.
fn live_version_destinations(conn: &Connection) -> Result<Vec<(String, Vec<String>)>> {
    let mut statement = conn
        .prepare(
            "SELECT profile_version, provider_type, endpoint, authorized_destinations \
             FROM provider_profiles WHERE revoked_at IS NULL ORDER BY profile_version",
        )
        .context("preparing live profile listing")?;
    let profiles = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                StoredProfile {
                    provider_type: row.get(1)?,
                    endpoint: row.get(2)?,
                    authorized_destinations: row.get(3)?,
                },
            ))
        })
        .context("listing live profiles")?;
    let mut versions = Vec::new();
    for profile in profiles {
        let (version, profile) = profile.context("reading live profile")?;
        let destinations = protocol_of(&profile.provider_type)
            .and_then(|protocol| contacted_destinations(protocol, &profile))
            .filter(|destinations| !destinations.is_empty());
        if let Some(destinations) = destinations {
            versions.push((version, destinations));
        }
    }
    Ok(versions)
}

fn destinations_of_remaining_grants(conn: &Connection, route_id: &str) -> Result<BTreeSet<String>> {
    let mut statement = conn
        .prepare("SELECT authorized_destinations FROM route_authorizations WHERE route_id = ?")
        .context("preparing remaining grants")?;
    let stored = statement
        .query_map([route_id], |row| row.get::<_, String>(0))
        .context("reading remaining grants")?;
    let mut used = BTreeSet::new();
    for entry in stored {
        // An unreadable grant authorizes nothing, so it keeps nothing in the ceiling.
        if let Some(origins) = parse_origin_list(&entry.context("reading remaining grant")?) {
            used.extend(origins);
        }
    }
    Ok(used)
}

fn destination_classes_by_origin(
    conn: &Connection,
) -> Result<BTreeMap<String, BTreeSet<ListedDestinationClass>>> {
    let mut statement = conn
        .prepare("SELECT provider_type, endpoint, authorized_destinations FROM provider_profiles")
        .context("preparing profile listing")?;
    let profiles = statement
        .query_map([], |row| {
            Ok(StoredProfile {
                provider_type: row.get(0)?,
                endpoint: row.get(1)?,
                authorized_destinations: row.get(2)?,
            })
        })
        .context("listing profiles")?;
    let mut classes: BTreeMap<String, BTreeSet<ListedDestinationClass>> = BTreeMap::new();
    for profile in profiles {
        let profile = profile.context("reading profile")?;
        let Some(protocol) = protocol_of(&profile.provider_type) else {
            continue;
        };
        let class = match protocol {
            ProviderProtocol::SelfHosted => ListedDestinationClass::PrivateServer,
            ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => ListedDestinationClass::Cloud,
        };
        for origin in contacted_destinations(protocol, &profile).unwrap_or_default() {
            classes.entry(origin).or_default().insert(class);
        }
    }
    Ok(classes)
}

fn union_preserving_order(existing: &[String], additions: &[String]) -> Vec<String> {
    let mut merged = existing.to_vec();
    for origin in additions {
        if !merged.contains(origin) {
            merged.push(origin.clone());
        }
    }
    merged
}

fn encode_origin_list(origins: &[String]) -> Result<String> {
    serde_json::to_string(origins).context("encoding origin list")
}
