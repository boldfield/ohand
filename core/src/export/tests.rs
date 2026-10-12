//! Behavior tests for the scoped core snapshot export (L04a). Each test drives the real import,
//! correction, transcription and deletion paths against a file-backed store.

use super::reader::{read_export, reconstruct_item_state, ReadError};
use super::*;
use crate::domain::items::load_item_state;
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
use crate::store::schema::SystemClock;
use chrono::Duration;
use std::sync::Arc;
use tempfile::TempDir;

const AUDIO_REFERENCE: &str = "staging/2026-10-08/clip-voice.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const PROFILE_ENDPOINT: &str = "https://private-endpoint.invalid/v1";
const PROFILE_CREDENTIAL: &str = "keychain:credential-sentinel";
const ROUTE_NAME_SENTINEL: &str = "route-name-sentinel";
const ROUTE_DESTINATION_SENTINEL: &str = "destination-sentinel";
const NATIVE_ID_SENTINEL: &str = "native-notification-sentinel";

fn exported_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

struct Store {
    _directory: TempDir,
    path: String,
}

struct Seeded {
    store: Store,
    typed_item: String,
    work_item: String,
    voice_item: String,
    deleted_item: String,
}

impl Store {
    fn new() -> Store {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("ohand.db").display().to_string();
        let store = Store {
            _directory: directory,
            path,
        };
        let database = store.open();
        for (route_id, scope) in [("route-personal", "personal"), ("route-work", "work")] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                         created_at)
                     VALUES (?, ?, ?, ?, '2026-10-08T09:00:00Z')",
                    rusqlite::params![
                        route_id,
                        format!("{ROUTE_NAME_SENTINEL}-{route_id}"),
                        scope,
                        format!("[\"{ROUTE_DESTINATION_SENTINEL}\"]"),
                    ],
                )
                .unwrap();
        }
        database
            .conn()
            .execute(
                "INSERT INTO provider_profiles (profile_version, profile_id, provider_type,
                     endpoint, model, credential_ref, timeout_seconds, retry_policy,
                     authorized_destinations, capabilities, created_at)
                 VALUES ('profile-v1', 'profile', 'self_hosted', ?, 'model-x', ?, 30, '{}',
                         '[]', '[]', '2026-10-08T09:00:00Z')",
                rusqlite::params![PROFILE_ENDPOINT, PROFILE_CREDENTIAL],
            )
            .unwrap();
        store
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }

    fn import(&self, capture: &Capture) -> String {
        import_foreground_ingress(&mut self.open(), capture)
            .unwrap()
            .item_id
    }

    fn revision(&self, item_id: &str) -> i32 {
        self.open()
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn correct(
        &self,
        item_id: &str,
        event_id: &str,
        kind: CorrectionKind,
        old: Option<&str>,
        new: &str,
    ) {
        let revision = self.revision(item_id);
        let event = Event::new(
            event_id.to_string(),
            item_id.to_string(),
            revision,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind,
                old_value: old.map(str::to_string),
                new_value: new.to_string(),
            }),
            "2026-10-08T11:00:00Z".to_string(),
        )
        .unwrap();
        save_event(&mut self.open(), &event, revision).unwrap();
    }

    fn attach_transcript(&self, item_id: &str, text: &str) {
        let mut database = self.open();
        let now = DateTime::parse_from_rfc3339("2026-10-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        enqueue_job(
            &mut database,
            "transcription-1".to_string(),
            item_id.to_string(),
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
            self.revision(item_id),
            None,
            None,
            1,
            now,
        )
        .unwrap();
        let job = claim_job_with_lease(&mut database, Duration::seconds(300), now)
            .unwrap()
            .expect("job is claimable");
        apply_transcription_outcome(
            &mut database,
            &TranscriptionOutcome {
                job_id: job.job_id.clone(),
                lease_attempt: job.attempt_count,
                audio_reference: AUDIO_REFERENCE.to_string(),
                result: RecognizerResult::Transcript(RecognizedTranscript {
                    text: text.to_string(),
                    confidence: Some(0.91),
                    detected_language: Some("en-US".to_string()),
                    recognizer: "on-device-recognizer/1".to_string(),
                    audio_sha256: AUDIO_SHA256.to_string(),
                }),
                interpretation: None,
            },
            now + Duration::minutes(5),
        )
        .unwrap();
    }

    fn seed_reminder(&self, item_id: &str) {
        let database = self.open();
        database
            .conn()
            .execute(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state,
                     delivery_state, acknowledgment_state, resolved_instant, timezone_id,
                     source_phrase, schedule_generation, created_at, updated_at)
                 VALUES ('reminder-1', ?, 'resolved', 'scheduled', 'delivered',
                         'not_acknowledged', '2026-10-10T08:00:00Z', 'America/New_York',
                         'tomorrow morning', 1, '2026-10-08T09:31:00Z', '2026-10-08T09:32:00Z')",
                [item_id],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type,
                     operation_state, effect_identity, external_id, scheduled_for, created_at)
                 VALUES ('operation-1', 'reminder-1', 'schedule', 'acknowledged',
                         'schedule#1', ?, '2026-10-10T08:00:00Z', '2026-10-08T09:31:30Z')",
                [NATIVE_ID_SENTINEL],
            )
            .unwrap();
    }

    fn seeded() -> Seeded {
        let store = Store::new();
        let typed_item = store.import(&capture(
            "3f9c1e52-0000-4000-8000-000000000001",
            Some("buy oat milk"),
            None,
            "personal",
            "2026-10-08T09:30:01Z",
        ));
        store.correct(
            &typed_item,
            "correction-text",
            CorrectionKind::Text,
            Some("buy oat milk"),
            "buy oat milk and eggs",
        );
        store.correct(
            &typed_item,
            "correction-type",
            CorrectionKind::Type,
            None,
            "action",
        );
        store.seed_reminder(&typed_item);

        let work_item = store.import(&capture(
            "3f9c1e52-0000-4000-8000-000000000002",
            Some("quarterly roadmap draft"),
            None,
            "work",
            "2026-10-08T09:40:01Z",
        ));

        let voice_item = store.import(&capture(
            "3f9c1e52-0000-4000-8000-000000000003",
            None,
            Some(AUDIO_REFERENCE),
            "personal",
            "2026-10-08T09:50:01Z",
        ));
        store.attach_transcript(&voice_item, "water the ferns tomorrow");

        let deleted_item = store.import(&capture(
            "3f9c1e52-0000-4000-8000-000000000004",
            Some("deleted-secret-text"),
            None,
            "personal",
            "2026-10-08T10:00:01Z",
        ));
        store.correct(
            &deleted_item,
            "correction-deleted",
            CorrectionKind::Text,
            Some("deleted-secret-text"),
            "deleted-corrected-text",
        );
        let revision = store.revision(&deleted_item);
        mark_deletion_intent(&mut store.open(), &deleted_item, revision, exported_at()).unwrap();

        Seeded {
            store,
            typed_item,
            work_item,
            voice_item,
            deleted_item,
        }
    }
}

fn capture(
    capture_id: &str,
    text: Option<&str>,
    audio_reference: Option<&str>,
    scope: &str,
    created_at: &str,
) -> Capture {
    Capture::new(
        capture_id.to_string(),
        text.map(str::to_string),
        audio_reference.map(str::to_string),
        "2026-10-08T09:30:00Z".to_string(),
        "America/New_York".to_string(),
        -240,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        format!("route-{scope}"),
        false,
        created_at.to_string(),
        None,
    )
    .unwrap()
}

fn request(scopes: &[ItemScope], audio: AudioInclusion) -> ExportRequest {
    ExportRequest {
        authentication: ReadAuthentication::Authenticated,
        scopes: scopes.to_vec(),
        audio,
    }
}

fn export_bytes(store: &Store, request: &ExportRequest) -> Vec<u8> {
    let mut output = Vec::new();
    export_core_snapshot(&mut store.open(), request, exported_at(), &mut output).unwrap();
    output
}

fn item_ids(snapshot: &CoreSnapshot) -> Vec<&str> {
    snapshot
        .items
        .iter()
        .map(|item| item.item_id.as_str())
        .collect()
}

#[test]
fn export_needs_authentication_and_an_explicit_scope_and_writes_nothing_otherwise() {
    let seeded = Store::seeded();
    let mut output = Vec::new();

    let locked = ExportRequest {
        authentication: ReadAuthentication::CaptureOnly,
        ..request(&[ItemScope::Personal], AudioInclusion::Excluded)
    };
    let error = export_core_snapshot(
        &mut seeded.store.open(),
        &locked,
        exported_at(),
        &mut output,
    )
    .unwrap_err();
    assert!(matches!(error, ExportError::NotAuthenticated), "{error:?}");

    let no_scope = request(&[], AudioInclusion::Excluded);
    let error = export_core_snapshot(
        &mut seeded.store.open(),
        &no_scope,
        exported_at(),
        &mut output,
    )
    .unwrap_err();
    assert!(matches!(error, ExportError::NoScopeSelected), "{error:?}");

    assert!(output.is_empty(), "a refused export must write no byte");
}

#[test]
fn scopes_limit_the_export_and_deleted_items_never_appear() {
    let seeded = Store::seeded();
    let personal = read_core_snapshot(
        &mut seeded.store.open(),
        &request(&[ItemScope::Personal], AudioInclusion::Excluded),
        exported_at(),
    )
    .unwrap();
    assert_eq!(
        item_ids(&personal),
        vec![seeded.typed_item.as_str(), seeded.voice_item.as_str()]
    );
    assert_eq!(personal.header.scopes, vec!["personal"]);

    let work = read_core_snapshot(
        &mut seeded.store.open(),
        &request(&[ItemScope::Work], AudioInclusion::Excluded),
        exported_at(),
    )
    .unwrap();
    assert_eq!(item_ids(&work), vec![seeded.work_item.as_str()]);

    let both = read_core_snapshot(
        &mut seeded.store.open(),
        &request(
            &[ItemScope::Work, ItemScope::Personal, ItemScope::Work],
            AudioInclusion::Excluded,
        ),
        exported_at(),
    )
    .unwrap();
    assert_eq!(both.items.len(), 3);
    assert_eq!(both.header.scopes, vec!["personal", "work"]);

    let bytes = export_bytes(
        &seeded.store,
        &request(
            &[ItemScope::Personal, ItemScope::Work],
            AudioInclusion::References,
        ),
    );
    let text = String::from_utf8(bytes).unwrap();
    for absent in [
        seeded.deleted_item.as_str(),
        "deleted-secret-text",
        "deleted-corrected-text",
        "correction-deleted",
        "\"deletion\"",
    ] {
        assert!(!text.contains(absent), "deleted content leaked: {absent}");
    }
    let personal_text = String::from_utf8(export_bytes(
        &seeded.store,
        &request(&[ItemScope::Personal], AudioInclusion::Excluded),
    ))
    .unwrap();
    assert!(!personal_text.contains("quarterly roadmap draft"));
    assert!(!personal_text.contains(&seeded.work_item));
}

#[test]
fn a_scope_correction_moves_the_item_between_scope_exports() {
    let seeded = Store::seeded();
    seeded.store.correct(
        &seeded.work_item,
        "correction-scope",
        CorrectionKind::Scope,
        Some("work"),
        "personal",
    );
    let personal = read_core_snapshot(
        &mut seeded.store.open(),
        &request(&[ItemScope::Personal], AudioInclusion::Excluded),
        exported_at(),
    )
    .unwrap();
    assert!(item_ids(&personal).contains(&seeded.work_item.as_str()));
    let work = read_core_snapshot(
        &mut seeded.store.open(),
        &request(&[ItemScope::Work], AudioInclusion::Excluded),
        exported_at(),
    )
    .unwrap();
    assert!(work.items.is_empty());
}

#[test]
fn credentials_endpoints_and_route_configuration_never_appear() {
    let seeded = Store::seeded();
    let bytes = export_bytes(
        &seeded.store,
        &request(
            &[ItemScope::Personal, ItemScope::Work],
            AudioInclusion::References,
        ),
    );
    let text = String::from_utf8(bytes).unwrap();
    for absent in [
        PROFILE_ENDPOINT,
        PROFILE_CREDENTIAL,
        "profile-v1",
        ROUTE_NAME_SENTINEL,
        ROUTE_DESTINATION_SENTINEL,
        NATIVE_ID_SENTINEL,
        "provider_profiles",
    ] {
        assert!(!text.contains(absent), "configuration leaked: {absent}");
    }
}

#[test]
fn a_reader_reconstructs_equivalent_item_state_and_history() {
    let seeded = Store::seeded();
    let bytes = export_bytes(
        &seeded.store,
        &request(
            &[ItemScope::Personal, ItemScope::Work],
            AudioInclusion::References,
        ),
    );
    let snapshot = read_export(&bytes).unwrap();
    assert_eq!(snapshot.header.format, FORMAT_NAME);
    assert_eq!(snapshot.header.format_version, FORMAT_VERSION);
    assert_eq!(snapshot.header.exported_at, exported_at().to_rfc3339());
    assert_eq!(
        snapshot.header.store_schema_version,
        seeded.store.open().schema_version().unwrap()
    );

    let mut database = seeded.store.open();
    let transaction = database.transaction().unwrap();
    for item in &snapshot.items {
        let state = load_item_state(&transaction, &item.item_id)
            .unwrap()
            .unwrap();
        assert_eq!(reconstruct_item_state(item), state, "{}", item.item_id);
    }
    drop(transaction);

    let typed = snapshot
        .items
        .iter()
        .find(|item| item.item_id == seeded.typed_item)
        .unwrap();
    assert_eq!(typed.text.basis, "corrected");
    assert_eq!(typed.text.text.as_deref(), Some("buy oat milk and eggs"));
    assert_eq!(typed.capture.text.as_deref(), Some("buy oat milk"));
    assert_eq!(typed.item_type.as_deref(), Some("action"));
    assert!(typed.provenance.text_corrected && typed.provenance.type_corrected);
    let kinds: Vec<&str> = typed
        .corrections
        .iter()
        .map(|correction| correction.kind.as_str())
        .collect();
    assert_eq!(kinds, vec!["text", "type"]);
    assert_eq!(
        typed.corrections[0].old_value.as_deref(),
        Some("buy oat milk")
    );
    assert_eq!(typed.history.len(), 2);
    assert!(typed
        .history
        .iter()
        .all(|event| event.event_type == "correction"));

    let reminder = typed.reminder.as_ref().expect("reminder history");
    assert_eq!(reminder.request_state, "resolved");
    assert_eq!(reminder.delivery_state, "delivered");
    assert_eq!(reminder.source_phrase.as_deref(), Some("tomorrow morning"));
    assert_eq!(reminder.operations.len(), 1);
    assert_eq!(reminder.operations[0].operation_state, "acknowledged");
}

#[test]
fn audio_is_explicit_per_item_and_only_referenced_on_request() {
    let seeded = Store::seeded();
    let excluded_bytes = export_bytes(
        &seeded.store,
        &request(&[ItemScope::Personal], AudioInclusion::Excluded),
    );
    let excluded_text = String::from_utf8(excluded_bytes.clone()).unwrap();
    assert!(!excluded_text.contains(AUDIO_REFERENCE));
    assert!(!excluded_text.contains(AUDIO_SHA256));
    let excluded = read_export(&excluded_bytes).unwrap();
    assert_eq!(excluded.header.audio, "excluded");
    let audio_of = |snapshot: &CoreSnapshot, item_id: &str| {
        snapshot
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .unwrap()
            .audio
            .clone()
    };
    assert_eq!(
        audio_of(&excluded, &seeded.voice_item),
        ExportedAudio::Omitted
    );
    assert_eq!(audio_of(&excluded, &seeded.typed_item), ExportedAudio::None);

    let referenced = read_export(&export_bytes(
        &seeded.store,
        &request(&[ItemScope::Personal], AudioInclusion::References),
    ))
    .unwrap();
    assert_eq!(referenced.header.audio, "references");
    assert_eq!(
        audio_of(&referenced, &seeded.voice_item),
        ExportedAudio::Referenced {
            reference: AUDIO_REFERENCE.to_string()
        }
    );
    assert_eq!(
        audio_of(&referenced, &seeded.typed_item),
        ExportedAudio::None
    );

    let voice = referenced
        .items
        .iter()
        .find(|item| item.item_id == seeded.voice_item)
        .unwrap();
    assert_eq!(voice.text.basis, "transcribed");
    let transcript = voice.transcript.as_ref().expect("transcript provenance");
    assert_eq!(transcript.text, "water the ferns tomorrow");
    assert_eq!(transcript.audio_sha256.as_deref(), Some(AUDIO_SHA256));
    let omitted_voice = excluded
        .items
        .iter()
        .find(|item| item.item_id == seeded.voice_item)
        .unwrap();
    assert_eq!(
        omitted_voice.transcript.as_ref().unwrap().audio_sha256,
        None
    );
}

#[test]
fn identical_snapshots_serialize_identically_and_the_summary_matches_the_footer() {
    let seeded = Store::seeded();
    let both = request(
        &[ItemScope::Personal, ItemScope::Work],
        AudioInclusion::Excluded,
    );
    let first = export_bytes(&seeded.store, &both);
    let second = export_bytes(&seeded.store, &both);
    assert_eq!(first, second);

    let mut output = Vec::new();
    let summary =
        export_core_snapshot(&mut seeded.store.open(), &both, exported_at(), &mut output).unwrap();
    assert_eq!(summary.item_count, 3);
    let text = String::from_utf8(output).unwrap();
    assert!(text.lines().last().unwrap().contains(&summary.sha256));
    for line in text.lines() {
        assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
    }
}

#[test]
fn an_empty_scope_still_produces_a_complete_export() {
    let store = Store::new();
    let bytes = export_bytes(
        &store,
        &request(&[ItemScope::Work], AudioInclusion::Excluded),
    );
    let snapshot = read_export(&bytes).unwrap();
    assert!(snapshot.items.is_empty());
}

#[test]
fn every_truncation_of_an_export_is_refused() {
    let seeded = Store::seeded();
    let bytes = export_bytes(
        &seeded.store,
        &request(
            &[ItemScope::Personal, ItemScope::Work],
            AudioInclusion::References,
        ),
    );
    assert!(read_export(&bytes).is_ok());
    for length in 0..bytes.len() {
        assert!(
            read_export(&bytes[..length]).is_err(),
            "a {length}-byte prefix of {} bytes was accepted",
            bytes.len()
        );
    }
}

#[test]
fn malformed_or_altered_exports_are_refused() {
    let seeded = Store::seeded();
    let bytes = export_bytes(
        &seeded.store,
        &request(
            &[ItemScope::Personal, ItemScope::Work],
            AudioInclusion::Excluded,
        ),
    );
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let join = |lines: &[&str]| format!("{}\n", lines.join("\n"));

    // A complete line prefix without the footer.
    assert_eq!(
        read_export(join(&lines[..lines.len() - 1]).as_bytes()).unwrap_err(),
        ReadError::FooterNotLast
    );
    // Content changed after the digest was computed.
    let altered = text.replacen("buy oat milk and eggs", "buy oat milk and legs", 1);
    assert_ne!(altered, text);
    assert_eq!(
        read_export(altered.as_bytes()).unwrap_err(),
        ReadError::DigestMismatch
    );
    // A dropped item line.
    let mut without_item = lines.clone();
    without_item.remove(1);
    assert_eq!(
        read_export(join(&without_item).as_bytes()).unwrap_err(),
        ReadError::DigestMismatch
    );
    // A footer claiming a different count, with a digest valid for the content.
    let digest = hex_lower(&Sha256::digest(
        format!("{}\n", lines[..lines.len() - 1].join("\n")).as_bytes(),
    ));
    let wrong_count = format!(
        "{}\n{{\"record\":\"footer\",\"item_count\":99,\"sha256\":\"{digest}\"}}\n",
        lines[..lines.len() - 1].join("\n")
    );
    assert!(matches!(
        read_export(wrong_count.as_bytes()).unwrap_err(),
        ReadError::ItemCountMismatch { footer: 99, .. }
    ));
    // Not JSON, wrong first record, blank line, unknown format version, non-UTF-8.
    assert!(matches!(
        read_export(b"not an export\n").unwrap_err(),
        ReadError::Malformed { line: 1, .. }
    ));
    let mut item_first = lines.clone();
    item_first.swap(0, 1);
    assert_eq!(
        read_export(join(&item_first).as_bytes()).unwrap_err(),
        ReadError::HeaderNotFirst
    );
    let mut with_blank = lines.clone();
    with_blank.insert(2, "");
    assert_eq!(
        read_export(join(&with_blank).as_bytes()).unwrap_err(),
        ReadError::BlankLine { line: 3 }
    );
    let future = text.replacen("\"format_version\":1", "\"format_version\":2", 1);
    assert!(matches!(
        read_export(future.as_bytes()).unwrap_err(),
        ReadError::UnsupportedFormat { version: 2, .. }
    ));
    assert_eq!(
        read_export(&[0xff, 0xfe, b'\n']).unwrap_err(),
        ReadError::NotUtf8
    );
    // Duplicate item, and an item after the footer.
    let mut duplicated = lines.clone();
    duplicated.insert(2, lines[1]);
    assert!(matches!(
        read_export(join(&duplicated).as_bytes()).unwrap_err(),
        ReadError::DuplicateItem { .. }
    ));
    let mut after_footer = lines.clone();
    after_footer.push(lines[1]);
    assert!(matches!(
        read_export(join(&after_footer).as_bytes()).unwrap_err(),
        ReadError::UnexpectedRecord { .. }
    ));
}

struct FailingWriter {
    allowed_bytes: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.allowed_bytes {
            return Err(std::io::Error::other("disk full"));
        }
        self.allowed_bytes -= buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_write_is_reported() {
    let seeded = Store::seeded();
    let both = request(
        &[ItemScope::Personal, ItemScope::Work],
        AudioInclusion::Excluded,
    );
    let complete = export_bytes(&seeded.store, &both);
    let header_length = complete.iter().position(|byte| *byte == b'\n').unwrap() + 1;

    let mut writer = FailingWriter {
        allowed_bytes: header_length,
    };
    let error = export_core_snapshot(&mut seeded.store.open(), &both, exported_at(), &mut writer)
        .unwrap_err();
    assert!(matches!(error, ExportError::Write(_)), "{error:?}");
}

fn writer_edits(store: &Store, already_read_item: &str, not_yet_read_item: &str) {
    let mut writer = store.open();
    writer
        .conn()
        .busy_timeout(std::time::Duration::from_millis(0))
        .unwrap();
    for (event_id, item_id) in [
        ("late-completion-read", already_read_item),
        ("late-completion-unread", not_yet_read_item),
    ] {
        let revision = store.revision(item_id);
        let event = Event::new(
            event_id.to_string(),
            item_id.to_string(),
            revision,
            EventType::Completion,
            EventPayload::Completion,
            "2026-10-09T12:00:01Z".to_string(),
        )
        .unwrap();
        save_event(&mut writer, &event, revision).unwrap();
    }
    import_foreground_ingress(
        &mut writer,
        &capture(
            "3f9c1e52-0000-4000-8000-0000000000ff",
            Some("written during the export"),
            None,
            "personal",
            "2026-10-09T12:00:02Z",
        ),
    )
    .unwrap();
}

#[test]
fn concurrent_edits_cannot_split_a_snapshot() {
    let seeded = Store::seeded();
    seeded
        .store
        .open()
        .conn()
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let both = request(
        &[ItemScope::Personal, ItemScope::Work],
        AudioInclusion::Excluded,
    );
    let before = read_core_snapshot(&mut seeded.store.open(), &both, exported_at()).unwrap();

    let mut database = seeded.store.open();
    let mut edited = false;
    let during = read_core_snapshot_observed(&mut database, &both, exported_at(), &mut |read| {
        if read == 1 && !edited {
            edited = true;
            // Items are read oldest first: typed_item is already read at this point and
            // work_item is not, so a snapshot that is not one transaction would show the latter.
            writer_edits(&seeded.store, &seeded.typed_item, &seeded.work_item);
        }
    })
    .unwrap();
    assert!(edited);
    assert_eq!(during, before, "the snapshot must show one moment only");

    let after = read_core_snapshot(&mut seeded.store.open(), &both, exported_at()).unwrap();
    assert_eq!(after.items.len(), before.items.len() + 1);
    let typed = after
        .items
        .iter()
        .find(|item| item.item_id == seeded.typed_item)
        .unwrap();
    assert_eq!(typed.lifecycle_state, "completed");
    let work = after
        .items
        .iter()
        .find(|item| item.item_id == seeded.work_item)
        .unwrap();
    assert_eq!(work.lifecycle_state, "completed");
}

#[test]
fn without_wal_a_writer_is_held_off_until_the_snapshot_is_released() {
    let seeded = Store::seeded();
    let both = request(
        &[ItemScope::Personal, ItemScope::Work],
        AudioInclusion::Excluded,
    );
    let mut database = seeded.store.open();
    let mut blocked = None;
    let during = read_core_snapshot_observed(&mut database, &both, exported_at(), &mut |read| {
        if read == 1 {
            let writer = seeded.store.open();
            writer
                .conn()
                .busy_timeout(std::time::Duration::from_millis(0))
                .unwrap();
            blocked = Some(
                writer
                    .conn()
                    .execute(
                        "UPDATE items SET updated_at = 'changed' WHERE item_id = ?",
                        [&seeded.typed_item],
                    )
                    .is_err(),
            );
        }
    })
    .unwrap();
    assert_eq!(blocked, Some(true));
    assert!(during.items.iter().all(|item| item.updated_at != "changed"));
    seeded
        .store
        .open()
        .conn()
        .execute("UPDATE items SET updated_at = 'changed'", [])
        .unwrap();
}
