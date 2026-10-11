//! Destination authorization for the two arms.
//!
//! Core only issues an `AuthorizationDecision` from its routing rules over a store, so the
//! command evaluates those same rules over a private scratch store held in memory. The scratch
//! store is built from the origins the caller hands in and from nothing else: choosing a profile
//! adds no destination, and a profile whose origin was not handed in is denied by the production
//! rules rather than by this module. The command hands in the origins of the destinations
//! granted with `--allow-destination` (live mode). It holds no case text, no capture of the user
//! and no handle the executor could apply a result to.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use ohand_core::jobs::queue::enqueue_job;
use ohand_core::privacy::routing::{authorize_job, AuthorizationDecision, JOB_TYPE_INTERPRET};
use ohand_core::providers::contracts::{ProviderProfile, ProviderProtocol};
use ohand_core::review::compare::ArmLabel;
use ohand_core::store::schema::{Database, SystemClock};
use uuid::Uuid;

use super::corpus::ROUTE_ID;
use crate::executor::{ArmAuthorizer, AuthorizationQuery};

const STAMP: &str = "2026-01-01T00:00:00Z";

struct Scratch {
    database: Database,
    item_id: String,
    stored_profiles: HashSet<String>,
}

pub struct ScratchAuthority {
    scratch: RefCell<Scratch>,
    profile_a: ProviderProfile,
    profile_b: ProviderProfile,
}

fn stamp<T: serde::de::DeserializeOwned>() -> T {
    serde_json::from_value(serde_json::json!(STAMP)).expect("fixed timestamp parses")
}

fn provider_type(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAi => "open_ai",
        ProviderProtocol::Anthropic => "anthropic",
        ProviderProtocol::SelfHosted => "self_hosted",
    }
}

impl ScratchAuthority {
    /// `granted_origins` are the bare origins the caller explicitly granted; any other is denied.
    pub fn new(
        granted_origins: &[String],
        profile_a: ProviderProfile,
        profile_b: ProviderProfile,
    ) -> Result<ScratchAuthority, String> {
        let database = Database::open(":memory:", Arc::new(SystemClock))
            .map_err(|error| format!("scratch authorization store: {error}"))?;
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        let grants = serde_json::to_string(granted_origins).expect("origins serialize");
        {
            let conn = database.conn();
            let failed =
                |error: &dyn std::fmt::Display| format!("scratch authorization store: {error}");
            conn.execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
                 created_at) VALUES (?, 'default', 'personal', ?, ?)",
                (ROUTE_ID, &grants, STAMP),
            )
            .map_err(|error| failed(&error))?;
            conn.execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, \
                 created_at) VALUES (?, 'scratch', ?, 'UTC', 0, 'en', 'gregorian', 'personal', \
                 ?, 0, ?)",
                (&capture_id, STAMP, ROUTE_ID, STAMP),
            )
            .map_err(|error| failed(&error))?;
            conn.execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
                 'not_applicable', ?, ?)",
                (&item_id, &capture_id, STAMP, STAMP),
            )
            .map_err(|error| failed(&error))?;
            conn.execute(
                "INSERT INTO route_authorizations (auth_id, route_id, capability, \
                 authorized_destinations, created_at) \
                 VALUES ('explicit-grant', ?, 'text_interpretation', ?, ?)",
                (ROUTE_ID, &grants, STAMP),
            )
            .map_err(|error| failed(&error))?;
        }
        Ok(ScratchAuthority {
            scratch: RefCell::new(Scratch {
                database,
                item_id,
                stored_profiles: HashSet::new(),
            }),
            profile_a,
            profile_b,
        })
    }

    fn decide(&self, profile: &ProviderProfile) -> AuthorizationDecision {
        let mut scratch = self.scratch.borrow_mut();
        if scratch
            .stored_profiles
            .insert(profile.profile_version().to_string())
        {
            scratch
                .database
                .conn()
                .execute(
                    "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, \
                     endpoint, model, timeout_seconds, retry_policy, authorized_destinations, \
                     capabilities, created_at) VALUES (?, ?, ?, ?, 'model', 30, '{}', ?, '{}', ?)",
                    (
                        profile.profile_version(),
                        profile.profile_id(),
                        provider_type(profile.protocol()),
                        profile.endpoint(),
                        serde_json::to_string(profile.authorized_destinations())
                            .expect("origins serialize"),
                        STAMP,
                    ),
                )
                .expect("scratch profile row inserts");
        }
        let job_id = Uuid::new_v4().to_string();
        let item_id = scratch.item_id.clone();
        enqueue_job(
            &mut scratch.database,
            job_id.clone(),
            item_id,
            JOB_TYPE_INTERPRET.to_string(),
            0,
            Some(profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            stamp(),
        )
        .expect("scratch job enqueues");
        authorize_job(scratch.database.conn(), &job_id).expect("scratch authorization evaluates")
    }
}

impl ArmAuthorizer for ScratchAuthority {
    fn authorize(&self, query: &AuthorizationQuery<'_>) -> AuthorizationDecision {
        let profile = match query.arm {
            ArmLabel::A => &self.profile_a,
            ArmLabel::B => &self.profile_b,
        };
        debug_assert_eq!(query.profile_version, profile.profile_version());
        self.decide(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ProfileSpec;
    use crate::cli::profiles::{build_profile, EndpointOverrides};

    fn arm(protocol: &str, reference: &str) -> crate::cli::profiles::ArmProfile {
        let spec = ProfileSpec {
            protocol: if protocol == "openai" {
                ProviderProtocol::OpenAi
            } else {
                ProviderProtocol::Anthropic
            },
            model: "model".to_string(),
            credential_ref: reference.to_string(),
        };
        build_profile(&spec, 30, EndpointOverrides::default()).unwrap()
    }

    fn decision(authority: &ScratchAuthority, label: ArmLabel, version: &str) -> bool {
        authority
            .authorize(&AuthorizationQuery {
                arm: label,
                case_id: "case",
                route_id: ROUTE_ID,
                profile_version: version,
            })
            .authorization()
            .is_some()
    }

    #[test]
    fn production_rules_deny_an_arm_whose_origin_was_not_granted() {
        let arm_a = arm("anthropic", "ref-a");
        let arm_b = arm("openai", "ref-b");
        let version_a = arm_a.profile.profile_version().to_string();
        let version_b = arm_b.profile.profile_version().to_string();
        let authority = ScratchAuthority::new(
            std::slice::from_ref(&arm_a.origin),
            arm_a.profile.clone(),
            arm_b.profile.clone(),
        )
        .unwrap();
        assert!(decision(&authority, ArmLabel::A, &version_a));
        assert!(!decision(&authority, ArmLabel::B, &version_b));
    }

    #[test]
    fn nothing_is_authorized_without_grants() {
        let arm_a = arm("anthropic", "ref-a");
        let arm_b = arm("openai", "ref-b");
        let version_a = arm_a.profile.profile_version().to_string();
        let authority =
            ScratchAuthority::new(&[], arm_a.profile.clone(), arm_b.profile.clone()).unwrap();
        assert!(!decision(&authority, ArmLabel::A, &version_a));
    }
}
