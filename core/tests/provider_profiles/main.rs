//! Integration fixtures for versioned provider profiles and the per-capability default (V01b).
//! Profiles are written through the production write path, captures enter through the real
//! foreground import, and assertions read durable state.

use chrono::{DateTime, Utc};
use ohand_core::ingress::import_foreground_ingress;
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::queue::{get_jobs_for_item, Job};
use ohand_core::privacy::routing::{
    authorize_job, AuthorizationDecision, ProcessingCapability, JOB_TYPE_INTERPRET,
};
use ohand_core::providers::profiles::{
    clear_default_profile, create_profile_version, default_profile_version, list_profiles,
    parse_default_capability, set_default_profile, ProfileDraft, ProfileFailure, ProfileListing,
    RetryDraft, StoredProfileVersion,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock as StoreClock, Database};
use std::sync::Arc;
use tempfile::TempDir;

const NOW: &str = "2026-10-14T10:30:00Z";
const ROUTE_ID: &str = "route-1";
const OTHER_ROUTE_ID: &str = "route-2";
const OPENAI_ORIGIN: &str = "https://api.openai.com";
const PRIVATE_ORIGIN: &str = "https://llm.example.test";
const CREDENTIAL: &str = "credential-ref/synthetic-primary";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .with_timezone(&Utc)
}

struct FixedClock;

impl StoreClock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        now()
    }
}

struct Store {
    _directory: TempDir,
    path: String,
}

impl Store {
    fn new() -> Store {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("ohand.db").display().to_string();
        let store = Store {
            _directory: directory,
            path,
        };
        for route in [ROUTE_ID, OTHER_ROUTE_ID] {
            store
                .open()
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
                     created_at) VALUES (?, 'private notes', 'personal', '[]', ?)",
                    rusqlite::params![route, NOW],
                )
                .unwrap();
        }
        store
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(FixedClock)).unwrap()
    }

    /// The explicit route grant V02b will own: ceiling and capability grant for `origin`.
    fn grant(&self, route_id: &str, origin: &str) {
        let origins = format!("[\"{origin}\"]");
        let database = self.open();
        database
            .conn()
            .execute(
                "UPDATE routes SET processing_destinations = ? WHERE route_id = ?",
                rusqlite::params![origins, route_id],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO route_authorizations (auth_id, route_id, capability, \
                 authorized_destinations, created_at) VALUES (?, ?, 'text_interpretation', ?, ?)",
                rusqlite::params![format!("auth-{route_id}"), route_id, origins, NOW],
            )
            .unwrap();
    }

    fn create(&self, draft: &ProfileDraft) -> Result<StoredProfileVersion, ProfileFailure> {
        create_profile_version(&mut self.open(), draft, now())
    }

    fn create_ok(&self, draft: &ProfileDraft) -> StoredProfileVersion {
        self.create(draft).expect("the draft is valid")
    }

    fn set_default(&self, version: &str) -> Result<(), ProfileFailure> {
        set_default_profile(
            &mut self.open(),
            ProcessingCapability::TextInterpretation,
            version,
            now(),
        )
    }

    fn default_version(&self) -> Option<String> {
        default_profile_version(self.open().conn(), ProcessingCapability::TextInterpretation)
            .unwrap()
    }

    fn listing(&self) -> ProfileListing {
        list_profiles(self.open().conn()).unwrap()
    }

    fn row_count(&self) -> i64 {
        self.open()
            .conn()
            .query_row("SELECT COUNT(*) FROM provider_profiles", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn import(&self, capture_id: &str, route_id: &str) -> (String, Option<Job>) {
        let capture = Capture::new(
            capture_id.to_string(),
            Some("call the roofer about the leak".to_string()),
            None,
            NOW.to_string(),
            "UTC".to_string(),
            0,
            "en_US".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            route_id.to_string(),
            false,
            NOW.to_string(),
            None,
        )
        .unwrap();
        let item_id = import_foreground_ingress(&mut self.open(), &capture)
            .unwrap()
            .item_id;
        let job = get_jobs_for_item(&self.open(), &item_id)
            .unwrap()
            .into_iter()
            .find(|job| job.job_type == JOB_TYPE_INTERPRET);
        (item_id, job)
    }
}

fn openai_draft(profile_id: &str) -> ProfileDraft {
    serde_json::from_value(serde_json::json!({
        "profile_id": profile_id,
        "protocol": "open_ai",
        "model": "synthetic-model",
        "credential_ref": CREDENTIAL,
        "timeout_seconds": 30,
        "capabilities": ["text_interpretation"],
    }))
    .unwrap()
}

fn private_draft(profile_id: &str) -> ProfileDraft {
    ProfileDraft {
        protocol: "self_hosted".to_string(),
        endpoint: Some(format!("{PRIVATE_ORIGIN}/v1")),
        ..openai_draft(profile_id)
    }
}

fn refused(result: Result<StoredProfileVersion, ProfileFailure>) -> &'static str {
    result.expect_err("the draft must be refused").code()
}

fn stored_row(store: &Store, profile_version: &str) -> Vec<String> {
    store
        .open()
        .conn()
        .query_row(
            "SELECT profile_id, provider_type, IFNULL(endpoint, ''), model, \
             IFNULL(credential_ref, ''), timeout_seconds, retry_policy, authorized_destinations, \
             capabilities, created_at, IFNULL(revoked_at, '') FROM provider_profiles \
             WHERE profile_version = ?",
            [profile_version],
            |row| {
                (0..11)
                    .map(|index| {
                        row.get::<_, rusqlite::types::Value>(index)
                            .map(|value| match value {
                                rusqlite::types::Value::Text(text) => text,
                                other => format!("{other:?}"),
                            })
                    })
                    .collect()
            },
        )
        .unwrap()
}

#[test]
fn editing_a_profile_adds_an_immutable_version_and_leaves_earlier_rows_untouched() {
    let store = Store::new();
    let first = store.create_ok(&openai_draft("profile-a"));
    let before = stored_row(&store, &first.profile_version);

    let mut edited = openai_draft("profile-a");
    edited.model = "another-model".to_string();
    edited.timeout_seconds = 60;
    let second = store.create_ok(&edited);

    assert_eq!(first.profile_id, second.profile_id);
    assert_ne!(first.profile_version, second.profile_version);
    assert_eq!(store.row_count(), 2);
    assert_eq!(stored_row(&store, &first.profile_version), before);
    let listing = store.listing();
    assert_eq!(listing.profiles.len(), 1);
    let versions: Vec<_> = listing.profiles[0]
        .versions
        .iter()
        .map(|version| (version.profile_version.as_str(), version.model.as_str()))
        .collect();
    assert_eq!(
        versions,
        [
            (first.profile_version.as_str(), "synthetic-model"),
            (second.profile_version.as_str(), "another-model")
        ]
    );
}

#[test]
fn destinations_are_derived_by_the_core_never_supplied() {
    let store = Store::new();
    let hosted = store.create_ok(&openai_draft("hosted"));
    let private = store.create_ok(&private_draft("private"));

    let destinations = |version: &str| stored_row(&store, version)[7].clone();
    assert_eq!(
        destinations(&hosted.profile_version),
        format!("[\"{OPENAI_ORIGIN}\"]")
    );
    assert_eq!(
        destinations(&private.profile_version),
        format!("[\"{PRIVATE_ORIGIN}\"]")
    );
    let mut shouting = private_draft("shouting");
    shouting.endpoint = Some("https://LLM.Example.Test:8443/v1".to_string());
    let shouting = store.create_ok(&shouting);
    assert_eq!(
        destinations(&shouting.profile_version),
        "[\"https://llm.example.test:8443\"]"
    );
    let unknown_field = serde_json::from_value::<ProfileDraft>(serde_json::json!({
        "profile_id": "x", "protocol": "open_ai", "model": "m", "timeout_seconds": 30,
        "capabilities": ["text_interpretation"], "authorized_destinations": ["https://evil.test"],
    }));
    assert!(unknown_field.is_err());
}

#[test]
fn every_validation_failure_has_a_fixed_code_and_writes_nothing() {
    let store = Store::new();
    let sentinel = "SENTINEL-must-not-echo";
    let cases: Vec<(&str, ProfileDraft)> = vec![
        (
            "unknown_protocol",
            ProfileDraft {
                protocol: sentinel.to_string(),
                ..openai_draft("p")
            },
        ),
        (
            "profile_id_invalid",
            ProfileDraft {
                profile_id: "  ".to_string(),
                ..openai_draft("p")
            },
        ),
        (
            "endpoint_required",
            ProfileDraft {
                endpoint: None,
                ..private_draft("p")
            },
        ),
        (
            "endpoint_required",
            ProfileDraft {
                endpoint: Some("  ".to_string()),
                ..private_draft("p")
            },
        ),
        (
            "endpoint_invalid",
            ProfileDraft {
                endpoint: Some(format!("http://{sentinel}.test/v1")),
                ..private_draft("p")
            },
        ),
        (
            "endpoint_invalid",
            ProfileDraft {
                endpoint: Some(format!("https://user:{sentinel}@llm.example.test")),
                ..private_draft("p")
            },
        ),
        (
            "endpoint_invalid",
            ProfileDraft {
                endpoint: Some(format!("https://llm.example.test/v1?key={sentinel}")),
                ..private_draft("p")
            },
        ),
        (
            "endpoint_invalid",
            ProfileDraft {
                endpoint: Some("https://not a host/v1".to_string()),
                ..private_draft("p")
            },
        ),
        (
            "endpoint_not_allowed",
            ProfileDraft {
                endpoint: Some(format!("https://{sentinel}.test")),
                ..openai_draft("p")
            },
        ),
        (
            "endpoint_not_allowed",
            ProfileDraft {
                protocol: "anthropic".to_string(),
                endpoint: Some(PRIVATE_ORIGIN.to_string()),
                ..openai_draft("p")
            },
        ),
        (
            "model_required",
            ProfileDraft {
                model: "   ".to_string(),
                ..openai_draft("p")
            },
        ),
        (
            "model_invalid",
            ProfileDraft {
                model: "x".repeat(300),
                ..openai_draft("p")
            },
        ),
        (
            "capability_unsupported",
            ProfileDraft {
                capabilities: vec!["transcription".to_string()],
                ..openai_draft("p")
            },
        ),
        (
            "capability_unsupported",
            ProfileDraft {
                capabilities: vec!["text_interpretation".to_string(), "embeddings".to_string()],
                ..private_draft("p")
            },
        ),
        (
            "capability_unsupported",
            ProfileDraft {
                capabilities: vec![sentinel.to_string()],
                ..openai_draft("p")
            },
        ),
        (
            "capability_unsupported",
            ProfileDraft {
                capabilities: vec![],
                ..openai_draft("p")
            },
        ),
        (
            "credential_required",
            ProfileDraft {
                credential_ref: None,
                ..openai_draft("p")
            },
        ),
        (
            "credential_required",
            ProfileDraft {
                credential_ref: Some(" ".to_string()),
                ..private_draft("p")
            },
        ),
        (
            "credential_invalid",
            ProfileDraft {
                credential_ref: Some("bad\nref".to_string()),
                ..openai_draft("p")
            },
        ),
        (
            "timeout_out_of_bounds",
            ProfileDraft {
                timeout_seconds: 0,
                ..openai_draft("p")
            },
        ),
        (
            "timeout_out_of_bounds",
            ProfileDraft {
                timeout_seconds: 301,
                ..openai_draft("p")
            },
        ),
        (
            "timeout_out_of_bounds",
            ProfileDraft {
                timeout_seconds: -5,
                ..openai_draft("p")
            },
        ),
    ];
    let retry = |max_attempts, initial_backoff_ms, max_backoff_ms| {
        Some(RetryDraft {
            max_attempts,
            initial_backoff_ms,
            max_backoff_ms,
        })
    };
    let retry_cases = [
        retry(0, 500, 30_000),
        retry(11, 500, 30_000),
        retry(3, 0, 30_000),
        retry(3, 40_000, 30_000),
        retry(-1, 500, 30_000),
        retry(3, -500, 30_000),
        retry(3, 500, -1),
    ];

    for (expected, draft) in &cases {
        let failure = store.create(draft).expect_err("the draft must be refused");
        assert_eq!(failure.code(), *expected, "{draft:?}");
        assert!(!failure.message().contains(sentinel));
    }
    for policy in retry_cases {
        let draft = ProfileDraft {
            retry_policy: policy.clone(),
            ..openai_draft("p")
        };
        assert_eq!(
            refused(store.create(&draft)),
            "retry_out_of_bounds",
            "{policy:?}"
        );
    }
    assert_eq!(store.row_count(), 0);

    let valid_retry = ProfileDraft {
        retry_policy: retry(5, 100, 1_000),
        ..openai_draft("p")
    };
    store.create_ok(&valid_retry);
}

#[test]
fn a_credential_claimed_by_a_pending_reset_is_refused_by_the_storage_fence() {
    let store = Store::new();
    let database = store.open();
    database
        .conn()
        .execute(
            "INSERT INTO reset_generations (phase, boundary_rowid, started_at) \
             VALUES ('pending', 0, ?)",
            [NOW],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO reset_credential_claims (credential_ref, generation) VALUES (?, 1)",
            [CREDENTIAL],
        )
        .unwrap();
    drop(database);

    assert_eq!(
        refused(store.create(&openai_draft("p"))),
        "credential_pending_removal"
    );
    assert_eq!(store.row_count(), 0);

    let mut other = openai_draft("p");
    other.credential_ref = Some("credential-ref/unclaimed".to_string());
    store.create_ok(&other);
}

#[test]
fn the_default_must_name_an_existing_unrevoked_version_and_can_be_cleared() {
    let store = Store::new();
    let first = store.create_ok(&openai_draft("p"));
    let second = store.create_ok(&openai_draft("q"));

    assert_eq!(store.default_version(), None);
    store.set_default(&first.profile_version).unwrap();
    assert_eq!(store.default_version(), Some(first.profile_version.clone()));
    store.set_default(&second.profile_version).unwrap();
    assert_eq!(
        store.default_version(),
        Some(second.profile_version.clone())
    );

    let missing = store.set_default("no-such-version").unwrap_err();
    assert_eq!(missing.code(), "profile_not_found");
    assert_eq!(
        store.default_version(),
        Some(second.profile_version.clone())
    );

    revoke_profile(&mut store.open(), &first.profile_version, now()).unwrap();
    let revoked = store.set_default(&first.profile_version).unwrap_err();
    assert_eq!(revoked.code(), "profile_revoked");
    assert_eq!(
        store.default_version(),
        Some(second.profile_version.clone())
    );

    clear_default_profile(&mut store.open(), ProcessingCapability::TextInterpretation).unwrap();
    assert_eq!(store.default_version(), None);
    clear_default_profile(&mut store.open(), ProcessingCapability::TextInterpretation).unwrap();

    assert_eq!(
        parse_default_capability("transcription")
            .unwrap_err()
            .code(),
        "capability_not_configurable"
    );
    assert_eq!(
        parse_default_capability("nonsense").unwrap_err().code(),
        "capability_not_configurable"
    );
}

#[test]
fn changing_the_default_never_touches_a_queued_job() {
    let store = Store::new();
    store.grant(ROUTE_ID, OPENAI_ORIGIN);
    let first = store.create_ok(&openai_draft("p"));
    let second = store.create_ok(&openai_draft("q"));
    store.set_default(&first.profile_version).unwrap();

    let (item_id, job) = store.import("3f9c1e52-0000-4000-8000-000000000001", ROUTE_ID);
    let job = job.expect("an interpret job is queued");
    assert_eq!(
        job.profile_version.as_deref(),
        Some(first.profile_version.as_str())
    );

    store.set_default(&second.profile_version).unwrap();
    clear_default_profile(&mut store.open(), ProcessingCapability::TextInterpretation).unwrap();

    let after = get_jobs_for_item(&store.open(), &item_id).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].job_id, job.job_id);
    assert_eq!(after[0].profile_version, job.profile_version);
    assert_eq!(after[0].status, job.status);
}

#[test]
fn the_resolver_pins_the_default_only_when_the_job_would_dispatch() {
    let store = Store::new();
    let hosted = store.create_ok(&openai_draft("hosted"));
    let private = store.create_ok(&private_draft("private"));
    let pin = |capture: &str, route: &str| {
        store
            .import(
                &format!("3f9c1e52-0000-4000-8000-0000000000{capture}"),
                route,
            )
            .1
            .expect("every text capture queues a job")
            .profile_version
    };

    assert_eq!(pin("10", ROUTE_ID), None, "no default is set");

    store.set_default(&hosted.profile_version).unwrap();
    assert_eq!(pin("11", ROUTE_ID), None, "the route has no grant");

    store.grant(ROUTE_ID, PRIVATE_ORIGIN);
    assert_eq!(
        pin("12", ROUTE_ID),
        None,
        "the grant covers another destination"
    );

    store.set_default(&private.profile_version).unwrap();
    assert_eq!(
        pin("13", ROUTE_ID),
        Some(private.profile_version.clone()),
        "granted destination"
    );
    assert_eq!(
        pin("14", OTHER_ROUTE_ID),
        None,
        "another route has no grant"
    );

    revoke_profile(&mut store.open(), &private.profile_version, now()).unwrap();
    assert_eq!(pin("15", ROUTE_ID), None, "the default is revoked");

    store
        .open()
        .conn()
        .execute(
            "DELETE FROM provider_profiles WHERE profile_version = ?",
            [&private.profile_version],
        )
        .unwrap();
    assert_eq!(pin("16", ROUTE_ID), None, "the default no longer exists");
}

#[test]
fn a_pinned_job_is_authorized_by_the_real_check_and_the_probe_leaves_no_trace() {
    let store = Store::new();
    store.grant(ROUTE_ID, PRIVATE_ORIGIN);
    let private = store.create_ok(&private_draft("private"));
    store.set_default(&private.profile_version).unwrap();

    let (item_id, job) = store.import("3f9c1e52-0000-4000-8000-000000000020", ROUTE_ID);
    let job = job.unwrap();
    let decision = authorize_job(store.open().conn(), &job.job_id).unwrap();
    assert!(
        matches!(decision, AuthorizationDecision::Authorized(_)),
        "{decision:?}"
    );
    assert_eq!(get_jobs_for_item(&store.open(), &item_id).unwrap().len(), 1);
    let total_jobs: i64 = store
        .open()
        .conn()
        .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total_jobs, 1);
}

#[test]
fn listing_reports_each_version_without_the_credential_reference() {
    let store = Store::new();
    let hosted = store.create_ok(&openai_draft("hosted"));
    let private = store.create_ok(&private_draft("private"));
    let newer = store.create_ok(&private_draft("private"));
    store.set_default(&newer.profile_version).unwrap();
    revoke_profile(&mut store.open(), &private.profile_version, now()).unwrap();

    let listing = store.listing();
    let serialized = serde_json::to_string(&listing).unwrap();
    assert!(!serialized.contains(CREDENTIAL));
    assert_eq!(listing.profiles.len(), 2);

    let hosted_version = &listing.profiles[0].versions[0];
    assert_eq!(listing.profiles[0].profile_id, "hosted");
    assert_eq!(hosted_version.profile_version, hosted.profile_version);
    assert_eq!(hosted_version.protocol, "open_ai");
    assert_eq!(hosted_version.endpoint, None);
    assert_eq!(hosted_version.model, "synthetic-model");
    assert!(hosted_version.credential_bound);
    assert!(!hosted_version.revoked);
    assert!(hosted_version.default_for.is_empty());
    let capabilities = serde_json::to_value(&hosted_version.capabilities).unwrap();
    assert_eq!(
        capabilities,
        serde_json::json!([{"capability": "text_interpretation", "support": "supported"}])
    );

    let versions = &listing.profiles[1].versions;
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].profile_version, private.profile_version);
    assert!(versions[0].revoked);
    assert_eq!(
        versions[0].endpoint.as_deref(),
        Some("https://llm.example.test/v1")
    );
    assert_eq!(versions[0].protocol, "self_hosted");
    assert!(versions[0].default_for.is_empty());
    assert_eq!(versions[1].default_for, ["text_interpretation"]);
    assert_eq!(listing.defaults.len(), 1);
    assert_eq!(listing.defaults[0].profile_version, newer.profile_version);
}

#[test]
fn a_profile_with_no_credential_binding_never_lists_as_bound() {
    let store = Store::new();
    let version = store.create_ok(&openai_draft("p"));
    store
        .open()
        .conn()
        .execute(
            "UPDATE provider_profiles SET credential_ref = NULL WHERE profile_version = ?",
            [&version.profile_version],
        )
        .unwrap();
    assert!(!store.listing().profiles[0].versions[0].credential_bound);
}
