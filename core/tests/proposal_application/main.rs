//! Behavior tests for proposal validation and atomic application (I05). Every test runs against a
//! migrated, file-backed SQLite store and asserts on durable state re-read through a fresh
//! connection, not on in-memory return values alone.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::items::ProposalApplicationError;
use ohand_core::domain::status::{
    ItemStatus, ProcessingState, ReminderRequestState, ReminderScheduleState, UnschedulableReason,
};
use ohand_core::interpretation::apply::{
    apply_interpretation_proposal, apply_interpretation_proposal_in_tx,
    record_interpretation_failure, ApplyError, ApplyOutcome, ReminderDisposition,
    INVALID_OUTPUT_REASON,
};
use ohand_core::interpretation::contracts::{
    AbstentionReason, Proposal, ReminderProposal, SessionTopicProposal, SourceSpan, TextBasis,
    TimeResolutionQuality,
};
use ohand_core::jobs::queue::{claim_job_with_lease, enqueue_job};
use ohand_core::privacy::routing::{DenialReason, JOB_TYPE_INTERPRET};
use ohand_core::providers::contracts::{FailureKind, ProviderFailure};
use ohand_core::reminders::state::{get_reminder, list_operations, OperationType};
use ohand_core::retrieval::index::search_source_direct;
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType, ItemScope, ItemType,
};
use ohand_core::store::schema::{Clock, Database};
use ohand_core::suggestions::eligibility::{check_eligibility_with_scope, EligibilityReason};
use ohand_core::time::span_covers_time_expression;
use std::sync::Arc;
use uuid::Uuid;

const CAPTURE_INSTANT: &str = "2026-01-15T10:30:00Z";
const LEASE_SECONDS: i64 = 60;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        capture_instant()
    }
}

fn capture_instant() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(CAPTURE_INSTANT)
        .unwrap()
        .with_timezone(&Utc)
}

struct Fixture {
    db: Database,
    path: String,
    item_id: String,
    capture_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Lease {
    job_id: String,
    attempt: i32,
    request_version: String,
}

fn open(path: &str) -> Database {
    Database::open(path, Arc::new(FixedClock)).unwrap()
}

impl Fixture {
    fn new(capture_text: &str) -> Fixture {
        Fixture::captured(capture_text, CAPTURE_INSTANT, "UTC", 0)
    }

    fn captured(
        capture_text: &str,
        capture_instant: &str,
        timezone_id: &str,
        utc_offset_minutes: i32,
    ) -> Fixture {
        let path = format!(
            "{}/test_proposal_application_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let db = open(&path);
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        db.conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, ?, ?, ?, ?, 'en', 'gregorian', 'personal', 'route-1', 0, ?)",
                rusqlite::params![
                    capture_id,
                    capture_text,
                    capture_instant,
                    timezone_id,
                    utc_offset_minutes,
                    capture_instant
                ],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, CAPTURE_INSTANT, CAPTURE_INSTANT],
            )
            .unwrap();
        Fixture {
            db,
            path,
            item_id,
            capture_id,
        }
    }

    /// Enqueue an on-device interpretation job at `source_revision` and claim it.
    fn claim_job(&mut self, source_revision: i32, now: DateTime<Utc>) -> Lease {
        self.claim_job_pinned(source_revision, None, now)
    }

    fn claim_job_pinned(
        &mut self,
        source_revision: i32,
        profile_version: Option<&str>,
        now: DateTime<Utc>,
    ) -> Lease {
        let job_id = Uuid::new_v4().to_string();
        let request_version = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            source_revision,
            profile_version.map(str::to_string),
            Some(request_version.clone()),
            1,
            now,
        )
        .unwrap();
        let claimed = claim_job_with_lease(&mut self.db, Duration::seconds(LEASE_SECONDS), now)
            .unwrap()
            .expect("job should be claimable");
        assert_eq!(claimed.job_id, job_id);
        Lease {
            job_id,
            attempt: claimed.attempt_count,
            request_version,
        }
    }

    fn proposal_for(&self, lease: &Lease, source_revision: i32) -> Proposal {
        Proposal::new(
            Uuid::new_v4().to_string(),
            self.item_id.clone(),
            self.capture_id.clone(),
            source_revision,
            1,
            TextBasis::Original {
                item_revision: source_revision as u64,
            },
            lease.request_version.clone(),
        )
    }

    fn action_proposal(&self, lease: &Lease, text: &str, evidence: &str) -> Proposal {
        self.proposal_for(lease, 0)
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![span_of(text, evidence)]))
    }

    fn apply(&mut self, lease: &Lease, proposal: &Proposal) -> Result<ApplyOutcome, ApplyError> {
        apply_interpretation_proposal(
            &mut self.db,
            &lease.job_id,
            lease.attempt,
            proposal,
            capture_instant(),
        )
    }

    fn fail(&mut self, lease: &Lease, kind: FailureKind) -> Result<ApplyOutcome, ApplyError> {
        record_interpretation_failure(
            &mut self.db,
            &lease.job_id,
            lease.attempt,
            &ProviderFailure::new(kind),
            capture_instant(),
        )
    }

    /// Durable state read through a brand-new connection to the file.
    fn durable(&self) -> Durable {
        let db = open(&self.path);
        let conn = db.conn();
        let (item_type, processing_state, revision, lifecycle, topic): (
            Option<String>,
            String,
            i32,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT item_type, processing_state, revision, lifecycle_state, current_session_topic \
                 FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        let mut statement = conn
            .prepare(
                "SELECT proposal_id, applied_state, abstained FROM proposals \
                 WHERE item_id = ? ORDER BY rowid",
            )
            .unwrap();
        let proposals = statement
            .query_map([&self.item_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut statement = conn
            .prepare(
                "SELECT job_id, status, failure_reason FROM jobs WHERE item_id = ? ORDER BY rowid",
            )
            .unwrap();
        let jobs = statement
            .query_map([&self.item_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let reminders: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reminders WHERE item_id = ?",
                [&self.item_id],
                |row| row.get(0),
            )
            .unwrap();
        Durable {
            item_type,
            processing_state,
            revision,
            lifecycle,
            topic,
            proposals,
            jobs,
            reminders,
        }
    }

    fn reminder_row(&self) -> Reminder {
        let db = open(&self.path);
        db.conn()
            .query_row(
                "SELECT request_state, schedule_state, resolved_instant, ambiguity_reason, \
                 unsupported_reason, unschedulable_reason, schedule_generation FROM reminders \
                 WHERE item_id = ?",
                [&self.item_id],
                |row| {
                    Ok(Reminder {
                        request_state: row.get(0)?,
                        schedule_state: row.get(1)?,
                        resolved_instant: row.get(2)?,
                        ambiguity_reason: row.get(3)?,
                        unsupported_reason: row.get(4)?,
                        unschedulable_reason: row.get(5)?,
                        schedule_generation: row.get(6)?,
                    })
                },
            )
            .unwrap()
    }

    fn job_status(&self, lease: &Lease) -> (String, Option<String>) {
        open(&self.path)
            .conn()
            .query_row(
                "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
                [&lease.job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn eligibility(&mut self) -> EligibilityReason {
        let tx = self.db.immediate_transaction().unwrap();
        let eligibility =
            check_eligibility_with_scope(&tx, &self.item_id, capture_instant(), None).unwrap();
        tx.commit().unwrap();
        assert!(!eligibility.eligible);
        eligibility.reason
    }

    /// Save a user correction through the public event path; returns the correction event's id,
    /// which is the correction record identity a request carries.
    fn correct(
        &mut self,
        kind: CorrectionKind,
        old: Option<&str>,
        new: &str,
        revision: i32,
    ) -> String {
        let event_id = Uuid::new_v4().to_string();
        let event = Event::new(
            event_id.clone(),
            self.item_id.clone(),
            revision,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind,
                old_value: old.map(str::to_string),
                new_value: new.to_string(),
            }),
            "2026-01-15T11:00:00Z".to_string(),
        )
        .unwrap();
        save_event(&mut self.db, &event, revision).unwrap();
        event_id
    }

    /// Reminder desired state and its operations, read through a fresh connection.
    fn reminder_effects(&self) -> Option<(i64, i64, Vec<OperationType>, Option<String>)> {
        let mut db = open(&self.path);
        let tx = db.immediate_transaction().unwrap();
        let record = get_reminder(&tx, &self.item_id).unwrap()?;
        let operations = list_operations(&tx, &record.reminder_id).unwrap();
        Some((
            record.state_version,
            record.schedule_generation,
            operations
                .iter()
                .map(|operation| operation.operation_type)
                .collect(),
            record.source_phrase,
        ))
    }
}

#[derive(Debug, PartialEq)]
struct Durable {
    item_type: Option<String>,
    processing_state: String,
    revision: i32,
    lifecycle: String,
    topic: Option<String>,
    proposals: Vec<(String, String, bool)>,
    jobs: Vec<(String, String, Option<String>)>,
    reminders: i64,
}

struct Reminder {
    request_state: String,
    schedule_state: String,
    resolved_instant: Option<String>,
    ambiguity_reason: Option<String>,
    unsupported_reason: Option<String>,
    unschedulable_reason: Option<String>,
    schedule_generation: i64,
}

fn span_of(text: &str, phrase: &str) -> SourceSpan {
    let byte_start = text.find(phrase).expect("phrase in text");
    let start = text[..byte_start].chars().count();
    SourceSpan::new(start, start + phrase.chars().count())
}

fn reminder_candidate(
    text: &str,
    phrase: &str,
    instant: Option<&str>,
    quality: TimeResolutionQuality,
) -> ReminderProposal {
    ReminderProposal {
        instant: instant.map(str::to_string),
        timezone_id: Some("UTC".to_string()),
        quality,
        source_span: Some(span_of(text, phrase)),
    }
}

fn applied_reminder(outcome: ApplyOutcome) -> ReminderDisposition {
    match outcome {
        ApplyOutcome::Applied { reminder, .. } => reminder,
        other => panic!("expected an applied proposal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Atomic application and job completion
// ---------------------------------------------------------------------------------------------

#[test]
fn applied_proposal_and_job_completion_commit_together() {
    let text = "call the roofer about the leak";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .action_proposal(&lease, text, "call the roofer")
        .with_session_topic_proposal(Some(SessionTopicProposal {
            topic: " home repairs ".to_string(),
            source_span: Some(span_of(text, "leak")),
        }));

    let outcome = fixture.apply(&lease, &proposal).unwrap();

    let ApplyOutcome::Applied { item, reminder } = outcome else {
        panic!("expected Applied");
    };
    assert_eq!(item.item_type, Some(ItemType::Action));
    assert_eq!(reminder, ReminderDisposition::NotRecorded);
    let durable = fixture.durable();
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(durable.topic.as_deref(), Some("home repairs"));
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(
        durable.revision, 0,
        "derived output never bumps the revision"
    );
    assert_eq!(
        durable.proposals,
        vec![(proposal.proposal_id.clone(), "applied".into(), false)]
    );
    assert_eq!(fixture.job_status(&lease).0, "completed");
}

#[test]
fn crash_before_commit_leaves_no_partial_effect_and_retry_applies_exactly_once() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let before = fixture.durable();
    let proposal = fixture.action_proposal(&lease, text, "call the roofer");

    {
        let tx = fixture.db.immediate_transaction().unwrap();
        let outcome = apply_interpretation_proposal_in_tx(
            &tx,
            &lease.job_id,
            lease.attempt,
            &proposal,
            capture_instant(),
        )
        .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Applied { .. }));
        // The process dies here: the transaction is dropped without commit.
    }

    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_status(&lease).0, "running");

    let outcome = fixture.apply(&lease, &proposal).unwrap();
    assert!(matches!(outcome, ApplyOutcome::Applied { .. }));
    let durable = fixture.durable();
    assert_eq!(durable.proposals.len(), 1);
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(fixture.job_status(&lease).0, "completed");
}

#[test]
fn redelivery_after_commit_is_a_duplicate_with_no_second_effect() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .action_proposal(&lease, text, "call the roofer")
        .with_reminder_proposal(Some(reminder_candidate(
            text,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
        )));
    fixture.apply(&lease, &proposal).unwrap();
    let after_first = fixture.durable();
    let generation = fixture.reminder_row().schedule_generation;

    let replay = fixture.apply(&lease, &proposal).unwrap();

    assert!(matches!(replay, ApplyOutcome::Duplicate));
    assert_eq!(fixture.durable(), after_first);
    assert_eq!(fixture.reminder_row().schedule_generation, generation);
}

#[test]
fn a_reused_proposal_id_under_a_different_job_is_rejected() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let first = fixture.claim_job(0, capture_instant());
    let proposal = fixture.action_proposal(&first, text, "call the roofer");
    fixture.apply(&first, &proposal).unwrap();
    let after_first = fixture.durable();
    let second = fixture.claim_job(0, capture_instant());
    let mut replayed = proposal.clone();
    replayed.request_version = second.request_version.clone();

    let error = fixture.apply(&second, &replayed).unwrap_err();

    assert!(
        matches!(error, ApplyError::DuplicateProposalId(_)),
        "{error:?}"
    );
    assert_eq!(fixture.job_status(&second).0, "running");
    assert_eq!(fixture.durable().proposals, after_first.proposals);
}

// ---------------------------------------------------------------------------------------------
// Rejected results preserve prior state
// ---------------------------------------------------------------------------------------------

#[test]
fn stale_lease_holder_is_rejected_and_the_current_holder_still_applies() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let t0 = capture_instant();
    let first = fixture.claim_job(0, t0);
    let reclaimed = claim_job_with_lease(
        &mut fixture.db,
        Duration::seconds(LEASE_SECONDS),
        t0 + Duration::seconds(LEASE_SECONDS * 2),
    )
    .unwrap()
    .expect("expired lease is reclaimable");
    assert_eq!(reclaimed.attempt_count, first.attempt + 1);
    let before = fixture.durable();
    let proposal = fixture.action_proposal(&first, text, "call the roofer");

    let error = fixture.apply(&first, &proposal).unwrap_err();

    assert!(matches!(error, ApplyError::StaleLease { .. }), "{error:?}");
    assert_eq!(fixture.durable(), before);

    let current = Lease {
        job_id: first.job_id.clone(),
        attempt: reclaimed.attempt_count,
        request_version: first.request_version.clone(),
    };
    assert!(matches!(
        fixture.apply(&current, &proposal).unwrap(),
        ApplyOutcome::Applied { .. }
    ));
}

#[test]
fn a_result_for_an_item_revised_since_the_job_is_stale_and_changes_nothing() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    fixture.correct(CorrectionKind::Scope, Some("personal"), "work", 0);
    let before = fixture.durable();
    assert_eq!(before.revision, 1);
    let proposal = fixture.action_proposal(&lease, text, "call the roofer");

    let error = fixture.apply(&lease, &proposal).unwrap_err();

    assert!(
        matches!(
            error,
            ApplyError::StaleRevision {
                proposal_revision: 0,
                current_revision: 1
            }
        ),
        "{error:?}"
    );
    assert_eq!(fixture.durable(), before);
}

#[test]
fn results_that_do_not_match_their_job_are_rejected() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let before = fixture.durable();
    let valid = fixture.action_proposal(&lease, text, "call the roofer");

    let mut wrong_request = valid.clone();
    wrong_request.request_version = Uuid::new_v4().to_string();
    let mut wrong_item = valid.clone();
    wrong_item.item_id = Uuid::new_v4().to_string();
    let mut wrong_revision = valid.clone();
    wrong_revision.source_revision = 1;
    wrong_revision.text_basis = TextBasis::Original { item_revision: 1 };
    let mut wrong_capture = valid.clone();
    wrong_capture.capture_id = Uuid::new_v4().to_string();

    for (candidate, field) in [
        (&wrong_request, Some("request_version")),
        (&wrong_item, Some("item_id")),
        (&wrong_revision, Some("source_revision")),
        (&wrong_capture, None),
    ] {
        let error = fixture.apply(&lease, candidate).unwrap_err();
        match (field, &error) {
            (Some(expected), ApplyError::JobBindingMismatch { field }) => {
                assert_eq!(*field, expected)
            }
            (None, ApplyError::CaptureMismatch) => {}
            _ => panic!("unexpected rejection {error:?}"),
        }
        assert_eq!(fixture.durable(), before);
    }
}

#[test]
fn a_job_without_an_authorized_destination_cannot_apply_results() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    fixture
        .db
        .conn()
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) \
             VALUES ('profile-v1', 'profile-1', 'open_ai', NULL, 'model', 30, '{}', \
             '[\"https://api.openai.com\"]', '{}', ?)",
            [CAPTURE_INSTANT],
        )
        .unwrap();
    let lease = fixture.claim_job_pinned(0, Some("profile-v1"), capture_instant());
    let before = fixture.durable();
    let proposal = fixture.action_proposal(&lease, text, "call the roofer");

    let error = fixture.apply(&lease, &proposal).unwrap_err();

    assert!(
        matches!(
            error,
            ApplyError::Unauthorized(DenialReason::RouteNotConfigured)
        ),
        "{error:?}"
    );
    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_status(&lease).0, "running");
}

#[test]
fn a_completed_item_refuses_model_output() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'completed' WHERE item_id = ?",
            [&fixture.item_id],
        )
        .unwrap();
    let before = fixture.durable();
    let proposal = fixture.action_proposal(&lease, text, "call the roofer");

    let error = fixture.apply(&lease, &proposal).unwrap_err();

    assert!(matches!(error, ApplyError::ItemNotActive(_)), "{error:?}");
    assert_eq!(fixture.durable(), before);
}

#[test]
fn a_user_corrected_type_rejects_the_proposal_and_rolls_back_its_row() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    fixture.correct(CorrectionKind::Type, None, "idea", 0);
    let lease = fixture.claim_job(1, capture_instant());
    let before = fixture.durable();
    assert_eq!(before.item_type.as_deref(), Some("idea"));
    let mut proposal = fixture.action_proposal(&lease, text, "call the roofer");
    proposal.source_revision = 1;
    proposal.text_basis = TextBasis::Original { item_revision: 1 };

    let error = fixture.apply(&lease, &proposal).unwrap_err();

    assert!(
        matches!(
            error,
            ApplyError::Rejected(ProposalApplicationError::ForbiddenByUserCorrection(_))
        ),
        "{error:?}"
    );
    let after = fixture.durable();
    assert_eq!(
        after, before,
        "the inserted proposal row must be rolled back"
    );
    assert!(after.proposals.is_empty());
}

#[test]
fn text_basis_must_be_the_items_current_effective_text() {
    let text = "call the roofer";
    let corrected = "call the plumber tomorrow";
    let mut fixture = Fixture::new(text);
    let correction_id = fixture.correct(CorrectionKind::Text, Some(text), corrected, 0);
    let lease = fixture.claim_job(1, capture_instant());
    let before = fixture.durable();

    let mut original_basis = fixture.proposal_for(&lease, 1);
    original_basis.text_basis = TextBasis::Original { item_revision: 1 };
    original_basis.item_type = Some(ItemType::Action);
    original_basis.source_spans = Some(vec![span_of(text, "roofer")]);
    let mut other_correction = original_basis.clone();
    other_correction.text_basis = TextBasis::Correction {
        correction_record_id: Uuid::new_v4().to_string(),
        item_revision: 1,
    };
    for stale in [&original_basis, &other_correction] {
        let error = fixture.apply(&lease, stale).unwrap_err();
        assert!(matches!(error, ApplyError::TextBasisStale), "{error:?}");
        assert_eq!(fixture.durable(), before);
    }

    let mut current = fixture.proposal_for(&lease, 1);
    current.text_basis = TextBasis::Correction {
        correction_record_id: correction_id,
        item_revision: 1,
    };
    current.item_type = Some(ItemType::Action);
    current.source_spans = Some(vec![span_of(corrected, "plumber tomorrow")]);
    assert!(matches!(
        fixture.apply(&lease, &current).unwrap(),
        ApplyOutcome::Applied { .. }
    ));
    assert_eq!(fixture.durable().item_type.as_deref(), Some("action"));
}

// ---------------------------------------------------------------------------------------------
// Invalid output, abstention and failure
// ---------------------------------------------------------------------------------------------

#[test]
fn invalid_first_output_leaves_an_uninterpreted_searchable_unsuggested_record() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let mut proposal = fixture.action_proposal(&lease, text, "call the roofer");
    proposal.source_spans = Some(vec![SourceSpan::new(0, 500)]);

    let outcome = fixture.apply(&lease, &proposal).unwrap();

    assert!(matches!(
        outcome,
        ApplyOutcome::Failed {
            processing_state: ProcessingState::Uninterpreted
        }
    ));
    let durable = fixture.durable();
    assert_eq!(durable.processing_state, "uninterpreted");
    assert_eq!(durable.item_type, None);
    assert!(
        durable.proposals.is_empty(),
        "invalid output is never stored as a proposal"
    );
    assert_eq!(
        fixture.job_status(&lease),
        (
            "failed".to_string(),
            Some(INVALID_OUTPUT_REASON.to_string())
        )
    );
    let hits = search_source_direct(fixture.db.conn(), "roofer", &[ItemScope::Personal]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item_id, fixture.item_id);
    assert_eq!(fixture.eligibility(), EligibilityReason::Uninterpreted);
}

#[test]
fn explicit_first_abstention_is_recorded_searchable_and_unsuggested() {
    let text = "maybe look into that thing";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .proposal_for(&lease, 0)
        .with_abstention(Some(AbstentionReason::UncertainTarget));

    let outcome = fixture.apply(&lease, &proposal).unwrap();

    assert!(matches!(
        outcome,
        ApplyOutcome::Abstained {
            processing_state: ProcessingState::Abstained
        }
    ));
    let durable = fixture.durable();
    assert_eq!(durable.processing_state, "abstained");
    assert_eq!(durable.item_type, None);
    assert_eq!(
        durable.proposals,
        vec![(proposal.proposal_id.clone(), "abstained".into(), true)]
    );
    assert_eq!(fixture.job_status(&lease).0, "completed");
    let hits =
        search_source_direct(fixture.db.conn(), "look into", &[ItemScope::Personal]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(fixture.eligibility(), EligibilityReason::Uninterpreted);
}

fn stored_abstention(fixture: &Fixture, proposal_id: &str) -> (Option<String>, Option<String>) {
    fixture
        .db
        .conn()
        .query_row(
            "SELECT abstention_reason, abstention_detail FROM proposals WHERE proposal_id = ?",
            [proposal_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn every_abstention_reason_is_stored_as_a_fixed_label_and_only_other_keeps_a_detail() {
    let cases = [
        (AbstentionReason::UncertainTarget, "uncertain_target", None),
        (AbstentionReason::Negated, "negated", None),
        (AbstentionReason::Ambiguous, "ambiguous", None),
        (
            AbstentionReason::UnsupportedOperation,
            "unsupported_operation",
            None,
        ),
        (
            AbstentionReason::Other("not a task".to_string()),
            "other",
            Some("not a task"),
        ),
    ];
    for (reason, label, detail) in cases {
        let mut fixture = Fixture::new("maybe look into that thing");
        let lease = fixture.claim_job(0, capture_instant());
        let proposal = fixture
            .proposal_for(&lease, 0)
            .with_abstention(Some(reason));
        fixture.apply(&lease, &proposal).unwrap();
        assert_eq!(
            stored_abstention(&fixture, &proposal.proposal_id),
            (Some(label.to_string()), detail.map(str::to_string))
        );
    }
}

#[test]
fn an_other_detail_is_truncated_to_200_unicode_scalars() {
    let mut fixture = Fixture::new("maybe look into that thing");
    let lease = fixture.claim_job(0, capture_instant());
    let detail = "é".repeat(250);
    let proposal = fixture
        .proposal_for(&lease, 0)
        .with_abstention(Some(AbstentionReason::Other(detail)));
    fixture.apply(&lease, &proposal).unwrap();

    let (label, stored) = stored_abstention(&fixture, &proposal.proposal_id);
    assert_eq!(label.as_deref(), Some("other"));
    assert_eq!(stored.unwrap().chars().count(), 200);
}

#[test]
fn an_applied_proposal_stores_no_abstention_reason() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture.action_proposal(&lease, text, "call the roofer");
    fixture.apply(&lease, &proposal).unwrap();

    assert_eq!(
        stored_abstention(&fixture, &proposal.proposal_id),
        (None, None)
    );
}

#[test]
fn permanent_failure_first_is_uninterpreted_and_transient_failure_changes_nothing() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let before = fixture.durable();

    let transient = fixture.fail(&lease, FailureKind::Timeout).unwrap();
    assert!(matches!(transient, ApplyOutcome::RetryLater));
    assert_eq!(
        fixture.durable(),
        before,
        "a transient failure is never uninterpreted"
    );

    let outcome = fixture.fail(&lease, FailureKind::InvalidOutput).unwrap();
    assert!(matches!(
        outcome,
        ApplyOutcome::Failed {
            processing_state: ProcessingState::Uninterpreted
        }
    ));
    assert_eq!(fixture.durable().processing_state, "uninterpreted");
    assert_eq!(
        fixture.job_status(&lease),
        ("failed".to_string(), Some("invalid_output".to_string()))
    );
    assert!(matches!(
        fixture.fail(&lease, FailureKind::InvalidOutput).unwrap(),
        ApplyOutcome::Duplicate
    ));
    assert_eq!(fixture.eligibility(), EligibilityReason::Uninterpreted);
}

#[test]
fn unauthorized_failure_stops_the_job_for_configuration_without_marking_uninterpreted() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET processing_state = 'processing' WHERE item_id = ?",
            [&fixture.item_id],
        )
        .unwrap();

    let outcome = fixture.fail(&lease, FailureKind::Unauthorized).unwrap();

    assert!(matches!(
        outcome,
        ApplyOutcome::Failed {
            processing_state: ProcessingState::Unprocessed
        }
    ));
    assert_eq!(fixture.durable().processing_state, "unprocessed");
    assert_eq!(
        fixture.job_status(&lease),
        ("failed".to_string(), Some("unauthorized".to_string()))
    );
}

#[test]
fn later_abstention_and_failure_retain_the_prior_authoritative_state() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    let first = fixture.claim_job(0, capture_instant());
    let applied = fixture.action_proposal(&first, text, "call the roofer");
    fixture.apply(&first, &applied).unwrap();

    let second = fixture.claim_job(0, capture_instant());
    let abstention = fixture
        .proposal_for(&second, 0)
        .with_abstention(Some(AbstentionReason::Ambiguous));
    let outcome = fixture.apply(&second, &abstention).unwrap();
    assert!(matches!(
        outcome,
        ApplyOutcome::Abstained {
            processing_state: ProcessingState::Processed
        }
    ));

    let third = fixture.claim_job(0, capture_instant());
    let outcome = fixture.fail(&third, FailureKind::Rejected).unwrap();
    assert!(matches!(
        outcome,
        ApplyOutcome::Failed {
            processing_state: ProcessingState::Processed
        }
    ));

    let durable = fixture.durable();
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(
        durable.proposals,
        vec![
            (applied.proposal_id.clone(), "applied".into(), false),
            (abstention.proposal_id.clone(), "abstained".into(), true),
        ]
    );
    assert_eq!(
        durable
            .jobs
            .iter()
            .map(|job| job.1.as_str())
            .collect::<Vec<_>>(),
        ["completed", "completed", "failed"]
    );
}

#[test]
fn abstention_then_failure_keeps_the_abstained_state() {
    let text = "maybe look into that thing";
    let mut fixture = Fixture::new(text);
    let first = fixture.claim_job(0, capture_instant());
    let abstention = fixture
        .proposal_for(&first, 0)
        .with_abstention(Some(AbstentionReason::Negated));
    fixture.apply(&first, &abstention).unwrap();
    let second = fixture.claim_job(0, capture_instant());

    fixture.fail(&second, FailureKind::OutputTooLarge).unwrap();

    assert_eq!(fixture.durable().processing_state, "abstained");
}

// ---------------------------------------------------------------------------------------------
// Reminder candidates
// ---------------------------------------------------------------------------------------------

fn apply_reminder_candidate(
    text: &str,
    phrase: &str,
    instant: Option<&str>,
    quality: TimeResolutionQuality,
    item_type: ItemType,
) -> (Fixture, ReminderDisposition) {
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .proposal_for(&lease, 0)
        .with_item_type(Some(item_type))
        .with_source_spans(Some(vec![span_of(
            text,
            text.split_whitespace().next().unwrap(),
        )]))
        .with_reminder_proposal(Some(reminder_candidate(text, phrase, instant, quality)));
    let outcome = fixture.apply(&lease, &proposal).unwrap();
    (fixture, applied_reminder(outcome))
}

#[test]
fn explicit_future_datetime_resolves_to_a_pending_one_shot_reminder() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let (mut fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-16 09:00:00",
        Some("2026-01-16T09:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "resolved");
    assert_eq!(reminder.schedule_state, "pending_schedule");
    assert_eq!(
        reminder.resolved_instant.as_deref(),
        Some("2026-01-16T09:00:00Z")
    );
    assert_eq!(reminder.schedule_generation, 1);
    assert_eq!(
        fixture.reminder_effects(),
        Some((
            1,
            1,
            vec![OperationType::Schedule],
            Some("2026-01-16 09:00:00".to_string())
        )),
        "a resolved reminder carries its phrase and exactly one schedule operation"
    );
    let tx = fixture.db.immediate_transaction().unwrap();
    let status = ItemStatus::load(&tx, &fixture.item_id).unwrap().unwrap();
    tx.commit().unwrap();
    assert_eq!(
        status.reminder_request_state,
        Some(ReminderRequestState::Resolved)
    );
    assert_eq!(
        status.reminder_schedule_state,
        Some(ReminderScheduleState::PendingSchedule)
    );
    assert_eq!(fixture.eligibility(), EligibilityReason::HasReminder);
}

/// Apply a clock-time reminder proposal for `text` (captured 2026-10-08T14:00:00Z, 10:00 EDT, in
/// America/New_York) whose source span quotes `quoted`.
fn apply_clock_proposal(
    text: &str,
    quoted: &str,
    instant: &str,
    item_span: &str,
) -> (Fixture, ReminderDisposition) {
    apply_clock_proposal_with(
        text,
        quoted,
        Some(instant),
        TimeResolutionQuality::Explicit,
        item_span,
    )
}

fn apply_clock_proposal_with(
    text: &str,
    quoted: &str,
    instant: Option<&str>,
    quality: TimeResolutionQuality,
    item_span: &str,
) -> (Fixture, ReminderDisposition) {
    let mut fixture = Fixture::captured(text, "2026-10-08T14:00:00Z", "America/New_York", -240);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .proposal_for(&lease, 0)
        .with_item_type(Some(ItemType::Action))
        .with_source_spans(Some(vec![span_of(text, item_span)]))
        .with_reminder_proposal(Some(ReminderProposal {
            instant: instant.map(str::to_string),
            timezone_id: Some("America/New_York".to_string()),
            quality,
            source_span: Some(span_of(text, quoted)),
        }));
    let outcome = fixture.apply(&lease, &proposal).unwrap();
    (fixture, applied_reminder(outcome))
}

fn apply_friday_three_pm(instant: &str) -> (Fixture, ReminderDisposition) {
    apply_clock_proposal(
        "Remind me Friday at 3 p.m. to call the roofer",
        "Friday at 3 p.m.",
        instant,
        "call the roofer",
    )
}

fn assert_rejected_and_unscheduled(fixture: &Fixture, disposition: ReminderDisposition) {
    assert_rejected_and_unscheduled_for(&source_text_of(fixture), fixture, disposition);
}

/// The span quoted less than the capture's time expression: the request is kept as not scheduled
/// yet, needing correction, with the whole capture as its phrase, no instant and no schedule
/// operation. The model's instant is never used.
fn assert_held_for_correction(fixture: &Fixture, disposition: ReminderDisposition) {
    let text = source_text_of(fixture);
    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet),
        "{text}"
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "not_scheduled_yet", "{text}");
    assert_eq!(reminder.schedule_state, "not_scheduled", "{text}");
    assert_eq!(reminder.resolved_instant, None, "{text}");
    assert!(reminder.ambiguity_reason.is_some(), "{text}");
    let (_, _, operations, phrase) = fixture.reminder_effects().expect("a reminder row");
    assert_eq!(operations, vec![], "{text}");
    assert_eq!(phrase.as_deref(), Some(text.trim()), "{text}");
    assert_eq!(fixture.durable().reminders, 1, "{text}");
}

fn assert_scheduled_at(fixture: &Fixture, disposition: ReminderDisposition, instant: &str) {
    let text = source_text_of(fixture);
    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved),
        "{text}"
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "resolved", "{text}");
    let stored = DateTime::parse_from_rfc3339(reminder.resolved_instant.as_deref().unwrap())
        .expect("stored instant is RFC 3339");
    assert_eq!(
        stored,
        DateTime::parse_from_rfc3339(instant).unwrap(),
        "{text}"
    );
}

fn assert_rejected_and_unscheduled_for(
    text: &str,
    fixture: &Fixture,
    disposition: ReminderDisposition,
) {
    assert!(
        matches!(disposition, ReminderDisposition::CandidateRejected(_)),
        "{text}: {disposition:?}"
    );
    assert_eq!(fixture.durable().reminders, 0, "{text}");
    assert_eq!(fixture.reminder_effects(), None, "{text}");
}

#[test]
fn a_span_missing_the_zone_qualifier_is_held_for_correction() {
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me tomorrow at 9 a.m. Tokyo time to join the call",
        "tomorrow at 9 a.m.",
        "2026-10-09T09:00:00-04:00",
        "join the call",
    );
    assert_held_for_correction(&fixture, disposition);
}

#[test]
fn a_span_missing_a_multiword_or_bracketed_zone_is_held_for_correction() {
    for text in [
        "Remind me tomorrow at 9 a.m. New York time to join the call",
        "Remind me tomorrow at 9 a.m. Los Angeles time to join the call",
        "Remind me tomorrow at 9 a.m. (EST) to join the call",
        "Remind me tomorrow at 9 a.m. in Tokyo to join the call",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "join the call",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_span_followed_by_any_other_place_or_zone_text_is_held_for_correction() {
    for text in [
        "Remind me tomorrow at 9 a.m. Ho Chi Minh City time to join the call",
        "Remind me tomorrow at 9 a.m. America/New_York to join the call",
        "Remind me tomorrow at 9 a.m. Tokyo, Japan time to join the call",
        "Remind me tomorrow at 9 a.m. (Tokyo) to join the call",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "join the call",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_span_missing_the_surrounding_date_is_held_for_correction() {
    for text in [
        "Remind me at 3 p.m. on October 15 to join the call",
        "Remind me on the 15th at 3 p.m. to join the call",
        "Remind me in two days at 3 p.m. to join the call",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
            "join the call",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_dayless_span_after_a_spelled_or_abbreviated_day_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me on the twelfth at 3 p.m. to call the roofer",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me the twenty-first at 3 p.m. to call the roofer",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me Christmas Eve at 6 p.m. to call the roofer",
            "at 6 p.m.",
            "2026-10-08T18:00:00-04:00",
        ),
        (
            "Remind me tmrw at 5 p.m. to call the roofer",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_span_holding_one_of_two_alternative_times_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m. or Saturday at 4 p.m. to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. or maybe at 4 p.m. to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday or Saturday at 4 p.m. to call the roofer",
            "Saturday at 4 p.m.",
            "2026-10-10T16:00:00-04:00",
        ),
        (
            "Remind me Friday or maybe Saturday at 4 p.m. to call the roofer",
            "Saturday at 4 p.m.",
            "2026-10-10T16:00:00-04:00",
        ),
        // A modifier before the second clock, or a relative time instead of a clock.
        (
            "Remind me Friday at 3 p.m. or around 4 p.m. to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. or roughly four to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. or after the meeting to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. or at lunch to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. but Saturday works too, to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_day_naming_span_after_another_day_in_its_sentence_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Friday, remind me tomorrow at 5 p.m. to call the roofer",
            "tomorrow at 5 p.m.",
            "2026-10-09T17:00:00-04:00",
        ),
        (
            "Next week sometime, remind me Friday at 3 p.m. to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "In two days, remind me Friday at 3 p.m. to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_span_followed_by_a_self_correction_or_zone_aside_after_a_comma_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m., sorry, Saturday at 3 p.m., to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m., I mean 10 a.m., to call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m., we could also do Saturday at 4 p.m., to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m., oh, make it 4 p.m., to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m., it's Tokyo time, to call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m., I mean four, to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_clause_or_aside_after_the_time_is_rejected_whether_or_not_it_names_a_place() {
    for text in [
        "Remind me tomorrow at 9 a.m., I will be in Tokyo, to call the roofer",
        "Remind me tomorrow at 9 a.m., I\u{2019}ll be in Tokyo, to call the roofer",
        "Remind me tomorrow at 9 a.m. (I'll be in Lisbon) to call the roofer",
        "Remind me tomorrow at 9 a.m. - I'll be in Lisbon - to call the roofer",
        "Remind me tomorrow at 9 a.m., after the meeting, to call the roofer",
        "Remind me tomorrow at 9 a.m., I don't want to miss it, to call the roofer",
        "Remind me tomorrow at 9 a.m., sorry, I forgot the two quotes, to call the roofer",
        "Remind me tomorrow at 9 a.m. Tokyo time, to call the roofer",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "call the roofer",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_self_correction_to_a_relative_time_or_a_possessive_zone_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m., sorry, the day after, to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m., I mean later, to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m., oh, make it earlier, to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m., I mean their time, to call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. my time to call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m. their time to call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_list_in_the_content_needs_correction_in_either_order() {
    // With the time last, the commas of the list are clause marks in the content before the
    // time, which the whole-capture rule holds for correction. The explicit-intent check reads
    // the comma between "remind me" and the time as a clause break, but only while the time
    // could still be scheduled: once the timing is held, the request is kept.
    for text in [
        "Remind me tomorrow at 9 a.m., buy eggs, milk and bread",
        "Remind me tomorrow at 9 a.m. to buy eggs, milk and bread",
        "Remind me to buy eggs, milk and bread tomorrow at 9 a.m.",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "buy eggs",
        );
        assert_held_for_correction(&fixture, disposition);
    }
    // The same list without commas schedules.
    let text = "Remind me to buy eggs and milk and bread tomorrow at 9 a.m.";
    let (fixture, disposition) = apply_clock_proposal(
        text,
        "tomorrow at 9 a.m.",
        "2026-10-09T09:00:00-04:00",
        "buy eggs",
    );
    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved),
        "{text}"
    );
    assert_eq!(fixture.reminder_row().request_state, "resolved", "{text}");
}

#[test]
fn the_allowed_boundaries_around_the_time_still_schedule() {
    for text in [
        "Remind me tomorrow at 9 a.m. to call the roofer",
        "Remind me tomorrow at 9 a.m., to call the roofer",
        "Remind me to call the roofer tomorrow at 9 a.m.",
        "Remind me to call the roofer tomorrow at 9 a.m., thanks",
        "Remind me to call the roofer tomorrow at 9 a.m.!",
        "Remind me tomorrow at 9 a.m. to call the roofer. Thanks",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "call the roofer",
        );
        assert_eq!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert_eq!(fixture.reminder_row().request_state, "resolved", "{text}");
    }
}

#[test]
fn a_day_naming_span_after_a_relative_modifier_is_held_for_correction() {
    for text in [
        "Remind me before Friday at 3 p.m. to call the roofer",
        "Remind me after Friday at 3 p.m. to call the roofer",
        "Remind me prior to Friday at 3 p.m. to call the roofer",
        "Remind me sometime Friday at 3 p.m. to call the roofer",
        "Remind me by Friday at 3 p.m. to call the roofer",
        "Remind me to call the roofer by Friday at 3 p.m.",
        "Remind me to call the roofer after the meeting Friday at 3 p.m.",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_dayless_span_after_a_day_earlier_in_the_sentence_is_held_for_correction() {
    for (text, instant) in [
        (
            "Friday, remind me at 3 p.m. to call the roofer",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "On Friday, remind me at 3 p.m. to call the roofer",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Tomorrow, remind me at 5 p.m. to call the roofer",
            "2026-10-08T17:00:00-04:00",
        ),
        (
            "On October 15, remind me at 3 p.m. to call the roofer",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "In two days, remind me at 3 p.m. to call the roofer",
            "2026-10-08T15:00:00-04:00",
        ),
    ] {
        let quoted = if text.contains("5 p.m.") {
            "at 5 p.m."
        } else {
            "at 3 p.m."
        };
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_dayless_span_after_a_day_in_an_earlier_sentence_or_past_a_pause_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Tomorrow... remind me at 5 p.m. to call the roofer",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
        (
            "Tomorrow! Remind me at 5 p.m. to call the roofer",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
        (
            "Tues. remind me at 3 p.m. to call the roofer",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Call Sam on Friday. Remind me at 5 p.m. to call the roofer",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_dayless_span_with_a_day_in_the_task_text_is_held_for_correction_not_anchored_to_today() {
    for (text, quoted, instant) in [
        // "... at 5 p.m. to call the roofer tomorrow" is the split shape (F46), which schedules.
        (
            "Remind me at 5 p.m. to call the roofer tmrw",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
        (
            "Remind me at 3 p.m. to call the roofer on October 15",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me at 3 p.m. to call the roofer in two days",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me at 3 p.m. to call the roofer on the twelfth",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me at 3 p.m. to call the roofer next week",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me at 6 p.m. to call the roofer Christmas Eve",
            "at 6 p.m.",
            "2026-10-08T18:00:00-04:00",
        ),
        (
            "Remind me at 3 p.m. to call the roofer on Fri",
            "at 3 p.m.",
            "2026-10-08T15:00:00-04:00",
        ),
        (
            "Remind me at 5 p.m. to call the roofer. Tomorrow is busy",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_day_naming_span_whose_task_text_names_another_time_still_schedules() {
    for text in [
        "Remind me Friday at 3 p.m. to call the roofer on Saturday",
        "Remind me Friday at 3 p.m. to call the roofer before the 9 a.m. meeting",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_eq!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert_eq!(fixture.reminder_row().request_state, "resolved");
    }
}

#[test]
fn a_dayless_span_after_only_request_words_still_schedules() {
    for text in [
        "Remind me at 5 p.m. to call Sam",
        "Set a reminder at 5 p.m. to call Sam",
        "I talked to the roofer. Remind me at 5 p.m. to call Sam",
    ] {
        let (fixture, disposition) =
            apply_clock_proposal(text, "at 5 p.m.", "2026-10-08T17:00:00-04:00", "call Sam");
        assert_eq!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert_eq!(fixture.reminder_row().request_state, "resolved");
    }
}

#[test]
fn a_bracketed_aside_after_the_time_is_held_for_correction() {
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me tomorrow at 9 a.m. (call Sam about the quote)",
        "tomorrow at 9 a.m.",
        "2026-10-09T09:00:00-04:00",
        "call Sam about the quote",
    );
    assert_held_for_correction(&fixture, disposition);
}

/// After the time come optionally a comma, optionally `to` or a filler, then the task content:
/// plain words are content whether or not `to` introduces them. A bare place with no zone word
/// next to the clock is plain content too, an accepted limit: places are unbounded and no list
/// of them is kept.
#[test]
fn plain_words_after_the_time_are_task_content_with_or_without_to() {
    for (text, quoted, instant, item_span) in [
        (
            "Remind me tomorrow at 9 a.m. about the dentist",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "the dentist",
        ),
        (
            "Remind me Friday at 3 p.m., call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        ),
        (
            "Remind me Friday at 3 p.m. call the roofer",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        ),
        (
            "Remind me tomorrow at 9 a.m. and call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "call the roofer",
        ),
        (
            "Remind me Friday at 3 p.m., I don't want to miss it",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "miss it",
        ),
        (
            "Remind me tomorrow at 9 a.m. Tokyo to join the call",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "join the call",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, item_span);
        assert_scheduled_at(&fixture, disposition, instant);
    }
}

/// Task first: between the content and the time only nothing, a single comma or a filler may
/// appear, and the content may hold no clause mark.
#[test]
fn a_clause_mark_in_the_content_before_the_time_is_held_for_correction() {
    // A bracket or colon between the request and the time is a clause break too: while the
    // timing is held, it does not drop the request.
    for text in [
        "Remind me to call the roofer - urgent - Friday at 3 p.m.",
        "Remind me to call the roofer \u{2014} urgent \u{2014} Friday at 3 p.m.",
        "Remind me to call the roofer (urgent) Friday at 3 p.m.",
        "Remind me to call the roofer: Friday at 3 p.m.",
    ] {
        let span = span_of(text, "Friday at 3 p.m.");
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_held_for_correction(&fixture, disposition);
    }
    // A semicolon, a sentence end and every word that detaches a time from the request still
    // do, held timing or not.
    for (text, item_span) in [
        (
            "Remind me to call Bob, who called Friday at 3 p.m.",
            "call Bob",
        ),
        (
            "Remind me why the quote failed: it expired Friday at 3 p.m.",
            "the quote failed",
        ),
        (
            "Remind me to call Bob; the quote expires Friday at 3 p.m.",
            "call Bob",
        ),
        (
            "Remind me to call Bob. The quote expires Friday at 3 p.m.",
            "call Bob",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            item_span,
        );
        assert_eq!(disposition, ReminderDisposition::NoExplicitIntent, "{text}");
        assert_eq!(fixture.durable().reminders, 0, "{text}");
    }
}

/// Task first: after the time only the end of the text, closing punctuation or a filler may
/// follow.
#[test]
fn task_content_after_a_trailing_time_is_held_for_correction() {
    for text in [
        "Remind me to call the roofer Friday at 3 p.m. to fix the leak",
        "Remind me to call the roofer Friday at 3 p.m. about the leak",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_held_for_correction(&fixture, disposition);
    }
}

/// Whole-capture scope: a temporal token in any other sentence of the capture makes the timing
/// ambiguous, whether it corrects the time, qualifies its zone, or is the accepted cost of a
/// date mentioned elsewhere.
#[test]
fn a_temporal_token_in_another_sentence_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, make it Saturday at 4 p.m.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Sorry, I mean Saturday.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, make it 4.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m. to call the roofer. That's Tokyo time.",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m.! Tokyo time, to call the roofer",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Talked to Jim on Monday. Remind me Friday at 3 p.m. to call the roofer.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Tomorrow is busy",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
    // A closing "no" followed by a new time corrects the time; it does not withdraw the request.
    for text in [
        "Remind me to call the roofer tomorrow at 9 a.m. No wait, 10 a.m.",
        "Remind me to call the roofer tomorrow at 9 a.m., actually no, Saturday.",
    ] {
        let span = span_of(text, "tomorrow at 9 a.m.");
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "call the roofer",
        );
        assert_held_for_correction(&fixture, disposition);
    }
    // A withdrawal is still a withdrawal, whatever comes after it.
    for text in [
        "Remind me to call the roofer tomorrow at 9 a.m., actually no",
        "Remind me to call the roofer tomorrow at 9 a.m. No wait, cancel that. Saturday is busy.",
        "Remind me to call the roofer tomorrow at 9 a.m., never mind, 10 a.m. is busy",
        // A negation that ends its sentence is not joined to a time in the next one.
        "Remind me to call the roofer tomorrow at 9 a.m. Actually no. Saturday is busy.",
        "Remind me to call the roofer tomorrow at 9 a.m., actually no! Saturday works better for Sam.",
        "Remind me to call the roofer tomorrow at 9 a.m. No. 10 a.m. is when I'm out.",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
            "call the roofer",
        );
        assert_eq!(disposition, ReminderDisposition::NoExplicitIntent, "{text}");
        assert_eq!(fixture.durable().reminders, 0, "{text}");
    }
}

/// A dotted abbreviation outside the span ends its sentence like any other word: "Jan" and "sun"
/// are also a name and an ordinary word, and the sentence after them is checked. Every number
/// word counts, "zero" included.
#[test]
fn a_correction_after_a_dotted_abbreviation_or_a_number_word_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m. to call Jan. Actually make it Saturday at 4 p.m.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to bring the cushions in from the sun. Make it Saturday instead.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me tomorrow at 9 a.m. to call Jan. That's Tokyo time.",
            "tomorrow at 9 a.m.",
            "2026-10-09T09:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, make it zero.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
    ] {
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let item_span = if text.contains("cushions") {
            "bring the cushions in from the sun"
        } else if text.contains("Jan") {
            "call Jan"
        } else {
            "call the roofer"
        };
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, item_span);
        assert_held_for_correction(&fixture, disposition);
    }
    // The control: a name with a dot that is followed by a sentence with no temporal token
    // still schedules.
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me Friday at 3 p.m. to call Jan. Thanks",
        "Friday at 3 p.m.",
        "2026-10-09T15:00:00-04:00",
        "call Jan",
    );
    assert_scheduled_at(&fixture, disposition, "2026-10-09T15:00:00-04:00");
}

/// A time word carrying any clitic ("Saturday's better", "Saturday'll work", "Tomorrow'd be
/// better", with the ASCII or the typographic apostrophe that dictation produces) is still a
/// temporal token in another sentence, for a span that names its day and for one that does not.
#[test]
fn a_correction_with_a_clitic_time_word_is_held_for_correction() {
    for (text, quoted, instant) in [
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday's better.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday\u{2019}s better.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Four's better.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Saturday's no good. Remind me Friday at 3 p.m. to call the roofer.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me at 5 p.m. to call the roofer. Tomorrow's better.",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday'll work.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Actually, Saturday\u{2019}ll work.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Saturday'd be better.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me Friday at 3 p.m. to call the roofer. Four'll do.",
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
        ),
        (
            "Remind me at 5 p.m. to call the roofer. Tomorrow'd be better.",
            "at 5 p.m.",
            "2026-10-08T17:00:00-04:00",
        ),
    ] {
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call the roofer");
        assert_held_for_correction(&fixture, disposition);
    }
    // The controls: a clitic on a word that names no time leaves the reminder scheduled.
    for text in [
        "Remind me Friday at 3 p.m. to call the roofer. It's urgent.",
        "Remind me Friday at 3 p.m. to call the roofer. That\u{2019}s all.",
        "Remind me Friday at 3 p.m. to call the roofer. Sam's roof leaks.",
        "Remind me Friday at 3 p.m. to call the roofer. I'll bring the quote.",
        "Remind me Friday at 3 p.m. to call the roofer. He\u{2019}d like that.",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_scheduled_at(&fixture, disposition, "2026-10-09T15:00:00-04:00");
    }
}

/// Context in another sentence that holds no temporal token (a filler, a person, a place) is
/// outside the guard's reach and still schedules.
#[test]
fn another_sentence_without_a_temporal_token_still_schedules() {
    for text in [
        "Remind me Friday at 3 p.m. to call the roofer. Thanks",
        "I talked to Sam. Remind me Friday at 3 p.m. to call the roofer",
        "Remind me Friday at 3 p.m. to call the roofer. He is in Tokyo",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call the roofer",
        );
        assert_scheduled_at(&fixture, disposition, "2026-10-09T15:00:00-04:00");
    }
}

#[test]
fn a_span_missing_the_day_is_held_for_correction() {
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me tomorrow at 5 p.m. to join the call",
        "at 5 p.m.",
        "2026-10-08T17:00:00-04:00",
        "join the call",
    );
    assert_held_for_correction(&fixture, disposition);
}

#[test]
fn the_full_span_still_schedules_and_trailing_words_are_not_truncation() {
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me tomorrow at 5 p.m. to join the call",
        "tomorrow at 5 p.m.",
        "2026-10-09T17:00:00-04:00",
        "join the call",
    );
    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    assert_eq!(fixture.reminder_row().request_state, "resolved");
}

#[test]
fn a_clock_time_phrase_resolves_to_a_scheduled_reminder() {
    let (fixture, disposition) = apply_friday_three_pm("2026-10-09T15:00:00-04:00");

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "resolved");
    assert_eq!(reminder.schedule_state, "pending_schedule");
    let stored = DateTime::parse_from_rfc3339(reminder.resolved_instant.as_deref().unwrap())
        .expect("stored instant is RFC 3339");
    assert_eq!(
        stored,
        DateTime::parse_from_rfc3339("2026-10-09T15:00:00-04:00").unwrap()
    );
}

#[test]
fn a_clock_time_proposal_with_the_wrong_instant_contradicts_the_source() {
    let (fixture, disposition) = apply_friday_three_pm("2026-10-09T16:00:00-04:00");

    assert_rejected_and_unscheduled(&fixture, disposition);
}

#[test]
fn a_date_without_an_hour_stays_unscheduled_and_keeps_the_phrase() {
    let text = "remind me to call the roofer 2026-01-16";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-16",
        None,
        TimeResolutionQuality::Ambiguous,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "not_scheduled_yet");
    assert_eq!(reminder.schedule_state, "not_scheduled");
    assert_eq!(reminder.resolved_instant, None);
    assert!(reminder.ambiguity_reason.is_some());
    assert_eq!(
        fixture.reminder_effects().unwrap().3.as_deref(),
        Some("2026-01-16")
    );
    assert_eq!(fixture.reminder_effects().unwrap().2, vec![]);
    assert_eq!(fixture.durable().item_type.as_deref(), Some("action"));
}

#[test]
fn unsupported_grammar_is_unscheduled_with_the_intention_intact() {
    let text = "remind me to call the roofer after lunch";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "after lunch",
        Some("2026-01-15T13:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "not_scheduled_yet");
    assert_eq!(reminder.resolved_instant, None);
    assert!(reminder.ambiguity_reason.is_some());
    assert_eq!(
        fixture.reminder_effects().unwrap().3.as_deref(),
        Some("after lunch")
    );
    assert_eq!(fixture.durable().item_type.as_deref(), Some("action"));
    assert_eq!(fixture.durable().processing_state, "processed");
}

#[test]
fn recurrence_is_recorded_as_unsupported_never_reduced_to_a_one_shot() {
    let text = "remind me to call the roofer every Monday";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "every Monday",
        Some("2026-01-19T09:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::UnsupportedRecurrence)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "unsupported_recurrence");
    assert_eq!(reminder.schedule_state, "not_scheduled");
    assert_eq!(reminder.resolved_instant, None);
    assert!(reminder.unsupported_reason.is_some());
    assert_eq!(
        fixture.reminder_effects().unwrap().3.as_deref(),
        Some("every Monday")
    );
    assert_eq!(fixture.reminder_effects().unwrap().2, vec![]);
}

#[test]
fn an_instant_the_source_does_not_support_is_never_scheduled() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-16 09:00:00",
        Some("2026-01-16T17:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert!(matches!(
        disposition,
        ReminderDisposition::CandidateRejected(_)
    ));
    assert_eq!(fixture.durable().reminders, 0);
    assert_eq!(fixture.reminder_effects(), None);
    assert_eq!(fixture.durable().processing_state, "processed");
}

#[test]
fn a_candidate_in_another_timezone_than_the_capture_is_never_scheduled() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let mut candidate = reminder_candidate(
        text,
        "2026-01-16 09:00:00",
        Some("2026-01-16T09:00:00Z"),
        TimeResolutionQuality::Explicit,
    );
    candidate.timezone_id = Some("America/New_York".to_string());
    let proposal = fixture
        .action_proposal(&lease, text, "call the roofer")
        .with_reminder_proposal(Some(candidate));

    let disposition = applied_reminder(fixture.apply(&lease, &proposal).unwrap());

    assert!(matches!(
        disposition,
        ReminderDisposition::CandidateRejected(_)
    ));
    assert_eq!(fixture.reminder_effects(), None);
}

#[test]
fn a_time_already_past_at_capture_is_unschedulable_without_adjustment() {
    let text = "remind me to call the roofer 2026-01-10 09:00:00";
    let (mut fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-10 09:00:00",
        Some("2026-01-10T09:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Unschedulable)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "unschedulable");
    assert_eq!(
        reminder.unschedulable_reason.as_deref(),
        Some("time_in_past")
    );
    assert_eq!(
        reminder.resolved_instant.as_deref(),
        Some("2026-01-10T09:00:00Z")
    );
    let tx = fixture.db.immediate_transaction().unwrap();
    let status = ItemStatus::load(&tx, &fixture.item_id).unwrap().unwrap();
    tx.commit().unwrap();
    assert_eq!(
        status.unschedulable_reason,
        Some(UnschedulableReason::TimeInPast)
    );
}

#[test]
fn a_reminder_on_an_item_that_is_not_an_action_is_not_recorded() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-16 09:00:00",
        Some("2026-01-16T09:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Idea,
    );

    assert_eq!(disposition, ReminderDisposition::NotRecorded);
    assert_eq!(fixture.durable().reminders, 0);
    assert_eq!(fixture.durable().item_type.as_deref(), Some("idea"));
}

#[test]
fn a_later_proposal_refines_an_unresolved_reminder_but_never_replaces_a_committed_time() {
    let text = "remind me 2026-01-16 09:00:00 to call the roofer and again 2026-01-17 09:00:00";
    let mut fixture = Fixture::new(text);
    let build = |fixture: &Fixture, lease: &Lease, phrase: &str, instant: Option<&str>, quality| {
        fixture
            .action_proposal(lease, text, "call the roofer")
            .with_reminder_proposal(Some(reminder_candidate(text, phrase, instant, quality)))
    };

    let first = fixture.claim_job(0, capture_instant());
    let date_only = build(
        &fixture,
        &first,
        "2026-01-16",
        None,
        TimeResolutionQuality::Ambiguous,
    );
    assert_eq!(
        applied_reminder(fixture.apply(&first, &date_only).unwrap()),
        ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet)
    );
    assert_eq!(fixture.reminder_row().schedule_generation, 0);
    assert_eq!(fixture.reminder_effects().unwrap().2, vec![]);

    let second = fixture.claim_job(0, capture_instant());
    let resolved = build(
        &fixture,
        &second,
        "2026-01-16 09:00:00",
        Some("2026-01-16T09:00:00Z"),
        TimeResolutionQuality::Explicit,
    );
    assert_eq!(
        applied_reminder(fixture.apply(&second, &resolved).unwrap()),
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    assert_eq!(fixture.reminder_row().schedule_generation, 1);
    let committed = fixture.reminder_effects().unwrap();
    assert_eq!(committed.2, vec![OperationType::Schedule]);

    let third = fixture.claim_job(0, capture_instant());
    let different_time = build(
        &fixture,
        &third,
        "2026-01-17 09:00:00",
        Some("2026-01-17T09:00:00Z"),
        TimeResolutionQuality::Explicit,
    );
    assert_eq!(
        applied_reminder(fixture.apply(&third, &different_time).unwrap()),
        ReminderDisposition::ExistingKept
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "resolved");
    assert_eq!(
        reminder.resolved_instant.as_deref(),
        Some("2026-01-16T09:00:00Z")
    );
    assert_eq!(fixture.reminder_effects().unwrap(), committed);
    assert_eq!(fixture.job_status(&third).0, "completed");
    let durable = fixture.durable();
    assert_eq!(
        durable
            .proposals
            .iter()
            .map(|p| p.1.as_str())
            .collect::<Vec<_>>(),
        ["superseded", "superseded", "applied"]
    );
}

#[test]
fn reapplying_the_same_resolved_request_keeps_one_schedule_operation_and_its_version() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);
    let phrase = "2026-01-16 09:00:00";
    let build = |fixture: &Fixture, lease: &Lease| {
        fixture
            .action_proposal(lease, text, "call the roofer")
            .with_reminder_proposal(Some(reminder_candidate(
                text,
                phrase,
                Some("2026-01-16T09:00:00Z"),
                TimeResolutionQuality::Explicit,
            )))
    };

    let first = fixture.claim_job(0, capture_instant());
    fixture.apply(&first, &build(&fixture, &first)).unwrap();
    let after_first = fixture.reminder_effects().unwrap();
    assert_eq!(after_first.2, vec![OperationType::Schedule]);

    let second = fixture.claim_job(0, capture_instant());
    let disposition = applied_reminder(fixture.apply(&second, &build(&fixture, &second)).unwrap());

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    assert_eq!(
        fixture.reminder_effects().unwrap(),
        after_first,
        "a retry reaching the same time changes neither the version, the generation nor the operations"
    );
}

#[test]
fn a_crash_before_commit_leaves_neither_reminder_nor_schedule_operation() {
    let text = "remind me to call the roofer 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0, capture_instant());
    let proposal = fixture
        .action_proposal(&lease, text, "call the roofer")
        .with_reminder_proposal(Some(reminder_candidate(
            text,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
        )));

    {
        let tx = fixture.db.immediate_transaction().unwrap();
        apply_interpretation_proposal_in_tx(
            &tx,
            &lease.job_id,
            lease.attempt,
            &proposal,
            capture_instant(),
        )
        .unwrap();
    }

    assert_eq!(fixture.reminder_effects(), None);
    assert_eq!(fixture.job_status(&lease).0, "running");
    fixture.apply(&lease, &proposal).unwrap();
    assert_eq!(
        fixture.reminder_effects().unwrap().2,
        vec![OperationType::Schedule]
    );
}

#[test]
fn a_later_proposal_cannot_turn_an_unsupported_recurrence_into_a_one_shot() {
    let text = "remind me to call the roofer every Monday 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);

    let first = fixture.claim_job(0, capture_instant());
    let recurring = fixture
        .action_proposal(&first, text, "call the roofer")
        .with_reminder_proposal(Some(reminder_candidate(
            text,
            "every Monday",
            None,
            TimeResolutionQuality::Ambiguous,
        )));
    assert_eq!(
        applied_reminder(fixture.apply(&first, &recurring).unwrap()),
        ReminderDisposition::Recorded(ReminderRequestState::UnsupportedRecurrence)
    );
    let stored = fixture.reminder_effects().unwrap();

    let second = fixture.claim_job(0, capture_instant());
    let one_shot = fixture
        .action_proposal(&second, text, "call the roofer")
        .with_reminder_proposal(Some(reminder_candidate(
            text,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
        )));
    fixture.apply(&second, &one_shot).unwrap();

    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "unsupported_recurrence");
    assert_eq!(reminder.resolved_instant, None);
    assert_eq!(reminder.schedule_state, "not_scheduled");
    assert_eq!(fixture.reminder_effects().unwrap(), stored);
    assert!(stored.2.is_empty());
}

#[test]
fn a_repeat_marker_elsewhere_in_the_text_is_never_reduced_to_a_one_shot() {
    let text = "remind me to call the roofer every Monday starting 2026-01-16 09:00:00";
    let (fixture, disposition) = apply_reminder_candidate(
        text,
        "2026-01-16 09:00:00",
        Some("2026-01-16T09:00:00Z"),
        TimeResolutionQuality::Explicit,
        ItemType::Action,
    );

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::UnsupportedRecurrence)
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "unsupported_recurrence");
    assert_eq!(reminder.resolved_instant, None);
    assert_eq!(fixture.reminder_effects().unwrap().2, vec![]);
}

#[test]
fn a_dated_fact_that_does_not_ask_for_a_reminder_stays_unscheduled() {
    for text in [
        "the roof quote expires 2026-01-16 09:00:00",
        "call the roofer 2026-01-16 09:00:00",
        "don't remind me to call the roofer 2026-01-16 09:00:00",
        "she said \"remind me to call the roofer 2026-01-16 09:00:00\"",
        "if you remind me call the roofer 2026-01-16 09:00:00",
        "remind me to call Bob. the roof quote expires 2026-01-16 09:00:00",
        "remind me to call Bob tomorrow; the roof quote expires 2026-01-16 09:00:00",
        "I already set a reminder for 2026-01-16 09:00:00",
        "Bob will remind me 2026-01-16 09:00:00",
        "Bob is going to remind me 2026-01-16 09:00:00",
        "Bob wants to remind me 2026-01-16 09:00:00",
        "Bob promised to remind me 2026-01-16 09:00:00",
        "they need to remind me 2026-01-16 09:00:00",
        "I told Bob to remind me 2026-01-16 09:00:00",
        "my assistant will call and remind me 2026-01-16 09:00:00",
        "she needs to set a reminder for 2026-01-16 09:00:00",
        "I don't think I need you to remind me 2026-01-16 09:00:00",
        "I never said I want you to remind me 2026-01-16 09:00:00",
        "Bob thinks we need to set a reminder for 2026-01-16 09:00:00",
        "remind me not on 2026-01-16 09:00:00",
        "remind me any day except 2026-01-16 09:00:00",
        "remind me to call Bob who called on 2026-01-16 09:00:00",
        "remind me why the roof quote expires on 2026-01-16 09:00:00",
        "remind me what the roofer said on 2026-01-16 09:00:00",
        "remind me whether the roof quote expires 2026-01-16 09:00:00",
        "remind me of the meeting on 2026-01-16 09:00:00",
        "you remind me of the meeting on 2026-01-16 09:00:00",
        "remind me about the call that ended 2026-01-16 09:00:00",
        "remind me again why the roof quote expires on 2026-01-16 09:00:00",
        "please remind me once more why the quote expires on 2026-01-16 09:00:00",
        "remind me exactly what happened on 2026-01-16 09:00:00",
        "remind me later of the meeting on 2026-01-16 09:00:00",
        "remind me when the quote expired on 2026-01-16 09:00:00",
        "remind me later about the meeting on 2026-01-16 09:00:00",
        "remind me what I already did on 2026-01-16 09:00:00",
        "Bob told me I need to set a reminder for 2026-01-16 09:00:00",
        "Bob hopes I want to set a reminder for 2026-01-16 09:00:00",
    ] {
        let (fixture, disposition) = apply_reminder_candidate(
            text,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
            ItemType::Action,
        );

        assert_eq!(disposition, ReminderDisposition::NoExplicitIntent, "{text}");
        assert_eq!(fixture.reminder_effects(), None, "{text}");
        assert_eq!(fixture.durable().item_type.as_deref(), Some("action"));
        assert_eq!(fixture.durable().processing_state, "processed");
    }
}

#[test]
fn natural_first_person_reminder_requests_bind_to_the_quoted_time() {
    for text in [
        "please remind me to call the roofer 2026-01-16 09:00:00",
        "can you remind me to call the roofer at 2026-01-16 09:00:00",
        "I need to set a reminder for 2026-01-16 09:00:00",
        "set a reminder for 2026-01-16 09:00:00 to call the roofer",
        "I'll need you to remind me on 2026-01-16 09:00:00",
        "I need you to remind me on 2026-01-16 09:00:00",
        "I'd like you to remind me on 2026-01-16 09:00:00",
        "we want to set a reminder for 2026-01-16 09:00:00",
        "I'm going to set a reminder for 2026-01-16 09:00:00",
        "need to set a reminder for 2026-01-16 09:00:00",
        "could you please remind me on 2026-01-16 09:00:00",
        "remind me to call the café roofer 2026-01-16 09:00:00",
    ] {
        let (fixture, disposition) = apply_reminder_candidate(
            text,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
            ItemType::Action,
        );

        assert_eq!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert!(fixture.reminder_effects().is_some(), "{text}");
    }
}

const WITHDRAWAL_CORPUS: &str = include_str!("withdrawal_corpus.txt");
const CORPUS_TIME_PHRASE: &str = "2026-01-16 09:00:00";

#[derive(Debug, PartialEq)]
enum CorpusExpectation {
    Unscheduled,
    Scheduled,
    Recognised,
}

/// Parse `<expectation> | <capture text>` lines; `{time}` becomes the quoted time phrase.
fn withdrawal_corpus() -> Vec<(CorpusExpectation, String)> {
    WITHDRAWAL_CORPUS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (expectation, text) = line
                .split_once(" | ")
                .unwrap_or_else(|| panic!("malformed corpus line: {line}"));
            let expectation = match expectation {
                "unscheduled" => CorpusExpectation::Unscheduled,
                "scheduled" => CorpusExpectation::Scheduled,
                "recognised" => CorpusExpectation::Recognised,
                other => panic!("unknown corpus expectation {other:?} in: {line}"),
            };
            (expectation, text.replace("{time}", CORPUS_TIME_PHRASE))
        })
        .collect()
}

fn source_text_of(fixture: &Fixture) -> String {
    open(&fixture.path)
        .conn()
        .query_row(
            "SELECT text FROM captures WHERE capture_id = ?",
            [&fixture.capture_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn the_withdrawal_corpus_decides_which_reminder_requests_schedule() {
    let corpus = withdrawal_corpus();
    let withdrawals = corpus
        .iter()
        .filter(|(expectation, _)| *expectation == CorpusExpectation::Unscheduled)
        .count();
    let scheduled = corpus
        .iter()
        .filter(|(expectation, _)| *expectation == CorpusExpectation::Scheduled)
        .count();
    let recognised = corpus.len() - withdrawals - scheduled;
    assert!(
        withdrawals > 0 && scheduled > 0,
        "corpus needs withdrawals and scheduled controls"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for (expectation, text) in &corpus {
        let (fixture, disposition) = apply_reminder_candidate(
            text,
            CORPUS_TIME_PHRASE,
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
            ItemType::Action,
        );
        let durable = fixture.durable();
        assert_eq!(durable.item_type.as_deref(), Some("action"), "{text}");
        assert_eq!(durable.processing_state, "processed", "{text}");
        assert_eq!(
            source_text_of(&fixture),
            *text,
            "the source record stays intact: {text}"
        );

        let matched = match expectation {
            CorpusExpectation::Unscheduled => {
                disposition == ReminderDisposition::NoExplicitIntent
                    && fixture.reminder_effects().is_none()
                    && durable.reminders == 0
            }
            // Not a withdrawal, but the clause after the time keeps the timing ambiguous under
            // the F28 content boundary: kept as not scheduled yet with the whole capture as its
            // phrase, needing correction, never scheduled.
            CorpusExpectation::Recognised => {
                disposition == ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet)
                    && fixture
                        .reminder_effects()
                        .is_some_and(|(_, _, operations, phrase)| {
                            operations.is_empty() && phrase.as_deref() == Some(text.as_str())
                        })
                    && durable.reminders == 1
                    && fixture.reminder_row().resolved_instant.is_none()
            }
            CorpusExpectation::Scheduled => {
                disposition == ReminderDisposition::Recorded(ReminderRequestState::Resolved)
                    && fixture
                        .reminder_effects()
                        .is_some_and(|(_, _, operations, _)| {
                            operations == vec![OperationType::Schedule]
                        })
                    && durable.reminders == 1
                    && fixture.reminder_row().resolved_instant.as_deref()
                        == Some("2026-01-16T09:00:00Z")
            }
        };
        if !matched {
            mismatches.push(format!(
                "{expectation:?} expected, got {disposition:?}: {text}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} corpus lines did not match:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    println!(
        "withdrawal corpus: {withdrawals} withdrawals stayed unscheduled, {scheduled} controls scheduled, {recognised} controls were recognised but held for correction"
    );
}

#[test]
fn a_reminder_request_on_a_corrected_text_resolves_against_the_correction() {
    let text = "call the roofer";
    let corrected = "remind me to call the plumber 2026-01-16 09:00:00";
    let mut fixture = Fixture::new(text);
    let correction_id = fixture.correct(CorrectionKind::Text, Some(text), corrected, 0);
    let lease = fixture.claim_job(1, capture_instant());
    let mut proposal = fixture.proposal_for(&lease, 1);
    proposal.text_basis = TextBasis::Correction {
        correction_record_id: correction_id,
        item_revision: 1,
    };
    let proposal = proposal
        .with_item_type(Some(ItemType::Action))
        .with_source_spans(Some(vec![span_of(corrected, "plumber")]))
        .with_reminder_proposal(Some(reminder_candidate(
            corrected,
            "2026-01-16 09:00:00",
            Some("2026-01-16T09:00:00Z"),
            TimeResolutionQuality::Explicit,
        )));

    let disposition = applied_reminder(fixture.apply(&lease, &proposal).unwrap());

    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::Resolved)
    );
    assert_eq!(
        fixture.reminder_effects().unwrap().2,
        vec![OperationType::Schedule]
    );
}

// ---------------------------------------------------------------------------------------------
// Hedged reminder requests (F29)
// ---------------------------------------------------------------------------------------------

fn assert_hedged_and_unscheduled(fixture: &Fixture, disposition: ReminderDisposition) {
    let text = source_text_of(fixture);
    assert_eq!(
        disposition,
        ReminderDisposition::Recorded(ReminderRequestState::NotScheduledYet),
        "{text}"
    );
    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "not_scheduled_yet", "{text}");
    assert_eq!(reminder.schedule_state, "not_scheduled", "{text}");
    assert_eq!(reminder.resolved_instant, None, "{text}");
    assert_eq!(
        reminder.ambiguity_reason.as_deref(),
        Some("the request is hedged"),
        "{text}"
    );
    let (_, _, operations, _) = fixture.reminder_effects().expect("a reminder row");
    assert_eq!(operations, vec![], "{text}");
}

#[test]
fn the_maybe_friday_fixture_oracle_is_stored_as_an_ambiguous_reminder_needing_correction() {
    let (fixture, disposition) = {
        let text = "Maybe remind me Friday to water the plants?";
        let mut fixture = Fixture::captured(text, "2026-10-08T14:00:00Z", "America/New_York", -240);
        let lease = fixture.claim_job(0, capture_instant());
        let proposal = fixture
            .proposal_for(&lease, 0)
            .with_item_type(Some(ItemType::Action))
            .with_source_spans(Some(vec![span_of(text, text)]))
            .with_reminder_proposal(Some(ReminderProposal {
                instant: None,
                timezone_id: None,
                quality: TimeResolutionQuality::Ambiguous,
                source_span: Some(span_of(text, "Friday")),
            }));
        let outcome = fixture.apply(&lease, &proposal).unwrap();
        (fixture, applied_reminder(outcome))
    };
    assert_hedged_and_unscheduled(&fixture, disposition);
}

#[test]
fn a_hedged_request_with_a_clock_time_is_never_scheduled() {
    for text in [
        "Maybe remind me Friday at 3 p.m. to call Sam",
        "perhaps remind me Friday at 3 p.m. to call Sam",
        "Maybe please remind me Friday at 3 p.m. to call Sam",
        "possibly remind me Friday at 3 p.m. to call Sam",
        "Maybe can you remind me Friday at 3 p.m. to call Sam",
        "Perhaps can you remind me Friday at 3 p.m. to call Sam",
        "Possibly you can remind me Friday at 3 p.m. to call Sam",
        "Maybe could you please remind me Friday at 3 p.m. to call Sam",
        "Maybe I need you to remind me Friday at 3 p.m. to call Sam",
        "Maybe I'd like you to remind me Friday at 3 p.m. to call Sam",
        "Maybe, remind me Friday at 3 p.m. to call Sam",
        "Perhaps, remind me Friday at 3 p.m. to call Sam",
        "Okay, maybe, remind me Friday at 3 p.m. to call Sam",
        "Maybe. Remind me Friday at 3 p.m. to call Sam",
        "Maybe, can you remind me Friday at 3 p.m. to call Sam",
        // A hedge after an earlier sentence still reaches the request.
        "Busy week. Maybe remind me Friday at 3 p.m. to call Sam",
        "Sam called. Maybe. Remind me Friday at 3 p.m. to call Sam",
        "Water the plants. Maybe, remind me Friday at 3 p.m. to call Sam",
        "Water the plants. Okay, maybe. Remind me Friday at 3 p.m. to call Sam",
        "Okay so maybe, remind me Friday at 3 p.m. to call Sam",
        // Transcript punctuation is inferred, so a clause ending in a hedge counts as hedging.
        "Sam called maybe. Remind me Friday at 3 p.m. to call Sam",
    ] {
        // A proposal that quotes the time but claims no instant is stored as needing correction.
        let (fixture, disposition) = apply_clock_proposal_with(
            text,
            "Friday at 3 p.m.",
            None,
            TimeResolutionQuality::Ambiguous,
            "call Sam",
        );
        assert_hedged_and_unscheduled(&fixture, disposition);
        // One claiming an explicit instant, right or wrong, is rejected as a contradiction.
        for instant in ["2026-10-09T15:00:00-04:00", "2026-10-09T16:00:00-04:00"] {
            let (fixture, disposition) =
                apply_clock_proposal(text, "Friday at 3 p.m.", instant, "call Sam");
            assert_rejected_and_unscheduled_for(text, &fixture, disposition);
        }
    }
}

#[test]
fn a_hedged_recurring_request_is_kept_as_needing_correction_not_as_an_unsupported_recurrence() {
    // The hedge is decided before the phrase is read, so a recurrence does not move a hedged
    // request to the unsupported-recurrence state: it needs correction, with the hedge reason.
    for (text, quoted) in [
        (
            "Maybe remind me every Friday to water the plants",
            "every Friday",
        ),
        (
            "perhaps remind me every day to water the plants",
            "every day",
        ),
        (
            "Possibly can you remind me every Friday to water the plants",
            "every Friday",
        ),
        (
            "Maybe, remind me every Friday to water the plants",
            "every Friday",
        ),
    ] {
        let (fixture, disposition) = apply_clock_proposal_with(
            text,
            quoted,
            None,
            TimeResolutionQuality::Ambiguous,
            "water the plants",
        );
        assert_hedged_and_unscheduled(&fixture, disposition);
        assert_eq!(fixture.reminder_row().unsupported_reason, None, "{text}");
        // An explicit instant claimed for the recurrence is still rejected as a contradiction.
        let (fixture, disposition) = apply_clock_proposal(
            text,
            quoted,
            "2026-10-09T09:00:00-04:00",
            "water the plants",
        );
        assert_rejected_and_unscheduled_for(text, &fixture, disposition);
    }
}

#[test]
fn a_hedged_request_leaves_a_committed_recurrence_unchanged() {
    // A recurrence already recorded as unsupported is protected: when a correction hedges the
    // request, the hedged request neither reduces it to a one-shot nor rewrites its state.
    let text = "remind me to call the roofer every Monday";
    let mut fixture = Fixture::new(text);
    let first = fixture.claim_job(0, capture_instant());
    let recurring = fixture
        .action_proposal(&first, text, "call the roofer")
        .with_reminder_proposal(Some(reminder_candidate(
            text,
            "every Monday",
            None,
            TimeResolutionQuality::Ambiguous,
        )));
    assert_eq!(
        applied_reminder(fixture.apply(&first, &recurring).unwrap()),
        ReminderDisposition::Recorded(ReminderRequestState::UnsupportedRecurrence)
    );
    let stored = fixture.reminder_effects().unwrap();

    let corrected = "maybe remind me to call the roofer every Monday";
    let correction_id = fixture.correct(CorrectionKind::Text, Some(text), corrected, 0);
    let second = fixture.claim_job(1, capture_instant());
    let mut hedged = fixture.proposal_for(&second, 1);
    hedged.text_basis = TextBasis::Correction {
        correction_record_id: correction_id,
        item_revision: 1,
    };
    let hedged = hedged
        .with_item_type(Some(ItemType::Action))
        .with_source_spans(Some(vec![span_of(corrected, "call the roofer")]))
        .with_reminder_proposal(Some(reminder_candidate(
            corrected,
            "every Monday",
            None,
            TimeResolutionQuality::Ambiguous,
        )));
    assert_eq!(
        applied_reminder(fixture.apply(&second, &hedged).unwrap()),
        ReminderDisposition::ExistingKept
    );

    let reminder = fixture.reminder_row();
    assert_eq!(reminder.request_state, "unsupported_recurrence");
    assert_eq!(reminder.schedule_state, "not_scheduled");
    assert_eq!(reminder.resolved_instant, None);
    assert_eq!(reminder.ambiguity_reason, None);
    assert_eq!(fixture.reminder_effects().unwrap(), stored);
    assert!(stored.2.is_empty());
}

#[test]
fn a_hedge_does_not_turn_a_non_request_into_one() {
    for text in [
        "maybe Bob will remind me Friday at 3 p.m. to call Sam",
        "maybe I reminded him Friday at 3 p.m. to call Sam",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            "call Sam",
        );
        assert_eq!(disposition, ReminderDisposition::NoExplicitIntent, "{text}");
        assert_eq!(fixture.durable().reminders, 0, "{text}");
    }
}

#[test]
fn a_hedge_before_an_earlier_content_clause_does_not_reach_the_request() {
    // The walk back stops at the first clause with other words, so a hedge before it is that
    // clause's own and the request stays plain.
    let text = "Maybe Sam called. Remind me Friday at 3 p.m. to call Sam";
    let (fixture, disposition) = apply_clock_proposal(
        text,
        "Friday at 3 p.m.",
        "2026-10-09T15:00:00-04:00",
        "call Sam",
    );
    assert_scheduled_at(&fixture, disposition, "2026-10-09T19:00:00Z");
}

#[test]
fn a_quoted_hedge_in_an_earlier_clause_does_not_hedge_the_request() {
    // A quoted hedge is reported speech, not the speaker's own, so the request stays plain.
    for text in [
        "Sam said \"Maybe.\" Remind me Friday at 3 p.m. to call him",
        "\"Maybe.\" Remind me Friday at 3 p.m. to call Sam",
    ] {
        let (fixture, disposition) = apply_clock_proposal(
            text,
            "Friday at 3 p.m.",
            "2026-10-09T15:00:00-04:00",
            if text.ends_with("him") {
                "call him"
            } else {
                "call Sam"
            },
        );
        assert_scheduled_at(&fixture, disposition, "2026-10-09T19:00:00Z");
    }
}

#[test]
fn an_unhedged_clock_request_still_schedules() {
    let (fixture, disposition) = apply_clock_proposal(
        "Remind me Friday at 3 p.m. to call Sam",
        "Friday at 3 p.m.",
        "2026-10-09T15:00:00-04:00",
        "call Sam",
    );
    assert_scheduled_at(&fixture, disposition, "2026-10-09T19:00:00Z");
}

/// The split shape (F46): the day and the clock sit on either side of the task. The capture is
/// 2026-10-08T14:00:00Z, 10:00 EDT, so these instants are in America/New_York.
const SPLIT_SHAPE_CAPTURES: [(&str, &str, &str, &str); 7] = [
    (
        "Remind me today to call the dentist at 5 p.m.",
        "today to call the dentist at 5 p.m.",
        "at 5 p.m.",
        "2026-10-08T17:00:00-04:00",
    ),
    (
        "Remind me Friday to call Sam at 3 p.m.",
        "Friday to call Sam at 3 p.m.",
        "at 3 p.m.",
        "2026-10-09T15:00:00-04:00",
    ),
    (
        "Remind me at 10am to call the dentist tomorrow",
        "at 10am to call the dentist tomorrow",
        "at 10am",
        "2026-10-09T10:00:00-04:00",
    ),
    (
        "Remind me today to call the dentist at 15:00",
        "today to call the dentist at 15:00",
        "at 15:00",
        "2026-10-08T15:00:00-04:00",
    ),
    (
        "Remind me at 00:00 to call the dentist tomorrow",
        "at 00:00 to call the dentist tomorrow",
        "at 00:00",
        "2026-10-09T00:00:00-04:00",
    ),
    (
        "Remind me next Monday to call Sam at 8 a.m.",
        "next Monday to call Sam at 8 a.m.",
        "at 8 a.m.",
        "2026-10-12T08:00:00-04:00",
    ),
    (
        "Remind me at 3 p.m. to call Sam next Monday",
        "at 3 p.m. to call Sam next Monday",
        "at 3 p.m.",
        "2026-10-12T15:00:00-04:00",
    ),
];

#[test]
fn a_split_shape_reminder_schedules_with_either_span_form() {
    for (text, whole, clock, instant) in SPLIT_SHAPE_CAPTURES {
        for quoted in [whole, clock] {
            let (fixture, disposition) = apply_clock_proposal(text, quoted, instant, "call");
            assert_scheduled_at(&fixture, disposition, instant);
        }
    }
}

#[test]
fn a_split_shape_reminder_whose_instant_differs_from_the_combined_resolution_is_rejected() {
    for (text, whole, clock, instant) in SPLIT_SHAPE_CAPTURES {
        // The clock on the right hour but another day, and the day at another hour.
        let other_day = instant
            .replace("-08T", "-20T")
            .replace("-09T", "-21T")
            .replace("-12T", "-26T");
        let other_hour = instant
            .replace("T17:", "T18:")
            .replace("T00:", "T01:")
            .replace("T08:", "T09:")
            .replace("T15:", "T16:")
            .replace("T10:", "T11:");
        for wrong in [other_day.as_str(), other_hour.as_str()] {
            for quoted in [whole, clock] {
                let (fixture, disposition) = apply_clock_proposal(text, quoted, wrong, "call");
                assert_rejected_and_unscheduled(&fixture, disposition);
            }
        }
    }
}

#[test]
fn a_reminder_that_is_not_exactly_the_split_shape_is_held_for_correction() {
    for text in [
        "Remind me today to call Sam tomorrow at 5 p.m.",
        "Remind me today to call Sam at 5 p.m. or 6 p.m.",
        "Remind me today to call the dentist, then Sam, at 5 p.m.",
        "Remind me today to call the dentist for 2 fillings at 5 p.m.",
        "Talked to Jim on Monday. Remind me today to call the dentist at 5 p.m.",
        "Remind me today to call the dentist at 5 p.m. Actually, make it Saturday",
        "Remind me today to call the dentist by 5 p.m.",
    ] {
        let quoted = if text.contains("by 5") {
            "5 p.m."
        } else {
            "at 5 p.m."
        };
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) =
            apply_clock_proposal(text, quoted, "2026-10-08T17:00:00-04:00", "call");
        assert_held_for_correction(&fixture, disposition);
    }
    for text in [
        "Remind me at 5pm to call Sam tomorrow at 6pm",
        "Remind me at 5pm to call Sam on Friday tomorrow",
        "Remind me at 5pm to call the dentist (urgent) tomorrow",
        "Remind me at 5pm to call the dentist before tomorrow",
        "Remind me at 5pm to call the dentist tomorrow morning",
    ] {
        let span = span_of(text, "at 5pm");
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) =
            apply_clock_proposal(text, "at 5pm", "2026-10-08T17:00:00-04:00", "call");
        assert_held_for_correction(&fixture, disposition);
    }
}

#[test]
fn a_split_shape_part_that_is_not_complete_on_its_own_is_held_or_rejected() {
    for (text, quoted) in [
        ("Remind me today to call the dentist at 5", "at 5"),
        ("Remind me at 10 to call the dentist tomorrow", "at 10"),
    ] {
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) =
            apply_clock_proposal(text, quoted, "2026-10-08T17:00:00-04:00", "call");
        assert_ne!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert!(
            fixture
                .reminder_effects()
                .is_none_or(|(_, _, operations, _)| operations.is_empty()),
            "{text}"
        );
    }
}

#[test]
fn a_hedged_split_shape_reminder_is_never_scheduled() {
    for (text, quoted) in [
        (
            "Maybe remind me today to call the dentist at 5 p.m.",
            "at 5 p.m.",
        ),
        (
            "Remind me maybe today to call the dentist at 5 p.m.",
            "at 5 p.m.",
        ),
        (
            "Maybe remind me at 5pm to call the dentist tomorrow",
            "at 5pm",
        ),
    ] {
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) =
            apply_clock_proposal(text, quoted, "2026-10-08T17:00:00-04:00", "call");
        assert_ne!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert!(
            fixture
                .reminder_effects()
                .is_none_or(|(_, _, operations, _)| operations.is_empty()),
            "{text}"
        );
    }
}

#[test]
fn a_split_shape_reminder_with_a_repeat_word_is_an_unsupported_recurrence() {
    for (text, quoted) in [
        ("Remind me today to pay the recurring bill at 5pm", "at 5pm"),
        (
            "Remind me today to pay the recurring bill at 5pm",
            "today to pay the recurring bill at 5pm",
        ),
        (
            "Remind me at 5pm to pay the recurring bill tomorrow",
            "at 5pm",
        ),
        (
            "Remind me at 5pm to pay the recurring bill tomorrow",
            "at 5pm to pay the recurring bill tomorrow",
        ),
    ] {
        let (fixture, disposition) =
            apply_clock_proposal(text, quoted, "2026-10-08T17:00:00-04:00", "pay");
        assert_ne!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert!(
            fixture
                .reminder_effects()
                .is_none_or(|(_, _, operations, _)| operations.is_empty()),
            "{text}"
        );
    }
}

#[test]
fn a_split_shape_part_the_resolver_cannot_read_is_not_covered() {
    for (text, quoted) in [
        ("Remind me today to call the dentist at 1:00", "at 1:00"),
        ("Remind me at 1:00 to call the dentist tomorrow", "at 1:00"),
        ("Remind me tonight to call the dentist at 10pm", "at 10pm"),
        ("Remind me at 10pm to call the dentist tonight", "at 10pm"),
    ] {
        let span = span_of(text, quoted);
        assert!(
            !span_covers_time_expression(text, span.start, span.end),
            "{text}"
        );
        let (fixture, disposition) =
            apply_clock_proposal(text, quoted, "2026-10-08T22:00:00-04:00", "call");
        assert_ne!(
            disposition,
            ReminderDisposition::Recorded(ReminderRequestState::Resolved),
            "{text}"
        );
        assert!(
            fixture
                .reminder_effects()
                .is_none_or(|(_, _, operations, _)| operations.is_empty()),
            "{text}"
        );
    }
}
