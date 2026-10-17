//! Drives the provider-profile export against a real store through a core handle.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::{OHAND_CORE_STATUS_OK, OHAND_CORE_STATUS_PERMANENT};
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, WAIT};
use serde_json::{json, Value};

const SENTINEL: &str = "SENTINEL-must-not-echo";

fn profile(profile_id: &str) -> Value {
    json!({
        "profile_id": profile_id,
        "protocol": "open_ai",
        "model": "synthetic-model",
        "credential_ref": "credential-ref/synthetic-primary",
        "timeout_seconds": 30,
        "capabilities": ["text_interpretation"],
    })
}

struct Harness {
    handle: OhandCoreHandle,
    events: std::sync::mpsc::Receiver<crate::ffi::core_handle::tests::Event>,
    recorder: Box<crate::ffi::core_handle::tests::Recorder>,
    directory: std::path::PathBuf,
    next_operation: u64,
}

impl Harness {
    fn new(name: &str) -> Harness {
        let directory = std::env::temp_dir().join(format!(
            "ohand-provider-profiles-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let mut handle = 0;
        let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Harness {
            handle,
            events,
            recorder,
            directory,
            next_operation: 1,
        }
    }

    /// Submits `request` and returns the outcome event's status and JSON body.
    fn run(&mut self, request: &Value) -> (u32, Value) {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let bytes = serde_json::to_vec(request).unwrap();
        let accepted = consume(unsafe {
            ohand_core_start_provider_profiles(
                self.handle,
                operation_id,
                bytes.as_ptr(),
                bytes.len(),
            )
        });
        assert_eq!(accepted.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        (
            event.status,
            serde_json::from_slice(&event.payload).unwrap(),
        )
    }

    fn rejected_up_front(&self, bytes: &[u8]) -> (u32, String) {
        let outcome = consume(unsafe {
            ohand_core_start_provider_profiles(self.handle, 99, bytes.as_ptr(), bytes.len())
        });
        let code = if outcome.payload.is_empty() {
            String::new()
        } else {
            outcome.code()
        };
        (outcome.status, code)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = consume(ohand_core_set_event_callback(
            self.handle,
            None,
            std::ptr::null_mut(),
        ));
        let _ = consume(ohand_core_close(self.handle));
        let _ = &self.recorder;
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn create_set_list_and_clear_round_trip_through_the_export() {
    let _guard = serial();
    let mut harness = Harness::new("round-trip");

    let (status, created) =
        harness.run(&json!({"command": "create_profile", "profile": profile("p")}));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    assert_eq!(created["operation_id"], 1);
    assert_eq!(created["profile_id"], "p");
    let version = created["profile_version"].as_str().unwrap().to_string();

    let (status, set) = harness.run(&json!({
        "command": "set_default", "capability": "text_interpretation", "profile_version": version
    }));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    assert_eq!(set["capability"], "text_interpretation");
    assert_eq!(set["profile_version"], version);

    let (status, listing) = harness.run(&json!({"command": "list"}));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    let entry = &listing["profiles"][0]["versions"][0];
    assert_eq!(listing["profiles"][0]["profile_id"], "p");
    assert_eq!(entry["profile_version"], version);
    assert_eq!(entry["protocol"], "open_ai");
    assert_eq!(entry["model"], "synthetic-model");
    assert_eq!(entry["credential_bound"], true);
    assert_eq!(entry["revoked"], false);
    assert_eq!(entry["default_for"], json!(["text_interpretation"]));
    assert_eq!(
        listing["defaults"],
        json!([{"capability": "text_interpretation", "profile_version": version}])
    );
    assert!(!listing.to_string().contains("credential-ref"));

    let (status, cleared) =
        harness.run(&json!({"command": "clear_default", "capability": "text_interpretation"}));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    assert_eq!(cleared["capability"], "text_interpretation");
    let (_, listing) = harness.run(&json!({"command": "list"}));
    assert_eq!(listing["defaults"], json!([]));
}

#[test]
fn refusals_carry_fixed_codes_and_never_echo_the_request() {
    let _guard = serial();
    let mut harness = Harness::new("refusals");

    let mut hostile = profile(SENTINEL);
    hostile["protocol"] = json!(SENTINEL);
    let mut oversized_timeout = profile("p");
    oversized_timeout["timeout_seconds"] = json!(100_000);
    let mut missing_credential = profile("p");
    missing_credential["credential_ref"] = Value::Null;
    let mut bad_endpoint = profile("p");
    bad_endpoint["protocol"] = json!("self_hosted");
    bad_endpoint["endpoint"] = json!(format!("http://{SENTINEL}.test"));

    for (profile, code) in [
        (hostile, "unknown_protocol"),
        (oversized_timeout, "timeout_out_of_bounds"),
        (missing_credential, "credential_required"),
        (bad_endpoint, "endpoint_invalid"),
    ] {
        let (status, body) = harness.run(&json!({"command": "create_profile", "profile": profile}));
        assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
        assert_eq!(body["code"], code);
        assert!(!body.to_string().contains(SENTINEL));
    }

    let (status, body) = harness.run(&json!({
        "command": "set_default", "capability": "text_interpretation", "profile_version": SENTINEL
    }));
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(body["code"], "profile_not_found");
    assert!(!body.to_string().contains(SENTINEL));

    let (status, body) = harness.run(&json!({
        "command": "set_default", "capability": SENTINEL, "profile_version": "v"
    }));
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(body["code"], "capability_not_configurable");
    assert!(!body.to_string().contains(SENTINEL));

    let (status, body) =
        harness.run(&json!({"command": "clear_default", "capability": "transcription"}));
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(body["code"], "capability_not_configurable");

    let (_, listing) = harness.run(&json!({"command": "list"}));
    assert_eq!(listing["profiles"], json!([]));
}

#[test]
fn a_credential_claimed_by_a_pending_reset_is_refused_through_the_export() {
    let _guard = serial();
    let mut harness = Harness::new("pending-removal");
    let path = harness
        .directory
        .join("core.sqlite")
        .to_string_lossy()
        .into_owned();
    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO reset_generations (phase, boundary_rowid, started_at) \
             VALUES ('pending', 0, '2026-10-14T10:00:00Z')",
            [],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO reset_credential_claims (credential_ref, generation) \
             VALUES ('credential-ref/synthetic-primary', 1)",
            [],
        )
        .unwrap();
    drop(database);

    let (status, body) =
        harness.run(&json!({"command": "create_profile", "profile": profile("p")}));
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(body["code"], "credential_pending_removal");
}

#[test]
fn malformed_requests_are_rejected_before_anything_is_queued() {
    let _guard = serial();
    let harness = Harness::new("malformed");
    let rejected: [Vec<u8>; 7] = [
        b"not json".to_vec(),
        serde_json::to_vec(&json!({"command": "delete_everything"})).unwrap(),
        serde_json::to_vec(&json!({"command": "list", "extra": 1})).unwrap(),
        serde_json::to_vec(&json!({
            "command": "create_profile",
            "profile": {"profile_id": "p", "protocol": "open_ai", "model": "m",
                        "timeout_seconds": 30, "capabilities": [],
                        "authorized_destinations": ["https://evil.test"]}
        }))
        .unwrap(),
        serde_json::to_vec(&json!({
            "command": "set_default", "capability": "text_interpretation", "profile_version": ""
        }))
        .unwrap(),
        serde_json::to_vec(&json!({
            "command": "set_default", "capability": "x".repeat(MAX_IDENTIFIER_BYTES + 1),
            "profile_version": "v"
        }))
        .unwrap(),
        Vec::new(),
    ];
    for bytes in rejected {
        let (status, code) = harness.rejected_up_front(&bytes);
        assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
        assert_eq!(code, "invalid_request");
    }
    assert!(harness.events.try_recv().is_err());
}

#[test]
fn credential_reference_is_read_by_version_and_the_listing_still_omits_it() {
    let _guard = serial();
    let mut harness = Harness::new("credential-reference");
    let (_, created) = harness.run(&json!({"command": "create_profile", "profile": profile("p")}));
    let version = created["profile_version"].as_str().unwrap().to_string();

    let (status, read) =
        harness.run(&json!({"command": "credential_reference", "profile_version": version}));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    assert_eq!(read["profile_version"], version);
    assert_eq!(read["credential_ref"], "credential-ref/synthetic-primary");

    let (_, listing) = harness.run(&json!({"command": "list"}));
    assert!(!listing.to_string().contains("credential-ref"));
}

#[test]
fn credential_reference_distinguishes_unknown_and_unbound_versions() {
    let _guard = serial();
    let mut harness = Harness::new("credential-reference-refusals");
    let (_, created) = harness.run(&json!({"command": "create_profile", "profile": profile("p")}));
    let version = created["profile_version"].as_str().unwrap().to_string();
    let path = harness
        .directory
        .join("core.sqlite")
        .to_string_lossy()
        .into_owned();
    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    database
        .conn()
        .execute(
            "UPDATE provider_profiles SET credential_ref = NULL WHERE profile_version = ?",
            [&version],
        )
        .unwrap();
    drop(database);

    let (status, unbound) =
        harness.run(&json!({"command": "credential_reference", "profile_version": version}));
    assert_eq!(status, OHAND_CORE_STATUS_OK);
    assert_eq!(unbound["credential_ref"], Value::Null);

    let (status, unknown) =
        harness.run(&json!({"command": "credential_reference", "profile_version": SENTINEL}));
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(unknown["code"], "profile_not_found");
    assert!(!unknown.to_string().contains(SENTINEL));

    let (status, code) = harness.rejected_up_front(
        &serde_json::to_vec(&json!({"command": "credential_reference", "profile_version": ""}))
            .unwrap(),
    );
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(code, "invalid_request");
}
