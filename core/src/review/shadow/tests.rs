use chrono::{DateTime, Duration, Utc};
use rusqlite::types::Value;
use std::sync::Arc;
use uuid::Uuid;

use super::*;
use crate::domain::items::verify_state_integrity;
use crate::interpretation::apply::{
    apply_interpretation_proposal, record_interpretation_failure, ApplyError,
};
use crate::interpretation::contracts::{Proposal, TextBasis};
use crate::jobs::configuration::revoke_profile;
use crate::jobs::queue::{claim_job_with_lease, complete_job, enqueue_job, get_job, JobStatus};
use crate::privacy::routing::{DenialReason, JOB_TYPE_INTERPRET, JOB_TYPE_SHADOW_REVIEW};
use crate::providers::contracts::{FailureKind, ProviderFailure};
use crate::store::schema::{Clock, Database};

const ROUTE: &str = "route-1";
const OPENAI: &str = "https://api.openai.com";
const REVIEWER: &str = "https://reviewer.example.test";
const REVIEW_PROFILE: &str = "review-profile-v1";
const INTERPRET_PROFILE: &str = "interpret-profile-v1";
const STAMP: &str = "2026-01-15T10:30:00Z";
const LEASE_SECONDS: i64 = 60;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        t0()
    }
}

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(STAMP)
        .unwrap()
        .with_timezone(&Utc)
}

fn enabled_policy(max_requests: u32, max_attempts: u32) -> ShadowPolicy {
    ShadowPolicy {
        enabled: true,
        sample_per_mille: SAMPLE_SCALE,
        window_seconds: 3600,
        max_requests_per_window: max_requests,
        max_attempts_per_sample: max_attempts,
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

impl Fixture {
    /// Migrated store with the route, review profile and (when `approved`) the review grant.
    fn new(approved: bool) -> Fixture {
        let path = format!(
            "{}/test_shadow_review_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let db = Database::open(&path, Arc::new(FixedClock)).unwrap();
        let fixture = Fixture { db, path };
        fixture.exec(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
             VALUES (?, 'default', 'personal', ?, ?)",
            rusqlite::params![ROUTE, format!("[\"{OPENAI}\",\"{REVIEWER}\"]"), STAMP],
        );
        fixture.add_profile(INTERPRET_PROFILE, "open_ai", None, OPENAI);
        fixture.add_profile(REVIEW_PROFILE, "self_hosted", Some(REVIEWER), REVIEWER);
        fixture.grant("text_interpretation", OPENAI);
        if approved {
            fixture.grant("review", REVIEWER);
        }
        fixture
    }

    fn exec(&self, sql: &str, params: impl rusqlite::Params) {
        self.db.conn().execute(sql, params).unwrap();
    }

    fn grant(&self, capability: &str, destination: &str) {
        self.exec(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, \
             authorized_destinations, created_at) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                ROUTE,
                capability,
                format!("[\"{destination}\"]"),
                STAMP
            ],
        );
    }

    fn add_profile(
        &self,
        version: &str,
        provider_type: &str,
        endpoint: Option<&str>,
        origin: &str,
    ) {
        self.exec(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) \
             VALUES (?, ?, ?, ?, 'model', 30, '{}', ?, '{}', ?)",
            rusqlite::params![
                version,
                format!("id-{version}"),
                provider_type,
                endpoint,
                format!("[\"{origin}\"]"),
                STAMP
            ],
        );
    }

    fn add_item(&self, text: &str) -> String {
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        self.exec(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
             utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
             VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
            rusqlite::params![capture_id, text, STAMP, ROUTE, STAMP],
        );
        self.exec(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
             'not_applicable', ?, ?)",
            rusqlite::params![item_id, capture_id, STAMP, STAMP],
        );
        item_id
    }

    fn select(
        &mut self,
        item_id: &str,
        request_version: &str,
        policy: &ShadowPolicy,
        now: DateTime<Utc>,
    ) -> ShadowSelection {
        let candidate = ShadowCandidate {
            item_id,
            source_revision: 0,
            request_version,
            review_profile_version: REVIEW_PROFILE,
        };
        select_for_shadow_review(&mut self.db, &candidate, policy, now).unwrap()
    }

    fn select_new(&mut self, policy: &ShadowPolicy, now: DateTime<Utc>) -> ShadowSelection {
        let item_id = self.add_item("review me");
        self.select(&item_id, &Uuid::new_v4().to_string(), policy, now)
    }

    fn select_record(&mut self, policy: &ShadowPolicy, now: DateTime<Utc>) -> ShadowRecord {
        match self.select_new(policy, now) {
            ShadowSelection::Selected(record) => record,
            other => panic!("expected a selected case, got {other:?}"),
        }
    }

    fn shadow_job_count(&self) -> i64 {
        self.db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE job_type = ?",
                [JOB_TYPE_SHADOW_REVIEW],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn claim(&mut self, now: DateTime<Utc>) -> (String, i32) {
        let job = claim_job_with_lease(&mut self.db, Duration::seconds(LEASE_SECONDS), now)
            .unwrap()
            .expect("a job should be claimable");
        (job.job_id, job.attempt_count)
    }

    fn gate(
        &mut self,
        job_id: &str,
        attempt: i32,
        policy: &ShadowPolicy,
    ) -> ShadowDispatchDecision {
        self.gate_at(job_id, attempt, policy, t0())
    }

    fn gate_at(
        &mut self,
        job_id: &str,
        attempt: i32,
        policy: &ShadowPolicy,
        now: DateTime<Utc>,
    ) -> ShadowDispatchDecision {
        authorize_shadow_dispatch(&mut self.db, job_id, attempt, policy, now).unwrap()
    }

    fn record(&self, job_id: &str) -> ShadowRecord {
        load_shadow_record(self.db.conn(), job_id).unwrap().unwrap()
    }

    /// Every row of every table a shadow outcome must never touch, as comparable text.
    fn authoritative_state(&self) -> Vec<String> {
        let mut rows_text = Vec::new();
        for table in [
            "captures",
            "items",
            "corrections",
            "events",
            "proposals",
            "reminders",
            "reminder_operations",
            "reminder_commands",
            "routes",
            "route_authorizations",
            "provider_profiles",
            "suggestion_eligibility",
            "deletion_work",
        ] {
            let mut statement = self
                .db
                .conn()
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                let values: Vec<Value> =
                    (0..columns).map(|index| row.get(index).unwrap()).collect();
                rows_text.push(format!("{table}: {values:?}"));
            }
        }
        rows_text
    }
}

fn request_version() -> String {
    Uuid::new_v4().to_string()
}

#[test]
fn default_policy_dispatches_nothing_and_stores_nothing() {
    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("call the dentist");
    let policy = ShadowPolicy::default();
    assert!(!policy.enabled);

    let selection = fixture.select(&item_id, &request_version(), &policy, t0());
    assert_eq!(selection, ShadowSelection::Skipped(ShadowSkip::Disabled));
    assert_eq!(fixture.shadow_job_count(), 0);

    // Enabling alone is not enough: the default budget is zero.
    let enabled_without_budget = ShadowPolicy {
        enabled: true,
        sample_per_mille: SAMPLE_SCALE,
        ..ShadowPolicy::default()
    };
    let selection = fixture.select(&item_id, &request_version(), &enabled_without_budget, t0());
    assert!(matches!(
        selection,
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { limit: 0, .. })
    ));
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn invalid_policy_and_missing_identity_fail_closed() {
    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("note");
    for broken in [
        ShadowPolicy {
            sample_per_mille: SAMPLE_SCALE + 1,
            ..enabled_policy(5, 1)
        },
        ShadowPolicy {
            window_seconds: 0,
            ..enabled_policy(5, 1)
        },
        ShadowPolicy {
            max_attempts_per_sample: 0,
            ..enabled_policy(5, 1)
        },
    ] {
        assert_eq!(
            fixture.select(&item_id, &request_version(), &broken, t0()),
            ShadowSelection::Skipped(ShadowSkip::InvalidPolicy)
        );
    }
    assert_eq!(
        fixture.select(&item_id, "  ", &enabled_policy(5, 1), t0()),
        ShadowSelection::Skipped(ShadowSkip::InvalidPolicy)
    );
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn unapproved_route_is_denied_and_nothing_is_persisted() {
    // The interpretation grant exists; a review grant does not, and does not follow from it.
    let mut fixture = Fixture::new(false);
    let selection = fixture.select_new(&enabled_policy(5, 1), t0());
    assert_eq!(
        selection,
        ShadowSelection::Skipped(ShadowSkip::NotAuthorized(
            DenialReason::CapabilityNotAuthorized
        ))
    );
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn a_review_grant_for_a_different_destination_is_denied() {
    let mut fixture = Fixture::new(false);
    fixture.grant("review", "https://other.example.test");
    assert_eq!(
        fixture.select_new(&enabled_policy(5, 1), t0()),
        ShadowSelection::Skipped(ShadowSkip::NotAuthorized(
            DenialReason::DestinationNotAuthorizedForCapability
        ))
    );
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn revoked_or_unknown_review_profile_is_denied() {
    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("note");
    revoke_profile(&mut fixture.db, REVIEW_PROFILE, t0()).unwrap();
    assert_eq!(
        fixture.select(&item_id, &request_version(), &enabled_policy(5, 1), t0()),
        ShadowSelection::Skipped(ShadowSkip::NotAuthorized(DenialReason::ProfileRevoked))
    );

    let candidate = ShadowCandidate {
        item_id: &item_id,
        source_revision: 0,
        request_version: &request_version(),
        review_profile_version: "never-registered",
    };
    assert_eq!(
        select_for_shadow_review(&mut fixture.db, &candidate, &enabled_policy(5, 1), t0()).unwrap(),
        ShadowSelection::Skipped(ShadowSkip::NotAuthorized(DenialReason::ProfileUnavailable))
    );
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn deleted_or_revised_items_are_not_selected() {
    let mut fixture = Fixture::new(true);
    let deleted = fixture.add_item("gone");
    fixture.exec(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [&deleted],
    );
    let revised = fixture.add_item("edited");
    fixture.exec(
        "UPDATE items SET revision = 3 WHERE item_id = ?",
        [&revised],
    );

    let policy = enabled_policy(5, 1);
    for item_id in [deleted.as_str(), revised.as_str(), "no-such-item"] {
        assert_eq!(
            fixture.select(item_id, &request_version(), &policy, t0()),
            ShadowSelection::Skipped(ShadowSkip::ItemUnavailable)
        );
    }
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn sampling_is_deterministic_and_proportional() {
    let fixture = Fixture::new(true);
    let none = ShadowPolicy {
        sample_per_mille: 0,
        ..enabled_policy(5, 1)
    };
    let all = enabled_policy(5, 1);
    let half = ShadowPolicy {
        sample_per_mille: 500,
        ..enabled_policy(5, 1)
    };
    let mut sampled_by_half = 0;
    for index in 0..400 {
        let item_id = format!("item-{index}");
        assert!(!none.samples(&item_id, 0, "request", REVIEW_PROFILE));
        assert!(all.samples(&item_id, 0, "request", REVIEW_PROFILE));
        let first = half.samples(&item_id, 0, "request", REVIEW_PROFILE);
        assert_eq!(first, half.samples(&item_id, 0, "request", REVIEW_PROFILE));
        sampled_by_half += usize::from(first);
    }
    assert!(
        (140..=260).contains(&sampled_by_half),
        "expected about half of 400 cases, got {sampled_by_half}"
    );
    drop(fixture);

    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("not sampled");
    assert_eq!(
        fixture.select(&item_id, &request_version(), &none, t0()),
        ShadowSelection::Skipped(ShadowSkip::NotSampled)
    );
    assert_eq!(fixture.shadow_job_count(), 0);
}

#[test]
fn exhausted_budget_dispatches_nothing() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(2, 1);
    let first = fixture.select_record(&policy, t0());
    let second = fixture.select_record(&policy, t0() + Duration::seconds(10));
    assert_eq!(fixture.shadow_job_count(), 2);
    assert_eq!(first.outcome, ShadowOutcome::Pending);
    assert_eq!(second.attempts_used, 0);

    let third = fixture.select_new(&policy, t0() + Duration::seconds(20));
    assert_eq!(
        third,
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted {
            requests_in_use: 2,
            requests_needed: 1,
            limit: 2
        })
    );
    assert_eq!(fixture.shadow_job_count(), 2);
}

/// Dispatch times at which a gate said `Allowed`, checked against every sliding window.
fn assert_never_more_than_limit_in_any_window(sent: &[DateTime<Utc>], policy: &ShadowPolicy) {
    for (index, start) in sent.iter().enumerate() {
        let in_window = sent[index..]
            .iter()
            .filter(|sent_at| **sent_at < *start + Duration::seconds(policy.window_seconds))
            .count();
        assert!(
            in_window <= policy.max_requests_per_window as usize,
            "{in_window} requests inside one window starting at {start}"
        );
    }
}

#[test]
fn an_old_pending_case_keeps_its_reservation_across_the_window() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(2, 1);
    fixture.select_record(&policy, t0());
    fixture.select_record(&policy, t0() + Duration::seconds(10));

    // Both cases are still unsent when their creation time leaves the window: nothing is freed.
    let later = t0() + Duration::seconds(3600 + 5);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, later).unwrap(),
        2
    );
    assert!(matches!(
        fixture.select_new(&policy, later),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));
    assert_eq!(fixture.shadow_job_count(), 2);

    // Sending them settles the reservations, now accounted at their send time.
    let mut sent = Vec::new();
    for _ in 0..2 {
        let (job_id, attempt) = fixture.claim(later);
        assert!(matches!(
            fixture.gate_at(&job_id, attempt, &policy, later),
            ShadowDispatchDecision::Allowed(_)
        ));
        sent.push(later);
        complete_job(&mut fixture.db, &job_id, attempt).unwrap();
    }
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, later).unwrap(),
        2
    );
    assert!(matches!(
        fixture.select_new(&policy, later + Duration::seconds(30)),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));

    // Only once those sends leave the window does the budget renew.
    let renewed = later + Duration::seconds(3600 + 1);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, renewed).unwrap(),
        0
    );
    for _ in 0..2 {
        fixture.select_record(&policy, renewed);
        let (job_id, attempt) = fixture.claim(renewed);
        assert!(matches!(
            fixture.gate_at(&job_id, attempt, &policy, renewed),
            ShadowDispatchDecision::Allowed(_)
        ));
        sent.push(renewed);
        complete_job(&mut fixture.db, &job_id, attempt).unwrap();
    }
    assert!(matches!(
        fixture.select_new(&policy, renewed),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));
    assert_never_more_than_limit_in_any_window(&sent, &policy);
}

#[test]
fn a_retry_after_the_window_is_accounted_to_the_window_it_is_sent_in() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(2, 2);
    let record = fixture.select_record(&policy, t0());
    let transient = ProviderFailure::new(FailureKind::Unavailable);
    let mut sent = Vec::new();

    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, record.job_id);
    assert!(matches!(
        fixture.gate_at(&job_id, attempt, &policy, t0()),
        ShadowDispatchDecision::Allowed(_)
    ));
    sent.push(t0());
    record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &policy, t0()).unwrap();

    // The retry goes out after the first window ended; the case is open, so it still holds 2.
    let retry_at = t0() + Duration::seconds(3600 + 5);
    assert!(matches!(
        fixture.select_new(&policy, retry_at),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));
    let (job_id, attempt) = fixture.claim(retry_at);
    assert_eq!(attempt, 2);
    assert!(matches!(
        fixture.gate_at(&job_id, attempt, &policy, retry_at),
        ShadowDispatchDecision::Allowed(_)
    ));
    sent.push(retry_at);
    let ended = record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &transient,
        &policy,
        retry_at,
    )
    .unwrap();
    assert_eq!(ended.attempts_used, 2);

    // Both of its requests stay charged while the retry is inside the window, so nothing new fits.
    let soon = retry_at + Duration::seconds(60);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, soon).unwrap(),
        2
    );
    assert!(matches!(
        fixture.select_new(&policy, soon),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));

    let renewed = retry_at + Duration::seconds(3600 + 1);
    let next = fixture.select_record(&policy, renewed);
    let (job_id, attempt) = fixture.claim(renewed);
    assert_eq!(job_id, next.job_id);
    assert!(matches!(
        fixture.gate_at(&job_id, attempt, &policy, renewed),
        ShadowDispatchDecision::Allowed(_)
    ));
    sent.push(renewed);
    assert_never_more_than_limit_in_any_window(&sent, &policy);
}

#[test]
fn raising_the_attempt_allowance_after_selection_cannot_overspend_the_window() {
    let mut fixture = Fixture::new(true);
    let reserved_under = enabled_policy(2, 1);
    let raised = enabled_policy(2, 2);
    fixture.select_record(&reserved_under, t0());
    fixture.select_record(&reserved_under, t0() + Duration::seconds(1));
    let transient = ProviderFailure::new(FailureKind::Unavailable);
    let mut sent = Vec::new();

    for offset in [2, 3] {
        let at = t0() + Duration::seconds(offset);
        let (job_id, attempt) = fixture.claim(at);
        assert!(matches!(
            fixture.gate_at(&job_id, attempt, &raised, at),
            ShadowDispatchDecision::Allowed(_)
        ));
        sent.push(at);
        record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &raised, at).unwrap();
    }

    for offset in [100, 101] {
        let at = t0() + Duration::seconds(offset);
        let (job_id, attempt) = fixture.claim(at);
        assert_eq!(attempt, 2);
        match fixture.gate_at(&job_id, attempt, &raised, at) {
            ShadowDispatchDecision::Denied(ShadowDispatchDenial::BudgetExhausted {
                requests_spent,
                limit,
            }) => {
                assert_eq!(limit, 2);
                assert!(requests_spent > limit);
            }
            other => panic!("retry beyond the reserved budget must be denied, got {other:?}"),
        }
        record_shadow_unreviewed(&mut fixture.db, &job_id, attempt, UnreviewedReason::Timeout)
            .unwrap();
    }
    assert_never_more_than_limit_in_any_window(&sent, &raised);
}

#[test]
fn lowering_the_window_limit_after_selection_denies_unsent_reserved_cases() {
    let mut fixture = Fixture::new(true);
    let selected_under = enabled_policy(2, 1);
    let lowered = enabled_policy(1, 1);
    fixture.select_record(&selected_under, t0());
    fixture.select_record(&selected_under, t0());

    let (first, first_attempt) = fixture.claim(t0());
    assert!(matches!(
        fixture.gate(&first, first_attempt, &lowered),
        ShadowDispatchDecision::Allowed(_)
    ));
    let (second, second_attempt) = fixture.claim(t0());
    assert!(matches!(
        fixture.gate(&second, second_attempt, &lowered),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::BudgetExhausted { .. })
    ));
}

#[test]
fn a_denied_gate_stamps_nothing_and_a_replayed_gate_moves_nothing() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 2);
    let record = fixture.select_record(&policy, t0());
    let stamp = |fixture: &Fixture| -> Option<String> {
        fixture
            .db
            .conn()
            .query_row(
                "SELECT next_attempt_at FROM jobs WHERE job_id = ?",
                [&record.job_id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(
        fixture.gate_at(&job_id, attempt, &ShadowPolicy::default(), t0()),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::Disabled)
    );
    assert_eq!(stamp(&fixture), None);

    let later = t0() + Duration::seconds(100);
    assert!(matches!(
        fixture.gate_at(&job_id, attempt, &policy, later),
        ShadowDispatchDecision::Allowed(_)
    ));
    assert_eq!(
        fixture.gate_at(&job_id, attempt, &policy, t0()),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::AlreadyDispatched)
    );
    assert_eq!(stamp(&fixture), Some(later.to_rfc3339()));
}

#[test]
fn a_replayed_gate_under_a_limit_of_one_permits_exactly_one_request() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(1, 1);
    fixture.select_record(&policy, t0());
    let (job_id, attempt) = fixture.claim(t0());
    let mut sent = Vec::new();
    for step in 0..5 {
        let now = t0() + Duration::seconds(step);
        match fixture.gate_at(&job_id, attempt, &policy, now) {
            ShadowDispatchDecision::Allowed(_) => sent.push(now),
            ShadowDispatchDecision::Denied(denial) => {
                assert_eq!(denial, ShadowDispatchDenial::AlreadyDispatched)
            }
        }
    }
    assert_eq!(sent, vec![t0()]);
    assert_never_more_than_limit_in_any_window(&sent, &policy);
    // The marker does not leak into the record of the open case.
    let record = fixture.record(&job_id);
    assert_eq!(record.outcome, ShadowOutcome::Pending);
    assert_eq!(record.last_failure, None);
}

#[test]
fn each_retry_lease_permits_one_request_and_keeps_the_previous_failure_visible() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(2, 2);
    fixture.select_record(&policy, t0());
    let transient = ProviderFailure::new(FailureKind::Unavailable);
    let mut now = t0();
    let mut sent = Vec::new();
    for _ in 0..2 {
        let (job_id, attempt) = fixture.claim(now);
        for _ in 0..3 {
            if let ShadowDispatchDecision::Allowed(_) =
                fixture.gate_at(&job_id, attempt, &policy, now)
            {
                sent.push(now);
            }
        }
        if attempt == 2 {
            assert_eq!(
                fixture.record(&job_id).last_failure.as_deref(),
                Some("unavailable")
            );
        }
        record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &policy, now).unwrap();
        now += Duration::seconds(3600);
    }
    assert_eq!(sent.len(), 2);
    assert_never_more_than_limit_in_any_window(&sent, &policy);
}

#[test]
fn an_expired_lease_is_charged_as_a_further_attempt() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(1, 1);
    fixture.select_record(&policy, t0());
    let (job_id, attempt) = fixture.claim(t0());
    assert!(matches!(
        fixture.gate(&job_id, attempt, &policy),
        ShadowDispatchDecision::Allowed(_)
    ));
    // The worker stalls; its lease expires and the queue hands the case out again.
    let reclaimed_at = t0() + Duration::seconds(LEASE_SECONDS + 1);
    let (reclaimed_id, reclaimed_attempt) = fixture.claim(reclaimed_at);
    assert_eq!(
        (reclaimed_id.as_str(), reclaimed_attempt),
        (job_id.as_str(), attempt + 1)
    );
    assert_eq!(
        fixture.gate_at(&job_id, reclaimed_attempt, &policy, reclaimed_at),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::AttemptsExhausted)
    );
}

#[test]
fn extreme_windows_fail_closed_without_panicking() {
    let mut fixture = Fixture::new(true);
    let valid = enabled_policy(5, 1);
    let record = fixture.select_record(&valid, t0());
    let (job_id, attempt) = fixture.claim(t0());
    for window_seconds in [MAX_WINDOW_SECONDS + 1, i64::MAX / 1000, i64::MAX] {
        let extreme = ShadowPolicy {
            window_seconds,
            ..valid
        };
        assert_eq!(extreme.validate(), Err(PolicyError::WindowTooLong));
        assert_eq!(
            fixture.select_new(&extreme, t0()),
            ShadowSelection::Skipped(ShadowSkip::InvalidPolicy)
        );
        assert_eq!(
            fixture.gate(&job_id, attempt, &extreme),
            ShadowDispatchDecision::Denied(ShadowDispatchDenial::InvalidPolicy)
        );
        // The budget read is public and may see an unvalidated policy: it errors, never panics.
        assert!(requests_in_use(fixture.db.conn(), &extreme, t0()).is_err());
    }
    for window_seconds in [i64::MIN, -1, 0] {
        let extreme = ShadowPolicy {
            window_seconds,
            ..valid
        };
        assert_eq!(extreme.validate(), Err(PolicyError::NonPositiveWindow));
        assert_eq!(
            fixture.gate(&job_id, attempt, &extreme),
            ShadowDispatchDecision::Denied(ShadowDispatchDenial::InvalidPolicy)
        );
        assert!(requests_in_use(fixture.db.conn(), &extreme, t0()).is_err());
    }
    assert_eq!(fixture.shadow_job_count(), 1);

    // The longest allowed window works on both paths.
    let longest = ShadowPolicy {
        window_seconds: MAX_WINDOW_SECONDS,
        ..valid
    };
    assert_eq!(longest.validate(), Ok(()));
    assert!(matches!(
        fixture.gate(&job_id, attempt, &longest),
        ShadowDispatchDecision::Allowed(_)
    ));
    assert_eq!(
        requests_in_use(fixture.db.conn(), &longest, t0()).unwrap(),
        1
    );
    assert!(matches!(
        fixture.select_new(&longest, t0()),
        ShadowSelection::Selected(_)
    ));
    assert_eq!(
        fixture.record(&record.job_id).outcome,
        ShadowOutcome::Pending
    );
}

#[test]
fn offering_the_same_case_again_reserves_nothing_more() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(1, 1);
    let item_id = fixture.add_item("once");
    let request = request_version();
    let ShadowSelection::Selected(record) = fixture.select(&item_id, &request, &policy, t0())
    else {
        panic!("first offer should be selected");
    };
    for _ in 0..3 {
        assert_eq!(
            fixture.select(&item_id, &request, &policy, t0() + Duration::seconds(5)),
            ShadowSelection::AlreadySelected(record.clone())
        );
    }
    assert_eq!(fixture.shadow_job_count(), 1);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
        1
    );
}

#[test]
fn retries_spend_the_reservation_without_adding_to_it() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(3, 3);
    let record = fixture.select_record(&policy, t0());
    // One case reserves its whole allowance up front, so a second case cannot fit.
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
        3
    );
    assert!(matches!(
        fixture.select_new(&policy, t0()),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));

    let mut now = t0();
    let transient = ProviderFailure::new(FailureKind::Unavailable);
    for expected_attempt in 1..=2 {
        let (job_id, attempt) = fixture.claim(now);
        assert_eq!(
            (job_id.as_str(), attempt),
            (record.job_id.as_str(), expected_attempt)
        );
        assert!(matches!(
            fixture.gate_at(&job_id, attempt, &policy, now),
            ShadowDispatchDecision::Allowed(_)
        ));
        let after =
            record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &policy, now)
                .unwrap();
        assert_eq!(after.outcome, ShadowOutcome::Pending);
        assert_eq!(after.last_failure.as_deref(), Some("unavailable"));
        assert_eq!(requests_in_use(fixture.db.conn(), &policy, now).unwrap(), 3);
        now += Duration::seconds(3600 / 4);
    }
    assert_eq!(fixture.shadow_job_count(), 1);

    let (job_id, attempt) = fixture.claim(now);
    assert_eq!(attempt, 3);
    let last =
        record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &policy, now).unwrap();
    assert_eq!(
        last.outcome,
        ShadowOutcome::Error("unavailable".to_string())
    );
    assert_eq!(last.attempts_used, 3);
    assert_eq!(requests_in_use(fixture.db.conn(), &policy, now).unwrap(), 3);
}

#[test]
fn unused_reservation_is_released_when_a_case_ends_early() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 4);
    let record = fixture.select_record(&policy, t0());
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, record.job_id);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
        4
    );
    assert!(matches!(
        fixture.select_new(&policy, t0()),
        ShadowSelection::Skipped(ShadowSkip::BudgetExhausted { .. })
    ));
    complete_job(&mut fixture.db, &job_id, attempt).unwrap();

    assert_eq!(fixture.record(&job_id).outcome, ShadowOutcome::Reviewed);
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
        1
    );
    assert!(matches!(
        fixture.select_new(&policy, t0()),
        ShadowSelection::Selected(_)
    ));
}

#[test]
fn a_job_cancelled_before_dispatch_releases_its_whole_reservation() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(1, 1);
    let record = fixture.select_record(&policy, t0());
    revoke_profile(&mut fixture.db, REVIEW_PROFILE, t0()).unwrap();

    let after = fixture.record(&record.job_id);
    assert_eq!(
        after.outcome,
        ShadowOutcome::Unreviewed(UnreviewedReason::Retired)
    );
    assert_eq!(
        requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
        0
    );
}

#[test]
fn dispatch_gate_rechecks_policy_grant_profile_and_lease_on_every_attempt() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 2);
    let record = fixture.select_record(&policy, t0());

    // Queued, not leased yet.
    assert_eq!(
        fixture.gate(&record.job_id, 1, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::NotLeased)
    );
    let (job_id, attempt) = fixture.claim(t0());
    let allowed = fixture.gate(&job_id, attempt, &policy);
    let ShadowDispatchDecision::Allowed(authorization) = allowed else {
        panic!("a leased, approved case should be allowed");
    };
    assert_eq!(authorization.destinations(), [REVIEWER.to_string()]);
    assert_eq!(authorization.profile_version(), Some(REVIEW_PROFILE));

    assert_eq!(
        fixture.gate(&job_id, attempt + 1, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::NotLeased)
    );
    assert_eq!(
        fixture.gate(&job_id, attempt, &ShadowPolicy::default()),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::Disabled)
    );
    assert_eq!(
        fixture.gate(
            &job_id,
            attempt,
            &ShadowPolicy {
                window_seconds: -1,
                ..policy
            }
        ),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::InvalidPolicy)
    );
    assert_eq!(
        fixture.gate("no-such-job", 1, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::JobNotFound)
    );

    // Lowering the allowance below the attempt in flight stops the request.
    assert_eq!(
        fixture.gate(
            &job_id,
            attempt,
            &ShadowPolicy {
                max_attempts_per_sample: 0,
                ..policy
            }
        ),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::InvalidPolicy)
    );

    // Removing the review grant after selection stops the next request.
    fixture.exec(
        "DELETE FROM route_authorizations WHERE capability = 'review'",
        [],
    );
    assert_eq!(
        fixture.gate(&job_id, attempt, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::NotAuthorized(
            DenialReason::CapabilityNotAuthorized
        ))
    );
}

#[test]
fn dispatch_gate_enforces_the_per_sample_attempt_allowance() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 2);
    fixture.select_record(&policy, t0());
    let transient = ProviderFailure::new(FailureKind::RateLimited);
    let mut now = t0();
    for _ in 0..2 {
        let (job_id, attempt) = fixture.claim(now);
        assert!(matches!(
            fixture.gate_at(&job_id, attempt, &policy, now),
            ShadowDispatchDecision::Allowed(_)
        ));
        if attempt == 1 {
            record_shadow_failure(&mut fixture.db, &job_id, attempt, &transient, &policy, now)
                .unwrap();
            now += Duration::seconds(3600);
        } else {
            // The allowance was lowered after selection; this lease is already over it.
            let tighter = ShadowPolicy {
                max_attempts_per_sample: 1,
                ..policy
            };
            assert_eq!(
                fixture.gate(&job_id, attempt, &tighter),
                ShadowDispatchDecision::Denied(ShadowDispatchDenial::AttemptsExhausted)
            );
        }
    }
}

#[test]
fn dispatch_gate_refuses_interpretation_jobs_and_deleted_items() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 1);
    let item_id = fixture.add_item("interpret me");
    enqueue_job(
        &mut fixture.db,
        "interpret-job".to_string(),
        item_id.clone(),
        JOB_TYPE_INTERPRET.to_string(),
        0,
        Some(INTERPRET_PROFILE.to_string()),
        Some(request_version()),
        1,
        t0(),
    )
    .unwrap();
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, "interpret-job");
    assert_eq!(
        fixture.gate(&job_id, attempt, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::NotAShadowJob)
    );

    let shadow = match fixture.select(&item_id, &request_version(), &policy, t0()) {
        ShadowSelection::Selected(record) => record,
        other => panic!("{other:?}"),
    };
    let (shadow_id, shadow_attempt) = fixture.claim(t0());
    assert_eq!(shadow_id, shadow.job_id);
    fixture.exec(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [&item_id],
    );
    assert_eq!(
        fixture.gate(&shadow_id, shadow_attempt, &policy),
        ShadowDispatchDecision::Denied(ShadowDispatchDenial::ItemUnavailable)
    );
}

#[test]
fn timeout_and_cancellation_record_unreviewed_without_a_retry() {
    for (kind, reason) in [
        (FailureKind::Timeout, UnreviewedReason::Timeout),
        (FailureKind::Cancelled, UnreviewedReason::Cancelled),
    ] {
        let mut fixture = Fixture::new(true);
        let policy = enabled_policy(5, 3);
        let selected = fixture.select_record(&policy, t0());
        let (job_id, attempt) = fixture.claim(t0());
        let failure = ProviderFailure::new(kind);
        let record =
            record_shadow_failure(&mut fixture.db, &job_id, attempt, &failure, &policy, t0())
                .unwrap();
        assert_eq!(record.outcome, ShadowOutcome::Unreviewed(reason));
        assert_eq!(record.job_id, selected.job_id);

        // Terminal: nothing left to claim, and reporting it again is a no-op.
        assert!(claim_job_with_lease(
            &mut fixture.db,
            Duration::seconds(60),
            t0() + Duration::days(1)
        )
        .unwrap()
        .is_none());
        let again =
            record_shadow_failure(&mut fixture.db, &job_id, attempt, &failure, &policy, t0())
                .unwrap();
        assert_eq!(again, record);
        assert_eq!(
            requests_in_use(fixture.db.conn(), &policy, t0()).unwrap(),
            1
        );
    }
}

#[test]
fn permanent_failure_is_a_terminal_error_and_conflicting_reports_are_rejected() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(5, 3);
    fixture.select_record(&policy, t0());
    let (job_id, attempt) = fixture.claim(t0());
    let rejected = ProviderFailure::new(FailureKind::Rejected);
    let record =
        record_shadow_failure(&mut fixture.db, &job_id, attempt, &rejected, &policy, t0()).unwrap();
    assert_eq!(record.outcome, ShadowOutcome::Error("rejected".to_string()));

    let conflicting = ProviderFailure::new(FailureKind::InvalidOutput);
    assert!(record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &conflicting,
        &policy,
        t0()
    )
    .is_err());
    assert!(
        record_shadow_unreviewed(&mut fixture.db, &job_id, attempt, UnreviewedReason::Timeout)
            .is_err()
    );
    assert_eq!(
        fixture.record(&job_id).outcome,
        ShadowOutcome::Error("rejected".to_string())
    );
    assert!(record_shadow_unreviewed(
        &mut fixture.db,
        &job_id,
        attempt + 1,
        UnreviewedReason::Timeout
    )
    .is_err());
    assert!(
        record_shadow_unreviewed(&mut fixture.db, "no-such-job", 1, UnreviewedReason::Timeout)
            .is_err()
    );
}

#[test]
fn outcome_recorders_refuse_non_shadow_jobs() {
    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("interpret me");
    enqueue_job(
        &mut fixture.db,
        "interpret-job".to_string(),
        item_id,
        JOB_TYPE_INTERPRET.to_string(),
        0,
        Some(INTERPRET_PROFILE.to_string()),
        Some(request_version()),
        1,
        t0(),
    )
    .unwrap();
    let (job_id, attempt) = fixture.claim(t0());
    assert!(
        record_shadow_unreviewed(&mut fixture.db, &job_id, attempt, UnreviewedReason::Timeout)
            .is_err()
    );
    let failure = ProviderFailure::new(FailureKind::Unavailable);
    assert!(record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &failure,
        &enabled_policy(5, 3),
        t0()
    )
    .is_err());
    let job = get_job(&fixture.db, &job_id).unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Running);
    assert!(load_shadow_record(fixture.db.conn(), &job_id)
        .unwrap()
        .is_none());
}

#[test]
fn records_carry_provenance() {
    let mut fixture = Fixture::new(true);
    let item_id = fixture.add_item("remember the milk");
    let request = request_version();
    let ShadowSelection::Selected(record) =
        fixture.select(&item_id, &request, &enabled_policy(5, 1), t0())
    else {
        panic!("expected selection");
    };
    assert_eq!(record.item_id, item_id);
    assert_eq!(record.source_revision, 0);
    assert_eq!(record.request_version, request);
    assert_eq!(record.review_profile_version, REVIEW_PROFILE);
    assert_eq!(record.route_id, ROUTE);
    assert_eq!(record.created_at, t0());
    assert_eq!(record.attempts_used, 0);
    assert_eq!(record.outcome, ShadowOutcome::Pending);
    assert_eq!(
        list_shadow_records(fixture.db.conn(), &item_id).unwrap(),
        vec![record]
    );
}

#[test]
fn shadow_lifecycle_cannot_change_authoritative_state() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(10, 2);
    let item_id = fixture.add_item("remind me to call mom tomorrow at 9");
    let before = fixture.authoritative_state();

    // Select, lease, retry, and end one case as unreviewed; end another as an error.
    let ShadowSelection::Selected(first) =
        fixture.select(&item_id, &request_version(), &policy, t0())
    else {
        panic!("expected selection");
    };
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, first.job_id);
    record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &ProviderFailure::new(FailureKind::Unavailable),
        &policy,
        t0(),
    )
    .unwrap();
    let (job_id, attempt) = fixture.claim(t0() + Duration::seconds(3600));
    record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &ProviderFailure::new(FailureKind::Timeout),
        &policy,
        t0(),
    )
    .unwrap();
    let ShadowSelection::Selected(second) =
        fixture.select(&item_id, &request_version(), &policy, t0())
    else {
        panic!("expected selection");
    };
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, second.job_id);
    record_shadow_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &ProviderFailure::new(FailureKind::Rejected),
        &policy,
        t0(),
    )
    .unwrap();

    assert_eq!(fixture.authoritative_state(), before);
    let tx = fixture.db.transaction().unwrap();
    assert!(verify_state_integrity(&tx, &item_id).unwrap().is_none());
}

#[test]
fn the_interpretation_apply_guard_rejects_shadow_jobs() {
    let mut fixture = Fixture::new(true);
    let policy = enabled_policy(10, 2);
    let item_id = fixture.add_item("buy milk");
    let request = request_version();
    let ShadowSelection::Selected(record) = fixture.select(&item_id, &request, &policy, t0())
    else {
        panic!("expected selection");
    };
    let (job_id, attempt) = fixture.claim(t0());
    assert_eq!(job_id, record.job_id);
    let capture_id: String = fixture
        .db
        .conn()
        .query_row(
            "SELECT capture_id FROM items WHERE item_id = ?",
            [&item_id],
            |row| row.get(0),
        )
        .unwrap();
    let before = fixture.authoritative_state();

    let proposal = Proposal::new(
        Uuid::new_v4().to_string(),
        item_id.clone(),
        capture_id,
        0,
        1,
        TextBasis::Original { item_revision: 0 },
        request,
    );
    let applied = apply_interpretation_proposal(&mut fixture.db, &job_id, attempt, &proposal, t0());
    assert!(matches!(
        applied,
        Err(ApplyError::NotAnInterpretationJob { .. })
    ));
    let failed = record_interpretation_failure(
        &mut fixture.db,
        &job_id,
        attempt,
        &ProviderFailure::new(FailureKind::Rejected),
        t0(),
    );
    assert!(matches!(
        failed,
        Err(ApplyError::NotAnInterpretationJob { .. })
    ));

    assert_eq!(fixture.authoritative_state(), before);
    let job = get_job(&fixture.db, &job_id).unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(fixture.record(&job_id).outcome, ShadowOutcome::Pending);
}
