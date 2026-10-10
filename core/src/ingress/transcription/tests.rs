use super::*;
use crate::domain::items::verify_state_integrity;
use crate::ingress::import::import_foreground_ingress;
use crate::jobs::queue::{claim_job_with_lease, enqueue_job, get_job, get_jobs_for_item};
use crate::lifecycle::delete_intent::mark_deletion_intent;
use crate::retrieval::index::search_index;
use crate::store::captures::Capture;
use crate::store::events::{save_event, ItemScope};
use crate::store::schema::SystemClock;
use chrono::Duration;
use std::sync::Arc;
use tempfile::TempDir;

const CAPTURE_ID: &str = "3f9c1e52-0000-4000-8000-0000000000c5";
const ROUTE_ID: &str = "route-personal";
const AUDIO_REFERENCE: &str = "staging/2026-10-08/clip-5.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const TRANSCRIPT: &str = "remind me to water the ferns tomorrow";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-08T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn later() -> DateTime<Utc> {
    now() + Duration::minutes(5)
}

struct Store {
    _directory: TempDir,
    path: String,
    item_id: String,
}

impl Store {
    /// A store holding one imported voice capture and no jobs.
    fn with_voice_capture() -> Store {
        Store::with_capture(voice_capture())
    }

    fn with_capture(capture: Capture) -> Store {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("ohand.db").display().to_string();
        let mut store = Store {
            _directory: directory,
            path,
            item_id: String::new(),
        };
        let mut database = store.open();
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                 VALUES (?, ?, 'personal', '[]', '2026-10-08T09:00:00Z')",
                rusqlite::params![ROUTE_ID, ROUTE_ID],
            )
            .unwrap();
        store.item_id = import_foreground_ingress(&mut database, &capture)
            .unwrap()
            .item_id;
        store
    }

    /// Opening again stands in for a restart.
    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }

    /// Queue a transcription job at the item's current revision and claim it, as the runner does.
    fn claim_transcription_job(&self, job_id: &str) -> Job {
        self.claim_transcription_job_with_request(job_id, None)
    }

    fn claim_transcription_job_with_request(
        &self,
        job_id: &str,
        request_version: Option<&str>,
    ) -> Job {
        let mut database = self.open();
        let revision: i32 = database
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| row.get(0),
            )
            .unwrap();
        enqueue_job(
            &mut database,
            job_id.to_string(),
            self.item_id.clone(),
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
            revision,
            None,
            request_version.map(str::to_string),
            1,
            now(),
        )
        .unwrap();
        let claimed = claim_job_with_lease(&mut database, Duration::seconds(300), now())
            .unwrap()
            .expect("the job is eligible");
        assert_eq!(claimed.job_id, job_id);
        claimed
    }

    fn scalar_text(&self, sql: &str) -> String {
        self.open()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        self.open()
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn item_row(&self) -> (String, String, String, i32) {
        self.open()
            .conn()
            .query_row(
                "SELECT save_state, processing_state, transcription_state, revision
                   FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
    }

    fn job(&self, job_id: &str) -> Job {
        get_job(&self.open(), job_id).unwrap().unwrap()
    }

    fn search(&self, query: &str) -> Vec<crate::retrieval::index::SearchHit> {
        search_index(self.open().conn(), query, &[ItemScope::Personal]).unwrap()
    }

    fn interpretation_jobs(&self) -> Vec<Job> {
        get_jobs_for_item(&self.open(), &self.item_id)
            .unwrap()
            .into_iter()
            .filter(|job| job.job_type == JOB_TYPE_INTERPRET)
            .collect()
    }
}

fn voice_capture() -> Capture {
    Capture::new(
        CAPTURE_ID.to_string(),
        None,
        Some(AUDIO_REFERENCE.to_string()),
        "2026-10-08T09:30:00Z".to_string(),
        "America/New_York".to_string(),
        -240,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        ROUTE_ID.to_string(),
        false,
        "2026-10-08T09:30:01Z".to_string(),
        None,
    )
    .unwrap()
}

fn typed_capture() -> Capture {
    Capture {
        text: Some("typed note".to_string()),
        audio_reference: None,
        ..voice_capture()
    }
}

fn recognized(text: &str) -> RecognizerResult {
    RecognizerResult::Transcript(RecognizedTranscript {
        text: text.to_string(),
        confidence: Some(0.91),
        detected_language: Some("en-US".to_string()),
        recognizer: "on-device-recognizer/1".to_string(),
        audio_sha256: AUDIO_SHA256.to_string(),
    })
}

fn outcome(job: &Job, result: RecognizerResult) -> TranscriptionOutcome {
    TranscriptionOutcome {
        job_id: job.job_id.clone(),
        lease_attempt: job.attempt_count,
        audio_reference: AUDIO_REFERENCE.to_string(),
        result,
        interpretation: None,
    }
}

fn with_interpretation(mut outcome: TranscriptionOutcome, job_id: &str) -> TranscriptionOutcome {
    outcome.interpretation = Some(InterpretationRequest {
        job_id: job_id.to_string(),
        profile_version: None,
        request_version: "request-v1".to_string(),
    });
    outcome
}

fn kind_of(error: &TranscriptionError) -> &TranscriptionErrorKind {
    &error.kind
}

fn user_text_correction(item_id: &str, revision: i32, text: &str) -> Event {
    Event::new(
        format!("user-text-{revision}"),
        item_id.to_string(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: None,
            new_value: text.to_string(),
        }),
        "2026-10-08T09:45:00Z".to_string(),
    )
    .unwrap()
}

#[test]
fn a_transcript_becomes_the_item_text_with_provenance_in_one_transaction() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");

    let acknowledgment = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    assert_eq!(acknowledgment.item_id, store.item_id);
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached {
            revision: 1,
            interpretation: InterpretationQueued::Queued {
                job_id: "interpret-1".to_string()
            },
        }
    );

    // Saved audio, transcript completion and interpretation completion are separate facts.
    let (save, processing, transcription, revision) = store.item_row();
    assert_eq!(save, "saved_local");
    assert_eq!(transcription, "transcribed");
    assert_eq!(processing, "unprocessed", "interpretation has not run");
    assert_eq!(revision, 1);

    let interpretation = store.job("interpret-1");
    assert_eq!(interpretation.status, JobStatus::Queued);
    assert_eq!(interpretation.source_revision, 1);
    assert_eq!(interpretation.profile_version, None);
    assert_eq!(store.job("transcription-1").status, JobStatus::Completed);

    // Provenance: recognizer, original-audio identity and the successful-transcription time.
    let (job_id, digest, recognizer, language, confidence, transcribed_at, attached): (
        String,
        String,
        String,
        Option<String>,
        Option<f64>,
        String,
        i32,
    ) = store
        .open()
        .conn()
        .query_row(
            "SELECT source_job_id, audio_sha256, recognizer, detected_language, confidence,
                    transcribed_at, attached_revision
               FROM transcript_attachments WHERE item_id = ?",
            [&store.item_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(job_id, "transcription-1");
    assert_eq!(digest, AUDIO_SHA256);
    assert_eq!(recognizer, "on-device-recognizer/1");
    assert_eq!(language.as_deref(), Some("en-US"));
    assert_eq!(confidence, Some(0.91));
    assert_eq!(
        DateTime::parse_from_rfc3339(&transcribed_at).unwrap(),
        later()
    );
    assert_eq!(attached, 1);

    // The transcript is searchable and shown as the current text; the capture is untouched.
    let hits = store.search("ferns");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].current_text, TRANSCRIPT);
    assert_eq!(
        store.scalar_text("SELECT audio_reference FROM captures"),
        AUDIO_REFERENCE
    );
    let capture_text: Option<String> = store
        .open()
        .conn()
        .query_row("SELECT text FROM captures", [], |row| row.get(0))
        .unwrap();
    assert_eq!(capture_text, None);

    let mut database = store.open();
    let transaction = database.immediate_transaction().unwrap();
    assert!(verify_state_integrity(&transaction, &store.item_id)
        .unwrap()
        .is_none());
}

#[test]
fn attaching_without_an_interpretation_request_queues_no_interpretation() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");

    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached {
            revision: 1,
            interpretation: InterpretationQueued::NotRequested,
        }
    );
    assert!(store.interpretation_jobs().is_empty());
}

#[test]
fn an_unauthorized_interpretation_is_not_queued_but_the_transcript_is_kept() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let mut request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");
    request.interpretation.as_mut().unwrap().profile_version =
        Some("profile-that-does-not-exist".to_string());

    let acknowledgment = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached {
            revision: 1,
            interpretation: InterpretationQueued::NotAuthorized(DenialReason::ProfileUnavailable),
        }
    );
    assert!(store.interpretation_jobs().is_empty());
    assert_eq!(store.item_row().2, "transcribed");
    assert_eq!(store.search("ferns").len(), 1);
}

#[test]
fn redelivery_after_commit_returns_the_original_result_and_duplicates_nothing() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");

    let first = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    let retry =
        apply_transcription_outcome(&mut store.open(), &request, later() + Duration::hours(1))
            .unwrap();

    assert_eq!(
        retry.disposition,
        match first.disposition {
            TranscriptionDisposition::Attached {
                revision,
                interpretation,
            } => TranscriptionDisposition::AlreadyAttached {
                revision,
                interpretation,
            },
            other => panic!("unexpected first disposition {other:?}"),
        }
    );
    assert_eq!(store.count("transcript_attachments"), 1);
    assert_eq!(store.count("corrections"), 1);
    assert_eq!(store.count("events"), 1);
    assert_eq!(store.interpretation_jobs().len(), 1);
    assert_eq!(store.item_row().3, 1, "the revision is bumped once");
    let transcribed_at = store.scalar_text("SELECT transcribed_at FROM transcript_attachments");
    assert_eq!(
        DateTime::parse_from_rfc3339(&transcribed_at).unwrap(),
        later(),
        "a retry keeps the original success time"
    );
}

#[test]
fn a_different_transcript_for_an_attached_job_is_a_conflict() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();

    let error = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized("something else entirely")),
        later(),
    )
    .unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::ConflictingResult
    ));
    assert_eq!(store.search("ferns").len(), 1);
    assert!(store.search("entirely").is_empty());
}

#[test]
fn a_failure_before_commit_rolls_back_everything_and_the_retry_succeeds() {
    for failing_stage in [
        TranscriptionStage::EventWritten,
        TranscriptionStage::AttachmentWritten,
        TranscriptionStage::JobSettled,
        TranscriptionStage::InterpretationQueued,
    ] {
        let store = Store::with_voice_capture();
        let job = store.claim_transcription_job("transcription-1");
        let request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");

        let error = apply_with_fault_points(&mut store.open(), &request, later(), &mut |stage| {
            if stage == failing_stage {
                Err(anyhow!("injected failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(
            error.commit_status,
            CommitStatus::NotCommitted,
            "{failing_stage:?}"
        );
        assert!(matches!(
            kind_of(&error),
            TranscriptionErrorKind::Storage(_)
        ));

        assert_eq!(
            store.count("transcript_attachments"),
            0,
            "{failing_stage:?}"
        );
        assert_eq!(store.count("events"), 0, "{failing_stage:?}");
        assert_eq!(store.count("search_index"), 0, "{failing_stage:?}");
        assert!(store.interpretation_jobs().is_empty(), "{failing_stage:?}");
        assert_eq!(store.item_row().2, "audio_pending", "{failing_stage:?}");
        assert_eq!(store.item_row().3, 0, "{failing_stage:?}");
        assert_eq!(
            store.job("transcription-1").status,
            JobStatus::Running,
            "{failing_stage:?}"
        );

        let acknowledgment =
            apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
        assert!(
            matches!(
                acknowledgment.disposition,
                TranscriptionDisposition::Attached { .. }
            ),
            "{failing_stage:?}"
        );
        assert_eq!(store.count("transcript_attachments"), 1);
        assert_eq!(store.interpretation_jobs().len(), 1);
    }
}

#[test]
fn a_lost_acknowledgment_reports_committed_and_the_retry_is_idempotent() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");

    let error = apply_with_fault_points(&mut store.open(), &request, later(), &mut |stage| {
        if stage == TranscriptionStage::Committed {
            Err(anyhow!("process interrupted after commit"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.commit_status, CommitStatus::Committed);
    assert_eq!(store.count("transcript_attachments"), 1);

    let retry = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    assert!(matches!(
        retry.disposition,
        TranscriptionDisposition::AlreadyAttached { revision: 1, .. }
    ));
    assert_eq!(store.count("events"), 1);
    assert_eq!(store.interpretation_jobs().len(), 1);
}

#[test]
fn unsupported_failed_and_empty_outcomes_keep_the_audio_and_an_explicit_pending_state() {
    let cases = [
        (
            RecognizerResult::Unsupported,
            "transcription_unsupported",
            UNSUPPORTED_REASON,
        ),
        (
            RecognizerResult::Failed,
            "transcription_failed",
            FAILED_REASON,
        ),
        (recognized("   \n"), "audio_pending", NO_SPEECH_REASON),
    ];
    for (result, expected_state, expected_reason) in cases {
        let store = Store::with_voice_capture();
        let job = store.claim_transcription_job("transcription-1");
        let request = with_interpretation(outcome(&job, result), "interpret-1");

        let acknowledgment =
            apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
        assert!(
            matches!(
                &acknowledgment.disposition,
                TranscriptionDisposition::Pending { state } if state.as_str() == expected_state
            ),
            "{expected_state}: {acknowledgment:?}"
        );

        let (save, processing, transcription, revision) = store.item_row();
        assert_eq!(save, "saved_local");
        assert_eq!(processing, "unprocessed");
        assert_eq!(transcription, expected_state);
        assert_eq!(revision, 0, "no text was attached");
        assert_eq!(store.count("transcript_attachments"), 0);
        assert_eq!(store.count("events"), 0);
        assert_eq!(store.count("search_index"), 0);
        assert!(store.interpretation_jobs().is_empty());
        assert_eq!(
            store.scalar_text("SELECT audio_reference FROM captures"),
            AUDIO_REFERENCE
        );
        let ended = store.job("transcription-1");
        assert_eq!(ended.status, JobStatus::Failed);
        assert_eq!(ended.failure_reason.as_deref(), Some(expected_reason));

        // The same delivery again changes nothing and still reports the pending state.
        let retry = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
        assert_eq!(retry.disposition, acknowledgment.disposition);
    }
}

#[test]
fn a_later_job_can_attach_after_an_unsupported_outcome() {
    let store = Store::with_voice_capture();
    let first = store.claim_transcription_job("transcription-1");
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&first, RecognizerResult::Unsupported),
        now(),
    )
    .unwrap();
    assert_eq!(store.item_row().2, "transcription_unsupported");

    let retry = store.claim_transcription_job_with_request("transcription-2", Some("retry-1"));
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&retry, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();
    assert!(matches!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached { .. }
    ));
    assert_eq!(store.item_row().2, "transcribed");
}

#[test]
fn a_late_result_after_a_user_correction_cannot_replace_the_corrected_text() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");

    save_event(
        &mut store.open(),
        &user_text_correction(&store.item_id, 0, "water the ferns on friday"),
        0,
    )
    .unwrap();

    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1"),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::TextAlreadyPresent)
    );

    let hits = store.search("ferns");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].current_text, "water the ferns on friday");
    assert_eq!(store.count("transcript_attachments"), 0);
    assert_eq!(store.count("corrections"), 1);
    assert_eq!(store.item_row().2, "audio_pending");
    assert!(store.interpretation_jobs().is_empty());
    let retired = store.job("transcription-1");
    assert_eq!(retired.status, JobStatus::Cancelled);
    assert_eq!(
        retired.failure_reason.as_deref(),
        Some("text_already_present")
    );

    // An unsupported result arriving that late is also ignored.
    let second = store.claim_transcription_job("transcription-2");
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&second, RecognizerResult::Failed),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::TextAlreadyPresent)
    );
    assert_eq!(store.item_row().2, "audio_pending");
}

#[test]
fn a_user_correction_of_the_transcript_is_a_normal_correction() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();

    let mut correction = user_text_correction(&store.item_id, 1, "water the ferns on friday");
    correction.payload = EventPayload::Correction(Correction {
        kind: CorrectionKind::Text,
        old_value: Some(TRANSCRIPT.to_string()),
        new_value: "water the ferns on friday".to_string(),
    });
    save_event(&mut store.open(), &correction, 1).unwrap();

    let hits = store.search("friday");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].current_text, "water the ferns on friday");
}

#[test]
fn a_late_result_after_deletion_attaches_nothing() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    mark_deletion_intent(&mut store.open(), &store.item_id, 0, now()).unwrap();

    // Deletion cancels the job, so the lease is gone.
    let error = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::LeaseNotHeld { .. }
    ));

    // Even when the job still looks leased, the lifecycle guard refuses.
    store
        .open()
        .conn()
        .execute(
            "UPDATE jobs SET status = 'running' WHERE job_id = 'transcription-1'",
            [],
        )
        .unwrap();
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1"),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::ItemDeleted)
    );
    assert_eq!(store.count("transcript_attachments"), 0);
    assert_eq!(store.count("search_index"), 0);
    assert!(store.interpretation_jobs().is_empty());
    assert_eq!(
        store.scalar_text(
            "SELECT lifecycle_state FROM items WHERE item_id IN (SELECT item_id FROM items)"
        ),
        "deleted"
    );
    assert_eq!(
        store.scalar_text("SELECT COALESCE(GROUP_CONCAT(correction_new_value), '') FROM events"),
        ""
    );
}

#[test]
fn a_redelivery_after_the_item_was_deleted_reveals_and_restores_nothing() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let request = outcome(&job, recognized(TRANSCRIPT));
    apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    mark_deletion_intent(&mut store.open(), &store.item_id, 1, later()).unwrap();

    let retry = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap();
    assert_eq!(
        retry.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::ItemDeleted)
    );
    assert_eq!(store.count("search_index"), 0);
    assert_eq!(
        store.scalar_text("SELECT COALESCE(GROUP_CONCAT(new_value), '') FROM corrections"),
        ""
    );
}

#[test]
fn a_second_job_cannot_replace_an_attached_transcript() {
    let store = Store::with_voice_capture();
    let first = store.claim_transcription_job("transcription-1");
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&first, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();

    let second = store.claim_transcription_job("transcription-2");
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&second, recognized("a different transcript")),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AlreadyTranscribed)
    );
    assert_eq!(store.count("transcript_attachments"), 1);
    assert_eq!(store.search("ferns")[0].current_text, TRANSCRIPT);
    assert_eq!(store.item_row().3, 1);
}

#[test]
fn a_result_for_different_or_missing_audio_is_not_attached() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let mut wrong_audio = outcome(&job, recognized(TRANSCRIPT));
    wrong_audio.audio_reference = "staging/2026-10-08/other.m4a".to_string();
    let acknowledgment =
        apply_transcription_outcome(&mut store.open(), &wrong_audio, later()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioIdentityMismatch)
    );
    assert_eq!(store.count("transcript_attachments"), 0);
    assert_eq!(store.item_row().2, "audio_pending");

    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    store
        .open()
        .conn()
        .execute("UPDATE captures SET audio_reference = NULL, text = ''", [])
        .unwrap();
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioUnavailable)
    );
    assert_eq!(store.count("transcript_attachments"), 0);
}

#[test]
fn a_text_capture_never_receives_a_transcript() {
    let store = Store::with_capture(typed_capture());
    let job = store.claim_transcription_job("transcription-1");

    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::NotAudioCapture)
    );
    assert_eq!(store.count("events"), 0);
    assert_eq!(store.search("typed")[0].current_text, "typed note");
}

#[test]
fn a_revision_change_that_leaves_the_text_alone_does_not_block_the_transcript() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let mut scope_correction = user_text_correction(&store.item_id, 0, "work");
    scope_correction.payload = EventPayload::Correction(Correction {
        kind: CorrectionKind::Type,
        old_value: None,
        new_value: "note".to_string(),
    });
    save_event(&mut store.open(), &scope_correction, 0).unwrap();

    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1"),
        later(),
    )
    .unwrap();
    assert!(matches!(
        acknowledgment.disposition,
        TranscriptionDisposition::Attached { revision: 2, .. }
    ));
    assert_eq!(store.job("interpret-1").source_revision, 2);
}

#[test]
fn the_lease_fences_stale_and_foreign_deliveries() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");

    let mut stale = outcome(&job, recognized(TRANSCRIPT));
    stale.lease_attempt = job.attempt_count + 1;
    let error = apply_transcription_outcome(&mut store.open(), &stale, later()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::LeaseNotHeld { .. }
    ));

    let mut unknown = outcome(&job, recognized(TRANSCRIPT));
    unknown.job_id = "no-such-job".to_string();
    let error = apply_transcription_outcome(&mut store.open(), &unknown, later()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::LeaseNotHeld { .. }
    ));

    let mut interpretation_job = store.open();
    enqueue_job(
        &mut interpretation_job,
        "interpret-0".to_string(),
        store.item_id.clone(),
        JOB_TYPE_INTERPRET.to_string(),
        0,
        None,
        Some("request-v1".to_string()),
        1,
        now(),
    )
    .unwrap();
    let mut wrong_type = outcome(&job, recognized(TRANSCRIPT));
    wrong_type.job_id = "interpret-0".to_string();
    let error = apply_transcription_outcome(&mut store.open(), &wrong_type, later()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::NotATranscriptionJob { .. }
    ));

    assert_eq!(store.count("transcript_attachments"), 0);
    assert_eq!(store.item_row().2, "audio_pending");
    assert_eq!(store.job("transcription-1").status, JobStatus::Running);
}

#[test]
fn a_transcription_job_pinned_to_a_provider_is_not_authorized() {
    let store = Store::with_voice_capture();
    let mut database = store.open();
    enqueue_job(
        &mut database,
        "transcription-1".to_string(),
        store.item_id.clone(),
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
        0,
        Some("remote-profile".to_string()),
        None,
        1,
        now(),
    )
    .unwrap();
    database
        .conn()
        .execute(
            "UPDATE jobs SET status = 'running', attempt_count = 1 WHERE job_id = 'transcription-1'",
            [],
        )
        .unwrap();
    let job = get_job(&database, "transcription-1").unwrap().unwrap();
    drop(database);

    let error = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::NotAuthorized(DenialReason::LocalOnlyJobHasProfile)
    ));
    assert_eq!(store.count("transcript_attachments"), 0);
}

#[test]
fn malformed_outcomes_are_rejected_before_any_write() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let transcript = || RecognizedTranscript {
        text: TRANSCRIPT.to_string(),
        confidence: Some(0.5),
        detected_language: Some("en-US".to_string()),
        recognizer: "on-device-recognizer/1".to_string(),
        audio_sha256: AUDIO_SHA256.to_string(),
    };
    let malformed: Vec<(TranscriptionOutcome, TranscriptionRejection)> = vec![
        (
            TranscriptionOutcome {
                lease_attempt: 0,
                ..outcome(&job, recognized(TRANSCRIPT))
            },
            TranscriptionRejection::MalformedField("lease_attempt"),
        ),
        (
            TranscriptionOutcome {
                audio_reference: String::new(),
                ..outcome(&job, recognized(TRANSCRIPT))
            },
            TranscriptionRejection::MissingField("audio_reference"),
        ),
        (
            outcome(
                &job,
                RecognizerResult::Transcript(RecognizedTranscript {
                    audio_sha256: "not-a-digest".to_string(),
                    ..transcript()
                }),
            ),
            TranscriptionRejection::MalformedField("audio_sha256"),
        ),
        (
            outcome(
                &job,
                RecognizerResult::Transcript(RecognizedTranscript {
                    confidence: Some(1.5),
                    ..transcript()
                }),
            ),
            TranscriptionRejection::MalformedField("confidence"),
        ),
        (
            outcome(
                &job,
                RecognizerResult::Transcript(RecognizedTranscript {
                    recognizer: " ".to_string(),
                    ..transcript()
                }),
            ),
            TranscriptionRejection::MissingField("recognizer"),
        ),
        (
            outcome(
                &job,
                RecognizerResult::Transcript(RecognizedTranscript {
                    text: "x".repeat(MAX_TRANSCRIPT_BYTES + 1),
                    ..transcript()
                }),
            ),
            TranscriptionRejection::TooLarge("text"),
        ),
        (
            with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "transcription-1"),
            TranscriptionRejection::MalformedField("interpretation.job_id"),
        ),
    ];
    for (request, expected) in malformed {
        let error = apply_transcription_outcome(&mut store.open(), &request, later()).unwrap_err();
        assert!(
            matches!(kind_of(&error), TranscriptionErrorKind::Validation(rejection) if rejection == &expected),
            "{expected:?}: {error}"
        );
        assert_eq!(error.commit_status, CommitStatus::NotCommitted);
    }
    assert_eq!(store.count("events"), 0);
    assert_eq!(store.job("transcription-1").status, JobStatus::Running);
}

#[test]
fn an_interpretation_job_id_already_in_use_is_rejected_without_attaching() {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    let request = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-1");
    let in_use = TranscriptionOutcome {
        interpretation: Some(InterpretationRequest {
            job_id: "transcription-1".to_string(),
            ..request.interpretation.clone().unwrap()
        }),
        ..request
    };
    // The job's own ID is rejected structurally; a different job's ID is rejected against the queue.
    assert!(apply_transcription_outcome(&mut store.open(), &in_use, later()).is_err());

    let mut database = store.open();
    enqueue_job(
        &mut database,
        "interpret-taken".to_string(),
        store.item_id.clone(),
        JOB_TYPE_INTERPRET.to_string(),
        0,
        None,
        Some("request-v0".to_string()),
        1,
        now(),
    )
    .unwrap();
    drop(database);
    let taken = with_interpretation(outcome(&job, recognized(TRANSCRIPT)), "interpret-taken");
    let error = apply_transcription_outcome(&mut store.open(), &taken, later()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::Validation(TranscriptionRejection::InterpretationJobIdInUse)
    ));
    assert_eq!(store.count("transcript_attachments"), 0);
    assert_eq!(store.item_row().2, "audio_pending");
}
