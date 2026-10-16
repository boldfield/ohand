//! Integration fixtures for the job configuration controls (A07): the explicit re-pin of waiting
//! unpinned work, the requeue and revocation controls it sits beside, and the bounded listing of
//! jobs pinned to a profile. Profiles, defaults and grants are written through the production
//! writers, captures enter through the real foreground import, and assertions read durable state.

use chrono::{DateTime, Duration, Utc};
use ohand_core::ingress::import_foreground_ingress;
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, DispatchOutcome, InterpretationDispatcher,
};
use ohand_core::jobs::configuration::{
    get_jobs_pinned_to_profile, get_requeue_record, repin_waiting_interpretations,
    requeue_job_to_new_profile, revoke_profile, MAX_REPIN_BATCH,
};
use ohand_core::jobs::queue::{claim_job_with_lease, get_job, get_jobs_for_item, Job, JobStatus};
use ohand_core::privacy::routing::{
    authorize_job, grant_route_capability, revoke_route_capability, AuthorizationDecision,
    ProcessingCapability, JOB_TYPE_INTERPRET,
};
use ohand_core::providers::contracts::{CancelToken, ManualClock};
use ohand_core::providers::profiles::{
    clear_default_profile, create_profile_version, set_default_profile, ProfileDraft,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock as StoreClock, Database};
use std::sync::Arc;
use tempfile::TempDir;

const NOW: &str = "2026-10-14T10:30:00Z";
const ROUTE_ID: &str = "route-1";
const OTHER_ROUTE_ID: &str = "route-2";
const FREE_FORM: &str = "call the roofer about the leak";
const CREDENTIAL: &str = "credential-ref/synthetic-primary";
const INTERPRET: ProcessingCapability = ProcessingCapability::TextInterpretation;

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
        for route_id in [ROUTE_ID, OTHER_ROUTE_ID] {
            store
                .open()
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                     VALUES (?, 'private notes', 'personal', '[]', ?)",
                    rusqlite::params![route_id, NOW],
                )
                .unwrap();
        }
        store
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(FixedClock)).unwrap()
    }

    /// Import a typed capture on `route_id` and return its item id.
    fn capture(
        &self,
        number: usize,
        route_id: &str,
        text: Option<&str>,
        audio: Option<&str>,
    ) -> String {
        let capture = Capture::new(
            format!("3f9c1e52-0000-4000-8000-{number:012}"),
            text.map(str::to_string),
            audio.map(str::to_string),
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
        import_foreground_ingress(&mut self.open(), &capture)
            .unwrap()
            .item_id
    }

    /// Capture that no fast path recognizes, so it waits for a provider.
    fn free_form(&self, number: usize, route_id: &str) -> String {
        self.capture(number, route_id, Some(FREE_FORM), None)
    }

    /// Claim and dispatch the next queued job with no provider adapters.
    fn run_next_job(&self) -> (Job, DispatchOutcome) {
        let mut database = self.open();
        let job = claim_job_with_lease(&mut database, Duration::seconds(60), now())
            .unwrap()
            .expect("a job is queued");
        let registry = AdapterRegistry::new();
        let clock = ManualClock::new();
        let outcome = InterpretationDispatcher::new(&registry, &clock)
            .run_job(&mut database, &job, &CancelToken::new(), now())
            .expect("the dispatcher runs the job");
        (job, outcome)
    }

    /// Stop every queued job for configuration, as the dispatcher does without a provider.
    fn settle_waiting_work(&self, jobs: usize) {
        for _ in 0..jobs {
            self.run_next_job();
        }
    }

    fn profile(&self, profile_id: &str, credential_ref: Option<&str>) -> String {
        let draft: ProfileDraft = serde_json::from_value(serde_json::json!({
            "profile_id": profile_id,
            "protocol": "open_ai",
            "model": "synthetic-model",
            "credential_ref": credential_ref,
            "timeout_seconds": 30,
            "capabilities": ["text_interpretation"],
        }))
        .unwrap();
        create_profile_version(&mut self.open(), &draft, now())
            .unwrap()
            .profile_version
    }

    fn make_default(&self, profile_version: &str) {
        set_default_profile(&mut self.open(), INTERPRET, profile_version, now()).unwrap();
    }

    fn grant(&self, route_id: &str, profile_version: &str) {
        grant_route_capability(&mut self.open(), route_id, INTERPRET, profile_version, NOW)
            .unwrap();
    }

    /// A configured, granted default: the setup a user completes after capturing offline.
    fn configure(&self, profile_id: &str, route_id: &str) -> String {
        let profile_version = self.profile(profile_id, Some(CREDENTIAL));
        self.make_default(&profile_version);
        self.grant(route_id, &profile_version);
        profile_version
    }

    fn repin(&self, route_id: &str) -> usize {
        repin_waiting_interpretations(&mut self.open(), route_id, now()).unwrap()
    }

    fn jobs(&self, item_id: &str) -> Vec<Job> {
        get_jobs_for_item(&self.open(), item_id)
            .unwrap()
            .into_iter()
            .filter(|job| job.job_type == JOB_TYPE_INTERPRET)
            .collect()
    }

    fn job(&self, job_id: &str) -> Job {
        get_job(&self.open(), job_id).unwrap().unwrap()
    }

    fn processing_state(&self, item_id: &str) -> String {
        self.open()
            .conn()
            .query_row(
                "SELECT processing_state FROM items WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn job_row_count(&self) -> i64 {
        self.open()
            .conn()
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .unwrap()
    }

    fn snapshot_of_jobs(&self) -> Vec<String> {
        let database = self.open();
        let mut statement = database
            .conn()
            .prepare(
                "SELECT job_id || '|' || status || '|' || COALESCE(profile_version, '') || '|' \
                 || COALESCE(failure_reason, '') FROM jobs ORDER BY job_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    fn is_authorized(&self, job_id: &str) -> bool {
        matches!(
            authorize_job(self.open().conn(), job_id).unwrap(),
            AuthorizationDecision::Authorized(_)
        )
    }
}

#[test]
fn a_capture_made_before_any_provider_moves_only_when_the_user_repins_after_configuring() {
    let store = Store::new();
    let item_id = store.free_form(1, ROUTE_ID);
    store.settle_waiting_work(1);
    assert_eq!(store.processing_state(&item_id), "unprocessed");
    let waiting = store.jobs(&item_id);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].status, JobStatus::Failed);
    assert_eq!(waiting[0].profile_version, None);

    assert_eq!(store.repin(ROUTE_ID), 0, "nothing is configured yet");

    let profile_version = store.configure("primary", ROUTE_ID);
    assert_eq!(
        store.jobs(&item_id).len(),
        1,
        "configuring never moves waiting work by itself"
    );

    assert_eq!(store.repin(ROUTE_ID), 1);

    let jobs = store.jobs(&item_id);
    assert_eq!(jobs.len(), 2);
    let moved = jobs
        .iter()
        .find(|job| job.profile_version.as_deref() == Some(profile_version.as_str()))
        .expect("a job pinned to the resolved profile");
    assert_eq!(moved.status, JobStatus::Queued);
    assert_eq!(moved.source_revision, 0);
    assert!(store.is_authorized(&moved.job_id));
    assert_eq!(
        store.job(&waiting[0].job_id).status,
        JobStatus::Failed,
        "the waiting job is not rewritten"
    );
    let record = get_requeue_record(store.open().conn(), &waiting[0].job_id)
        .unwrap()
        .expect("the waiting job is linked to its replacement");
    assert_eq!(record.new_job_id, moved.job_id);
    assert_eq!(record.from_profile_version, None);
    assert_eq!(record.to_profile_version, profile_version);
}

#[test]
fn repinning_twice_moves_nothing_the_second_time() {
    let store = Store::new();
    store.free_form(1, ROUTE_ID);
    store.free_form(2, ROUTE_ID);
    store.settle_waiting_work(2);
    store.configure("primary", ROUTE_ID);

    assert_eq!(store.repin(ROUTE_ID), 2);
    let after_first = store.snapshot_of_jobs();
    assert_eq!(store.repin(ROUTE_ID), 0);
    assert_eq!(store.snapshot_of_jobs(), after_first);

    store.settle_waiting_work(2);
    assert_eq!(
        store.repin(ROUTE_ID),
        0,
        "work that already ran on the target profile is not queued again"
    );
}

#[test]
fn a_route_without_a_grant_moves_nothing() {
    let store = Store::new();
    let item_id = store.free_form(1, ROUTE_ID);
    store.settle_waiting_work(1);
    let profile_version = store.profile("primary", Some(CREDENTIAL));
    store.make_default(&profile_version);
    let before = store.snapshot_of_jobs();

    assert_eq!(store.repin(ROUTE_ID), 0);
    assert_eq!(store.snapshot_of_jobs(), before);
    assert_eq!(store.processing_state(&item_id), "unprocessed");

    store.grant(OTHER_ROUTE_ID, &profile_version);
    assert_eq!(
        store.repin(ROUTE_ID),
        0,
        "a grant on another route does not authorize this one"
    );

    store.grant(ROUTE_ID, &profile_version);
    assert_eq!(store.repin(ROUTE_ID), 1);
}

#[test]
fn a_revoked_grant_or_a_revoked_or_cleared_default_moves_nothing() {
    let store = Store::new();
    store.free_form(1, ROUTE_ID);
    store.settle_waiting_work(1);
    let profile_version = store.configure("primary", ROUTE_ID);
    let before = store.snapshot_of_jobs();

    revoke_route_capability(&mut store.open(), ROUTE_ID, INTERPRET).unwrap();
    assert_eq!(store.repin(ROUTE_ID), 0);
    store.grant(ROUTE_ID, &profile_version);

    clear_default_profile(&mut store.open(), INTERPRET).unwrap();
    assert_eq!(store.repin(ROUTE_ID), 0);
    store.make_default(&profile_version);

    revoke_profile(&mut store.open(), &profile_version, now()).unwrap();
    assert_eq!(
        store.repin(ROUTE_ID),
        0,
        "a revoked default is never a target"
    );

    assert_eq!(store.snapshot_of_jobs(), before);
}

#[test]
fn the_move_goes_only_to_the_profile_the_resolver_selects_now() {
    let store = Store::new();
    let item_id = store.free_form(1, ROUTE_ID);
    store.settle_waiting_work(1);
    let first = store.configure("first", ROUTE_ID);
    let second = store.configure("second", ROUTE_ID);
    assert_ne!(first, second);

    assert_eq!(store.repin(ROUTE_ID), 1);

    let pinned: Vec<Option<String>> = store
        .jobs(&item_id)
        .into_iter()
        .filter(|job| job.status == JobStatus::Queued)
        .map(|job| job.profile_version)
        .collect();
    assert_eq!(pinned, vec![Some(second)]);
}

#[test]
fn a_job_pinned_to_another_profile_is_never_changed() {
    let store = Store::new();
    let first = store.configure("first", ROUTE_ID);
    let item_id = store.free_form(1, ROUTE_ID);
    let pinned = store.jobs(&item_id);
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].profile_version.as_deref(), Some(first.as_str()));
    store.configure("second", ROUTE_ID);
    let before = store.snapshot_of_jobs();

    assert_eq!(
        store.repin(ROUTE_ID),
        0,
        "an item with a live pinned job is not moved"
    );
    assert_eq!(store.snapshot_of_jobs(), before);
    assert_eq!(store.job(&pinned[0].job_id).status, JobStatus::Queued);

    store.settle_waiting_work(1);
    let after_run = store.snapshot_of_jobs();
    let finished = store.job(&pinned[0].job_id);
    assert_eq!(finished.profile_version.as_deref(), Some(first.as_str()));

    let moved = store.repin(ROUTE_ID);
    let current = store.job(&pinned[0].job_id);
    assert_eq!(
        (current.status, current.profile_version.clone()),
        (finished.status, finished.profile_version.clone()),
        "the earlier job keeps its pin and its state"
    );
    for row in after_run {
        assert!(
            store.snapshot_of_jobs().contains(&row),
            "{row} was changed by the re-pin ({moved} moved)"
        );
    }
}

#[test]
fn only_the_requested_routes_items_that_have_text_move() {
    let store = Store::new();
    let mine = store.free_form(1, ROUTE_ID);
    let theirs = store.free_form(2, OTHER_ROUTE_ID);
    let voice = store.capture(3, ROUTE_ID, None, Some("staging/synthetic-voice.m4a"));
    store.settle_waiting_work(2);
    store.configure("primary", ROUTE_ID);
    let their_jobs = store.jobs(&theirs);

    assert_eq!(store.repin(ROUTE_ID), 1);

    assert_eq!(store.jobs(&mine).len(), 2);
    assert_eq!(store.jobs(&theirs).len(), their_jobs.len());
    assert!(
        store.jobs(&voice).is_empty(),
        "an item with no text to interpret is skipped"
    );
}

#[test]
fn one_call_moves_a_bounded_batch_and_the_next_call_continues() {
    let store = Store::new();
    let total = MAX_REPIN_BATCH + 3;
    for number in 1..=total {
        store.free_form(number, ROUTE_ID);
    }
    store.settle_waiting_work(total);
    store.configure("primary", ROUTE_ID);

    assert_eq!(store.repin(ROUTE_ID), MAX_REPIN_BATCH);
    assert_eq!(
        store.repin(ROUTE_ID),
        3,
        "the remaining work moves on the next call"
    );
    assert_eq!(store.repin(ROUTE_ID), 0);
    assert_eq!(store.job_row_count(), (total * 2) as i64);
}

#[test]
fn a_route_with_no_items_moves_nothing() {
    let store = Store::new();
    store.configure("primary", ROUTE_ID);
    assert_eq!(store.repin(ROUTE_ID), 0);
    assert_eq!(store.repin("route-that-does-not-exist"), 0);
    assert_eq!(store.job_row_count(), 0);
}

#[test]
fn requeueing_a_pinned_job_moves_it_to_another_profile_and_records_the_link() {
    let store = Store::new();
    let first = store.configure("first", ROUTE_ID);
    let second = store.configure("second", ROUTE_ID);
    store.make_default(&first);
    let item_id = store.free_form(1, ROUTE_ID);
    let old = store.jobs(&item_id).remove(0);
    assert_eq!(old.profile_version.as_deref(), Some(first.as_str()));

    let replacement = requeue_job_to_new_profile(
        &mut store.open(),
        &old.job_id,
        "replacement-1".to_string(),
        second.clone(),
        now(),
    )
    .unwrap();

    assert_eq!(
        replacement.profile_version.as_deref(),
        Some(second.as_str())
    );
    assert_eq!(store.job(&old.job_id).status, JobStatus::Cancelled);
    let record = get_requeue_record(store.open().conn(), &old.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.new_job_id, "replacement-1");
    assert_eq!(record.from_profile_version.as_deref(), Some(first.as_str()));
    assert_eq!(
        get_requeue_record(store.open().conn(), "replacement-1")
            .unwrap()
            .unwrap(),
        record,
        "the record is found from either side"
    );
    assert!(get_requeue_record(store.open().conn(), "no-such-job")
        .unwrap()
        .is_none());

    let retried = requeue_job_to_new_profile(
        &mut store.open(),
        &old.job_id,
        "replacement-1".to_string(),
        second.clone(),
        now(),
    )
    .unwrap();
    assert_eq!(retried.job_id, replacement.job_id);
    assert_eq!(store.jobs(&item_id).len(), 2);
}

#[test]
fn requeueing_to_a_revoked_or_identical_profile_is_refused_and_changes_nothing() {
    let store = Store::new();
    let first = store.configure("first", ROUTE_ID);
    let second = store.profile("second", Some("credential-ref/synthetic-second"));
    let item_id = store.free_form(1, ROUTE_ID);
    let old = store.jobs(&item_id).remove(0);
    revoke_profile(&mut store.open(), &second, now()).unwrap();
    let before = store.snapshot_of_jobs();

    for target in [second.clone(), first.clone()] {
        assert!(requeue_job_to_new_profile(
            &mut store.open(),
            &old.job_id,
            "replacement-1".to_string(),
            target,
            now(),
        )
        .is_err());
    }
    assert_eq!(store.snapshot_of_jobs(), before);
}

#[test]
fn the_pinned_job_listing_names_only_jobs_of_that_profile() {
    let store = Store::new();
    let first = store.configure("first", ROUTE_ID);
    let item_id = store.free_form(1, ROUTE_ID);
    let second = store.configure("second", ROUTE_ID);
    let other_item = store.free_form(2, ROUTE_ID);

    let on_first = get_jobs_pinned_to_profile(store.open().conn(), &first).unwrap();
    let on_second = get_jobs_pinned_to_profile(store.open().conn(), &second).unwrap();
    assert_eq!(on_first, vec![store.jobs(&item_id)[0].job_id.clone()]);
    assert_eq!(on_second, vec![store.jobs(&other_item)[0].job_id.clone()]);
    assert!(
        get_jobs_pinned_to_profile(store.open().conn(), "no-such-profile")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn revoking_a_profile_retires_its_jobs_even_while_a_reset_claims_its_credential() {
    let store = Store::new();
    let profile_version = store.configure("primary", ROUTE_ID);
    let item_id = store.free_form(1, ROUTE_ID);
    {
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
    }

    revoke_profile(&mut store.open(), &profile_version, now()).unwrap();

    let job = &store.jobs(&item_id)[0];
    assert_eq!(job.status, JobStatus::Cancelled);
    let database = store.open();
    let (credential_ref, revoked_at): (Option<String>, Option<String>) = database
        .conn()
        .query_row(
            "SELECT credential_ref, revoked_at FROM provider_profiles WHERE profile_version = ?",
            [&profile_version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(credential_ref.as_deref(), Some(CREDENTIAL));
    assert!(revoked_at.is_some());
    let claims: i64 = database
        .conn()
        .query_row("SELECT COUNT(*) FROM reset_credential_claims", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(claims, 1, "revoking leaves the claim exactly as it was");

    let first_revoked_at = revoked_at;
    revoke_profile(
        &mut store.open(),
        &profile_version,
        now() + Duration::hours(1),
    )
    .unwrap();
    let again: Option<String> = store
        .open()
        .conn()
        .query_row(
            "SELECT revoked_at FROM provider_profiles WHERE profile_version = ?",
            [&profile_version],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        again, first_revoked_at,
        "revoking again keeps the first stamp"
    );
    assert!(revoke_profile(&mut store.open(), "no-such-profile", now()).is_err());
}

#[test]
fn a_job_fenced_for_an_unsupported_version_is_left_untouched_by_a_repin() {
    let store = Store::new();
    let fenced_item = store.free_form(1, ROUTE_ID);
    let waiting_item = store.free_form(2, ROUTE_ID);
    store.settle_waiting_work(2);
    store
        .open()
        .conn()
        .execute(
            "UPDATE jobs SET failure_reason = 'unsupported_job_version' WHERE item_id = ?",
            [&fenced_item],
        )
        .unwrap();
    store.configure("primary", ROUTE_ID);
    let fenced_before = store.jobs(&fenced_item);
    assert_eq!(fenced_before.len(), 1);

    assert_eq!(store.repin(ROUTE_ID), 1, "only the ordinary waiting item");

    assert_eq!(
        store.jobs(&fenced_item).len(),
        1,
        "no job is queued for the fenced item"
    );
    let fenced_after = store.job(&fenced_before[0].job_id);
    assert_eq!(fenced_after.status, JobStatus::Failed);
    assert_eq!(
        fenced_after.failure_reason.as_deref(),
        Some("unsupported_job_version")
    );
    assert_eq!(store.processing_state(&fenced_item), "unprocessed");
    assert_eq!(store.jobs(&waiting_item).len(), 2);
    assert!(
        get_requeue_record(store.open().conn(), &fenced_before[0].job_id)
            .unwrap()
            .is_none(),
        "the fenced job is not linked to a replacement"
    );
    assert_eq!(store.repin(ROUTE_ID), 0);
}
