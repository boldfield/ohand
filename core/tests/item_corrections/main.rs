//! Behavior tests for the user's item corrections (A02) against the real core store. Every test
//! asserts on durable state re-read through a fresh connection to the file.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::items::ProposalApplicationError;
use ohand_core::ffi::item_corrections::{
    apply_item_correction, CorrectionChange, CorrectionOutcome, ItemCorrection,
};
use ohand_core::interpretation::apply::{apply_interpretation_proposal, ApplyError, ApplyOutcome};
use ohand_core::interpretation::contracts::{
    Proposal, ReminderProposal, SourceSpan, TextBasis, TimeResolutionQuality,
};
use ohand_core::jobs::queue::{claim_job_with_lease, enqueue_job};
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::reminders::state::{
    desired_notifications, get_reminder, list_operations, OperationType, RequestState,
};
use ohand_core::store::events::ItemType;
use ohand_core::store::schema::{Clock, Database};
use std::sync::Arc;
use uuid::Uuid;

const CAPTURE_INSTANT: &str = "2026-01-15T10:30:00Z";
const REMINDER_TEXT: &str = "remind me to call the roofer 2026-01-16 09:00:00";

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

fn open(path: &str) -> Database {
    Database::open(path, Arc::new(FixedClock)).unwrap()
}

struct Lease {
    job_id: String,
    attempt: i32,
    request_version: String,
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

impl Fixture {
    fn new(capture_text: &str) -> Fixture {
        let path = format!(
            "{}/test_item_corrections_{}.db",
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

    fn claim_job(&mut self, source_revision: i32) -> Lease {
        let job_id = Uuid::new_v4().to_string();
        let request_version = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            source_revision,
            None,
            Some(request_version.clone()),
            1,
            capture_instant(),
        )
        .unwrap();
        let claimed = claim_job_with_lease(&mut self.db, Duration::seconds(60), capture_instant())
            .unwrap()
            .expect("job should be claimable");
        assert_eq!(claimed.job_id, job_id);
        Lease {
            job_id,
            attempt: claimed.attempt_count,
            request_version,
        }
    }

    fn model_result(&self, lease: &Lease, source_revision: i32, text: &str) -> Proposal {
        let start = text.find("call the roofer").unwrap();
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
        .with_item_type(Some(ItemType::Action))
        .with_source_spans(Some(vec![SourceSpan::new(
            text[..start].chars().count(),
            text[..start].chars().count() + "call the roofer".chars().count(),
        )]))
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

    fn run(
        &mut self,
        command_id: &str,
        expected_revision: i32,
        change: CorrectionChange,
    ) -> CorrectionOutcome {
        let correction = self.correction(command_id, expected_revision, change);
        apply_item_correction(&mut self.db, &correction).unwrap()
    }

    fn correct(&mut self, command_id: &str, expected_revision: i32, change: CorrectionChange) {
        let outcome = self.run(command_id, expected_revision, change);
        assert!(
            matches!(outcome, CorrectionOutcome::Corrected { .. }),
            "{outcome:?}"
        );
    }

    fn correction(
        &self,
        command_id: &str,
        expected_revision: i32,
        change: CorrectionChange,
    ) -> ItemCorrection {
        ItemCorrection {
            command_id: command_id.to_string(),
            item_id: self.item_id.clone(),
            expected_revision,
            change,
        }
    }

    /// The durable state this feature must never lose, read through a fresh connection.
    fn durable(&self) -> Durable {
        let db = open(&self.path);
        let conn = db.conn();
        let count = |sql: &str| -> i64 {
            conn.query_row(sql, [&self.item_id], |row| row.get(0))
                .unwrap()
        };
        let (item_type, revision, capture_text): (Option<String>, i32, Option<String>) = conn
            .query_row(
                "SELECT i.item_type, i.revision, c.text FROM items i
                 JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        Durable {
            item_type,
            revision,
            capture_text,
            proposals: count("SELECT COUNT(*) FROM proposals WHERE item_id = ?"),
            events: count("SELECT COUNT(*) FROM events WHERE item_id = ?"),
            reminders: count("SELECT COUNT(*) FROM reminders WHERE item_id = ?"),
        }
    }

    fn reminder_snapshot(&self) -> (RequestState, Vec<OperationType>, usize) {
        let mut db = open(&self.path);
        let tx = db.immediate_transaction().unwrap();
        let record = get_reminder(&tx, &self.item_id).unwrap().unwrap();
        let operations = list_operations(&tx, &record.reminder_id)
            .unwrap()
            .iter()
            .map(|operation| operation.operation_type)
            .collect();
        let desired = desired_notifications(&tx).unwrap().len();
        (record.request_state, operations, desired)
    }
}

#[derive(Debug, PartialEq)]
struct Durable {
    item_type: Option<String>,
    revision: i32,
    capture_text: Option<String>,
    proposals: i64,
    events: i64,
    reminders: i64,
}

#[test]
fn a_correction_made_while_an_interpretation_job_is_in_flight_beats_the_late_result() {
    let text = "call the roofer about the leak";
    let mut fixture = Fixture::new(text);
    let lease = fixture.claim_job(0);

    // The user corrects type and text while the job still runs against revision 0.
    fixture.correct("c-1", 0, CorrectionChange::Type(ItemType::Idea));
    fixture.correct(
        "c-2",
        1,
        CorrectionChange::Text("call the plumber about the leak".to_string()),
    );
    let before = fixture.durable();
    assert_eq!(before.item_type.as_deref(), Some("idea"));
    assert_eq!(before.revision, 2);

    let late = fixture.model_result(&lease, 0, text);
    let error = fixture.apply(&lease, &late).unwrap_err();

    assert!(
        matches!(
            error,
            ApplyError::StaleRevision {
                proposal_revision: 0,
                current_revision: 2
            }
        ),
        "{error:?}"
    );
    assert_eq!(fixture.durable(), before, "the late result changed nothing");
}

#[test]
fn a_type_correction_alone_is_never_overwritten_by_a_result_computed_after_it() {
    let text = "call the roofer";
    let mut fixture = Fixture::new(text);
    fixture.correct("c-1", 0, CorrectionChange::Type(ItemType::Note));
    let lease = fixture.claim_job(1);
    let before = fixture.durable();
    let result = fixture.model_result(&lease, 1, text);
    let mut result = result;
    result.text_basis = TextBasis::Original { item_revision: 1 };

    let error = fixture.apply(&lease, &result).unwrap_err();

    assert!(
        matches!(
            error,
            ApplyError::Rejected(ProposalApplicationError::ForbiddenByUserCorrection(_))
        ),
        "{error:?}"
    );
    assert_eq!(fixture.durable(), before);
}

#[test]
fn correcting_an_action_with_a_live_reminder_to_another_type_cancels_the_reminder() {
    let mut fixture = Fixture::new(REMINDER_TEXT);
    let lease = fixture.claim_job(0);
    let start = REMINDER_TEXT.find("2026-01-16 09:00:00").unwrap();
    let result = fixture
        .model_result(&lease, 0, REMINDER_TEXT)
        .with_reminder_proposal(Some(ReminderProposal {
            instant: Some("2026-01-16T09:00:00Z".to_string()),
            timezone_id: Some("UTC".to_string()),
            quality: TimeResolutionQuality::Explicit,
            source_span: Some(SourceSpan::new(start, start + "2026-01-16 09:00:00".len())),
        }));
    fixture.apply(&lease, &result).unwrap();
    let live = fixture.durable();
    assert_eq!(live.item_type.as_deref(), Some("action"));
    assert_eq!(
        fixture.reminder_snapshot(),
        (RequestState::Resolved, vec![OperationType::Schedule], 1)
    );

    let outcome = fixture.run("c-1", live.revision, CorrectionChange::Type(ItemType::Note));

    assert_eq!(
        outcome,
        CorrectionOutcome::Corrected {
            revision: live.revision + 1,
            replayed: false,
            reminder_cancelled: true
        }
    );
    let (state, operations, desired) = fixture.reminder_snapshot();
    assert_eq!(state, RequestState::Cancelled);
    assert_eq!(
        operations,
        vec![OperationType::Schedule, OperationType::Cancel]
    );
    assert_eq!(desired, 0, "nothing is left for the OS to hold");
    let after = fixture.durable();
    assert_eq!(after.item_type.as_deref(), Some("note"));
    assert_eq!(after.revision, live.revision + 1);
    assert_eq!(after.reminders, 1, "the reminder record stays inspectable");
}

#[test]
fn corrections_never_alter_the_capture_and_replays_leave_everything_untouched() {
    let text = "Original words";
    let mut fixture = Fixture::new(text);
    let correction =
        fixture.correction("c-1", 0, CorrectionChange::Text("Better words".to_string()));

    let first = apply_item_correction(&mut fixture.db, &correction).unwrap();
    let after_first = fixture.durable();
    let second = apply_item_correction(&mut fixture.db, &correction).unwrap();

    assert_eq!(
        first,
        CorrectionOutcome::Corrected {
            revision: 1,
            replayed: false,
            reminder_cancelled: false
        }
    );
    assert_eq!(
        second,
        CorrectionOutcome::Corrected {
            revision: 1,
            replayed: true,
            reminder_cancelled: false
        }
    );
    assert_eq!(fixture.durable(), after_first);
    assert_eq!(after_first.capture_text.as_deref(), Some(text));
    assert_eq!(after_first.events, 1);
}

#[test]
fn a_stale_revision_reports_the_current_one_and_a_deleted_item_refuses() {
    let mut fixture = Fixture::new("Original words");
    fixture.correct(
        "c-1",
        0,
        CorrectionChange::SessionTopic("Garden".to_string()),
    );
    let before = fixture.durable();

    let stale = fixture.run("c-2", 0, CorrectionChange::Type(ItemType::Idea));
    assert_eq!(
        stale,
        CorrectionOutcome::StaleRevision {
            current_revision: 1
        }
    );
    assert_eq!(fixture.durable(), before);

    fixture
        .db
        .conn()
        .execute("UPDATE items SET lifecycle_state = 'deleted'", [])
        .unwrap();
    let deleted = fixture.run("c-3", 1, CorrectionChange::Text("late".to_string()));
    assert_eq!(deleted, CorrectionOutcome::ItemDeleted);
    assert_eq!(fixture.durable(), before);
}
