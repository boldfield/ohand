//! Processing retry and deletion-race safety (T02b). Every test drives the real job runner,
//! interpretation dispatcher, provider adapter and apply boundary against a SQLite file whose
//! capture came in through the real foreground ingress import, and asserts on state re-read
//! through a fresh connection. Faults are injected where they happen in production: a provider
//! that times out or answers badly, a policy change, deletion, correction or another worker
//! while a provider call is in flight, and a hard-killed process.

use crate::harness::{open_database_at, synthetic_text_record, ChildProcess};
use crate::processing::*;
use chrono::{Duration, Utc};
use ohand_core::ingress::{import_foreground_ingress, IngressErrorKind};
use ohand_core::interpretation::apply::ApplyError;
use ohand_core::interpretation::dispatch::{
    DispatchError, DispatchOutcome, RetireReason, WaitReason,
};
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::queue::{claim_job_with_lease, Job};
use ohand_core::jobs::runner::{JobResult, Settlement, StopReason};
use ohand_core::lifecycle::delete_intent::{
    deletion_progress, mark_deletion_intent, DeletionProgress,
};
use ohand_core::providers::anthropic::fake::FakeAnthropicStep;
use ohand_core::providers::contracts::{CancelToken, FailureKind};
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn start_clock() -> Arc<SteppedClock> {
    SteppedClock::starting_at(setup_time() + Duration::seconds(1))
}

/// What no processing failure may change: the raw source, its route and scope, the policy that
/// governs where it may be sent, and the absence of any model-written result.
fn assert_source_and_boundaries_intact(before: &Snapshot, after: &Snapshot) {
    assert_eq!(after.capture_text, before.capture_text, "raw source");
    assert_eq!(after.capture_scope, before.capture_scope, "item scope");
    assert_eq!(after.capture_route, before.capture_route, "route");
    assert_eq!(
        after.route_destinations, before.route_destinations,
        "route ceiling"
    );
    assert_eq!(after.grants, before.grants, "capability grants");
    assert_eq!(after.lifecycle_state, before.lifecycle_state);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.item_type, None, "no model-chosen type");
    assert_eq!(after.proposals, 0, "no stored model result");
    assert_eq!(after.reminders, 0, "no reminder from a failed run");
    assert_eq!(after.search_rows, before.search_rows, "still searchable");
}

fn correction_event(
    item_id: &str,
    kind: CorrectionKind,
    old_value: Option<&str>,
    new_value: &str,
) -> Event {
    Event::new(
        Uuid::new_v4().to_string(),
        item_id.to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind,
            old_value: old_value.map(str::to_string),
            new_value: new_value.to_string(),
        }),
        Utc::now().to_rfc3339(),
    )
    .unwrap()
}

// ---- Provider timeout and invalid output ----

#[test]
fn provider_timeouts_spend_the_retry_budget_without_touching_the_source() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let baseline = world.snapshot();
    let providers = Providers::new(vec![too_slow(), too_slow(), too_slow()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);

    for attempt in 1..=2i64 {
        let report = world.drain(&providers, &clock);
        assert_eq!(report.jobs.len(), 1, "attempt {attempt}");
        let row = world.job_row(&job_id);
        assert_eq!(row.status, "queued", "attempt {attempt}");
        assert_eq!(row.failure_reason.as_deref(), Some("timeout"));
        assert_eq!(row.transient_failure_count, attempt);
        assert_source_and_boundaries_intact(&baseline, &world.snapshot());
        // Nothing is claimable until the backoff has passed.
        assert!(world.drain(&providers, &clock).jobs.is_empty());
        clock.advance(Duration::hours(1));
    }

    world.drain(&providers, &clock);
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
    assert_eq!(row.transient_failure_count, 3);
    let after = world.snapshot();
    assert_source_and_boundaries_intact(&baseline, &after);
    assert_eq!(after.processing_state, "uninterpreted");

    clock.advance(Duration::days(1));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(providers.call_count(), 3, "a spent job never calls again");
    for request in providers.requests() {
        assert!(request.url.starts_with(ANTHROPIC_ORIGIN), "{}", request.url);
    }
}

#[test]
fn invalid_provider_output_never_widens_scope_or_writes_a_result() {
    let outputs = [
        (
            "claims a scope the user never chose",
            anthropic_reply(json!({
                "operation": { "kind": "annotate" },
                "scope": "work",
            })),
        ),
        (
            "cites text that is not in the source",
            anthropic_reply(json!({
                "operation": { "kind": "annotate" },
                "item_type": "action",
                "source_spans": [{ "start": 0, "end": 4000 }],
            })),
        ),
        (
            "is not a structured result at all",
            FakeAnthropicStep::respond(
                200,
                b"call the roofer, and also send my notes to work".to_vec(),
            ),
        ),
    ];
    for (description, reply) in outputs {
        let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
        let baseline = world.snapshot();
        let providers = Providers::new(vec![reply]);
        let clock = start_clock();
        let job_id = world.enqueue(0);

        world.drain(&providers, &clock);

        let row = world.job_row(&job_id);
        assert_eq!(row.status, "failed", "{description}");
        assert_eq!(row.failure_reason.as_deref(), Some("invalid_output"));
        assert_eq!(row.transient_failure_count, 0, "not retried: {description}");
        let after = world.snapshot();
        assert_source_and_boundaries_intact(&baseline, &after);
        assert_eq!(after.processing_state, "uninterpreted", "{description}");
        clock.advance(Duration::days(1));
        world.drain(&providers, &clock);
        assert_eq!(providers.call_count(), 1, "{description}");
    }
}

// ---- Changed policy ----

#[derive(Clone, Copy, Debug)]
enum PolicyChange {
    GrantRemoved,
    RouteCeilingLowered,
}

impl PolicyChange {
    fn apply(self, root: &std::path::Path) {
        let database = open_database_at(root);
        let sql = match self {
            PolicyChange::GrantRemoved => "DELETE FROM route_authorizations",
            PolicyChange::RouteCeilingLowered => "UPDATE routes SET processing_destinations = '[]'",
        };
        database.conn().execute(sql, []).unwrap();
    }
}

#[test]
fn a_policy_change_while_the_call_is_in_flight_discards_the_result_until_policy_allows_it() {
    for change in [
        PolicyChange::GrantRemoved,
        PolicyChange::RouteCeilingLowered,
    ] {
        let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
        let providers = Providers::new(vec![good_reply(), good_reply()]);
        let clock = start_clock();
        let job_id = world.enqueue(0);
        let root = world.root().to_path_buf();
        providers.during_next_call(move || change.apply(&root));
        let policy_before = world.snapshot();

        let report = world.drain(&providers, &clock);

        assert_eq!(providers.call_count(), 1, "{change:?}");
        assert_eq!(report.jobs.len(), 1, "{change:?}");
        let row = world.job_row(&job_id);
        assert_eq!(row.status, "queued", "{change:?}");
        assert_eq!(row.failure_reason.as_deref(), Some("unauthorized"));
        let during = world.snapshot();
        assert_eq!(during.proposals, 0, "{change:?}");
        assert_eq!(during.item_type, None, "{change:?}: nothing applied");
        assert_eq!(during.capture_text, policy_before.capture_text);
        assert_eq!(during.capture_scope, policy_before.capture_scope);
        assert_eq!(during.search_rows, 1);

        // While policy still denies, the job never reaches the provider again.
        clock.advance(Duration::hours(2));
        world.drain(&providers, &clock);
        assert_eq!(
            providers.call_count(),
            1,
            "{change:?}: denied sends nothing"
        );
        assert_eq!(world.snapshot().proposals, 0);

        // Once the user grants it again, the retry applies the result exactly once.
        grant_route_at(world.root(), &[ANTHROPIC_ORIGIN]);
        clock.advance(Duration::hours(2));
        world.drain(&providers, &clock);
        clock.advance(Duration::hours(2));
        world.drain(&providers, &clock);
        assert_eq!(providers.call_count(), 2, "{change:?}");
        let applied = world.snapshot();
        assert_eq!(applied.proposals, 1, "{change:?}");
        assert_eq!(applied.item_type.as_deref(), Some("action"));
        assert_eq!(applied.capture_text, policy_before.capture_text);
        assert_eq!(applied.capture_scope, "personal", "model cannot move scope");
        assert_eq!(world.job_row(&job_id).status, "completed");
    }
}

#[test]
fn a_profile_revoked_while_the_call_is_in_flight_drops_the_result_for_good() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let baseline = world.snapshot();
    let providers = Providers::new(vec![good_reply(), good_reply()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);
    let (root, profile_version) = (
        world.root().to_path_buf(),
        world.profile.profile_version().to_string(),
    );
    providers.during_next_call(move || {
        revoke_profile(&mut open_database_at(&root), &profile_version, Utc::now()).unwrap();
    });

    let report = world.drain(&providers, &clock);

    assert_eq!(providers.call_count(), 1);
    assert_eq!(report.jobs.len(), 1);
    assert!(
        matches!(report.jobs[0].result, JobResult::LeaseLost),
        "{:?}",
        report.jobs[0].result
    );
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.failure_reason.as_deref(), Some("profile_revoked"));
    assert_source_and_boundaries_intact(&baseline, &world.snapshot());
    clock.advance(Duration::days(1));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(
        providers.call_count(),
        1,
        "a revoked profile is never called"
    );
}

#[test]
fn a_policy_that_already_changed_sends_nothing_at_all() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let baseline = world.snapshot();
    let providers = Providers::new(vec![good_reply()]);
    let clock = start_clock();
    let denied_job = world.enqueue(0);
    PolicyChange::GrantRemoved.apply(world.root());

    world.drain(&providers, &clock);

    assert_eq!(providers.call_count(), 0);
    let row = world.job_row(&denied_job);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some("unauthorized"));
    assert_eq!(row.transient_failure_count, 0, "waiting is not a failure");
    let after = world.snapshot();
    assert_eq!(after.capture_text, baseline.capture_text);
    assert_eq!(after.proposals, 0);
    assert_eq!(after.item_type, None);

    revoke_profile(
        &mut world.workspace.open_database(),
        world.profile.profile_version(),
        Utc::now(),
    )
    .unwrap();
    clock.advance(Duration::days(1));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(providers.call_count(), 0);
    assert_eq!(world.job_row(&denied_job).status, "cancelled");
}

// ---- Crash, retry and duplicate delivery ----

#[test]
fn transient_failures_then_success_apply_one_result() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(5)));
    let providers = Providers::new(vec![unavailable(), too_slow(), good_reply(), good_reply()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);

    for _ in 0..3 {
        world.drain(&providers, &clock);
        clock.advance(Duration::hours(1));
    }
    let applied = world.snapshot();
    assert_eq!(providers.call_count(), 3);
    assert_eq!(applied.proposals, 1);
    assert_eq!(applied.item_type.as_deref(), Some("action"));
    assert_eq!(applied.capture_text, FREE_FORM_TEXT);
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "completed");
    assert_eq!(row.transient_failure_count, 2);

    for _ in 0..3 {
        world.drain(&providers, &clock);
        clock.advance(Duration::days(1));
    }
    assert_eq!(providers.call_count(), 3, "a finished job is never re-run");
    assert_eq!(world.snapshot(), applied);
}

#[test]
fn a_hard_kill_during_the_provider_call_recovers_once_the_lease_expires() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let baseline = world.snapshot();
    let job_id = world.enqueue(0);

    let child = ChildProcess::spawn("interpret_in_flight", world.root(), 1);
    child.expect_message_starting_with("provider call in flight");
    child.kill();

    let row = world.job_row(&job_id);
    assert_eq!(row.status, "running", "the dead worker still holds the job");
    assert_eq!(row.attempt_count, 1);
    assert_source_and_boundaries_intact(&baseline, &world.snapshot());

    // The lease has not run out: a restarted process must not run the job a second time yet.
    let providers = Providers::new(vec![good_reply(), good_reply()]);
    let clock = SteppedClock::starting_at(Utc::now() + Duration::seconds(5));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(providers.call_count(), 0);
    assert_eq!(world.snapshot().proposals, 0);

    clock.set(Utc::now() + Duration::seconds(LEASE_SECONDS * 2));
    let report = world.drain(&providers, &clock);
    assert_eq!(report.jobs.len(), 1);
    assert_eq!(report.jobs[0].attempt, 2, "the recovery is a new lease");
    assert!(matches!(
        report.jobs[0].result,
        JobResult::Settled(Settlement::Completed)
    ));
    let applied = world.snapshot();
    assert_eq!(applied.proposals, 1);
    assert_eq!(applied.item_type.as_deref(), Some("action"));
    assert_eq!(applied.capture_text, FREE_FORM_TEXT);
    assert_eq!(world.job_row(&job_id).status, "completed");

    clock.advance(Duration::days(1));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(providers.call_count(), 1);
    assert_eq!(world.snapshot(), applied);
}

#[test]
fn a_hard_kill_after_the_result_was_applied_leaves_one_result_and_redelivery_changes_nothing() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let job_id = world.enqueue(0);

    let child = ChildProcess::spawn("interpret_applied", world.root(), 1);
    child.expect_message_starting_with("drained 1 job(s)");
    child.kill();

    let applied = world.snapshot();
    assert_eq!(applied.proposals, 1);
    assert_eq!(applied.item_type.as_deref(), Some("action"));
    assert_eq!(world.job_row(&job_id).status, "completed");

    // The restarted process finds nothing to do, even long after any lease could have expired.
    let providers = Providers::new(vec![good_reply()]);
    let clock = SteppedClock::starting_at(Utc::now() + Duration::days(1));
    assert!(world.drain(&providers, &clock).jobs.is_empty());
    assert_eq!(providers.call_count(), 0);

    // The dead worker's receipt is a duplicate delivery: rejected before any call or mutation.
    let receipt = receipt_of(world.root(), &job_id, 1);
    let result = providers.dispatcher().run_job(
        &mut world.workspace.open_database(),
        &receipt,
        &CancelToken::new(),
        Utc::now() + Duration::days(1),
    );
    assert!(result.is_err(), "{result:?}");
    assert_eq!(providers.call_count(), 0);
    assert_eq!(world.snapshot(), applied);
}

#[test]
fn a_result_arriving_after_another_worker_reclaimed_the_job_is_dropped() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let job_id = world.enqueue(0);
    let providers = Providers::new(vec![good_reply(), good_reply()]);
    let first_claim_at = Utc::now();
    let first = claim_job_with_lease(
        &mut world.workspace.open_database(),
        lease(),
        first_claim_at,
    )
    .unwrap()
    .expect("the job is ready");
    assert_eq!(first.attempt_count, 1);

    // While worker one waits on the provider its lease runs out and worker two takes the job.
    let reclaimed: Arc<Mutex<Option<Job>>> = Arc::new(Mutex::new(None));
    let (root, slot) = (world.root().to_path_buf(), reclaimed.clone());
    let second_claim_at = first_claim_at + Duration::seconds(LEASE_SECONDS + 1);
    providers.during_next_call(move || {
        let job = claim_job_with_lease(&mut open_database_at(&root), lease(), second_claim_at)
            .unwrap()
            .expect("the expired lease is reclaimed");
        *slot.lock().unwrap() = Some(job);
    });

    let late = providers.dispatcher().run_job(
        &mut world.workspace.open_database(),
        &first,
        &CancelToken::new(),
        first_claim_at,
    );

    assert!(
        matches!(
            late,
            Err(DispatchError::Apply(ApplyError::StaleLease { .. }))
        ),
        "{late:?}"
    );
    assert_eq!(providers.call_count(), 1);
    let nothing_applied = world.snapshot();
    assert_eq!(nothing_applied.proposals, 0);
    assert_eq!(nothing_applied.item_type, None);
    assert_eq!(world.job_row(&job_id).status, "running");

    let second = reclaimed
        .lock()
        .unwrap()
        .take()
        .expect("worker two claimed");
    assert_eq!(second.attempt_count, 2);
    let outcome = providers
        .dispatcher()
        .run_job(
            &mut world.workspace.open_database(),
            &second,
            &CancelToken::new(),
            second_claim_at,
        )
        .unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Interpreted { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        world.snapshot().proposals,
        1,
        "exactly one authoritative result"
    );
    assert_eq!(world.job_row(&job_id).status, "completed");

    // Worker one's receipt, presented once more after completion, still changes nothing.
    let replay = providers.dispatcher().run_job(
        &mut world.workspace.open_database(),
        &first,
        &CancelToken::new(),
        second_claim_at,
    );
    assert!(replay.is_err(), "{replay:?}");
    assert_eq!(providers.call_count(), 2);
    assert_eq!(world.snapshot().proposals, 1);
}

// ---- Deletion and correction races ----

#[test]
fn a_deletion_racing_the_provider_result_cannot_resurrect_the_source() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let providers = Providers::new(vec![good_reply(), good_reply()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);
    let (root, item_id) = (world.root().to_path_buf(), world.item_id.clone());
    providers.during_next_call(move || {
        mark_deletion_intent(&mut open_database_at(&root), &item_id, 0, Utc::now()).unwrap();
    });

    let report = world.drain(&providers, &clock);

    assert_eq!(providers.call_count(), 1);
    assert_eq!(report.jobs.len(), 1);
    assert!(
        matches!(report.jobs[0].result, JobResult::LeaseLost),
        "{:?}",
        report.jobs[0].result
    );
    assert_deleted_and_not_resurrected(&world);
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "cancelled");
    assert_ne!(row.status, "completed");

    // Nothing later brings it back: no further run, no re-import of the same capture.
    clock.advance(Duration::days(1));
    let later = world.drain(&providers, &clock);
    assert!(later.jobs.is_empty());
    assert_eq!(later.stop, StopReason::Idle);
    assert_eq!(providers.call_count(), 1);
    let reimport = import_foreground_ingress(
        &mut world.workspace.open_database(),
        &synthetic_text_record(0).core_capture(),
    );
    assert!(
        matches!(
            reimport.as_ref().map_err(|error| &error.kind),
            Err(IngressErrorKind::ItemDeleted { .. })
        ),
        "{reimport:?}"
    );
    assert_deleted_and_not_resurrected(&world);
    let progress = deletion_progress(&mut world.workspace.open_database(), &world.item_id).unwrap();
    assert!(
        matches!(&progress, DeletionProgress::InProgress { pending, failed }
            if pending.len() == 2 && failed.is_empty()),
        "the deletion intent and its cleanup stay recorded: {progress:?}"
    );
}

#[test]
fn a_deletion_before_the_job_runs_leaves_nothing_to_send() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let providers = Providers::new(vec![good_reply()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);
    mark_deletion_intent(
        &mut world.workspace.open_database(),
        &world.item_id,
        0,
        Utc::now(),
    )
    .unwrap();

    let report = world.drain(&providers, &clock);

    assert!(report.jobs.is_empty());
    assert_eq!(providers.call_count(), 0, "deleted content is never sent");
    assert_eq!(world.job_row(&job_id).status, "cancelled");
    assert_deleted_and_not_resurrected(&world);
}

fn assert_deleted_and_not_resurrected(world: &ProcessingWorld) {
    let after = world.snapshot();
    assert_eq!(after.lifecycle_state, "deleted");
    assert_eq!(after.capture_text, "", "no readable source returns");
    assert_eq!(after.proposals, 0, "no model result is stored");
    assert_eq!(after.item_type, None);
    assert_eq!(after.reminders, 0);
    assert_eq!(after.search_rows, 0, "not searchable");
    assert_eq!(after.corrections, 0);
}

#[test]
fn an_explicit_text_correction_racing_the_provider_result_is_never_overwritten() {
    let world = ProcessingWorld::with_text("cal the rufer", anthropic_profile(retry_policy(3)));
    let corrected = "call the roofer today";
    let providers = Providers::new(vec![
        anthropic_reply(annotate_action("cal the rufer", "cal the rufer")),
        anthropic_reply(annotate_action(corrected, "call the roofer")),
    ]);
    let clock = start_clock();
    let stale_job = world.enqueue(0);
    let (root, item_id) = (world.root().to_path_buf(), world.item_id.clone());
    providers.during_next_call(move || {
        let event = correction_event(
            &item_id,
            CorrectionKind::Text,
            Some("cal the rufer"),
            corrected,
        );
        save_event(&mut open_database_at(&root), &event, 0).unwrap();
    });

    world.drain(&providers, &clock);

    let after_race = world.snapshot();
    assert_eq!(providers.call_count(), 1);
    assert_eq!(after_race.revision, 1, "the correction is authoritative");
    assert_eq!(after_race.corrections, 1);
    assert_eq!(after_race.proposals, 0, "the stale result was not stored");
    assert_eq!(after_race.item_type, None);
    assert_eq!(
        after_race.capture_text, "cal the rufer",
        "raw source is preserved"
    );
    let row = world.job_row(&stale_job);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.failure_reason.as_deref(), Some("source_revised"));

    // The corrected text is what gets interpreted next, and what the provider sees.
    let fresh_job = world.enqueue(1);
    clock.advance(Duration::hours(1));
    world.drain(&providers, &clock);
    let applied = world.snapshot();
    assert_eq!(providers.call_count(), 2);
    let second_body = request_body(&providers.requests()[1]);
    assert!(second_body.contains(corrected));
    assert!(!second_body.contains("cal the rufer"));
    assert_eq!(world.job_row(&fresh_job).status, "completed");
    assert_eq!(applied.proposals, 1);
    assert_eq!(applied.item_type.as_deref(), Some("action"));
    assert_eq!(
        applied.corrections, 1,
        "the correction survives reprocessing"
    );
    assert_eq!(applied.capture_text, "cal the rufer");
    assert_eq!(applied.capture_scope, "personal");
}

#[test]
fn an_explicit_type_correction_racing_the_provider_result_keeps_the_users_type() {
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let providers = Providers::new(vec![good_reply(), good_reply()]);
    let clock = start_clock();
    let job_id = world.enqueue(0);
    let (root, item_id) = (world.root().to_path_buf(), world.item_id.clone());
    providers.during_next_call(move || {
        let event = correction_event(&item_id, CorrectionKind::Type, None, "idea");
        save_event(&mut open_database_at(&root), &event, 0).unwrap();
    });

    world.drain(&providers, &clock);

    let after = world.snapshot();
    assert_eq!(
        after.item_type.as_deref(),
        Some("idea"),
        "the user's type wins"
    );
    assert_eq!(after.proposals, 0, "the provider's 'action' was not stored");
    assert_eq!(after.corrections, 1);
    assert_eq!(after.capture_text, FREE_FORM_TEXT);
    assert_eq!(after.capture_scope, "personal");
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.failure_reason.as_deref(), Some("source_revised"));

    clock.advance(Duration::days(1));
    world.drain(&providers, &clock);
    assert_eq!(providers.call_count(), 1);
    assert_eq!(world.snapshot(), after);
}

#[test]
fn the_dispatcher_reports_why_it_waited_or_retired_in_the_races_above() {
    // Pins the dispatcher-level outcomes behind the runner-level assertions, so a change in how a
    // race is classified is visible here and not only as a changed job row.
    let world = ProcessingWorld::new(anthropic_profile(retry_policy(3)));
    let providers = Providers::new(vec![too_slow(), good_reply()]);
    let first_claim_at = Utc::now();
    world.enqueue(0);
    let job = claim_job_with_lease(
        &mut world.workspace.open_database(),
        lease(),
        first_claim_at,
    )
    .unwrap()
    .unwrap();

    let outcome = providers
        .dispatcher()
        .run_job(
            &mut world.workspace.open_database(),
            &job,
            &CancelToken::new(),
            first_claim_at,
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            DispatchOutcome::BackedOff {
                reason: WaitReason::Transient(FailureKind::Timeout),
                ..
            }
        ),
        "{outcome:?}"
    );

    let (root, item_id) = (world.root().to_path_buf(), world.item_id.clone());
    providers.during_next_call(move || {
        let event = correction_event(&item_id, CorrectionKind::Type, None, "idea");
        save_event(&mut open_database_at(&root), &event, 0).unwrap();
    });
    let retry_at = first_claim_at + Duration::hours(1);
    let retry = claim_job_with_lease(&mut world.workspace.open_database(), lease(), retry_at)
        .unwrap()
        .unwrap();
    let outcome = providers
        .dispatcher()
        .run_job(
            &mut world.workspace.open_database(),
            &retry,
            &CancelToken::new(),
            retry_at,
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            DispatchOutcome::Retired(RetireReason::SourceRevised)
        ),
        "{outcome:?}"
    );
}
