use super::*;
use crate::ingress::import::import_foreground_ingress;
use crate::ingress::transcription::{
    apply_transcription_outcome, RecognizedTranscript, RecognizerResult, TranscriptionOutcome,
};
use crate::jobs::queue::{claim_job_with_lease, enqueue_job};
use crate::lifecycle::delete_intent::mark_deletion_intent;
use crate::privacy::routing::JOB_TYPE_TRANSCRIPTION_ATTACHMENT;
use crate::store::captures::Capture;
use crate::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use crate::store::schema::{Database, SystemClock};
use std::sync::Arc;
use tempfile::TempDir;

const ROUTE_ID: &str = "route-personal";
const FIRST_CAPTURE_ID: &str = "3f9c1e52-0000-4000-8000-0000000000d1";
const SECOND_CAPTURE_ID: &str = "3f9c1e52-0000-4000-8000-0000000000d2";
const FIRST_AUDIO: &str = "audio/2026-10-01/first.m4a";
const SECOND_AUDIO: &str = "audio/2026-10-01/second.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn captured_at() -> DateTime<Utc> {
    at("2026-10-01T08:00:00Z")
}

fn transcribed_at() -> DateTime<Utc> {
    at("2026-10-03T08:00:00Z")
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
                 VALUES (?, ?, 'personal', '[]', '2026-10-01T07:00:00Z')",
                rusqlite::params![ROUTE_ID, ROUTE_ID],
            )
            .unwrap();
        store
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }

    fn import(&self, capture_id: &str, audio: Option<&str>, text: Option<&str>) -> String {
        let capture = Capture::new(
            capture_id.to_string(),
            text.map(str::to_string),
            audio.map(str::to_string),
            "2026-10-01T08:00:00Z".to_string(),
            "UTC".to_string(),
            0,
            "en_US".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            ROUTE_ID.to_string(),
            false,
            captured_at().to_rfc3339(),
            None,
        )
        .unwrap();
        import_foreground_ingress(&mut self.open(), &capture)
            .unwrap()
            .item_id
    }

    fn voice_item(&self) -> String {
        self.import(FIRST_CAPTURE_ID, Some(FIRST_AUDIO), None)
    }

    fn transcription_result(
        &self,
        item_id: &str,
        audio: &str,
        result: RecognizerResult,
        when: DateTime<Utc>,
    ) {
        let mut database = self.open();
        let job_id = format!("transcription-{item_id}-{}", when.timestamp());
        enqueue_job(
            &mut database,
            job_id.clone(),
            item_id.to_string(),
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
            0,
            None,
            None,
            1,
            when,
        )
        .unwrap();
        let job = claim_job_with_lease(&mut database, Duration::seconds(300), when)
            .unwrap()
            .expect("the job is eligible");
        assert_eq!(job.job_id, job_id);
        apply_transcription_outcome(
            &mut database,
            &TranscriptionOutcome {
                job_id,
                lease_attempt: job.attempt_count,
                audio_reference: audio.to_string(),
                result,
                interpretation: None,
            },
            when,
        )
        .unwrap();
    }

    fn transcribe(&self, item_id: &str, audio: &str, when: DateTime<Utc>) {
        self.transcription_result(
            item_id,
            audio,
            RecognizerResult::Transcript(RecognizedTranscript {
                text: "water the ferns".to_string(),
                confidence: Some(0.9),
                detected_language: Some("en-US".to_string()),
                recognizer: "on-device-recognizer/1".to_string(),
                audio_sha256: AUDIO_SHA256.to_string(),
            }),
            when,
        );
    }

    fn correct_text(&self, item_id: &str, revision: i32, old_value: Option<&str>, text: &str) {
        let event = Event::new(
            format!("user-text-{item_id}-{revision}"),
            item_id.to_string(),
            revision,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind: CorrectionKind::Text,
                old_value: old_value.map(str::to_string),
                new_value: text.to_string(),
            }),
            "2026-10-04T09:00:00Z".to_string(),
        )
        .unwrap();
        save_event(&mut self.open(), &event, revision).unwrap();
    }

    fn enqueue_transcription(&self, item_id: &str, job_id: &str) {
        enqueue_job(
            &mut self.open(),
            job_id.to_string(),
            item_id.to_string(),
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
            0,
            None,
            None,
            1,
            captured_at(),
        )
        .unwrap();
    }

    fn assess(
        &self,
        item_id: &str,
        now: DateTime<Utc>,
        pressure: StoragePressure,
    ) -> Option<AudioRetentionAssessment> {
        assess_audio_retention(
            self.open().conn(),
            &FixedClock(now),
            &RetentionPolicy::default(),
            pressure,
            item_id,
        )
        .unwrap()
    }

    fn state(&self, item_id: &str, now: DateTime<Utc>) -> RetentionState {
        self.assess(item_id, now, StoragePressure::Normal)
            .expect("the item holds live audio")
            .state
    }

    fn execute(&self, sql: &str) {
        self.open().conn().execute(sql, []).unwrap();
    }
}

fn unresolved(reason: UnresolvedReason) -> RetentionState {
    RetentionState::Unresolved { reason }
}

#[test]
fn audio_expires_seven_days_after_transcription_success() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let expires_at = transcribed_at() + Duration::days(7);

    let just_before = store
        .assess(
            &item_id,
            expires_at - Duration::seconds(1),
            StoragePressure::Normal,
        )
        .unwrap();
    assert_eq!(just_before.state, RetentionState::Retained { expires_at });
    assert_eq!(just_before.cleanup, CleanupIntent::None);
    assert!(!just_before.sole_source);

    let at_expiry = store
        .assess(&item_id, expires_at, StoragePressure::Normal)
        .unwrap();
    assert_eq!(
        at_expiry.state,
        RetentionState::Expired {
            expired_at: expires_at
        }
    );
    assert_eq!(
        at_expiry.cleanup,
        CleanupIntent::RemoveAudio {
            audio_reference: FIRST_AUDIO.to_string(),
            expired_at: expires_at,
        }
    );
}

#[test]
fn the_clock_starts_at_transcription_not_at_capture() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());

    // Seven days after capture, but only five after transcription.
    let seven_days_after_capture = captured_at() + Duration::days(7);
    assert!(seven_days_after_capture < transcribed_at() + Duration::days(7));
    assert!(matches!(
        store.state(&item_id, seven_days_after_capture),
        RetentionState::Retained { .. }
    ));
}

#[test]
fn the_retention_period_is_configurable_and_bounded() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let one_day = RetentionPolicy::new(Duration::days(1)).unwrap();

    let assessment = assess_audio_retention(
        store.open().conn(),
        &FixedClock(transcribed_at() + Duration::days(1)),
        &one_day,
        StoragePressure::Normal,
        &item_id,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(assessment.state, RetentionState::Expired { .. }));

    assert_eq!(RetentionPolicy::default().period(), Duration::days(7));
    assert!(RetentionPolicy::new(Duration::zero()).is_err());
    assert!(RetentionPolicy::new(Duration::days(-1)).is_err());
    assert!(RetentionPolicy::new(Duration::days(MAX_RETENTION_DAYS + 1)).is_err());
    assert!(RetentionPolicy::new(Duration::days(MAX_RETENTION_DAYS)).is_ok());
}

#[test]
fn a_transcription_time_in_the_future_never_expires_audio_early() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());

    let before_transcription = transcribed_at() - Duration::days(30);
    assert!(matches!(
        store.state(&item_id, before_transcription),
        RetentionState::Retained { .. }
    ));
}

#[test]
fn pending_unsupported_and_failed_sole_sources_never_expire() {
    let ten_years_later = captured_at() + Duration::days(3650);
    let cases = [
        (
            None,
            UnresolvedReason::TranscriptionPending,
            vec![
                RetentionOption::RetryTranscription,
                RetentionOption::DeleteAudio,
            ],
        ),
        (
            Some(RecognizerResult::Unsupported),
            UnresolvedReason::TranscriptionUnsupported,
            vec![
                RetentionOption::RetryTranscription,
                RetentionOption::DeleteAudio,
            ],
        ),
        (
            Some(RecognizerResult::Failed),
            UnresolvedReason::TranscriptionFailed,
            vec![
                RetentionOption::RetryTranscription,
                RetentionOption::DeleteAudio,
            ],
        ),
    ];
    for (result, reason, options) in cases {
        let store = Store::new();
        let item_id = store.voice_item();
        if let Some(result) = result {
            store.transcription_result(&item_id, FIRST_AUDIO, result, transcribed_at());
        }

        for now in [captured_at(), transcribed_at(), ten_years_later] {
            let assessment = store
                .assess(&item_id, now, StoragePressure::Normal)
                .unwrap();
            assert_eq!(assessment.state, unresolved(reason), "{reason:?}");
            assert!(assessment.sole_source, "{reason:?}");
            assert_eq!(assessment.options, options, "{reason:?}");
            assert_eq!(assessment.cleanup, CleanupIntent::None, "{reason:?}");
            assert_eq!(assessment.audio_reference, FIRST_AUDIO);
        }
    }
}

#[test]
fn an_empty_recognition_keeps_the_audio_as_a_pending_sole_source() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcription_result(
        &item_id,
        FIRST_AUDIO,
        RecognizerResult::Transcript(RecognizedTranscript {
            text: "  \n".to_string(),
            confidence: None,
            detected_language: None,
            recognizer: "on-device-recognizer/1".to_string(),
            audio_sha256: AUDIO_SHA256.to_string(),
        }),
        transcribed_at(),
    );

    let assessment = store
        .assess(
            &item_id,
            transcribed_at() + Duration::days(30),
            StoragePressure::Normal,
        )
        .unwrap();
    assert_eq!(
        assessment.state,
        unresolved(UnresolvedReason::TranscriptionPending)
    );
    assert!(assessment.sole_source);
    assert_eq!(
        assessment.options,
        vec![
            RetentionOption::RetryTranscription,
            RetentionOption::DeleteAudio
        ]
    );
    assert_eq!(assessment.cleanup, CleanupIntent::None);
}

#[test]
fn retry_is_withheld_only_while_transcription_work_is_queued_or_running() {
    let store = Store::new();
    let item_id = store.voice_item();
    let options = |store: &Store| {
        store
            .assess(&item_id, captured_at(), StoragePressure::Normal)
            .unwrap()
            .options
    };
    let both = vec![
        RetentionOption::RetryTranscription,
        RetentionOption::DeleteAudio,
    ];
    assert_eq!(options(&store), both);

    store.enqueue_transcription(&item_id, "transcription-active");
    assert_eq!(options(&store), vec![RetentionOption::DeleteAudio]);

    store.execute("UPDATE jobs SET status = 'running'");
    assert_eq!(options(&store), vec![RetentionOption::DeleteAudio]);

    store.execute("UPDATE jobs SET status = 'failed'");
    assert_eq!(options(&store), both);
}

#[test]
fn a_user_correction_neither_restarts_nor_shortens_the_clock() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let expires_at = transcribed_at() + Duration::days(7);
    let before = store.state(&item_id, transcribed_at() + Duration::days(1));

    store.correct_text(
        &item_id,
        1,
        Some("water the ferns"),
        "water the ferns friday",
    );

    assert_eq!(before, RetentionState::Retained { expires_at });
    let after = store
        .assess(
            &item_id,
            transcribed_at() + Duration::days(2),
            StoragePressure::Normal,
        )
        .unwrap();
    assert_eq!(after.state, RetentionState::Retained { expires_at });
    assert!(!after.sole_source);
    assert!(matches!(
        store.state(&item_id, expires_at),
        RetentionState::Expired { .. }
    ));
}

#[test]
fn a_user_text_without_a_transcript_keeps_the_audio_with_delete_only() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcription_result(
        &item_id,
        FIRST_AUDIO,
        RecognizerResult::Failed,
        transcribed_at(),
    );
    store.correct_text(&item_id, 0, None, "water the ferns");

    let assessment = store
        .assess(
            &item_id,
            transcribed_at() + Duration::days(400),
            StoragePressure::Normal,
        )
        .unwrap();
    assert_eq!(
        assessment.state,
        unresolved(UnresolvedReason::TextWithoutTranscript)
    );
    assert!(!assessment.sole_source);
    assert_eq!(assessment.options, vec![RetentionOption::DeleteAudio]);
    assert_eq!(assessment.cleanup, CleanupIntent::None);
}

#[test]
fn a_text_correction_cleared_to_empty_returns_a_transcriptless_item_to_sole_source() {
    for cleared in ["", "  \n"] {
        let store = Store::new();
        let item_id = store.voice_item();
        store.transcription_result(
            &item_id,
            FIRST_AUDIO,
            RecognizerResult::Failed,
            transcribed_at(),
        );
        store.correct_text(&item_id, 0, None, "water the ferns");
        let with_text = store
            .assess(&item_id, transcribed_at(), StoragePressure::Low)
            .unwrap();
        assert!(!with_text.sole_source);

        store.correct_text(&item_id, 1, Some("water the ferns"), cleared);
        let assessment = store
            .assess(
                &item_id,
                transcribed_at() + Duration::days(400),
                StoragePressure::Low,
            )
            .unwrap();
        assert_eq!(
            assessment.state,
            unresolved(UnresolvedReason::TranscriptionFailed)
        );
        assert!(assessment.sole_source);
        assert_eq!(
            assessment.options,
            vec![
                RetentionOption::RetryTranscription,
                RetentionOption::DeleteAudio
            ]
        );
        assert_eq!(assessment.cleanup, CleanupIntent::None);
        assert_eq!(
            assessment.storage_pressure_notice,
            Some(StoragePressureNotice::SoleSourceRetained)
        );
    }
}

#[test]
fn clearing_a_correction_after_a_transcript_exists_does_not_stop_the_audio_expiring() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    store.correct_text(
        &item_id,
        1,
        Some("water the ferns"),
        "water the ferns friday",
    );
    store.correct_text(&item_id, 2, Some("water the ferns friday"), "");

    let expires_at = transcribed_at() + Duration::days(7);
    let assessment = store
        .assess(&item_id, expires_at, StoragePressure::Normal)
        .unwrap();
    assert!(!assessment.sole_source, "the transcript stays recoverable");
    assert!(matches!(assessment.state, RetentionState::Expired { .. }));
}

#[test]
fn captured_text_beside_audio_starts_no_clock() {
    let store = Store::new();
    let item_id = store.import(FIRST_CAPTURE_ID, Some(FIRST_AUDIO), Some("typed note"));

    let assessment = store
        .assess(
            &item_id,
            captured_at() + Duration::days(400),
            StoragePressure::Normal,
        )
        .unwrap();
    assert_eq!(
        assessment.state,
        unresolved(UnresolvedReason::TextWithoutTranscript)
    );
    assert!(!assessment.sole_source);
}

#[test]
fn absent_or_unusable_transcription_times_never_expire_audio() {
    let far_future = transcribed_at() + Duration::days(3650);

    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    store.execute("UPDATE transcript_attachments SET transcribed_at = 'yesterday-ish'");
    let unreadable = store
        .assess(&item_id, far_future, StoragePressure::Normal)
        .unwrap();
    assert_eq!(
        unreadable.state,
        unresolved(UnresolvedReason::TranscriptionTimeUnreadable)
    );
    assert_eq!(unreadable.cleanup, CleanupIntent::None);
    assert!(!unreadable.sole_source);

    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    store.execute("DELETE FROM transcript_attachments");
    let missing = store
        .assess(&item_id, far_future, StoragePressure::Normal)
        .unwrap();
    assert_eq!(
        missing.state,
        unresolved(UnresolvedReason::TranscriptRecordInconsistent)
    );
    assert!(missing.sole_source, "no usable text remains");
    assert_eq!(
        missing.options,
        vec![
            RetentionOption::RetryTranscription,
            RetentionOption::DeleteAudio
        ],
        "a transcribed item whose transcript record is missing can be retried or deleted"
    );
    assert_eq!(missing.cleanup, CleanupIntent::None);

    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    store.execute("UPDATE transcript_attachments SET audio_reference = 'audio/other.m4a'");
    let mismatched = store
        .assess(&item_id, far_future, StoragePressure::Normal)
        .unwrap();
    assert_eq!(
        mismatched.state,
        unresolved(UnresolvedReason::TranscriptAudioMismatch)
    );
    assert_eq!(mismatched.cleanup, CleanupIntent::None);
}

#[test]
fn a_blank_transcript_never_expires_the_only_source() {
    let far_future = transcribed_at() + Duration::days(3650);

    for blank in ["", "   ", "\t\n "] {
        let store = Store::new();
        let item_id = store.voice_item();
        store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
        store
            .open()
            .conn()
            .execute("UPDATE transcript_attachments SET text = ?", [blank])
            .unwrap();
        let assessment = store
            .assess(&item_id, far_future, StoragePressure::Low)
            .unwrap();
        assert_eq!(
            assessment.state,
            unresolved(UnresolvedReason::TranscriptEmpty),
            "blank transcript {blank:?}"
        );
        assert!(assessment.sole_source, "a blank transcript is not text");
        assert_eq!(
            assessment.options,
            vec![
                RetentionOption::RetryTranscription,
                RetentionOption::DeleteAudio
            ]
        );
        assert_eq!(assessment.cleanup, CleanupIntent::None);
        assert_eq!(
            assessment.storage_pressure_notice,
            Some(StoragePressureNotice::SoleSourceRetained)
        );
        assert_eq!(
            authorize_audio_removal(
                store.open().conn(),
                &FixedClock(far_future),
                &RetentionPolicy::default(),
                &item_id,
                FIRST_AUDIO,
            )
            .unwrap(),
            RemovalDecision::Denied(RemovalDenial::Unresolved {
                reason: UnresolvedReason::TranscriptEmpty
            })
        );
    }

    // Captured or user text beside a blank transcript keeps the audio from being a sole source,
    // but a blank transcript still starts no clock.
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    store.correct_text(&item_id, 1, Some("water the ferns"), "water the ferns");
    store.execute("UPDATE transcript_attachments SET text = ' '");
    let with_text = store
        .assess(&item_id, far_future, StoragePressure::Normal)
        .unwrap();
    assert_eq!(
        with_text.state,
        unresolved(UnresolvedReason::TranscriptEmpty)
    );
    assert!(!with_text.sole_source);
    assert_eq!(with_text.cleanup, CleanupIntent::None);
}

#[test]
fn deleted_items_are_not_assessed_and_removal_is_denied() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let expired = transcribed_at() + Duration::days(8);
    assert!(matches!(
        store.state(&item_id, expired),
        RetentionState::Expired { .. }
    ));

    let revision: i32 = store
        .open()
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [&item_id],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_intent(&mut store.open(), &item_id, revision, expired).unwrap();

    assert_eq!(store.assess(&item_id, expired, StoragePressure::Low), None);
    let sweep = sweep_audio_retention(
        store.open().conn(),
        &FixedClock(expired),
        &RetentionPolicy::default(),
        StoragePressure::Normal,
    )
    .unwrap();
    assert!(sweep.is_empty());
    let decision = authorize_audio_removal(
        store.open().conn(),
        &FixedClock(expired),
        &RetentionPolicy::default(),
        &item_id,
        FIRST_AUDIO,
    )
    .unwrap();
    assert_eq!(
        decision,
        RemovalDecision::Denied(RemovalDenial::DeletionOwnsCleanup)
    );
}

#[test]
fn storage_pressure_never_frees_sole_sources_or_unexpired_audio() {
    let store = Store::new();
    let sole_item = store.voice_item();
    let transcribed_item = store.import(SECOND_CAPTURE_ID, Some(SECOND_AUDIO), None);
    store.transcribe(&transcribed_item, SECOND_AUDIO, transcribed_at());
    let now = transcribed_at() + Duration::days(3);

    let sole = store.assess(&sole_item, now, StoragePressure::Low).unwrap();
    assert_eq!(
        sole.state,
        unresolved(UnresolvedReason::TranscriptionPending)
    );
    assert_eq!(sole.cleanup, CleanupIntent::None);
    assert_eq!(
        sole.storage_pressure_notice,
        Some(StoragePressureNotice::SoleSourceRetained)
    );
    assert!(sole.options.contains(&RetentionOption::DeleteAudio));

    let young = store
        .assess(&transcribed_item, now, StoragePressure::Low)
        .unwrap();
    assert!(matches!(young.state, RetentionState::Retained { .. }));
    assert_eq!(young.cleanup, CleanupIntent::None);
    assert_eq!(
        young.storage_pressure_notice,
        Some(StoragePressureNotice::NotYetEligible)
    );

    let normal = store
        .assess(&sole_item, now, StoragePressure::Normal)
        .unwrap();
    assert_eq!(normal.storage_pressure_notice, None);

    let expired = store
        .assess(
            &transcribed_item,
            transcribed_at() + Duration::days(7),
            StoragePressure::Low,
        )
        .unwrap();
    assert!(matches!(expired.cleanup, CleanupIntent::RemoveAudio { .. }));
    assert_eq!(expired.storage_pressure_notice, None);
}

#[test]
fn a_sweep_lists_every_live_audio_item_in_capture_order() {
    let store = Store::new();
    let first = store.voice_item();
    let second = store.import(SECOND_CAPTURE_ID, Some(SECOND_AUDIO), None);
    store.import(
        "3f9c1e52-0000-4000-8000-0000000000d3",
        None,
        Some("typed only"),
    );
    store.transcribe(&second, SECOND_AUDIO, transcribed_at());

    let sweep = sweep_audio_retention(
        store.open().conn(),
        &FixedClock(transcribed_at() + Duration::days(8)),
        &RetentionPolicy::default(),
        StoragePressure::Normal,
    )
    .unwrap();

    let mut listed: Vec<&str> = sweep.iter().map(|entry| entry.item_id.as_str()).collect();
    listed.sort_unstable();
    let mut expected = vec![first.as_str(), second.as_str()];
    expected.sort_unstable();
    assert_eq!(listed, expected, "typed-only items hold no audio");
    let by_item = |item_id: &str| sweep.iter().find(|entry| entry.item_id == item_id).unwrap();
    assert!(matches!(
        by_item(&second).cleanup,
        CleanupIntent::RemoveAudio { .. }
    ));
    assert_eq!(by_item(&first).cleanup, CleanupIntent::None);
}

#[test]
fn assessment_is_a_pure_read_that_repeats_identically_after_a_restart() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let now = transcribed_at() + Duration::days(9);
    let revision_before: i32 = store
        .open()
        .conn()
        .query_row("SELECT revision FROM items", [], |row| row.get(0))
        .unwrap();

    let first = store.assess(&item_id, now, StoragePressure::Normal);
    let second = store.assess(&item_id, now, StoragePressure::Normal);

    assert_eq!(first, second);
    let revision_after: i32 = store
        .open()
        .conn()
        .query_row("SELECT revision FROM items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(revision_before, revision_after);
    let audio: String = store
        .open()
        .conn()
        .query_row("SELECT audio_reference FROM captures", [], |row| row.get(0))
        .unwrap();
    assert_eq!(audio, FIRST_AUDIO, "the capture is never modified");
}

#[test]
fn removal_is_authorized_only_for_expired_audio_of_the_presented_reference() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcribe(&item_id, FIRST_AUDIO, transcribed_at());
    let expires_at = transcribed_at() + Duration::days(7);
    let decide = |now: DateTime<Utc>, policy: RetentionPolicy, item: &str, reference: &str| {
        authorize_audio_removal(
            store.open().conn(),
            &FixedClock(now),
            &policy,
            item,
            reference,
        )
        .unwrap()
    };
    let default_policy = RetentionPolicy::default();

    assert_eq!(
        decide(expires_at, default_policy, &item_id, FIRST_AUDIO),
        RemovalDecision::Approved {
            expired_at: expires_at
        }
    );
    assert_eq!(
        decide(
            expires_at - Duration::seconds(1),
            default_policy,
            &item_id,
            FIRST_AUDIO
        ),
        RemovalDecision::Denied(RemovalDenial::NotYetExpired { expires_at })
    );
    assert_eq!(
        decide(expires_at, default_policy, &item_id, "audio/other.m4a"),
        RemovalDecision::Denied(RemovalDenial::AudioReferenceMismatch)
    );
    assert_eq!(
        decide(expires_at, default_policy, "missing-item", FIRST_AUDIO),
        RemovalDecision::Denied(RemovalDenial::ItemNotFound)
    );

    // A stale plan made under a shorter period is denied once the period is lengthened.
    let longer = RetentionPolicy::new(Duration::days(30)).unwrap();
    assert_eq!(
        decide(expires_at, longer, &item_id, FIRST_AUDIO),
        RemovalDecision::Denied(RemovalDenial::NotYetExpired {
            expires_at: transcribed_at() + Duration::days(30)
        })
    );
}

#[test]
fn removal_is_denied_for_sole_source_audio() {
    let store = Store::new();
    let item_id = store.voice_item();
    store.transcription_result(
        &item_id,
        FIRST_AUDIO,
        RecognizerResult::Failed,
        transcribed_at(),
    );

    let decision = authorize_audio_removal(
        store.open().conn(),
        &FixedClock(transcribed_at() + Duration::days(365)),
        &RetentionPolicy::default(),
        &item_id,
        FIRST_AUDIO,
    )
    .unwrap();
    assert_eq!(
        decision,
        RemovalDecision::Denied(RemovalDenial::Unresolved {
            reason: UnresolvedReason::TranscriptionFailed
        })
    );
}
