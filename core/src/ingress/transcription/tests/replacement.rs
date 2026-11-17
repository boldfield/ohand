//! Replacing a stored machine transcript from its retained audio (F71a).

use super::*;
use crate::lifecycle::retention::{
    assess_audio_retention, AudioRetentionAssessment, RetentionPolicy, StoragePressure,
};
use crate::store::schema::Clock;

const LONG_TRANSCRIPT: &str =
    "first I need to find the watering can and then remind me to water the ferns tomorrow";

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn request_time() -> DateTime<Utc> {
    later() + Duration::hours(1)
}

fn settle_time() -> DateTime<Utc> {
    later() + Duration::hours(2)
}

/// A store whose voice note was transcribed by a pre-F69 recognizer (generation 1).
fn store_with_transcript() -> Store {
    let store = Store::with_voice_capture();
    let job = store.claim_transcription_job("transcription-1");
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(TRANSCRIPT)),
        later(),
    )
    .unwrap();
    store
}

fn text_correction_of_transcript(store: &Store, new_value: &str) {
    let (_, _, _, revision) = store.item_row();
    let event = Event::new(
        format!("user-text-{revision}"),
        store.item_id.clone(),
        revision,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Text,
            old_value: Some(TRANSCRIPT.to_string()),
            new_value: new_value.to_string(),
        }),
        "2026-10-08T11:00:00Z".to_string(),
    )
    .unwrap();
    save_event(&mut store.open(), &event, revision).unwrap();
}

/// Native code reported the removal of the item's audio file; the capture row keeps its reference.
fn record_removal_of_audio(store: &Store) {
    let mut database = store.open();
    let audio_reference: String = database
        .conn()
        .query_row("SELECT audio_reference FROM captures", [], |row| row.get(0))
        .unwrap();
    let transaction = database.immediate_transaction().unwrap();
    record_audio_removal(&transaction, &store.item_id, &audio_reference, later()).unwrap();
    record_audio_removal(&transaction, &store.item_id, &audio_reference, later()).unwrap();
    transaction.commit().unwrap();
}

fn request_all(store: &Store) -> RetranscriptionRequestResult {
    request_retranscription(
        &mut store.open(),
        RetranscriptionSelection::AllCandidates,
        MAX_RETRANSCRIPTION_BATCH,
        request_time(),
    )
    .unwrap()
}

fn request_items(store: &Store, item_ids: &[String]) -> RetranscriptionRequestResult {
    request_retranscription(
        &mut store.open(),
        RetranscriptionSelection::Items(item_ids),
        MAX_RETRANSCRIPTION_BATCH,
        request_time(),
    )
    .unwrap()
}

fn claim_by_id(store: &Store, job_id: &str) -> Job {
    let mut database = store.open();
    loop {
        let claimed = claim_job_with_lease(&mut database, Duration::seconds(300), request_time())
            .unwrap()
            .expect("the job is eligible");
        if claimed.job_id == job_id {
            return claimed;
        }
    }
}

/// Request a replacement for the store's only item and claim its job.
fn requested_and_claimed(store: &Store) -> Job {
    let queued = request_all(store).queued;
    assert_eq!(queued.len(), 1);
    claim_by_id(store, &queued[0].job_id)
}

fn request_row(store: &Store, job_id: &str) -> (Option<String>, Option<String>) {
    store
        .open()
        .conn()
        .query_row(
            "SELECT outcome, outcome_reason FROM transcript_replacement_requests WHERE job_id = ?",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn provenance(store: &Store) -> TranscriptProvenance {
    transcript_provenance(store.open().conn(), &store.item_id)
        .unwrap()
        .expect("the item has a transcript")
}

fn skip_of(store: &Store) -> RetranscriptionSkip {
    let result = request_items(store, std::slice::from_ref(&store.item_id));
    assert!(result.queued.is_empty(), "{result:?}");
    result.skipped[0].1
}

fn retention(store: &Store) -> AudioRetentionAssessment {
    assess_audio_retention(
        store.open().conn(),
        &FixedClock(settle_time()),
        &RetentionPolicy::default(),
        StoragePressure::Normal,
        &store.item_id,
    )
    .unwrap()
    .expect("the item holds audio")
}

// --- candidates --------------------------------------------------------------------------------

#[test]
fn a_live_transcribed_note_with_an_older_generation_is_a_candidate() {
    let store = store_with_transcript();
    assert_eq!(provenance(&store).transcript_generation, 1);
    assert_eq!(provenance(&store).replaced_at, None);
    let database = store.open();
    assert_eq!(
        retranscription_candidates(database.conn(), 10).unwrap(),
        vec![store.item_id.clone()]
    );
    assert_eq!(
        count_retranscription_candidates(database.conn()).unwrap(),
        1
    );
}

#[test]
fn every_exclusion_keeps_a_note_from_being_a_candidate() {
    type Setup = fn(&Store);
    let cases: [(&str, Setup, RetranscriptionSkip); 7] = [
        (
            "deleted",
            |store| {
                mark_deletion_intent(&mut store.open(), &store.item_id, 1, later()).unwrap();
            },
            RetranscriptionSkip::ItemDeleted,
        ),
        (
            "text correction in force",
            |store| text_correction_of_transcript(store, "water the ferns on friday"),
            RetranscriptionSkip::TextCorrected,
        ),
        (
            "cleared correction",
            |store| text_correction_of_transcript(store, ""),
            RetranscriptionSkip::TextCorrected,
        ),
        (
            "audio reference gone",
            |store| {
                store
                    .open()
                    .conn()
                    .execute("UPDATE captures SET audio_reference = NULL, text = ''", [])
                    .unwrap();
            },
            RetranscriptionSkip::AudioUnavailable,
        ),
        (
            "audio recorded removed",
            record_removal_of_audio,
            RetranscriptionSkip::AudioUnavailable,
        ),
        (
            "generation current",
            |store| {
                store
                    .open()
                    .conn()
                    .execute(
                        "UPDATE transcript_attachments SET transcript_generation = 2",
                        [],
                    )
                    .unwrap();
            },
            RetranscriptionSkip::GenerationCurrent,
        ),
        (
            "job in flight",
            |store| {
                assert_eq!(request_all(store).queued.len(), 1);
            },
            RetranscriptionSkip::JobInFlight,
        ),
    ];
    for (label, setup, expected) in cases {
        let store = store_with_transcript();
        setup(&store);
        assert_eq!(skip_of(&store), expected, "{label}");
        let database = store.open();
        assert!(
            retranscription_candidates(database.conn(), 10)
                .unwrap()
                .is_empty(),
            "{label}"
        );
        assert_eq!(
            count_retranscription_candidates(database.conn()).unwrap(),
            0,
            "{label}"
        );
    }
}

#[test]
fn a_typed_note_and_a_pending_voice_note_are_not_candidates() {
    let typed = Store::with_capture(typed_capture());
    assert_eq!(skip_of(&typed), RetranscriptionSkip::NotTranscribed);
    let pending = Store::with_voice_capture();
    assert_eq!(skip_of(&pending), RetranscriptionSkip::NotTranscribed);
    let unknown = request_items(&pending, &["no-such-item".to_string()]);
    assert_eq!(
        unknown.skipped,
        vec![(
            "no-such-item".to_string(),
            RetranscriptionSkip::ItemNotFound
        )]
    );
}

// --- request -----------------------------------------------------------------------------------

#[test]
fn a_request_queues_an_ordinary_job_and_leaves_the_item_readable() {
    let store = store_with_transcript();
    let before = store.item_row();
    let result = request_all(&store);
    assert_eq!(result.queued.len(), 1);
    assert_eq!(result.queued[0].item_id, store.item_id);
    assert_eq!(result.deferred, 0);

    let job = store.job(&result.queued[0].job_id);
    assert_eq!(job.job_type, JOB_TYPE_TRANSCRIPTION_ATTACHMENT);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.source_revision, before.3);
    assert_eq!(job.profile_version, None, "never pinned to a profile");
    assert_eq!(job.request_version, None);
    assert_eq!(
        store.item_row(),
        before,
        "the item is not changed by the request"
    );
    assert_eq!(store.item_row().2, "transcribed");
    assert_eq!(
        store.search("ferns").len(),
        1,
        "the old text stays searchable"
    );
    assert_eq!(request_row(&store, &job.job_id), (None, None));

    // While the job runs the item stays readable as well.
    let running = claim_by_id(&store, &job.job_id);
    assert_eq!(running.status, JobStatus::Running);
    assert_eq!(store.item_row().2, "transcribed");
    assert_eq!(store.search("ferns").len(), 1);
}

#[test]
fn repeating_a_request_while_jobs_are_in_flight_queues_nothing_more() {
    let store = store_with_transcript();
    let first = request_all(&store);
    assert_eq!(first.queued.len(), 1);
    let again = request_all(&store);
    assert!(again.queued.is_empty());
    let named = request_items(&store, std::slice::from_ref(&store.item_id));
    assert_eq!(
        named.skipped,
        vec![(store.item_id.clone(), RetranscriptionSkip::JobInFlight)]
    );
    assert_eq!(store.count("transcript_replacement_requests"), 1);
    // Running as well as queued counts as in flight.
    claim_by_id(&store, &first.queued[0].job_id);
    assert!(request_all(&store).queued.is_empty());
    assert_eq!(store.count("transcript_replacement_requests"), 1);
}

/// Import and transcribe `count` more voice notes into the store, each from its own audio.
fn add_transcribed_notes(store: &Store, count: usize) {
    let mut database = store.open();
    for index in 0..count {
        let audio_reference = format!("staging/extra-{index}.m4a");
        let capture = Capture {
            capture_id: format!("3f9c1e52-0000-4000-8000-{index:012}"),
            audio_reference: Some(audio_reference.clone()),
            ..voice_capture()
        };
        import_foreground_ingress(&mut database, &capture).unwrap();
        loop {
            let claimed = claim_job_with_lease(&mut database, Duration::seconds(300), now())
                .unwrap()
                .expect("a transcription job is eligible");
            if claimed.job_type != JOB_TYPE_TRANSCRIPTION_ATTACHMENT {
                continue;
            }
            let mut attached = outcome(&claimed, recognized(TRANSCRIPT));
            attached.audio_reference = audio_reference;
            apply_transcription_outcome(&mut database, &attached, later()).unwrap();
            break;
        }
    }
}

#[test]
fn a_request_is_bounded_by_the_batch_limit_and_commits_in_one_transaction() {
    let store = store_with_transcript();
    add_transcribed_notes(&store, MAX_RETRANSCRIPTION_BATCH + 1);
    let all = count_retranscription_candidates(store.open().conn()).unwrap();
    assert_eq!(all, MAX_RETRANSCRIPTION_BATCH + 2);

    // A larger limit is clamped to the batch maximum.
    let result = request_retranscription(
        &mut store.open(),
        RetranscriptionSelection::AllCandidates,
        usize::MAX,
        request_time(),
    )
    .unwrap();
    assert_eq!(result.queued.len(), MAX_RETRANSCRIPTION_BATCH);
    assert_eq!(result.deferred, 2);
    assert_eq!(
        store.count("transcript_replacement_requests"),
        MAX_RETRANSCRIPTION_BATCH as i64
    );

    // The next call takes the rest; none is queued twice.
    let rest = request_all(&store);
    assert_eq!(rest.queued.len(), 2);
    assert_eq!(rest.deferred, 0);

    // A smaller limit applies to named items too.
    let more = Store::with_voice_capture();
    let named = request_retranscription(
        &mut more.open(),
        RetranscriptionSelection::Items(&[more.item_id.clone(), "other".to_string()]),
        1,
        request_time(),
    )
    .unwrap();
    assert_eq!(named.deferred, 1);
    assert_eq!(named.queued.len() + named.skipped.len(), 1);
}

#[test]
fn a_failing_request_writes_nothing() {
    let store = store_with_transcript();
    add_transcribed_notes(&store, 1);
    // A temporary trigger (not part of the stored schema) makes the second insert fail, so the
    // first one has already been written inside the transaction when it fails.
    let mut database = store.open();
    database
        .conn()
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_second_request BEFORE INSERT ON transcript_replacement_requests
             WHEN (SELECT COUNT(*) FROM transcript_replacement_requests) >= 1
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .unwrap();
    let result = request_retranscription(
        &mut database,
        RetranscriptionSelection::AllCandidates,
        10,
        request_time(),
    );
    assert!(result.is_err());
    assert_eq!(store.count("transcript_replacement_requests"), 0);
    assert_eq!(
        store.count("jobs WHERE job_type = 'transcription_attachment' AND status = 'queued'"),
        0
    );
}

// --- settlement --------------------------------------------------------------------------------

#[test]
fn a_different_transcript_replaces_the_text_and_keeps_the_old_one_as_history() {
    let store = store_with_transcript();
    let before = provenance(&store);
    let retention_before = retention(&store);
    let job = requested_and_claimed(&store);
    let mut request = outcome(&job, recognized(LONG_TRANSCRIPT));
    request.interpretation = Some(InterpretationRequest {
        job_id: "interpret-2".to_string(),
        profile_version: None,
        request_version: "request-v1".to_string(),
    });

    let acknowledgment =
        apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Replaced {
            revision: 2,
            interpretation: InterpretationQueued::Queued {
                job_id: "interpret-2".to_string()
            },
        }
    );

    // The item's text and search results are the new transcript, and it is not a user edit.
    let state = store.item_state();
    assert_eq!(state.current_text.text(), Some(LONG_TRANSCRIPT));
    assert!(!state.provenance.text_corrected);
    assert_eq!(store.item_row().2, "transcribed");
    assert_eq!(store.item_row().3, 2, "the revision advanced by one");
    assert_eq!(store.search("watering can").len(), 1);
    assert_eq!(
        store.search("watering can")[0].text_basis,
        SearchBasis::Transcribed
    );
    assert_eq!(store.search("ferns").len(), 1);
    assert_eq!(store.count("events WHERE event_type = 'transcript'"), 2);

    // The attachment holds the new transcript; the retention clock did not restart.
    let after = provenance(&store);
    assert_eq!(after.text, LONG_TRANSCRIPT);
    assert_eq!(after.source_job_id, job.job_id);
    assert_ne!(after.transcript_record_id, before.transcript_record_id);
    assert_eq!(after.attached_revision, 2);
    assert_eq!(after.transcript_generation, CURRENT_TRANSCRIPT_GENERATION);
    assert_eq!(after.transcribed_at, before.transcribed_at);
    assert_eq!(
        DateTime::parse_from_rfc3339(after.replaced_at.as_deref().unwrap()).unwrap(),
        settle_time()
    );
    assert_eq!(retention(&store), retention_before);

    // The superseded transcript is readable history.
    let history = superseded_transcripts(store.open().conn(), &store.item_id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, TRANSCRIPT);
    assert_eq!(history[0].transcript_record_id, before.transcript_record_id);
    assert_eq!(history[0].source_job_id, "transcription-1");
    assert_eq!(history[0].attached_revision, 1);
    assert_eq!(history[0].recognizer, before.recognizer);
    assert_eq!(history[0].audio_sha256, AUDIO_SHA256);
    assert_eq!(history[0].transcript_generation, 1);
    assert_eq!(
        history[0].superseded_by_record_id,
        after.transcript_record_id
    );

    // An interpretation job exists at the new revision; the replacement job completed.
    let interpretation = store.job("interpret-2");
    assert_eq!(interpretation.source_revision, 2);
    assert_eq!(store.job(&job.job_id).status, JobStatus::Completed);
    assert_eq!(
        request_row(&store, &job.job_id),
        (Some("replaced".to_string()), None)
    );
    // The capture is untouched.
    assert_eq!(
        store.scalar_text("SELECT COALESCE(text, '') FROM captures"),
        ""
    );

    // The record that is now history cannot be asked for again, and the replaced one stays out
    // of the candidates: the generation is current.
    assert_eq!(skip_of(&store), RetranscriptionSkip::GenerationCurrent);
}

#[test]
fn an_identical_transcript_is_unchanged_and_writes_no_event_or_revision() {
    let store = store_with_transcript();
    let before = provenance(&store);
    let interpretation_jobs = store.interpretation_jobs().len();
    let job = requested_and_claimed(&store);
    let mut request = outcome(&job, recognized(&format!("  {TRANSCRIPT}\n")));
    if let RecognizerResult::Transcript(transcript) = &mut request.result {
        transcript.recognizer = "on-device-recognizer/2".to_string();
    }

    let acknowledgment =
        apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Unchanged
    );

    assert_eq!(store.item_row().3, 1, "no revision");
    assert_eq!(store.count("events"), 1, "no second event");
    assert_eq!(store.interpretation_jobs().len(), interpretation_jobs);
    let after = provenance(&store);
    assert_eq!(after.text, before.text);
    assert_eq!(after.transcript_record_id, before.transcript_record_id);
    assert_eq!(after.source_job_id, before.source_job_id);
    assert_eq!(after.transcript_generation, CURRENT_TRANSCRIPT_GENERATION);
    assert_eq!(after.recognizer, "on-device-recognizer/2");
    assert_eq!(after.transcribed_at, before.transcribed_at);
    assert_eq!(store.count("superseded_transcripts"), 0);
    assert_eq!(store.job(&job.job_id).status, JobStatus::Completed);
    assert_eq!(
        request_row(&store, &job.job_id),
        (Some("unchanged".to_string()), None)
    );
}

#[test]
fn unsupported_failed_and_empty_outcomes_keep_the_transcript() {
    let cases = [
        (RecognizerResult::Unsupported, UNSUPPORTED_REASON),
        (RecognizerResult::Failed, FAILED_REASON),
        (recognized("   "), NO_SPEECH_REASON),
    ];
    for (result, reason) in cases {
        let store = store_with_transcript();
        let before = provenance(&store);
        let job = requested_and_claimed(&store);
        let request = outcome(&job, result);

        let acknowledgment =
            apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
        assert_eq!(
            acknowledgment.disposition,
            TranscriptionDisposition::Kept { reason },
            "{reason}"
        );
        assert_eq!(provenance(&store), before, "{reason}");
        assert_eq!(
            store.item_row(),
            (
                "saved_local".into(),
                "unprocessed".into(),
                "transcribed".into(),
                1
            ),
            "a failed replacement never moves the item to transcription_failed"
        );
        assert_eq!(store.search("ferns").len(), 1, "{reason}");
        let ended = store.job(&job.job_id);
        assert_eq!(ended.status, JobStatus::Failed);
        assert_eq!(ended.failure_reason.as_deref(), Some(reason));
        assert_eq!(
            request_row(&store, &job.job_id),
            (Some("kept".to_string()), Some(reason.to_string()))
        );

        let retry =
            apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
        assert_eq!(
            retry.disposition,
            TranscriptionDisposition::Kept { reason },
            "redelivery"
        );
        assert_eq!(store.count("events"), 1);

        // A different outcome for the settled job is a conflict.
        let conflicting = outcome(&job, recognized(LONG_TRANSCRIPT));
        let error = apply_transcription_outcome(&mut store.open(), &conflicting, settle_time())
            .unwrap_err();
        assert!(matches!(
            kind_of(&error),
            TranscriptionErrorKind::ConflictingResult
        ));
    }
}

#[test]
fn a_user_correction_between_request_and_outcome_wins() {
    for corrected in ["water the ferns on friday", ""] {
        let store = store_with_transcript();
        let job = requested_and_claimed(&store);
        text_correction_of_transcript(&store, corrected);

        let acknowledgment = apply_transcription_outcome(
            &mut store.open(),
            &outcome(&job, recognized(LONG_TRANSCRIPT)),
            settle_time(),
        )
        .unwrap();
        assert_eq!(
            acknowledgment.disposition,
            TranscriptionDisposition::Discarded(DiscardReason::TextCorrected)
        );
        let state = store.item_state();
        assert!(state.provenance.text_corrected);
        assert_eq!(state.current_text.text(), Some(corrected));
        assert_eq!(provenance(&store).text, TRANSCRIPT);
        assert_eq!(store.count("superseded_transcripts"), 0);
        assert_eq!(store.count("corrections"), 1, "the correction is intact");
        let retired = store.job(&job.job_id);
        assert_eq!(retired.status, JobStatus::Cancelled);
        assert_eq!(retired.failure_reason.as_deref(), Some("text_corrected"));
        assert_eq!(
            request_row(&store, &job.job_id),
            (
                Some("discarded".to_string()),
                Some("text_corrected".to_string())
            )
        );
    }
}

#[test]
fn deletion_between_request_and_outcome_attaches_nothing_and_removes_the_rows() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    mark_deletion_intent(&mut store.open(), &store.item_id, 1, settle_time()).unwrap();
    assert_eq!(store.count("transcript_replacement_requests"), 0);
    assert_eq!(store.count("superseded_transcripts"), 0);

    // Deletion cancelled the job, so the late outcome finds no lease and changes nothing.
    let error = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::LeaseNotHeld { .. }
    ));
    assert_eq!(store.attachments(), 0);
    assert_eq!(store.count("search_index"), 0);
}

#[test]
fn an_item_deleted_while_its_job_still_runs_discards_the_outcome() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    store
        .open()
        .conn()
        .execute("UPDATE items SET lifecycle_state = 'deleted'", [])
        .unwrap();

    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::ItemDeleted)
    );
    assert_eq!(provenance(&store).text, TRANSCRIPT);
    assert_eq!(store.count("superseded_transcripts"), 0);
}

#[test]
fn an_outcome_from_other_or_missing_audio_is_discarded() {
    // A digest that differs from the attachment's.
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    let mut mismatched = outcome(&job, recognized(LONG_TRANSCRIPT));
    if let RecognizerResult::Transcript(transcript) = &mut mismatched.result {
        transcript.audio_sha256 = "f".repeat(64);
    }
    let acknowledgment =
        apply_transcription_outcome(&mut store.open(), &mismatched, settle_time()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioIdentityMismatch)
    );
    assert_eq!(provenance(&store).text, TRANSCRIPT);
    assert_eq!(store.item_row().3, 1);

    // A different audio reference.
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    let mut other_file = outcome(&job, recognized(LONG_TRANSCRIPT));
    other_file.audio_reference = "staging/other.m4a".to_string();
    let acknowledgment =
        apply_transcription_outcome(&mut store.open(), &other_file, settle_time()).unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioIdentityMismatch)
    );

    // The capture's audio reference is gone.
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    store
        .open()
        .conn()
        .execute("UPDATE captures SET audio_reference = NULL, text = ''", [])
        .unwrap();
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioUnavailable)
    );
    assert_eq!(provenance(&store).text, TRANSCRIPT);
    assert_eq!(
        store.job(&job.job_id).failure_reason.as_deref(),
        Some("audio_unavailable")
    );
}

#[test]
fn a_replacement_made_stale_by_another_replacement_is_discarded() {
    let store = store_with_transcript();
    let first = requested_and_claimed(&store);
    // A second request is not possible while the first is in flight, so stage one directly.
    store
        .open()
        .conn()
        .execute(
            "UPDATE transcript_replacement_requests SET replaces_record_id = 'another-record'",
            [],
        )
        .unwrap();
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&first, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::TranscriptChanged)
    );
}

#[test]
fn redelivery_of_a_replacement_returns_the_result_and_a_different_outcome_conflicts() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    let request = outcome(&job, recognized(LONG_TRANSCRIPT));
    let first = apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
    assert!(matches!(
        first.disposition,
        TranscriptionDisposition::Replaced { revision: 2, .. }
    ));

    let retry = apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
    assert!(matches!(
        retry.disposition,
        TranscriptionDisposition::AlreadyReplaced { revision: 2, .. }
    ));
    assert_eq!(store.count("events WHERE event_type = 'transcript'"), 2);
    assert_eq!(store.count("superseded_transcripts"), 1);
    assert_eq!(store.item_row().3, 2);

    let different = outcome(&job, recognized("something else entirely"));
    let error =
        apply_transcription_outcome(&mut store.open(), &different, settle_time()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::ConflictingResult
    ));
    let different_audio = {
        let mut other = request.clone();
        if let RecognizerResult::Transcript(transcript) = &mut other.result {
            transcript.recognizer = "on-device-recognizer/9".to_string();
        }
        other
    };
    let error = apply_transcription_outcome(&mut store.open(), &different_audio, settle_time())
        .unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::ConflictingResult
    ));
    assert_eq!(provenance(&store).text, LONG_TRANSCRIPT);
}

#[test]
fn a_stale_lease_cannot_settle_a_replacement() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    let mut stale = outcome(&job, recognized(LONG_TRANSCRIPT));
    stale.lease_attempt = job.attempt_count + 1;
    let error = apply_transcription_outcome(&mut store.open(), &stale, settle_time()).unwrap_err();
    assert!(matches!(
        kind_of(&error),
        TranscriptionErrorKind::LeaseNotHeld { .. }
    ));
    assert_eq!(provenance(&store).text, TRANSCRIPT);
    assert_eq!(store.job(&job.job_id).status, JobStatus::Running);
}

#[test]
fn a_failure_at_each_fault_point_rolls_back_and_the_retry_succeeds() {
    for failing_stage in [
        TranscriptionStage::EventWritten,
        TranscriptionStage::JobSettled,
        TranscriptionStage::InterpretationQueued,
    ] {
        let store = store_with_transcript();
        let before = provenance(&store);
        let interpretation_jobs = store.interpretation_jobs().len();
        let job = requested_and_claimed(&store);
        let request =
            with_interpretation(outcome(&job, recognized(LONG_TRANSCRIPT)), "interpret-2");

        let error =
            apply_with_fault_points(&mut store.open(), &request, settle_time(), &mut |stage| {
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

        assert_eq!(provenance(&store), before, "{failing_stage:?}");
        assert_eq!(
            store.count("superseded_transcripts"),
            0,
            "{failing_stage:?}"
        );
        assert_eq!(store.count("events"), 1, "{failing_stage:?}");
        assert_eq!(store.item_row().3, 1, "{failing_stage:?}");
        assert_eq!(store.search("ferns").len(), 1, "{failing_stage:?}");
        assert_eq!(
            store.interpretation_jobs().len(),
            interpretation_jobs,
            "{failing_stage:?}"
        );
        assert_eq!(
            store.job(&job.job_id).status,
            JobStatus::Running,
            "{failing_stage:?}"
        );
        assert_eq!(
            request_row(&store, &job.job_id),
            (None, None),
            "{failing_stage:?}"
        );

        let acknowledgment =
            apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
        assert!(
            matches!(
                acknowledgment.disposition,
                TranscriptionDisposition::Replaced { .. }
            ),
            "{failing_stage:?}"
        );
        assert_eq!(provenance(&store).text, LONG_TRANSCRIPT);
        assert_eq!(store.interpretation_jobs().len(), interpretation_jobs + 1);
    }
}

#[test]
fn a_lost_acknowledgment_reports_committed_and_the_retry_is_idempotent() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    let request = outcome(&job, recognized(LONG_TRANSCRIPT));
    let error = apply_with_fault_points(&mut store.open(), &request, settle_time(), &mut |stage| {
        if stage == TranscriptionStage::Committed {
            Err(anyhow!("process interrupted after commit"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.commit_status, CommitStatus::Committed);
    assert_eq!(provenance(&store).text, LONG_TRANSCRIPT);

    let retry = apply_transcription_outcome(&mut store.open(), &request, settle_time()).unwrap();
    assert!(matches!(
        retry.disposition,
        TranscriptionDisposition::AlreadyReplaced { revision: 2, .. }
    ));
    assert_eq!(store.count("events WHERE event_type = 'transcript'"), 2);
    assert_eq!(store.count("superseded_transcripts"), 1);
}

#[test]
fn the_first_job_redelivered_after_a_replacement_still_finds_its_own_result() {
    let store = Store::with_voice_capture();
    let first = store.claim_transcription_job("transcription-1");
    let first_outcome = outcome(&first, recognized(TRANSCRIPT));
    apply_transcription_outcome(&mut store.open(), &first_outcome, later()).unwrap();
    let job = requested_and_claimed(&store);
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();

    let retry = apply_transcription_outcome(&mut store.open(), &first_outcome, later()).unwrap();
    assert!(matches!(
        retry.disposition,
        TranscriptionDisposition::AlreadyAttached { revision: 1, .. }
    ));
    assert_eq!(provenance(&store).text, LONG_TRANSCRIPT);
}

#[test]
fn a_second_replacement_keeps_both_earlier_transcripts_as_history() {
    let store = store_with_transcript();
    let first = requested_and_claimed(&store);
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&first, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    // The generation is current, so a later request needs the row marked older again.
    store
        .open()
        .conn()
        .execute(
            "UPDATE transcript_attachments SET transcript_generation = 1",
            [],
        )
        .unwrap();
    let second = requested_and_claimed(&store);
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&second, recognized("the whole recording, third reading")),
        settle_time(),
    )
    .unwrap();

    let history: Vec<String> = superseded_transcripts(store.open().conn(), &store.item_id)
        .unwrap()
        .into_iter()
        .map(|transcript| transcript.text)
        .collect();
    assert_eq!(
        history,
        vec![TRANSCRIPT.to_string(), LONG_TRANSCRIPT.to_string()]
    );
    assert_eq!(store.item_row().3, 3);
    assert_eq!(
        store.item_state().current_text.text(),
        Some("the whole recording, third reading")
    );
}

#[test]
fn a_replacement_leaves_the_scheduled_reminder_alone_and_queues_interpretation_at_the_new_revision()
{
    use crate::interpretation::dispatch::{AdapterRegistry, InterpretationDispatcher};
    use crate::providers::contracts::{CancelToken, ManualClock};

    let store = Store::with_voice_capture();
    let (interpretation_job, _) = attach_and_claim_interpretation(&store, SPOKEN_COMMAND);
    let registry = AdapterRegistry::new();
    let clock = ManualClock::new();
    InterpretationDispatcher::new(&registry, &clock)
        .run_job(
            &mut store.open(),
            &interpretation_job,
            &CancelToken::new(),
            later(),
        )
        .unwrap();
    let reminder_before: (String, String, Option<String>, String) = store
        .open()
        .conn()
        .query_row(
            "SELECT reminder_id, request_state, resolved_instant, schedule_state FROM reminders
              WHERE item_id = ?",
            [&store.item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        reminder_before.1, "resolved",
        "the proposal scheduled a reminder"
    );
    let operations_before = store.count("reminder_operations");

    let replaced_text = format!("Buy milk first. {SPOKEN_COMMAND}");
    let job = requested_and_claimed(&store);
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(&replaced_text)),
        settle_time(),
    )
    .unwrap();
    let TranscriptionDisposition::Replaced {
        revision,
        interpretation: InterpretationQueued::Queued { job_id },
    } = acknowledgment.disposition
    else {
        panic!("{:?}", acknowledgment.disposition);
    };

    // The earlier proposal keeps its revision and basis; the reminder is untouched, as after a
    // user text correction.
    assert_eq!(
        store.count("proposals WHERE source_revision = 1 AND text_basis_kind = 'transcript'"),
        1
    );
    let reminder_after: (String, String, Option<String>, String) = store
        .open()
        .conn()
        .query_row(
            "SELECT reminder_id, request_state, resolved_instant, schedule_state FROM reminders
              WHERE item_id = ?",
            [&store.item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(reminder_after, reminder_before);
    assert_eq!(store.count("reminder_operations"), operations_before);
    assert_eq!(store.item_row().1, "processed");

    // The interpretation of the new text runs at the new revision and leaves one reminder.
    let queued = store.job(&job_id);
    assert_eq!(queued.source_revision, revision);
    let claimed = claim_job_with_lease(&mut store.open(), Duration::seconds(300), settle_time())
        .unwrap()
        .expect("the interpretation job is eligible");
    assert_eq!(claimed.job_id, job_id);
    InterpretationDispatcher::new(&registry, &clock)
        .run_job(
            &mut store.open(),
            &claimed,
            &CancelToken::new(),
            settle_time(),
        )
        .unwrap();
    assert_eq!(
        store.count("reminders"),
        1,
        "never two live reminders for one item"
    );
    let reminder_final: (String, String, Option<String>, String) = store
        .open()
        .conn()
        .query_row(
            "SELECT reminder_id, request_state, resolved_instant, schedule_state FROM reminders
              WHERE item_id = ?",
            [&store.item_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(reminder_final, reminder_before);
    let state = store.item_state();
    assert!(!state.provenance.text_corrected);
    assert_eq!(state.current_text.text(), Some(replaced_text.as_str()));
}

#[test]
fn deletion_leaves_no_superseded_transcript_and_no_request_for_the_item() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    assert_eq!(store.count("superseded_transcripts"), 1);
    assert_eq!(store.count("transcript_replacement_requests"), 1);

    mark_deletion_intent(&mut store.open(), &store.item_id, 2, settle_time()).unwrap();

    assert_eq!(store.count("superseded_transcripts"), 0);
    assert_eq!(store.count("transcript_replacement_requests"), 0);
    assert_eq!(store.attachments(), 0);
    assert!(superseded_transcripts(store.open().conn(), &store.item_id)
        .unwrap()
        .is_empty());
    let leftover: i64 = store
        .open()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM superseded_transcripts WHERE text LIKE '%ferns%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

#[test]
fn audio_recorded_removed_after_the_request_discards_the_outcome() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    record_removal_of_audio(&store);
    let acknowledgment = apply_transcription_outcome(
        &mut store.open(),
        &outcome(&job, recognized(LONG_TRANSCRIPT)),
        settle_time(),
    )
    .unwrap();
    assert_eq!(
        acknowledgment.disposition,
        TranscriptionDisposition::Discarded(DiscardReason::AudioUnavailable)
    );
    assert_eq!(provenance(&store).text, TRANSCRIPT);
    assert_eq!(
        request_row(&store, &job.job_id),
        (
            Some("discarded".to_string()),
            Some("audio_unavailable".to_string())
        )
    );
}

#[test]
fn a_queued_replacement_made_stale_by_a_non_text_revision_change_is_recorded_discarded() {
    let store = store_with_transcript();
    let queued = request_all(&store).queued;
    store
        .open()
        .conn()
        .execute("UPDATE items SET revision = revision + 1", [])
        .unwrap();
    assert_eq!(
        request_row(&store, &queued[0].job_id),
        (None, None),
        "still in flight until the queue drops it"
    );
    let claimed =
        claim_job_with_lease(&mut store.open(), Duration::seconds(300), request_time()).unwrap();
    assert!(claimed.is_none());
    assert_eq!(
        store.job(&queued[0].job_id).failure_reason.as_deref(),
        Some("stale_revision")
    );
    assert_eq!(
        request_row(&store, &queued[0].job_id),
        (
            Some("discarded".to_string()),
            Some("stale_revision".to_string())
        )
    );
    // The note is a candidate again, so the user's intent can be requested anew.
    assert_eq!(request_all(&store).queued.len(), 1);
}

#[test]
fn a_running_replacement_with_an_expired_lease_made_stale_is_recorded_discarded() {
    let store = store_with_transcript();
    let job = requested_and_claimed(&store);
    store
        .open()
        .conn()
        .execute("UPDATE items SET revision = revision + 1", [])
        .unwrap();
    let much_later = request_time() + Duration::hours(1);
    let claimed =
        claim_job_with_lease(&mut store.open(), Duration::seconds(300), much_later).unwrap();
    assert!(claimed.is_none());
    assert_eq!(
        request_row(&store, &job.job_id),
        (
            Some("discarded".to_string()),
            Some("stale_revision".to_string())
        )
    );
}
