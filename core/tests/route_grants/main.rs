use chrono::{DateTime, Utc};
use ohand_core::privacy::routing::{
    authorize_job, capability_from_key, grant_route_capability, list_route_policy,
    revoke_route_capability, AuthorizationDecision, DenialReason, GrantError, GrantOutcome,
    ListedDestinationClass, ProcessingCapability, RevokeOutcome, JOB_TYPE_INTERPRET,
    JOB_TYPE_SHADOW_REVIEW,
};
use ohand_core::store::schema::{Clock, Database};
use std::sync::Arc;

const OPENAI: &str = "https://api.openai.com";
const ANTHROPIC: &str = "https://api.anthropic.com";
const SPARK: &str = "https://spark.example.test:8443";
const CREATED_AT: &str = "2026-01-15T10:30:00Z";

const INTERPRET: ProcessingCapability = ProcessingCapability::TextInterpretation;
const REVIEW: ProcessingCapability = ProcessingCapability::Review;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00")
            .unwrap()
            .with_timezone(&Utc)
    }
}

struct Fixture {
    db: Database,
    path: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn fresh_install() -> Fixture {
    let path = format!(
        "{}/test_route_grants_{}.db",
        std::env::temp_dir().display(),
        uuid::Uuid::new_v4()
    );
    let db = Database::open(&path, Arc::new(FixedClock)).unwrap();
    Fixture { db, path }
}

impl Fixture {
    fn exec(&self, sql: &str, params: impl rusqlite::Params) {
        self.db.conn().execute(sql, params).unwrap();
    }

    fn add_route(&self, route_id: &str, route_name: &str, processing_destinations: &str) {
        self.exec(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
             VALUES (?, ?, 'personal', ?, ?)",
            rusqlite::params![route_id, route_name, processing_destinations, CREATED_AT],
        );
    }

    fn add_profile(
        &self,
        profile_version: &str,
        provider_type: &str,
        endpoint: Option<&str>,
        authorized_destinations: &str,
    ) {
        self.exec(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) \
             VALUES (?, ?, ?, ?, 'model', 30, '{}', ?, '{}', ?)",
            rusqlite::params![
                profile_version,
                format!("profile-for-{profile_version}"),
                provider_type,
                endpoint,
                authorized_destinations,
                CREATED_AT
            ],
        );
    }

    fn openai(&self, profile_version: &str) {
        self.add_profile(profile_version, "open_ai", None, &format!("[\"{OPENAI}\"]"));
    }

    fn anthropic(&self, profile_version: &str) {
        self.add_profile(
            profile_version,
            "anthropic",
            None,
            &format!("[\"{ANTHROPIC}\"]"),
        );
    }

    fn self_hosted(&self, profile_version: &str) {
        self.add_profile(
            profile_version,
            "self_hosted",
            Some("https://Spark.Example.Test:8443/v1/chat"),
            "[]",
        );
    }

    fn revoke_profile(&self, profile_version: &str) {
        self.exec(
            "UPDATE provider_profiles SET revoked_at = ? WHERE profile_version = ?",
            rusqlite::params![CREATED_AT, profile_version],
        );
    }

    fn add_job(&self, job_id: &str, job_type: &str, route_id: &str, profile_version: Option<&str>) {
        let capture_id = format!("capture-{job_id}");
        let item_id = format!("item-{job_id}");
        self.exec(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
             utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
             VALUES (?, 'buy oat milk', ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
            rusqlite::params![capture_id, CREATED_AT, route_id, CREATED_AT],
        );
        self.exec(
            "INSERT INTO items (item_id, capture_id, current_scope, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, 'personal', 'active', 'saved', 'local', 'queued', 'none', ?, ?)",
            rusqlite::params![item_id, capture_id, CREATED_AT, CREATED_AT],
        );
        self.exec(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
             profile_version, status, created_at) VALUES (?, 1, ?, ?, 0, ?, 'queued', ?)",
            rusqlite::params![job_id, item_id, job_type, profile_version, CREATED_AT],
        );
    }

    fn grant(
        &mut self,
        route_id: &str,
        capability: ProcessingCapability,
        profile_version: &str,
    ) -> Result<GrantOutcome, GrantError> {
        grant_route_capability(
            &mut self.db,
            route_id,
            capability,
            profile_version,
            CREATED_AT,
        )
    }

    fn revoke(
        &mut self,
        route_id: &str,
        capability: ProcessingCapability,
    ) -> Result<RevokeOutcome, GrantError> {
        revoke_route_capability(&mut self.db, route_id, capability)
    }

    fn decision(&self, job_id: &str) -> AuthorizationDecision {
        authorize_job(self.db.conn(), job_id).unwrap()
    }

    fn denial(&self, job_id: &str) -> Option<DenialReason> {
        self.decision(job_id).denial()
    }

    fn ceiling(&self, route_id: &str) -> String {
        self.db
            .conn()
            .query_row(
                "SELECT processing_destinations FROM routes WHERE route_id = ?",
                [route_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn grant_row(&self, route_id: &str, capability: ProcessingCapability) -> Option<String> {
        use rusqlite::OptionalExtension;
        self.db
            .conn()
            .query_row(
                "SELECT authorized_destinations FROM route_authorizations \
                 WHERE route_id = ? AND capability = ?",
                [route_id, capability.as_str()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    fn snapshot(&self) -> Vec<String> {
        let mut statement = self
            .db
            .conn()
            .prepare(
                "SELECT route_id || '|' || processing_destinations FROM routes \
                 UNION ALL SELECT route_id || '|' || capability || '|' || authorized_destinations \
                 FROM route_authorizations ORDER BY 1",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

#[test]
fn a_fresh_route_denies_until_granted_then_authorizes_only_the_profile_destinations() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture.add_job(
        "job-1",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );
    assert_eq!(
        fixture.denial("job-1"),
        Some(DenialReason::DestinationNotInRoute)
    );

    let outcome = fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();

    assert_eq!(outcome, GrantOutcome::Granted);
    assert_eq!(fixture.ceiling("route-a"), format!("[\"{OPENAI}\"]"));
    assert_eq!(
        fixture.grant_row("route-a", INTERPRET),
        Some(format!("[\"{OPENAI}\"]"))
    );
    let decision = fixture.decision("job-1");
    let authorization = decision
        .authorization()
        .expect("authorized after the grant");
    assert_eq!(authorization.destinations(), [OPENAI]);
}

#[test]
fn self_hosted_destinations_come_from_the_normalized_endpoint_origin() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.self_hosted("profile-spark");
    fixture.add_job(
        "job-1",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-spark"),
    );

    fixture
        .grant("route-a", INTERPRET, "profile-spark")
        .unwrap();

    assert_eq!(fixture.ceiling("route-a"), format!("[\"{SPARK}\"]"));
    assert_eq!(
        fixture
            .decision("job-1")
            .authorization()
            .unwrap()
            .destinations(),
        [SPARK]
    );
}

#[test]
fn refused_grants_change_nothing_and_report_fixed_codes() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-revoked");
    fixture.revoke_profile("profile-revoked");
    fixture.openai("profile-live");
    fixture.add_profile("profile-unknown-type", "mystery", None, "[]");
    fixture.add_profile("profile-no-destination", "open_ai", None, "[]");
    let before = fixture.snapshot();

    let cases = [
        ("route-a", INTERPRET, "profile-revoked", "profile_revoked"),
        (
            "route-a",
            INTERPRET,
            "profile-missing",
            "profile_unavailable",
        ),
        ("route-missing", INTERPRET, "profile-live", "route_unknown"),
        (
            "route-a",
            ProcessingCapability::Transcription,
            "profile-live",
            "capability_not_grantable",
        ),
        (
            "route-a",
            INTERPRET,
            "profile-unknown-type",
            "profile_malformed",
        ),
        (
            "route-a",
            INTERPRET,
            "profile-no-destination",
            "profile_malformed",
        ),
    ];
    for (route_id, capability, profile_version, code) in cases {
        let error = fixture
            .grant(route_id, capability, profile_version)
            .unwrap_err();
        assert_eq!(error.code(), code, "{route_id} {profile_version}");
        for secret in [route_id, profile_version] {
            assert!(!error.to_string().contains(secret));
        }
    }

    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn an_unreadable_route_ceiling_is_not_widened() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "not json");
    fixture.openai("profile-openai");

    let error = fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap_err();

    assert_eq!(error.code(), "route_policy_malformed");
    assert_eq!(fixture.ceiling("route-a"), "not json");
    assert_eq!(fixture.grant_row("route-a", INTERPRET), None);
}

#[test]
fn repeating_a_grant_is_idempotent() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    assert_eq!(
        fixture
            .grant("route-a", INTERPRET, "profile-openai")
            .unwrap(),
        GrantOutcome::Granted
    );
    let after_first = fixture.snapshot();

    assert_eq!(
        fixture
            .grant("route-a", INTERPRET, "profile-openai")
            .unwrap(),
        GrantOutcome::Unchanged
    );

    assert_eq!(fixture.snapshot(), after_first);
    let rows: i64 = fixture
        .db
        .conn()
        .query_row("SELECT COUNT(*) FROM route_authorizations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn granting_one_capability_does_not_authorize_another() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture.add_job(
        "job-interpret",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );
    fixture.add_job(
        "job-review",
        JOB_TYPE_SHADOW_REVIEW,
        "route-a",
        Some("profile-openai"),
    );

    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();

    assert!(fixture.decision("job-interpret").authorization().is_some());
    assert_eq!(
        fixture.denial("job-review"),
        Some(DenialReason::CapabilityNotAuthorized)
    );

    fixture.grant("route-a", REVIEW, "profile-openai").unwrap();
    assert!(fixture.decision("job-review").authorization().is_some());
}

#[test]
fn granting_one_route_does_not_authorize_another_route() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.add_route("route-b", "Work", "[]");
    fixture.openai("profile-openai");
    fixture.add_job(
        "job-a",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );
    fixture.add_job(
        "job-b",
        JOB_TYPE_INTERPRET,
        "route-b",
        Some("profile-openai"),
    );

    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();

    assert!(fixture.decision("job-a").authorization().is_some());
    assert_eq!(
        fixture.denial("job-b"),
        Some(DenialReason::DestinationNotInRoute)
    );
    assert_eq!(fixture.ceiling("route-b"), "[]");
    assert_eq!(fixture.grant_row("route-b", INTERPRET), None);
}

#[test]
fn a_grant_for_one_profile_never_authorizes_a_job_pinned_to_another() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture.anthropic("profile-anthropic");
    fixture.add_job(
        "job-openai",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );

    fixture
        .grant("route-a", INTERPRET, "profile-anthropic")
        .unwrap();

    assert_eq!(
        fixture.denial("job-openai"),
        Some(DenialReason::DestinationNotInRoute)
    );
}

#[test]
fn granting_a_second_profile_accumulates_destinations_in_both_places() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture.anthropic("profile-anthropic");

    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture
        .grant("route-a", INTERPRET, "profile-anthropic")
        .unwrap();

    let both = format!("[\"{OPENAI}\",\"{ANTHROPIC}\"]");
    assert_eq!(fixture.ceiling("route-a"), both);
    assert_eq!(fixture.grant_row("route-a", INTERPRET), Some(both));
}

#[test]
fn revoking_fails_queued_jobs_at_dispatch_and_never_falls_back() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture.anthropic("profile-anthropic");
    fixture.add_job(
        "job-1",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    assert!(fixture.decision("job-1").authorization().is_some());

    assert_eq!(
        fixture.revoke("route-a", INTERPRET).unwrap(),
        RevokeOutcome::Revoked
    );

    assert_eq!(fixture.grant_row("route-a", INTERPRET), None);
    assert_eq!(fixture.ceiling("route-a"), "[]");
    assert_eq!(
        fixture.denial("job-1"),
        Some(DenialReason::DestinationNotInRoute)
    );

    // Another provider being granted afterwards does not rescue the job pinned to the revoked one.
    fixture
        .grant("route-a", INTERPRET, "profile-anthropic")
        .unwrap();
    assert_eq!(
        fixture.denial("job-1"),
        Some(DenialReason::DestinationNotInRoute)
    );
}

#[test]
fn revoking_one_capability_keeps_a_destination_another_capability_still_uses() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", &format!("[\"{OPENAI}\"]"));
    fixture.openai("profile-openai");
    fixture.add_job(
        "job-interpret",
        JOB_TYPE_INTERPRET,
        "route-a",
        Some("profile-openai"),
    );
    fixture.add_job(
        "job-review",
        JOB_TYPE_SHADOW_REVIEW,
        "route-a",
        Some("profile-openai"),
    );
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture.grant("route-a", REVIEW, "profile-openai").unwrap();

    fixture.revoke("route-a", INTERPRET).unwrap();

    // Review still uses the destination, so the ceiling keeps it and review stays authorized.
    assert_eq!(fixture.ceiling("route-a"), format!("[\"{OPENAI}\"]"));
    assert_eq!(
        fixture.denial("job-interpret"),
        Some(DenialReason::CapabilityNotAuthorized)
    );
    assert!(fixture.decision("job-review").authorization().is_some());

    fixture.revoke("route-a", REVIEW).unwrap();
    assert_eq!(fixture.ceiling("route-a"), "[]");
}

#[test]
fn revoking_removes_only_the_destinations_no_remaining_grant_uses() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", &format!("[\"{SPARK}\"]"));
    fixture.openai("profile-openai");
    fixture.anthropic("profile-anthropic");
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture
        .grant("route-a", INTERPRET, "profile-anthropic")
        .unwrap();
    fixture
        .grant("route-a", REVIEW, "profile-anthropic")
        .unwrap();
    assert_eq!(
        fixture.ceiling("route-a"),
        format!("[\"{SPARK}\",\"{OPENAI}\",\"{ANTHROPIC}\"]")
    );

    fixture.revoke("route-a", INTERPRET).unwrap();

    // OpenAI left; Anthropic stays for review; the hand-configured Spark entry is untouched.
    assert_eq!(
        fixture.ceiling("route-a"),
        format!("[\"{SPARK}\",\"{ANTHROPIC}\"]")
    );
    assert_eq!(
        fixture.grant_row("route-a", REVIEW),
        Some(format!("[\"{ANTHROPIC}\"]"))
    );
}

#[test]
fn revoking_one_route_leaves_another_route_alone() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.add_route("route-b", "Work", "[]");
    fixture.openai("profile-openai");
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture
        .grant("route-b", INTERPRET, "profile-openai")
        .unwrap();

    fixture.revoke("route-a", INTERPRET).unwrap();

    assert_eq!(fixture.ceiling("route-a"), "[]");
    assert_eq!(fixture.ceiling("route-b"), format!("[\"{OPENAI}\"]"));
    assert!(fixture.grant_row("route-b", INTERPRET).is_some());
}

#[test]
fn revoking_without_a_grant_is_unchanged_and_an_unknown_route_is_refused() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", &format!("[\"{OPENAI}\"]"));
    let before = fixture.snapshot();

    assert_eq!(
        fixture.revoke("route-a", INTERPRET).unwrap(),
        RevokeOutcome::Unchanged
    );
    assert_eq!(
        fixture
            .revoke("route-missing", INTERPRET)
            .unwrap_err()
            .code(),
        "route_unknown"
    );

    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn revoking_a_grant_whose_ceiling_is_unreadable_still_removes_the_grant() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-openai");
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture.exec(
        "UPDATE routes SET processing_destinations = 'not json' WHERE route_id = 'route-a'",
        [],
    );

    assert_eq!(
        fixture.revoke("route-a", INTERPRET).unwrap(),
        RevokeOutcome::Revoked
    );

    assert_eq!(fixture.grant_row("route-a", INTERPRET), None);
    assert_eq!(fixture.ceiling("route-a"), "not json");
}

#[test]
fn a_revoked_profile_cannot_be_granted_even_after_an_earlier_grant() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", "[]");
    fixture.openai("profile-v1");
    fixture.openai("profile-v2");
    fixture.grant("route-a", INTERPRET, "profile-v1").unwrap();
    fixture.revoke_profile("profile-v2");

    let error = fixture.grant("route-a", REVIEW, "profile-v2").unwrap_err();

    assert_eq!(error.code(), "profile_revoked");
    assert_eq!(fixture.grant_row("route-a", REVIEW), None);
}

#[test]
fn the_listing_reports_each_routes_name_scope_capabilities_and_destination_classes() {
    let mut fixture = fresh_install();
    fixture.add_route("route-b", "Work", "[]");
    fixture.add_route("route-a", "Capture", "[]");
    fixture.add_route("route-c", "Local only", "[]");
    fixture.openai("profile-openai");
    fixture.self_hosted("profile-spark");
    fixture
        .grant("route-a", INTERPRET, "profile-openai")
        .unwrap();
    fixture
        .grant("route-a", INTERPRET, "profile-spark")
        .unwrap();
    fixture.grant("route-a", REVIEW, "profile-spark").unwrap();
    fixture.grant("route-b", REVIEW, "profile-openai").unwrap();

    let listing = list_route_policy(fixture.db.conn()).unwrap();

    let summary: Vec<_> = listing
        .iter()
        .map(|route| {
            (
                route.route_id.as_str(),
                route.route_name.as_str(),
                route.scope.as_str(),
                route
                    .granted_capabilities
                    .iter()
                    .map(|granted| (granted.capability, granted.destination_classes.clone()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "route-a",
                "Capture",
                "personal",
                vec![
                    (REVIEW, vec![ListedDestinationClass::PrivateServer]),
                    (
                        INTERPRET,
                        vec![
                            ListedDestinationClass::Cloud,
                            ListedDestinationClass::PrivateServer
                        ]
                    ),
                ]
            ),
            (
                "route-b",
                "Work",
                "personal",
                vec![(REVIEW, vec![ListedDestinationClass::Cloud])]
            ),
            ("route-c", "Local only", "personal", vec![]),
        ]
    );
}

#[test]
fn the_listing_marks_destinations_no_profile_contacts_as_unknown_and_drops_revoked_grants() {
    let mut fixture = fresh_install();
    fixture.add_route("route-a", "Capture", &format!("[\"{OPENAI}\"]"));
    fixture.exec(
        "INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, \
         created_at) VALUES ('auth-1', 'route-a', 'text_interpretation', ?, ?)",
        rusqlite::params![format!("[\"{OPENAI}\"]"), CREATED_AT],
    );

    let listing = list_route_policy(fixture.db.conn()).unwrap();
    assert_eq!(
        listing[0].granted_capabilities[0].destination_classes,
        vec![ListedDestinationClass::Unknown]
    );

    fixture.revoke("route-a", INTERPRET).unwrap();
    let listing = list_route_policy(fixture.db.conn()).unwrap();
    assert!(listing[0].granted_capabilities.is_empty());
}

#[test]
fn capability_keys_round_trip() {
    for capability in [
        ProcessingCapability::TextInterpretation,
        ProcessingCapability::Transcription,
        ProcessingCapability::Review,
    ] {
        assert_eq!(capability_from_key(capability.as_str()), Some(capability));
    }
    assert_eq!(capability_from_key("anything_else"), None);
}
