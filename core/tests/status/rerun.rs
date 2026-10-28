//! `rerun_state`: a "Try again" waiting or running is read from durable state alone (F56).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tempfile::TempDir;

use ohand_core::domain::status::ItemStatus;
use ohand_core::jobs::configuration::{
    list_failed_interpretations, rerun_failed_interpretations, RerunOutcome, RerunSelection,
};
use ohand_core::jobs::queue::{
    claim_job_with_lease, complete_job, enqueue_job, fail_job_with_backoff, Job,
};
use ohand_core::privacy::routing::{grant_route_capability, ProcessingCapability};
use ohand_core::providers::profiles::set_default_profile;
use ohand_core::store::events::ItemScope;
use ohand_core::store::schema::{Clock, Database};

const PROFILE_VERSION: &str = "profile-uuid-rerun-v1-0000001111";
const ITEM_ID: &str = "item-rerun-1";

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn open(path: &str, now: DateTime<Utc>) -> Result<Database> {
    Database::open(path, Arc::new(FixedClock(now)) as Arc<dyn Clock>)
}

/// A personal-scope note in `processing_state` whose only interpret job has ended as `ended`
/// (`failed` with a transient reason, or `completed`), with a provider allowed for rerun.
fn seeded_database(path: &str, now: DateTime<Utc>, processing_state: &str) -> Result<Database> {
    let mut db = open(path, now)?;
    let tx = db.transaction()?;
    super::insert_test_item_with_status(
        &tx,
        ITEM_ID,
        "synthetic note",
        "saved_local",
        "not_configured",
        processing_state,
        Some("not_applicable"),
    )?;
    tx.execute(
        "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
         VALUES ('route-1', 'test-route', 'personal', '[\"https://api.example.com\"]', \
                 '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) \
         VALUES ('route-1', 'text_interpretation', '[\"https://api.example.com\"]', '2026-01-01T00:00:00Z')",
        [],
    )?;
    tx.execute(
        "INSERT INTO provider_profiles (
            profile_id, profile_version, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at, credential_ref
        ) VALUES ('anthropic-rerun', ?, 'anthropic', 'synthetic-model', 30,
            '{\"max_attempts\":3,\"initial_backoff_ms\":500,\"max_backoff_ms\":30000}',
            '[\"https://api.example.com\"]',
            '{\"text_interpretation\":{\"capability\":\"text_interpretation\",\"support\":\"supported\",\"evidence\":\"V06\",\"input_size_limit\":8000,\"structured_output\":\"none\"}}',
            '2026-01-01T00:00:00Z', 'credential-1')",
        [PROFILE_VERSION],
    )?;
    tx.commit()?;
    set_default_profile(
        &mut db,
        ProcessingCapability::TextInterpretation,
        PROFILE_VERSION,
        now,
    )
    .map_err(|failure| anyhow::anyhow!("{failure:?}"))?;
    grant_route_capability(
        &mut db,
        "route-1",
        ProcessingCapability::TextInterpretation,
        PROFILE_VERSION,
        "2026-01-01T00:00:00Z",
    )
    .map_err(|failure| anyhow::anyhow!("{failure:?}"))?;
    Ok(db)
}

fn first_interpret_job(db: &mut Database, now: DateTime<Utc>) -> Result<Job> {
    enqueue_job(
        db,
        "job-rerun-original".to_string(),
        ITEM_ID.to_string(),
        "interpret".to_string(),
        0,
        Some(PROFILE_VERSION.to_string()),
        None,
        1,
        now,
    )?;
    Ok(claim_job_with_lease(db, Duration::minutes(5), now)?.expect("the original job"))
}

/// The queue's terminal failure: the job ends `failed` with a provider reason, no retry left.
fn fail_job_terminally(db: &Database, job: &Job) -> Result<()> {
    fail_job_with_reason(db, job, "provider_rejected")
}

fn fail_job_with_reason(db: &Database, job: &Job, failure_reason: &str) -> Result<()> {
    let affected = db.conn().execute(
        "UPDATE jobs SET status = 'failed', failure_reason = ?, \
         lease_expires_at = NULL WHERE job_id = ? AND status = 'running'",
        [failure_reason, &job.job_id],
    )?;
    assert_eq!(affected, 1);
    Ok(())
}

/// A new handle on the same file, as a fresh process would have.
fn fresh_status(path: &str, now: DateTime<Utc>) -> Result<ItemStatus> {
    let mut db = open(path, now)?;
    let tx = db.transaction()?;
    let status = ItemStatus::load(&tx, ITEM_ID)?.expect("the item");
    tx.commit()?;
    Ok(status)
}

fn failed_item_ids(path: &str, now: DateTime<Utc>) -> Result<Vec<String>> {
    let mut db = open(path, now)?;
    let page = list_failed_interpretations(&mut db, &[ItemScope::Personal], 10, None)?;
    Ok(page.failed.into_iter().map(|row| row.item_id).collect())
}

fn claim_replacement(path: &str, now: DateTime<Utc>) -> Result<Job> {
    let mut db = open(path, now)?;
    Ok(claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("the replacement job"))
}

#[test]
fn a_failed_note_reports_its_rerun_from_a_fresh_read() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = super::temp_db_path(&tmpdir, "rerun-failed");
    let now = Utc::now();

    // Completing the replacement moves the note on, and the rerun ends.
    {
        let mut db = seeded_database(&path, now, "uninterpreted")?;
        let original = first_interpret_job(&mut db, now)?;
        fail_job_terminally(&db, &original)?;
        drop(db);
        let status = fresh_status(&path, now)?;
        assert_eq!(status.rerun_state, None, "a failed job alone is no rerun");
        assert_eq!(failed_item_ids(&path, now)?, vec![ITEM_ID.to_string()]);

        let mut db = open(&path, now)?;
        let outcome = rerun_failed_interpretations(
            &mut db,
            &[ItemScope::Personal],
            &RerunSelection::AllListed,
            now,
        )?;
        assert_eq!(outcome.moved, 1);
        drop(db);
        assert!(failed_item_ids(&path, now)?.is_empty());
        let status = fresh_status(&path, now)?;
        assert_eq!(status.rerun_state, Some("queued"));
        assert_eq!(status.processing_job_status, None);

        let replacement = claim_replacement(&path, now)?;
        assert_eq!(fresh_status(&path, now)?.rerun_state, Some("running"));

        let mut db = open(&path, now)?;
        db.conn().execute(
            "UPDATE items SET processing_state = 'processed' WHERE item_id = ?",
            [ITEM_ID],
        )?;
        complete_job(&mut db, &replacement.job_id, replacement.attempt_count)?;
        drop(db);
        let status = fresh_status(&path, now)?;
        assert_eq!(status.processing_state.as_str(), "processed");
        assert_eq!(status.rerun_state, None);
    }

    // Failing the replacement ends the rerun and lists the note as failed again.
    let path = super::temp_db_path(&tmpdir, "rerun-failed-again");
    let mut db = seeded_database(&path, now, "uninterpreted")?;
    let original = first_interpret_job(&mut db, now)?;
    fail_job_terminally(&db, &original)?;
    rerun_failed_interpretations(
        &mut db,
        &[ItemScope::Personal],
        &RerunSelection::AllListed,
        now,
    )?;
    drop(db);
    let replacement = claim_replacement(&path, now)?;
    let db = open(&path, now)?;
    fail_job_terminally(&db, &replacement)?;
    drop(db);
    let status = fresh_status(&path, now)?;
    assert_eq!(status.processing_state.as_str(), "uninterpreted");
    assert_eq!(status.rerun_state, None);
    assert_eq!(failed_item_ids(&path, now)?, vec![ITEM_ID.to_string()]);
    Ok(())
}

#[test]
fn an_abstained_note_reports_its_rerun_from_a_fresh_read() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let now = Utc::now();

    let path = super::temp_db_path(&tmpdir, "rerun-abstained");
    let mut db = seeded_database(&path, now, "abstained")?;
    let original = first_interpret_job(&mut db, now)?;
    complete_job(&mut db, &original.job_id, original.attempt_count)?;
    drop(db);
    assert_eq!(fresh_status(&path, now)?.rerun_state, None);

    let mut db = open(&path, now)?;
    let outcome = RerunOutcome::rerun_abstained(&mut db, &[ItemScope::Personal], ITEM_ID, now)?;
    assert_eq!(outcome.as_str(), "queued");
    drop(db);
    let status = fresh_status(&path, now)?;
    assert_eq!(status.processing_state.as_str(), "abstained");
    assert_eq!(status.rerun_state, Some("queued"));
    assert_eq!(status.processing_job_status, None);

    let replacement = claim_replacement(&path, now)?;
    assert_eq!(fresh_status(&path, now)?.rerun_state, Some("running"));

    // Completing it with the note sorted ends the rerun.
    let mut db = open(&path, now)?;
    db.conn().execute(
        "UPDATE items SET processing_state = 'processed' WHERE item_id = ?",
        [ITEM_ID],
    )?;
    complete_job(&mut db, &replacement.job_id, replacement.attempt_count)?;
    drop(db);
    let status = fresh_status(&path, now)?;
    assert_eq!(status.processing_state.as_str(), "processed");
    assert_eq!(status.rerun_state, None);

    // A second abstention: the replacement completes and the note stays abstained, rerun over.
    let path = super::temp_db_path(&tmpdir, "rerun-abstained-again");
    let mut db = seeded_database(&path, now, "abstained")?;
    let original = first_interpret_job(&mut db, now)?;
    complete_job(&mut db, &original.job_id, original.attempt_count)?;
    RerunOutcome::rerun_abstained(&mut db, &[ItemScope::Personal], ITEM_ID, now)?;
    drop(db);
    let replacement = claim_replacement(&path, now)?;
    let mut db = open(&path, now)?;
    complete_job(&mut db, &replacement.job_id, replacement.attempt_count)?;
    drop(db);
    let status = fresh_status(&path, now)?;
    assert_eq!(status.processing_state.as_str(), "abstained");
    assert_eq!(status.rerun_state, None);

    // Failing the replacement ends the rerun too.
    let path = super::temp_db_path(&tmpdir, "rerun-abstained-failed");
    let mut db = seeded_database(&path, now, "abstained")?;
    let original = first_interpret_job(&mut db, now)?;
    complete_job(&mut db, &original.job_id, original.attempt_count)?;
    RerunOutcome::rerun_abstained(&mut db, &[ItemScope::Personal], ITEM_ID, now)?;
    drop(db);
    let replacement = claim_replacement(&path, now)?;
    let db = open(&path, now)?;
    fail_job_terminally(&db, &replacement)?;
    drop(db);
    assert_eq!(fresh_status(&path, now)?.rerun_state, None);
    Ok(())
}

#[test]
fn a_rerun_waiting_to_retry_is_still_a_rerun_in_flight() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let path = super::temp_db_path(&tmpdir, "rerun-retrying");
    let now = Utc::now();
    let mut db = seeded_database(&path, now, "uninterpreted")?;
    let original = first_interpret_job(&mut db, now)?;
    fail_job_terminally(&db, &original)?;
    rerun_failed_interpretations(
        &mut db,
        &[ItemScope::Personal],
        &RerunSelection::AllListed,
        now,
    )?;
    drop(db);
    let replacement = claim_replacement(&path, now)?;
    let mut db = open(&path, now)?;
    fail_job_with_backoff(
        &mut db,
        &replacement.job_id,
        "unavailable".to_string(),
        30,
        600,
        now,
        replacement.attempt_count,
    )?;
    drop(db);
    let status = fresh_status(&path, now)?;
    assert_eq!(status.rerun_state, Some("retrying_after_transient"));
    assert_eq!(status.processing_job_status, None);
    Ok(())
}

#[test]
fn a_rerun_stopped_for_configuration_reports_awaiting_configuration() -> Result<()> {
    let tmpdir = TempDir::new()?;
    let now = Utc::now();

    for (label, state, reason) in [
        ("unauthorized-failed", "uninterpreted", "unauthorized"),
        ("unsupported-abstained", "abstained", "unsupported"),
    ] {
        let path = super::temp_db_path(&tmpdir, label);
        let mut db = seeded_database(&path, now, state)?;
        let original = first_interpret_job(&mut db, now)?;
        if state == "uninterpreted" {
            fail_job_terminally(&db, &original)?;
            rerun_failed_interpretations(
                &mut db,
                &[ItemScope::Personal],
                &RerunSelection::AllListed,
                now,
            )?;
        } else {
            complete_job(&mut db, &original.job_id, original.attempt_count)?;
            RerunOutcome::rerun_abstained(&mut db, &[ItemScope::Personal], ITEM_ID, now)?;
        }
        drop(db);
        let replacement = claim_replacement(&path, now)?;
        let db = open(&path, now)?;
        fail_job_with_reason(&db, &replacement, reason)?;
        drop(db);
        let status = fresh_status(&path, now)?;
        assert_eq!(status.processing_state.as_str(), state);
        assert_eq!(status.rerun_state, Some("awaiting_configuration"));
    }
    Ok(())
}
