//! Drives route provisioning against a freshly migrated store, directly and through the export.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, WAIT};
use crate::ingress::import_foreground_ingress;
use crate::privacy::routing::{
    authorize_job, AuthorizationDecision, DenialReason, JOB_TYPE_INTERPRET, JOB_TYPE_SHADOW_REVIEW,
};
use crate::store::captures::Capture;
use crate::store::schema::SystemClock;
use serde_json::{json, Value};
use std::sync::Arc;

const ROUTE_ID: &str = "route-capture";
const ROUTE_NAME: &str = "Capture";
const CREATED_AT: &str = "2026-10-14T08:00:00Z";

struct Store {
    directory: std::path::PathBuf,
    path: String,
}

impl Store {
    fn fresh(name: &str) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-route-provisioning-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        Store { directory, path }
    }

    fn open(&self) -> Database {
        Database::open(&self.path, Arc::new(SystemClock)).unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn provision(database: &mut Database, scope: ItemScope) -> Result<Provisioning, ProvisioningError> {
    ensure_local_capture_route(database, ROUTE_ID, ROUTE_NAME, scope, CREATED_AT)
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn route_row(database: &Database) -> (String, String, String, String, i64, String) {
    database
        .conn()
        .query_row(
            "SELECT route_id, route_name, scope, processing_destinations, preview_safe, created_at \
             FROM routes WHERE route_id = ?",
            [ROUTE_ID],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap()
}

fn capture(capture_id: &str) -> Capture {
    Capture {
        capture_id: capture_id.to_string(),
        text: Some("buy oat milk".to_string()),
        audio_reference: None,
        capture_instant: "2026-10-14T08:30:00Z".to_string(),
        timezone_id: "UTC".to_string(),
        utc_offset_minutes: 0,
        locale: "en_US".to_string(),
        calendar: "gregorian".to_string(),
        item_scope: "personal".to_string(),
        route_id: ROUTE_ID.to_string(),
        entry_locked: false,
        created_at: "2026-10-14T08:30:01Z".to_string(),
        session_topic: None,
    }
}

fn add_remote_job(database: &Database, job_id: &str, job_type: &str, item_id: &str) {
    let profile_version = format!("profile-{job_id}");
    database
        .conn()
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) \
             VALUES (?, ?, 'open_ai', NULL, 'model', 30, '{}', '[\"https://api.openai.com\"]', '{}', ?)",
            rusqlite::params![profile_version, format!("id-{job_id}"), CREATED_AT],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
             profile_version, status, created_at) VALUES (?, 1, ?, ?, 0, ?, 'queued', ?)",
            rusqlite::params![job_id, item_id, job_type, profile_version, CREATED_AT],
        )
        .unwrap();
}

#[test]
fn a_fresh_store_gets_a_local_only_route_with_no_authorizations() {
    let store = Store::fresh("create");
    let mut database = store.open();
    assert_eq!(count(&database, "routes"), 0);

    assert_eq!(
        provision(&mut database, ItemScope::Personal).unwrap(),
        Provisioning::Created
    );

    assert_eq!(
        route_row(&database),
        (
            ROUTE_ID.to_string(),
            ROUTE_NAME.to_string(),
            "personal".to_string(),
            "[]".to_string(),
            0,
            CREATED_AT.to_string()
        )
    );
    assert_eq!(count(&database, "route_authorizations"), 0);
}

#[test]
fn a_capture_on_the_provisioned_route_imports_and_creates_its_item() {
    let store = Store::fresh("import");
    let mut database = store.open();
    provision(&mut database, ItemScope::Personal).unwrap();

    let acknowledgment = import_foreground_ingress(&mut database, &capture("capture-1")).unwrap();

    assert_eq!(acknowledgment.capture_id, "capture-1");
    let item_capture: String = database
        .conn()
        .query_row(
            "SELECT capture_id FROM items WHERE item_id = ?",
            [acknowledgment.item_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(item_capture, "capture-1");
}

#[test]
fn every_remote_capability_is_denied_on_the_provisioned_route() {
    let store = Store::fresh("deny");
    let mut database = store.open();
    provision(&mut database, ItemScope::Personal).unwrap();
    let item_id = import_foreground_ingress(&mut database, &capture("capture-1"))
        .unwrap()
        .item_id;
    add_remote_job(&database, "job-interpret", JOB_TYPE_INTERPRET, &item_id);
    add_remote_job(&database, "job-review", JOB_TYPE_SHADOW_REVIEW, &item_id);

    for job_id in ["job-interpret", "job-review"] {
        assert!(
            matches!(
                authorize_job(database.conn(), job_id).unwrap(),
                AuthorizationDecision::Denied(DenialReason::DestinationNotInRoute)
            ),
            "{job_id} must be denied: {:?}",
            authorize_job(database.conn(), job_id).unwrap()
        );
    }
}

#[test]
fn an_existing_route_keeps_its_configuration() {
    let store = Store::fresh("existing");
    let mut database = store.open();
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, preview_safe, \
             created_at) VALUES (?, 'Configured name', 'personal', '[\"https://api.openai.com\"]', 1, \
             '2026-01-01T00:00:00Z')",
            [ROUTE_ID],
        )
        .unwrap();
    let before = route_row(&database);

    assert_eq!(
        provision(&mut database, ItemScope::Personal).unwrap(),
        Provisioning::Unchanged
    );

    assert_eq!(route_row(&database), before);
    assert_eq!(before.3, "[\"https://api.openai.com\"]");
    assert_eq!(before.4, 1);
}

#[test]
fn a_scope_mismatch_is_refused_and_changes_nothing() {
    let store = Store::fresh("mismatch");
    let mut database = store.open();
    provision(&mut database, ItemScope::Personal).unwrap();
    let before = route_row(&database);

    let error = provision(&mut database, ItemScope::Work).unwrap_err();

    assert!(matches!(error, ProvisioningError::ScopeMismatch));
    let failure = provisioning_failure(&error);
    assert_eq!(failure.code, "route_scope_mismatch");
    assert_eq!(failure.status(), OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(route_row(&database), before);
    assert_eq!(count(&database, "routes"), 1);
}

#[test]
fn a_second_call_is_a_no_op() {
    let store = Store::fresh("repeat");
    let mut database = store.open();
    assert_eq!(
        provision(&mut database, ItemScope::Personal).unwrap(),
        Provisioning::Created
    );
    let before = route_row(&database);

    assert_eq!(
        ensure_local_capture_route(
            &mut database,
            ROUTE_ID,
            "A different name",
            ItemScope::Personal,
            "2030-01-01T00:00:00Z"
        )
        .unwrap(),
        Provisioning::Unchanged
    );

    assert_eq!(route_row(&database), before);
    assert_eq!(count(&database, "routes"), 1);
    assert_eq!(count(&database, "route_authorizations"), 0);
}

#[test]
fn concurrent_callers_create_the_route_exactly_once() {
    let store = Store::fresh("concurrent");
    drop(store.open());
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let path = store.path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut database = Database::open(&path, Arc::new(SystemClock)).unwrap();
                barrier.wait();
                provision(&mut database, ItemScope::Personal).unwrap()
            })
        })
        .collect();
    let outcomes: Vec<Provisioning> = workers.into_iter().map(|w| w.join().unwrap()).collect();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == Provisioning::Created)
            .count(),
        1
    );
    let database = store.open();
    assert_eq!(count(&database, "routes"), 1);
    assert_eq!(count(&database, "route_authorizations"), 0);
}

fn export_call(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: &Value,
) -> crate::ffi::core_handle::tests::Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_ensure_local_capture_route(
            handle,
            operation_id,
            bytes.as_ptr(),
            bytes.len(),
        )
    })
}

#[test]
fn the_export_reports_the_disposition_and_the_normalized_scope_mismatch() {
    let _guard = serial();
    let store = Store::fresh("export");
    let mut handle = 0;
    let outcome =
        consume(unsafe { ohand_core_open(store.path.as_ptr(), store.path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    let (recorder, events) = recorder();
    register(handle, &recorder);
    let request =
        |scope: &str| json!({"route_id": ROUTE_ID, "route_name": ROUTE_NAME, "scope": scope});

    for (operation_id, scope, disposition) in
        [(1, "personal", "created"), (2, "personal", "unchanged")]
    {
        assert_eq!(
            export_call(handle, operation_id, &request(scope)).status,
            OHAND_CORE_STATUS_OK
        );
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
        export_call(handle, 3, &request("work")).status,
        OHAND_CORE_STATUS_OK
    );
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.status, OHAND_CORE_STATUS_PERMANENT);
    let body: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(body["code"], "route_scope_mismatch");

    let invalid = [
        json!({"route_id": ROUTE_ID, "route_name": ROUTE_NAME, "scope": "family"}),
        json!({"route_id": "", "route_name": ROUTE_NAME, "scope": "personal"}),
        json!({"route_id": ROUTE_ID, "route_name": ROUTE_NAME, "scope": "personal", "extra": 1}),
    ];
    for rejected in invalid {
        let outcome = export_call(handle, 4, &rejected);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT);
        assert_eq!(outcome.code(), "invalid_request");
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
