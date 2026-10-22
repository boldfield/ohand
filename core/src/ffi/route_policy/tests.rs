//! Drives the route policy exports through a real handle and checks the normalized outcomes.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, Outcome, WAIT};
use crate::store::schema::SystemClock;
use serde_json::{json, Value};
use std::sync::Arc;

const CREATED_AT: &str = "2026-10-14T08:00:00Z";

struct Store {
    directory: std::path::PathBuf,
    path: String,
}

impl Store {
    fn fresh(name: &str) -> Store {
        let directory =
            std::env::temp_dir().join(format!("ohand-route-policy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        Store { directory, path }
    }

    fn seed(&self) {
        let database = Database::open(&self.path, Arc::new(SystemClock)).unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
                 preview_safe, created_at) VALUES ('route-a', 'Capture', 'personal', '[]', 0, ?)",
                [CREATED_AT],
            )
            .unwrap();
        for (version, revoked_at) in [
            ("profile-live", None),
            ("profile-revoked", Some(CREATED_AT)),
        ] {
            database
                .conn()
                .execute(
                    "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, \
                     endpoint, model, timeout_seconds, retry_policy, authorized_destinations, \
                     capabilities, created_at, revoked_at) VALUES (?, ?, 'open_ai', NULL, 'model', \
                     30, '{}', '[\"https://api.openai.com\"]', '{}', ?, ?)",
                    rusqlite::params![version, format!("id-{version}"), CREATED_AT, revoked_at],
                )
                .unwrap();
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn with_request(
    request: &Value,
    call: unsafe extern "C" fn(OhandCoreHandle, u64, *const u8, usize) -> OhandCoreResult,
    handle: OhandCoreHandle,
    operation_id: u64,
) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe { call(handle, operation_id, bytes.as_ptr(), bytes.len()) })
}

#[test]
fn grant_list_and_revoke_through_the_exports() {
    let _guard = serial();
    let store = Store::fresh("export");
    store.seed();
    let mut handle = 0;
    let outcome =
        consume(unsafe { ohand_core_open(store.path.as_ptr(), store.path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    let (recorder, events) = recorder();
    register(handle, &recorder);

    let grant = json!({
        "route_id": "route-a",
        "capability": "text_interpretation",
        "profile_version": "profile-live"
    });
    for (operation_id, disposition) in [(1, "granted"), (2, "unchanged")] {
        let queued = with_request(
            &grant,
            ohand_core_start_grant_route_capability,
            handle,
            operation_id,
        );
        assert_eq!(queued.status, OHAND_CORE_STATUS_OK);
        let event = events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.status, OHAND_CORE_STATUS_OK);
        assert_ne!(event.thread, std::thread::current().id());
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            body,
            json!({"operation_id": operation_id, "disposition": disposition})
        );
    }

    assert_eq!(
        consume(ohand_core_start_list_route_policy(handle, 3)).status,
        OHAND_CORE_STATUS_OK
    );
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.status, OHAND_CORE_STATUS_OK);
    let body: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(
        body,
        json!({
            "operation_id": 3,
            "routes": [{
                "route_id": "route-a",
                "route_name": "Capture",
                "scope": "personal",
                "granted_capabilities": [
                    {
                        "capability": "text_interpretation",
                        "destination_classes": ["cloud"],
                        "covered_profile_versions": ["profile-live"]
                    }
                ]
            }]
        })
    );

    let revoke = json!({"route_id": "route-a", "capability": "text_interpretation"});
    for (operation_id, disposition) in [(4, "revoked"), (5, "unchanged")] {
        let queued = with_request(
            &revoke,
            ohand_core_start_revoke_route_capability,
            handle,
            operation_id,
        );
        assert_eq!(queued.status, OHAND_CORE_STATUS_OK);
        let event = events.recv_timeout(WAIT).expect("an outcome event");
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(
            body,
            json!({"operation_id": operation_id, "disposition": disposition})
        );
    }

    assert_eq!(
        consume(ohand_core_set_event_callback(
            handle,
            None,
            std::ptr::null_mut()
        ))
        .status,
        0
    );
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    drop(recorder);
}

#[test]
fn refusals_are_normalized_and_never_echo_the_request() {
    let _guard = serial();
    let store = Store::fresh("refusals");
    store.seed();
    let mut handle = 0;
    let outcome =
        consume(unsafe { ohand_core_open(store.path.as_ptr(), store.path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    let (recorder, events) = recorder();
    register(handle, &recorder);

    let grant = |route: &str, capability: &str, profile: &str| json!({"route_id": route, "capability": capability, "profile_version": profile});
    let refusals = [
        (
            grant("route-a", "text_interpretation", "profile-revoked"),
            "profile_revoked",
        ),
        (
            grant("route-a", "text_interpretation", "profile-secret-missing"),
            "profile_unavailable",
        ),
        (
            grant(
                "route-secret-missing",
                "text_interpretation",
                "profile-live",
            ),
            "route_unknown",
        ),
        (
            grant("route-a", "transcription", "profile-live"),
            "capability_not_grantable",
        ),
    ];
    for (operation_id, (request, code)) in (1..).zip(refusals) {
        let queued = with_request(
            &request,
            ohand_core_start_grant_route_capability,
            handle,
            operation_id,
        );
        assert_eq!(queued.status, OHAND_CORE_STATUS_OK);
        let event = events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.status, OHAND_CORE_STATUS_PERMANENT);
        let text = String::from_utf8(event.payload.clone()).unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["code"], code);
        assert!(!text.contains("secret-missing"));
    }

    let revoke_unknown = json!({"route_id": "route-missing", "capability": "review"});
    let queued = with_request(
        &revoke_unknown,
        ohand_core_start_revoke_route_capability,
        handle,
        10,
    );
    assert_eq!(queued.status, OHAND_CORE_STATUS_OK);
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    let body: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(body["code"], "route_unknown");

    let invalid = [
        grant("route-a", "summarize", "profile-live"),
        grant("", "review", "profile-live"),
        grant("route-a", "review", " "),
        json!({"route_id": "route-a", "capability": "review", "profile_version": "profile-live",
               "destinations": ["https://evil.example.test"]}),
    ];
    for request in invalid {
        let outcome = with_request(
            &request,
            ohand_core_start_grant_route_capability,
            handle,
            11,
        );
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT);
        assert_eq!(outcome.code(), "invalid_request");
    }
    let outcome = with_request(
        &json!({"route_id": "route-a", "capability": "review", "extra": 1}),
        ohand_core_start_revoke_route_capability,
        handle,
        12,
    );
    assert_eq!(outcome.code(), "invalid_request");
    let outcome = consume(unsafe {
        ohand_core_start_grant_route_capability(handle, 13, std::ptr::null(), 0)
    });
    assert_eq!(outcome.code(), "invalid_request");

    assert_eq!(
        consume(ohand_core_set_event_callback(
            handle,
            None,
            std::ptr::null_mut()
        ))
        .status,
        0
    );
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    drop(recorder);
}
