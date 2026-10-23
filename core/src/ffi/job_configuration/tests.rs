//! Drives the job-configuration export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::ffi::job_runner::{
    ohand_core_cancel_job_drain, ohand_core_set_job_host, ohand_core_start_job_drain,
    OHAND_JOB_HOST_COMMAND_SEND,
};
use crate::ingress::import_foreground_ingress;
use crate::jobs::queue::{claim_job_with_lease, get_jobs_for_item};
use crate::privacy::routing::{grant_route_capability, ProcessingCapability};
use crate::providers::profiles::{create_profile_version, set_default_profile, ProfileDraft};
use crate::store::captures::Capture;
use crate::store::schema::SystemClock;
use chrono::Duration;
use std::ffi::c_void;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ROUTE_ID: &str = "route-1";
const INTERPRET: ProcessingCapability = ProcessingCapability::TextInterpretation;

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_job_configuration(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn open_database(path: &str) -> Database {
    Database::open(path, Arc::new(SystemClock)).unwrap()
}

struct Store {
    directory: std::path::PathBuf,
    path: String,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Store {
    fn new(name: &str) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-job-configuration-ffi-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        open_database(&path)
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                 VALUES (?, 'private notes', 'personal', '[]', '2026-10-14T10:30:00Z')",
                [ROUTE_ID],
            )
            .unwrap();
        Store { directory, path }
    }

    /// An unrecognized typed capture that waits for a provider; returns its item id.
    fn waiting_capture(&self, number: usize) -> String {
        let mut database = open_database(&self.path);
        let capture = Capture::new(
            format!("3f9c1e52-0000-4000-8000-{number:012}"),
            Some("call the roofer about the leak".to_string()),
            None,
            "2026-10-14T10:30:00Z".to_string(),
            "UTC".to_string(),
            0,
            "en_US".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            ROUTE_ID.to_string(),
            false,
            "2026-10-14T10:30:00Z".to_string(),
            None,
        )
        .unwrap();
        import_foreground_ingress(&mut database, &capture)
            .unwrap()
            .item_id
    }

    /// Stop the queued job for configuration, as the dispatcher does with no provider.
    fn fail_next_job_for_configuration(&self) {
        let mut database = open_database(&self.path);
        let job = claim_job_with_lease(&mut database, Duration::seconds(60), Utc::now())
            .unwrap()
            .expect("a job is queued");
        database
            .conn()
            .execute(
                "UPDATE jobs SET status = 'failed', failure_reason = 'unsupported', \
                 lease_expires_at = NULL WHERE job_id = ?",
                [&job.job_id],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "UPDATE items SET processing_state = 'unprocessed' WHERE item_id = ?",
                [&job.item_id],
            )
            .unwrap();
    }

    fn profile(&self, profile_id: &str) -> String {
        let draft: ProfileDraft = serde_json::from_value(json!({
            "profile_id": profile_id,
            "protocol": "open_ai",
            "model": "synthetic-model",
            "credential_ref": format!("credential-ref/{profile_id}"),
            "timeout_seconds": 30,
            "capabilities": ["text_interpretation"],
        }))
        .unwrap();
        create_profile_version(&mut open_database(&self.path), &draft, Utc::now())
            .unwrap()
            .profile_version
    }

    fn configure(&self, profile_id: &str) -> String {
        let profile_version = self.profile(profile_id);
        set_default_profile(
            &mut open_database(&self.path),
            INTERPRET,
            &profile_version,
            Utc::now(),
        )
        .unwrap();
        grant_route_capability(
            &mut open_database(&self.path),
            ROUTE_ID,
            INTERPRET,
            &profile_version,
            "2026-10-14T10:30:00Z",
        )
        .unwrap();
        profile_version
    }

    fn jobs(&self, item_id: &str) -> Vec<crate::jobs::queue::Job> {
        get_jobs_for_item(&open_database(&self.path), item_id).unwrap()
    }
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<CoreEvent>,
    _recorder: Box<Recorder>,
    next_operation_id: u64,
}

impl Session {
    fn over(path: &str) -> Session {
        let mut handle = 0;
        let opened = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(opened.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
            next_operation_id: 1,
        }
    }

    fn call(&mut self, request: &Value) -> CoreEvent {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn succeed(&mut self, request: &Value) -> Value {
        let event = self.call(request);
        assert_eq!(
            event.status,
            OHAND_CORE_STATUS_OK,
            "{:?}",
            String::from_utf8_lossy(&event.payload)
        );
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(body["operation_id"], event.operation_id);
        body
    }

    fn fail(&mut self, request: &Value) -> String {
        let event = self.call(request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        String::from_utf8_lossy(&event.payload).into_owned()
    }

    fn refuse(&mut self, request: &Value) {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
        assert!(
            self.events
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "a refused request must not queue work"
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        consume(ohand_core_close(self.handle));
    }
}

#[test]
fn repinning_through_the_handle_moves_waiting_work_once_the_route_is_granted() {
    let _guard = serial();
    let store = Store::new("repin");
    let item_id = store.waiting_capture(1);
    store.fail_next_job_for_configuration();
    let mut session = Session::over(&store.path);
    let repin = json!({"command": "repin_waiting_work", "route_id": ROUTE_ID});

    assert_eq!(
        session.succeed(&repin),
        json!({"operation_id": 1, "moved": 0}),
        "nothing is configured"
    );

    let profile_version = store.configure("primary");
    assert_eq!(
        session.succeed(&repin),
        json!({"operation_id": 2, "moved": 1})
    );
    assert_eq!(
        session.succeed(&repin),
        json!({"operation_id": 3, "moved": 0}),
        "repeating the call moves nothing"
    );
    let pinned: Vec<_> = store
        .jobs(&item_id)
        .into_iter()
        .filter(|job| job.profile_version.as_deref() == Some(profile_version.as_str()))
        .collect();
    assert_eq!(pinned.len(), 1);
}

#[test]
fn the_pinned_job_listing_carries_only_job_item_and_status_and_is_bounded() {
    let _guard = serial();
    let store = Store::new("listing");
    let profile_version = store.configure("primary");
    let first = store.waiting_capture(1);
    let second = store.waiting_capture(2);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&json!({"command": "jobs_pinned_to_profile",
                                       "profile_version": profile_version}));
    assert_eq!(body["total"], 2);
    assert_eq!(body["truncated"], false);
    let jobs = body["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    for job in jobs {
        let mut keys: Vec<_> = job.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["item_id", "job_id", "status"]);
        assert_eq!(job["status"], "queued");
    }
    let mut items: Vec<_> = jobs
        .iter()
        .map(|job| job["item_id"].as_str().unwrap())
        .collect();
    items.sort();
    let mut expected = vec![first.as_str(), second.as_str()];
    expected.sort();
    assert_eq!(items, expected);

    let limited = session.succeed(&json!({"command": "jobs_pinned_to_profile",
                                          "profile_version": profile_version, "limit": 1}));
    assert_eq!(limited["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(limited["total"], 2);
    assert_eq!(limited["truncated"], true);

    let none = session.succeed(&json!({"command": "jobs_pinned_to_profile",
                                       "profile_version": "no-such-profile"}));
    assert_eq!(none["jobs"], json!([]));
}

#[test]
fn requeue_and_its_record_round_trip_and_a_retry_answers_the_same_replacement() {
    let _guard = serial();
    let store = Store::new("requeue");
    let first = store.configure("first");
    let item_id = store.waiting_capture(1);
    let second = store.profile("second");
    let old_job_id = store.jobs(&item_id)[0].job_id.clone();
    let mut session = Session::over(&store.path);
    let requeue = json!({"command": "requeue_job", "job_id": old_job_id,
                         "new_job_id": "replacement-1", "new_profile_version": second});

    let body = session.succeed(&requeue);
    assert_eq!(
        body,
        json!({"operation_id": 1, "job_id": "replacement-1", "item_id": item_id,
               "status": "queued", "profile_version": second})
    );
    let retried = session.succeed(&requeue);
    assert_eq!(retried["job_id"], "replacement-1");
    assert_eq!(store.jobs(&item_id).len(), 2);

    for job_id in [old_job_id.as_str(), "replacement-1"] {
        let record = session.succeed(&json!({"command": "requeue_record", "job_id": job_id}));
        assert_eq!(record["record"]["old_job_id"], old_job_id);
        assert_eq!(record["record"]["new_job_id"], "replacement-1");
        assert_eq!(record["record"]["from_profile_version"], first);
        assert_eq!(record["record"]["to_profile_version"], second);
        assert!(record["record"]["requeued_at"].is_string());
    }
    let missing = session.succeed(&json!({"command": "requeue_record", "job_id": "unlinked"}));
    assert_eq!(missing["record"], Value::Null);
}

#[test]
fn a_refused_requeue_or_revocation_reports_a_fixed_code_that_does_not_echo_the_request() {
    let _guard = serial();
    let store = Store::new("refusals");
    let first = store.configure("first");
    let item_id = store.waiting_capture(1);
    let job_id = store.jobs(&item_id)[0].job_id.clone();
    let mut session = Session::over(&store.path);

    let unknown_job = session.fail(&json!({"command": "requeue_job", "job_id": "secret-job",
                                           "new_job_id": "secret-new", "new_profile_version": first}));
    assert!(unknown_job.contains("not_found"), "{unknown_job}");
    let unknown_profile = session.fail(&json!({"command": "revoke_profile",
                                               "profile_version": "secret-profile"}));
    assert!(unknown_profile.contains("not_found"), "{unknown_profile}");
    let same_profile = session.fail(&json!({"command": "requeue_job", "job_id": job_id,
                                            "new_job_id": "secret-new", "new_profile_version": first}));
    assert!(
        same_profile.contains("job_configuration_refused"),
        "{same_profile}"
    );
    let absent_target = session.fail(&json!({"command": "requeue_job", "job_id": job_id,
                                             "new_job_id": "secret-new",
                                             "new_profile_version": "secret-target"}));
    assert!(
        absent_target.contains("job_configuration_refused"),
        "{absent_target}"
    );
    for payload in [
        &unknown_job,
        &unknown_profile,
        &same_profile,
        &absent_target,
    ] {
        for secret in [
            "secret-job",
            "secret-new",
            "secret-profile",
            "secret-target",
        ] {
            assert!(!payload.contains(secret), "failure echoed {secret}");
        }
    }
    assert_eq!(store.jobs(&item_id).len(), 1);
}

#[test]
fn revoking_through_the_handle_retires_the_profiles_jobs_and_a_repeat_succeeds() {
    let _guard = serial();
    let store = Store::new("revoke");
    let profile_version = store.configure("primary");
    let item_id = store.waiting_capture(1);
    let mut session = Session::over(&store.path);
    let revoke = json!({"command": "revoke_profile", "profile_version": profile_version});

    assert_eq!(
        session.succeed(&revoke),
        json!({"operation_id": 1, "profile_version": profile_version})
    );
    assert_eq!(store.jobs(&item_id)[0].status.as_str(), "cancelled");
    session.succeed(&revoke);

    let repin = session.succeed(&json!({"command": "repin_waiting_work", "route_id": ROUTE_ID}));
    assert_eq!(repin["moved"], 0, "a revoked default is never a target");
}

#[test]
fn malformed_commands_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = Store::new("invalid");
    let mut session = Session::over(&store.path);

    session.refuse(&json!({"command": "delete_profile", "profile_version": "p"}));
    session.refuse(&json!({"command": "revoke_profile", "profile_version": "p", "force": true}));
    session.refuse(&json!({"command": "revoke_profile"}));
    session.refuse(&json!({"command": "revoke_profile", "profile_version": ""}));
    session.refuse(&json!({"command": "revoke_profile", "profile_version": "p".repeat(513)}));
    session.refuse(&json!({"command": "repin_waiting_work", "route_id": ""}));
    session.refuse(
        &json!({"command": "jobs_pinned_to_profile", "profile_version": "p",
                           "limit": 0}),
    );
    session.refuse(
        &json!({"command": "requeue_job", "job_id": "j", "new_job_id": "",
                           "new_profile_version": "p"}),
    );

    let empty = consume(unsafe {
        ohand_core_start_job_configuration(session.handle, 99, std::ptr::null(), 0)
    });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe {
        ohand_core_start_job_configuration(session.handle, 99, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let too_large = consume(unsafe {
        ohand_core_start_job_configuration(
            session.handle,
            99,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");
}

#[test]
fn the_retired_listing_follows_the_revoke_pages_and_rejects_unknown_versions() {
    let _guard = serial();
    let store = Store::new("retired");
    let profile_version = store.configure("primary");
    store.waiting_capture(1);
    store.waiting_capture(2);
    store.waiting_capture(3);
    let mut session = Session::over(&store.path);
    let list = |after: Option<&str>| {
        let mut request = json!({"command": "jobs_retired_by_revocation",
                                 "profile_version": profile_version, "limit": 2});
        if let Some(after) = after {
            request["after"] = json!(after);
        }
        request
    };

    let before = session.succeed(&list(None));
    assert_eq!(
        before["jobs"],
        json!([]),
        "nothing is retired before a revoke"
    );
    assert_eq!(before["truncated"], false);

    session.succeed(&json!({"command": "revoke_profile", "profile_version": profile_version}));
    let first = session.succeed(&list(None));
    assert_eq!(first["total"], 3);
    assert_eq!(first["truncated"], true);
    let first_jobs = first["jobs"].as_array().unwrap();
    assert_eq!(first_jobs.len(), 2);
    for job in first_jobs {
        let mut keys: Vec<_> = job.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["item_id", "job_id", "requeued_to", "retired_at"]);
        assert_eq!(job["requeued_to"], Value::Null);
    }
    let cursor = first["next_cursor"].as_str().unwrap().to_string();
    let second = session.succeed(&list(Some(&cursor)));
    assert_eq!(second["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(second["truncated"], false);
    assert_eq!(second["next_cursor"], Value::Null);

    let unknown = session.fail(&json!({"command": "jobs_retired_by_revocation",
                                       "profile_version": "secret-profile"}));
    assert!(unknown.contains("not_found"), "{unknown}");
    assert!(!unknown.contains("secret-profile"));
    session.refuse(&json!({"command": "jobs_retired_by_revocation",
                           "profile_version": profile_version, "limit": 0}));
    session.refuse(&json!({"command": "jobs_retired_by_revocation",
                           "profile_version": profile_version, "after": ""}));
}

// ---- failed interpretation work (F43) ----

const BOTH_SCOPES: [&str; 2] = ["personal", "work"];

impl Store {
    /// Ends the queued job as `failed` with `reason`, as the dispatcher does, and leaves the item
    /// `unprocessed`. Returns the failed job's id.
    fn fail_next_job(&self, reason: &str) -> String {
        let mut database = open_database(&self.path);
        let job = claim_job_with_lease(&mut database, Duration::seconds(60), Utc::now())
            .unwrap()
            .expect("a job is queued");
        database
            .conn()
            .execute(
                "UPDATE jobs SET status = 'failed', failure_reason = ?, lease_expires_at = NULL \
                 WHERE job_id = ?",
                [reason, &job.job_id],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "UPDATE items SET processing_state = 'unprocessed' WHERE item_id = ?",
                [&job.item_id],
            )
            .unwrap();
        job.job_id
    }

    fn set_lifecycle(&self, item_id: &str, state: &str) {
        open_database(&self.path)
            .conn()
            .execute(
                "UPDATE items SET lifecycle_state = ? WHERE item_id = ?",
                [state, item_id],
            )
            .unwrap();
    }
}

fn list_failed(session: &mut Session) -> Value {
    session.succeed(&json!({"command": "failed_interpretations", "read_scopes": BOTH_SCOPES}))
}

fn rerun(session: &mut Session, selection: Value) -> Value {
    let mut request = json!({"command": "rerun_failed_interpretations",
                             "read_scopes": BOTH_SCOPES});
    for (key, value) in selection.as_object().unwrap() {
        request[key] = value.clone();
    }
    session.succeed(&request)
}

struct HostSide {
    sender: std::sync::Mutex<std::sync::mpsc::Sender<(u32, Vec<u8>)>>,
}

unsafe extern "C" fn record_host_command(
    context: *mut c_void,
    _request_id: u64,
    command: u32,
    data: *const u8,
    len: usize,
) {
    let host = &*(context as *const HostSide);
    let data = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let _ = host.sender.lock().unwrap().send((command, data));
}

#[test]
fn a_rejected_pinned_failure_is_listed_and_runs_again_to_a_new_pinned_job_that_a_drain_sends() {
    let _guard = serial();
    let store = Store::new("rerun-pinned");
    let profile_version = store.configure("primary");
    let item_id = store.waiting_capture(1);
    let failed_job_id = store.fail_next_job("rejected");
    let mut session = Session::over(&store.path);

    let listed = list_failed(&mut session);
    let entries = listed["failed"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(listed["next_cursor"], Value::Null);
    assert_eq!(entries[0]["item_id"], item_id);
    assert_eq!(entries[0]["failure_reason"], "rejected");
    assert_eq!(entries[0]["pinned"], true);
    assert_eq!(entries[0]["profile_version"], profile_version);
    assert_eq!(entries[0]["recovery"], "run_again");
    assert_eq!(entries[0]["captured_at"], "2026-10-14T10:30:00Z");
    assert!(
        !listed.to_string().contains("roofer"),
        "the read returns no text"
    );

    let moved = rerun(&mut session, json!({"item_ids": [item_id]}));
    assert_eq!(moved["moved"], 1);
    assert_eq!(moved["skipped"], 0);
    let jobs = store.jobs(&item_id);
    assert_eq!(jobs.len(), 2);
    let failed = jobs.iter().find(|job| job.job_id == failed_job_id).unwrap();
    assert_eq!(
        failed.status.as_str(),
        "failed",
        "the failed job is untouched"
    );
    assert_eq!(failed.failure_reason.as_deref(), Some("rejected"));
    let replacement = jobs.iter().find(|job| job.job_id != failed_job_id).unwrap();
    assert_eq!(replacement.status.as_str(), "queued");
    assert_eq!(
        replacement.profile_version.as_deref(),
        Some(profile_version.as_str())
    );
    let record = session.succeed(&json!({"command": "requeue_record", "job_id": failed_job_id}));
    assert_eq!(record["record"]["new_job_id"], replacement.job_id);

    assert_eq!(list_failed(&mut session)["failed"], json!([]));
    let again = rerun(&mut session, json!({"item_ids": [item_id]}));
    assert_eq!(again["moved"], 0);
    assert_eq!(
        again["skipped_items"],
        json!([{"item_id": item_id, "reason": "not_failed"}])
    );
    let repeated_all = rerun(&mut session, json!({"all_listed": true}));
    assert_eq!(repeated_all["moved"], 0);
    assert_eq!(store.jobs(&item_id).len(), 2, "repeating creates nothing");

    // A drain sends the new job to the pinned provider.
    let (sender, commands) = std::sync::mpsc::channel();
    let host = Box::new(HostSide {
        sender: std::sync::Mutex::new(sender),
    });
    let context = &*host as *const HostSide as *mut c_void;
    let list = b"[]";
    assert_eq!(
        consume(unsafe {
            ohand_core_set_job_host(
                session.handle,
                Some(record_host_command),
                context,
                list.as_ptr(),
                list.len(),
            )
        })
        .status,
        OHAND_CORE_STATUS_OK
    );
    let path = store.path.clone();
    assert_eq!(
        consume(unsafe {
            ohand_core_start_job_drain(session.handle, 500, path.as_ptr(), path.len())
        })
        .status,
        OHAND_CORE_STATUS_OK
    );
    let (command, data) = commands
        .recv_timeout(WAIT)
        .expect("the drain sends a request");
    assert_eq!(command, OHAND_JOB_HOST_COMMAND_SEND);
    assert!(String::from_utf8_lossy(&data).contains("roofer"));
    assert_eq!(
        store
            .jobs(&item_id)
            .iter()
            .find(|job| job.job_id == replacement.job_id)
            .unwrap()
            .status
            .as_str(),
        "running"
    );
    consume(ohand_core_cancel_job_drain(session.handle));
    let _ = session.events.recv_timeout(WAIT);
    consume(unsafe {
        ohand_core_set_job_host(
            session.handle,
            None,
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
        )
    });
}

#[test]
fn an_unpinned_unsupported_failure_waits_for_settings_and_is_moved_once_a_provider_is_allowed() {
    let _guard = serial();
    let store = Store::new("rerun-unpinned");
    let item_id = store.waiting_capture(1);
    let failed_job_id = store.fail_next_job("unsupported");
    let mut session = Session::over(&store.path);

    let listed = list_failed(&mut session);
    assert_eq!(listed["failed"][0]["pinned"], false);
    assert_eq!(listed["failed"][0]["profile_version"], Value::Null);
    assert_eq!(listed["failed"][0]["failure_reason"], "unsupported");
    assert_eq!(listed["failed"][0]["recovery"], "change_settings");

    let blocked = rerun(&mut session, json!({"all_listed": true}));
    assert_eq!(blocked["moved"], 0);
    assert_eq!(blocked["skipped"], 1);
    assert_eq!(
        blocked["skipped_items"],
        json!([{"item_id": item_id, "reason": "no_provider_allowed"}])
    );
    assert_eq!(store.jobs(&item_id).len(), 1);

    let profile_version = store.configure("primary");
    assert_eq!(
        list_failed(&mut session)["failed"][0]["recovery"],
        "run_again"
    );
    let moved = rerun(&mut session, json!({"all_listed": true}));
    assert_eq!(moved["moved"], 1);
    let jobs = store.jobs(&item_id);
    assert_eq!(jobs.len(), 2);
    let replacement = jobs.iter().find(|job| job.job_id != failed_job_id).unwrap();
    assert_eq!(
        replacement.profile_version.as_deref(),
        Some(profile_version.as_str())
    );
    let record = session.succeed(&json!({"command": "requeue_record", "job_id": failed_job_id}));
    assert_eq!(record["record"]["from_profile_version"], Value::Null);
    assert_eq!(rerun(&mut session, json!({"all_listed": true}))["moved"], 0);
    assert_eq!(store.jobs(&item_id).len(), 2);
}

#[test]
fn deleted_and_archived_items_are_never_listed_or_moved_and_the_list_pages_newest_first() {
    let _guard = serial();
    let store = Store::new("rerun-lifecycle");
    store.configure("primary");
    let mut item_ids = Vec::new();
    for number in 1..=4 {
        item_ids.push(store.waiting_capture(number));
        store.fail_next_job("rejected");
    }
    store.set_lifecycle(&item_ids[0], "deleted");
    store.set_lifecycle(&item_ids[1], "cancelled");
    let mut session = Session::over(&store.path);

    let listed = list_failed(&mut session);
    let listed_ids: Vec<_> = listed["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["item_id"].as_str().unwrap().to_string())
        .collect();
    let mut expected = vec![item_ids[2].clone(), item_ids[3].clone()];
    expected.sort();
    assert_eq!(listed_ids, expected);

    let attempted = rerun(
        &mut session,
        json!({"item_ids": [item_ids[0], item_ids[1]]}),
    );
    assert_eq!(attempted["moved"], 0);
    assert_eq!(attempted["skipped"], 2);
    assert_eq!(store.jobs(&item_ids[0]).len(), 1);
    assert_eq!(store.jobs(&item_ids[1]).len(), 1);

    let first = session.succeed(&json!({"command": "failed_interpretations",
                                        "read_scopes": BOTH_SCOPES, "limit": 1}));
    assert_eq!(first["failed"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"].as_str().unwrap().to_string();
    let second = session.succeed(&json!({"command": "failed_interpretations",
                                         "read_scopes": BOTH_SCOPES, "limit": 1,
                                         "cursor": cursor}));
    assert_eq!(second["failed"].as_array().unwrap().len(), 1);
    assert_eq!(second["next_cursor"], Value::Null);
    assert_ne!(
        first["failed"][0]["item_id"],
        second["failed"][0]["item_id"]
    );
}

#[test]
fn a_locked_read_is_refused_and_a_scope_outside_the_session_sees_nothing() {
    let _guard = serial();
    let store = Store::new("rerun-locked");
    store.configure("primary");
    let item_id = store.waiting_capture(1);
    store.fail_next_job("rejected");
    let mut session = Session::over(&store.path);

    session.refuse(&json!({"command": "failed_interpretations"}));
    session.refuse(&json!({"command": "failed_interpretations", "read_scopes": []}));
    session.refuse(&json!({"command": "failed_interpretations", "read_scopes": ["secret"]}));
    session.refuse(&json!({"command": "failed_interpretations",
                           "read_scopes": BOTH_SCOPES, "cursor": "not-a-cursor"}));
    session.refuse(&json!({"command": "rerun_failed_interpretations", "all_listed": true}));
    session.refuse(&json!({"command": "rerun_failed_interpretations",
                           "read_scopes": BOTH_SCOPES}));
    session.refuse(&json!({"command": "rerun_failed_interpretations",
                           "read_scopes": BOTH_SCOPES, "all_listed": true,
                           "item_ids": [item_id]}));

    let work_only = session.succeed(&json!({"command": "failed_interpretations",
                                            "read_scopes": ["work"]}));
    assert_eq!(work_only["failed"], json!([]));
    let work_rerun = session.succeed(&json!({"command": "rerun_failed_interpretations",
                                             "read_scopes": ["work"], "all_listed": true}));
    assert_eq!(work_rerun["moved"], 0);
    assert_eq!(store.jobs(&item_id).len(), 1);
}
