//! Integration fixtures for queueing interpretation of every saved capture (I08). Captures enter
//! through the real foreground import and transcript attachment, and the queued jobs are run by
//! the real dispatcher with no provider profile configured, asserting on durable state.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::status::{ItemStatus, ProcessingJobStatus, ProcessingState};
use ohand_core::ingress::{
    apply_transcription_outcome, import_foreground_ingress, ImportDisposition,
    InterpretationQueued, InterpretationRequest, RecognizedTranscript, RecognizerResult,
    TranscriptionDisposition, TranscriptionOutcome,
};
use ohand_core::interpretation::apply::ApplyOutcome;
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, DispatchOutcome, InterpretationDispatcher, InterpretationRoute,
};
use ohand_core::interpretation::enqueue::{interpretation_job_id, interpretation_request_version};
use ohand_core::interpretation::instructions::M1_INSTRUCTION_VERSION;
use ohand_core::jobs::queue::{claim_job_with_lease, enqueue_job, get_jobs_for_item, Job};
use ohand_core::privacy::routing::{
    authorize_job, AuthorizationDecision, JOB_TYPE_INTERPRET, JOB_TYPE_TRANSCRIPTION_ATTACHMENT,
};
use ohand_core::providers::contracts::{CancelToken, FailureKind, ManualClock};
use ohand_core::retrieval::index::search_index;
use ohand_core::store::captures::Capture;
use ohand_core::store::events::ItemScope;
use ohand_core::store::schema::{Clock as StoreClock, Database};
use std::sync::Arc;
use tempfile::TempDir;

const NOW: &str = "2026-01-15T10:30:00Z";
const ROUTE_ID: &str = "route-1";
const AUDIO_REFERENCE: &str = "staging/synthetic-voice.m4a";
const COMMAND: &str = "Remind me 2026-02-20 14:30:00 to call mom";
const FREE_FORM: &str = "call the roofer about the leak";

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
        store
            .open()
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                 VALUES (?, 'private notes', 'personal', '[]', ?)",
                rusqlite::params![ROUTE_ID, NOW],
            )
            .unwrap();
        store
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(FixedClock)).unwrap()
    }

    fn jobs(&self, item_id: &str, job_type: &str) -> Vec<Job> {
        get_jobs_for_item(&self.open(), item_id)
            .unwrap()
            .into_iter()
            .filter(|job| job.job_type == job_type)
            .collect()
    }

    fn item_row(&self, item_id: &str) -> (String, Option<String>) {
        self.open()
            .conn()
            .query_row(
                "SELECT processing_state, item_type FROM items WHERE item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn reminder(&self, item_id: &str) -> Option<(String, Option<String>)> {
        use rusqlite::OptionalExtension;
        self.open()
            .conn()
            .query_row(
                "SELECT schedule_state, resolved_instant FROM reminders WHERE item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn status(&self, item_id: &str) -> ItemStatus {
        let mut database = self.open();
        let tx = database.immediate_transaction().unwrap();
        let status = ItemStatus::load(&tx, item_id).unwrap().unwrap();
        tx.commit().unwrap();
        status
    }

    fn search(&self, query: &str) -> usize {
        search_index(self.open().conn(), query, &[ItemScope::Personal])
            .unwrap()
            .len()
    }

    /// Claim the next queued job and run it through the dispatcher with no provider adapters.
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
}

fn capture(capture_id: &str, text: Option<&str>, audio_reference: Option<&str>) -> Capture {
    Capture::new(
        capture_id.to_string(),
        text.map(str::to_string),
        audio_reference.map(str::to_string),
        NOW.to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        ROUTE_ID.to_string(),
        false,
        NOW.to_string(),
        None,
    )
    .unwrap()
}

#[test]
fn an_imported_text_command_is_applied_by_the_fast_path_with_no_profiles() {
    let store = Store::new();
    let acknowledgment = import_foreground_ingress(
        &mut store.open(),
        &capture("3f9c1e52-0000-4000-8000-000000000001", Some(COMMAND), None),
    )
    .unwrap();
    let item_id = acknowledgment.item_id;

    let queued = store.jobs(&item_id, JOB_TYPE_INTERPRET);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].job_id, interpretation_job_id(&item_id, 0));
    assert_eq!(queued[0].source_revision, 0);
    assert_eq!(queued[0].profile_version, None);
    let request_version = queued[0].request_version.as_deref().unwrap();
    assert_eq!(request_version, interpretation_request_version(&item_id, 0));
    assert!(uuid::Uuid::parse_str(request_version).is_ok());
    assert_eq!(store.item_row(&item_id).0, "unprocessed");

    let (job, outcome) = store.run_next_job();

    assert_eq!(job.job_id, queued[0].job_id);
    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::FastPath,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        store.item_row(&item_id),
        ("processed".to_string(), Some("action".to_string()))
    );
    assert_eq!(
        store.reminder(&item_id),
        Some((
            "pending_schedule".to_string(),
            Some("2026-02-20T14:30:00Z".to_string())
        ))
    );
}

#[test]
fn an_unrecognized_capture_without_profiles_stays_saved_searchable_and_awaiting_a_provider() {
    let store = Store::new();
    let acknowledgment = import_foreground_ingress(
        &mut store.open(),
        &capture(
            "3f9c1e52-0000-4000-8000-000000000002",
            Some(FREE_FORM),
            None,
        ),
    )
    .unwrap();
    let item_id = acknowledgment.item_id;
    assert_eq!(store.search("roofer"), 1);

    let (_, outcome) = store.run_next_job();

    match outcome {
        DispatchOutcome::Failed { failure, .. } => {
            assert_eq!(failure.kind, FailureKind::CapabilityUnavailable)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(store.reminder(&item_id), None);
    assert_eq!(store.search("roofer"), 1);
    let status = store.status(&item_id);
    assert_eq!(status.processing_state, ProcessingState::Unprocessed);
    assert_eq!(
        status.processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    assert_eq!(store.item_row(&item_id).0, "unprocessed");
    let capture_text: String = store
        .open()
        .conn()
        .query_row(
            "SELECT text FROM captures WHERE capture_id = '3f9c1e52-0000-4000-8000-000000000002'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capture_text, FREE_FORM);
}

#[test]
fn retrying_an_import_never_queues_a_second_job() {
    let store = Store::new();
    let text_capture = capture("3f9c1e52-0000-4000-8000-000000000003", Some(COMMAND), None);

    let first = import_foreground_ingress(&mut store.open(), &text_capture).unwrap();
    let retry = import_foreground_ingress(&mut store.open(), &text_capture).unwrap();

    assert_eq!(first.disposition, ImportDisposition::Imported);
    assert_eq!(retry.disposition, ImportDisposition::AlreadyImported);
    assert_eq!(store.jobs(&first.item_id, JOB_TYPE_INTERPRET).len(), 1);

    let (_, outcome) = store.run_next_job();
    assert!(matches!(outcome, DispatchOutcome::Interpreted { .. }));
    let after_work = import_foreground_ingress(&mut store.open(), &text_capture).unwrap();
    assert_eq!(after_work.item_id, first.item_id);
    assert_eq!(store.jobs(&first.item_id, JOB_TYPE_INTERPRET).len(), 1);
}

#[test]
fn an_audio_only_capture_queues_no_interpretation_at_import() {
    let store = Store::new();
    let acknowledgment = import_foreground_ingress(
        &mut store.open(),
        &capture(
            "3f9c1e52-0000-4000-8000-000000000004",
            None,
            Some(AUDIO_REFERENCE),
        ),
    )
    .unwrap();

    assert!(store
        .jobs(&acknowledgment.item_id, JOB_TYPE_INTERPRET)
        .is_empty());
}

fn claim_transcription_job(store: &Store, item_id: &str) -> Job {
    let mut database = store.open();
    // Replace the job import queued with this one, as a test that scripts its own job.
    database
        .conn()
        .execute(
            "DELETE FROM jobs WHERE item_id = ? AND job_type = ? AND status = 'queued'",
            rusqlite::params![item_id, JOB_TYPE_TRANSCRIPTION_ATTACHMENT],
        )
        .unwrap();
    enqueue_job(
        &mut database,
        "transcription-1".to_string(),
        item_id.to_string(),
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
        0,
        None,
        None,
        1,
        now(),
    )
    .unwrap();
    claim_job_with_lease(&mut database, Duration::seconds(300), now())
        .unwrap()
        .expect("the transcription job is eligible")
}

fn transcript_outcome(
    job: &Job,
    interpretation: Option<InterpretationRequest>,
) -> TranscriptionOutcome {
    TranscriptionOutcome {
        job_id: job.job_id.clone(),
        lease_attempt: job.attempt_count,
        audio_reference: AUDIO_REFERENCE.to_string(),
        result: RecognizerResult::Transcript(RecognizedTranscript {
            text: COMMAND.to_string(),
            confidence: Some(0.9),
            detected_language: Some("en-US".to_string()),
            recognizer: "on-device-recognizer/1".to_string(),
            audio_sha256: "a".repeat(64),
        }),
        interpretation,
    }
}

#[test]
fn a_voice_capture_queues_exactly_one_job_when_its_transcript_attaches_and_it_is_interpreted() {
    let store = Store::new();
    let item_id = import_foreground_ingress(
        &mut store.open(),
        &capture(
            "3f9c1e52-0000-4000-8000-000000000004",
            None,
            Some(AUDIO_REFERENCE),
        ),
    )
    .unwrap()
    .item_id;
    let transcription_job = claim_transcription_job(&store, &item_id);
    let outcome = transcript_outcome(&transcription_job, None);

    let acknowledgment = apply_transcription_outcome(&mut store.open(), &outcome, now()).unwrap();
    let expected_job_id = interpretation_job_id(&item_id, 1);
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached {
            revision: 1,
            interpretation: InterpretationQueued::Queued {
                job_id: expected_job_id.clone()
            },
        }
    );
    let redelivery = apply_transcription_outcome(&mut store.open(), &outcome, now()).unwrap();
    assert!(matches!(
        redelivery.disposition,
        TranscriptionDisposition::AlreadyAttached { .. }
    ));

    let queued = store.jobs(&item_id, JOB_TYPE_INTERPRET);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].job_id, expected_job_id);
    assert_eq!(queued[0].source_revision, 1);
    assert_eq!(queued[0].profile_version, None);
    let mut database = store.open();
    let transaction = database.immediate_transaction().unwrap();
    assert!(matches!(
        authorize_job(&transaction, &expected_job_id).unwrap(),
        AuthorizationDecision::Authorized(_)
    ));
    drop(transaction);

    let (job, interpreted) = store.run_next_job();
    assert_eq!(job.job_id, expected_job_id);
    assert!(
        matches!(
            interpreted,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::FastPath,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{interpreted:?}"
    );
    assert!(store.reminder(&item_id).is_some());
}

#[test]
fn native_code_cannot_choose_the_interpretation_destination() {
    let store = Store::new();
    let item_id = import_foreground_ingress(
        &mut store.open(),
        &capture(
            "3f9c1e52-0000-4000-8000-000000000004",
            None,
            Some(AUDIO_REFERENCE),
        ),
    )
    .unwrap()
    .item_id;
    let transcription_job = claim_transcription_job(&store, &item_id);
    let outcome = transcript_outcome(
        &transcription_job,
        Some(InterpretationRequest {
            job_id: "native-chosen-job".to_string(),
            profile_version: Some("native-chosen-profile".to_string()),
            request_version: M1_INSTRUCTION_VERSION.to_string(),
        }),
    );

    let refused = apply_transcription_outcome(&mut store.open(), &outcome, now());

    assert!(refused.is_err());
    assert!(store.jobs(&item_id, JOB_TYPE_INTERPRET).is_empty());
    assert_eq!(store.search("mom"), 0);
}
