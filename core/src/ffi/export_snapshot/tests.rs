//! Drives the export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, Event, Outcome, WAIT};
use crate::ingress::import::import_foreground_ingress;
use crate::lifecycle::delete_intent::mark_deletion_intent;
use crate::store::captures::Capture;
use crate::store::schema::SystemClock;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tempfile::TempDir;

const AUDIO_REFERENCE: &str = "FinalizedAudio/voice-1.m4a";
const ENDPOINT_SENTINEL: &str = "https://private-endpoint.invalid/v1";
const CREDENTIAL_SENTINEL: &str = "keychain:credential-sentinel";
const DELETED_TEXT: &str = "deleted-secret-text";

fn capture(
    capture_id: &str,
    text: Option<&str>,
    audio: Option<&str>,
    scope: &str,
    created_at: &str,
) -> Capture {
    Capture::new(
        capture_id.to_string(),
        text.map(str::to_string),
        audio.map(str::to_string),
        "2026-10-10T09:30:00+00:00".to_string(),
        "UTC".to_string(),
        0,
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

struct Fixture {
    directory: TempDir,
    store_path: String,
    voice_item: String,
}

impl Fixture {
    fn new() -> Fixture {
        let directory = TempDir::new().unwrap();
        let store_path = directory.path().join("core.sqlite").display().to_string();
        let mut database = Database::open(&store_path, Arc::new(SystemClock)).unwrap();
        for scope in ["personal", "work"] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                         created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-08T09:00:00Z')",
                    rusqlite::params![format!("route-{scope}"), format!("route-{scope}"), scope],
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
                rusqlite::params![ENDPOINT_SENTINEL, CREDENTIAL_SENTINEL],
            )
            .unwrap();
        let mut import = |capture: &Capture| {
            import_foreground_ingress(&mut database, capture)
                .unwrap()
                .item_id
        };
        import(&capture(
            "note-1",
            Some("personal note"),
            None,
            "personal",
            "2026-10-10T09:30:01+00:00",
        ));
        import(&capture(
            "work-1",
            Some("work note"),
            None,
            "work",
            "2026-10-10T09:30:02+00:00",
        ));
        let voice_item = import(&capture(
            "voice-1",
            None,
            Some(AUDIO_REFERENCE),
            "personal",
            "2026-10-10T09:30:03+00:00",
        ));
        let deleted_item = import(&capture(
            "gone-1",
            Some(DELETED_TEXT),
            None,
            "personal",
            "2026-10-10T09:30:04+00:00",
        ));
        mark_deletion_intent(&mut database, &deleted_item, 0, Utc::now()).unwrap();
        drop(database);
        Fixture {
            directory,
            store_path,
            voice_item,
        }
    }

    fn output(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

fn open(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
}

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe { ohand_core_start_export(handle, operation_id, bytes.as_ptr(), bytes.len()) })
}

fn request(authentication: &str, scopes: &[&str], audio: &str, output: &Path) -> Value {
    json!({
        "read_authentication": authentication,
        "scopes": scopes,
        "audio": audio,
        "output_path": output.display().to_string(),
    })
}

struct Session {
    handle: OhandCoreHandle,
    events: std::sync::mpsc::Receiver<Event>,
    _recorder: Box<crate::ffi::core_handle::tests::Recorder>,
}

impl Session {
    fn over(path: &str) -> Session {
        let handle = open(path);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
        }
    }

    fn event(&self, request: &Value) -> Event {
        let outcome = start(self.handle, 7, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, 7);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn succeed(&self, request: &Value) -> Value {
        let event = self.event(request);
        assert_eq!(
            event.status,
            OHAND_CORE_STATUS_OK,
            "{:?}",
            String::from_utf8_lossy(&event.payload)
        );
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(body["operation_id"], 7);
        body
    }

    fn fail(&self, request: &Value) -> (u32, String) {
        let event = self.event(request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        let value: Value = serde_json::from_slice(&event.payload).unwrap();
        (event.status, value["code"].as_str().unwrap().to_string())
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn read_lines(path: &Path) -> (String, Vec<Value>) {
    let text = std::fs::read_to_string(path).unwrap();
    let lines = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (text, lines)
}

#[test]
fn an_authenticated_export_writes_only_the_selected_scope_and_names_its_audio() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("export.jsonl");

    let body = session.succeed(&request(
        "authenticated",
        &["personal"],
        "references",
        &output,
    ));

    let (text, lines) = read_lines(&output);
    assert_eq!(lines.first().unwrap()["record"], "header");
    let items: Vec<&Value> = lines
        .iter()
        .filter(|line| line["record"] == "item")
        .collect();
    assert_eq!(items.len(), 2, "one text note and one voice note");
    assert!(items.iter().all(|item| item["scope"] == "personal"));
    assert_eq!(body["item_count"], 2);
    assert!(!text.contains("work note"), "work scope stays out");
    assert!(!text.contains(DELETED_TEXT), "deleted content stays out");
    assert!(!text.contains(ENDPOINT_SENTINEL));
    assert!(!text.contains(CREDENTIAL_SENTINEL));

    let footer = lines.last().unwrap();
    assert_eq!(footer["record"], "footer");
    assert_eq!(footer["sha256"], body["sha256"]);
    let before_footer = &text[..text.rfind("{\"record\":\"footer\"").unwrap()];
    let digest = Sha256::digest(before_footer.as_bytes());
    assert_eq!(
        crate::export::hex_lower(&digest),
        body["sha256"].as_str().unwrap()
    );

    let references = body["audio_references"].as_array().unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0]["item_id"], fixture.voice_item.as_str());
    assert_eq!(references[0]["capture_id"], "voice-1");
    assert_eq!(references[0]["reference"], AUDIO_REFERENCE);
    session.close();
}

#[cfg(unix)]
#[test]
fn the_export_file_is_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("export.jsonl");
    session.succeed(&request(
        "authenticated",
        &["personal", "work"],
        "excluded",
        &output,
    ));
    let mode = std::fs::metadata(&output).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "no group or world access");
    session.close();
}

#[test]
fn excluded_audio_leaves_no_reference_in_the_file_or_the_event() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("export.jsonl");

    let body = session.succeed(&request(
        "authenticated",
        &["personal", "work"],
        "excluded",
        &output,
    ));

    let (text, lines) = read_lines(&output);
    assert_eq!(body["item_count"], 3);
    assert_eq!(body["audio_references"], json!([]));
    assert!(!text.contains(AUDIO_REFERENCE));
    let voice = lines
        .iter()
        .find(|line| line["capture_id"] == "voice-1")
        .unwrap();
    assert_eq!(voice["audio"]["status"], "omitted");
    session.close();
}

#[test]
fn a_capture_only_session_reads_nothing_and_creates_no_file() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("export.jsonl");

    let (status, code) = session.fail(&request("capture_only", &["personal"], "excluded", &output));

    assert_eq!(status, OHAND_CORE_STATUS_UNAUTHORIZED);
    assert_eq!(code, "export_not_authorized");
    assert!(!output.exists());
    session.close();
}

#[test]
fn an_existing_destination_is_never_replaced() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("export.jsonl");
    std::fs::write(&output, "earlier file").unwrap();

    let (status, code) = session.fail(&request(
        "authenticated",
        &["personal"],
        "excluded",
        &output,
    ));

    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(code, "export_write_failed");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "earlier file");
    session.close();
}

#[test]
fn an_unwritable_destination_is_a_normalized_failure_without_a_file() {
    let _guard = serial();
    let fixture = Fixture::new();
    let session = Session::over(&fixture.store_path);
    let output = fixture.output("missing-directory").join("export.jsonl");

    let (_, code) = session.fail(&request(
        "authenticated",
        &["personal"],
        "excluded",
        &output,
    ));

    assert_eq!(code, "export_write_failed");
    assert!(!output.exists());
    session.close();
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let fixture = Fixture::new();
    let handle = open(&fixture.store_path);
    let output = fixture.output("export.jsonl");
    let good = request("authenticated", &["personal"], "excluded", &output);

    let mut variants: Vec<Value> = Vec::new();
    for (field, value) in [
        ("read_authentication", json!("locked")),
        ("scopes", json!([])),
        ("scopes", json!(["everything"])),
        ("audio", json!("all")),
        ("output_path", json!("")),
        ("output_path", json!("relative/export.jsonl")),
        ("unexpected", json!(true)),
    ] {
        let mut variant = good.clone();
        variant[field] = value;
        variants.push(variant);
    }
    for variant in &variants {
        let outcome = start(handle, 1, variant);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{variant}");
        assert_eq!(outcome.code(), "invalid_request", "{variant}");
    }
    let bytes: &[u8] = b"not json";
    let outcome =
        consume(unsafe { ohand_core_start_export(handle, 1, bytes.as_ptr(), bytes.len()) });
    assert_eq!(outcome.code(), "invalid_request");
    let outcome = consume(unsafe { ohand_core_start_export(handle, 1, std::ptr::null(), 0) });
    assert_eq!(outcome.code(), "invalid_request");
    let outcome = consume(unsafe { ohand_core_start_export(handle, 1, std::ptr::null(), 4) });
    assert_eq!(outcome.code(), "null_argument");
    assert!(!output.exists());
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}
