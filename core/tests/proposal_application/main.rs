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
                 VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, ?)",
                rusqlite::params![capture_id, capture_text, CAPTURE_INSTANT, CAPTURE_INSTANT],
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
    let text = "remind me to call the roofer 2026-01-16 09:00:00 or 2026-01-17 09:00:00";
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
