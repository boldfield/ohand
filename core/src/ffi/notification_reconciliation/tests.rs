//! Drives the reminder reconciliation exports the way the native layer does, against the real
//! core store and the real capacity and reconcile passes, with a recording stand-in for the OS
//! notification center.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{consume, recorder, register, serial, Event, Outcome, WAIT};
use crate::reminders::state::{apply_user_time_correction, get_reminder, UserTimeCorrection};
use crate::store::events::{
    save_event, Correction, CorrectionKind, Event as StoredEvent, EventPayload, EventType,
};
use crate::store::schema::Clock;
use chrono::Duration as ChronoDuration;
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

const CAPTURE_TIME: &str = "2026-01-15T10:30:00Z";

struct SeedClock;

impl Clock for SeedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(CAPTURE_TIME)
            .unwrap()
            .with_timezone(&Utc)
    }
}

fn far_future() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2040-06-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn temporary_store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-reconcile-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    (directory, path)
}

fn item_id(index: usize) -> String {
    format!("item-{index:03}")
}

fn seed_reminders(path: &str, count: usize) {
    let mut database = Database::open(path, Arc::new(SeedClock)).unwrap();
    for index in 0..count {
        let item = item_id(index);
        let capture_id = format!("cap-{item}");
        let transaction = database.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
                 VALUES (?, 'synthetic reminder text', NULL, '2026-01-15T10:30:00Z', 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, '2026-01-15T10:30:00Z', NULL)",
                rusqlite::params![&capture_id],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', '2026-01-15T10:30:00Z', '2026-01-15T10:30:00Z')",
                rusqlite::params![&item, &capture_id],
            )
            .unwrap();
        transaction.commit().unwrap();
        let event = StoredEvent::new(
            format!("evt-type-{item}"),
            item.clone(),
            0,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind: CorrectionKind::Type,
                old_value: None,
                new_value: "action".to_string(),
            }),
            CAPTURE_TIME.to_string(),
        )
        .unwrap();
        save_event(&mut database, &event, 0).unwrap();
        let transaction = database.immediate_transaction().unwrap();
        apply_user_time_correction(
            &transaction,
            &SeedClock,
            &UserTimeCorrection {
                command_id: format!("cmd-{item}"),
                item_id: item.clone(),
                expected_revision: 1,
                expected_reminder_version: get_reminder(&transaction, &item)
                    .unwrap()
                    .map_or(0, |record| record.state_version),
                instant: far_future() + ChronoDuration::hours(index as i64),
                timezone_id: "UTC".to_string(),
            },
        )
        .unwrap();
        transaction.commit().unwrap();
    }
}

fn seed_reminders_by_sql(path: &str, count: usize) {
    let mut database = Database::open(path, Arc::new(SeedClock)).unwrap();
    let transaction = database.transaction().unwrap();
    for index in 0..count {
        let capture_id = format!("capture-permission-{index}");
        let item_id = format!("7d1c5a1e-0000-4000-8000-{index:012}");
        let reminder_id = format!("4a2b8c1e-5d4a-4e0b-9a77-{index:012}");
        let due = format!("2090-01-{:02}T{:02}:00:00Z", 1 + index / 24, index % 24);
        let notification_id = format!("{reminder_id}#1");
        for statement in [
            format!("INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic) VALUES ('{capture_id}', 'synthetic', NULL, '2026-10-08T09:30:00Z', 'UTC', 0, 'en_US', 'gregorian', 'personal', 'route-default', 0, '2026-10-08T09:30:01Z', NULL)"),
            format!("INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at) VALUES ('{item_id}', '{capture_id}', 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')"),
            format!("INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) VALUES ('{reminder_id}', '{item_id}', 'resolved', 'pending_schedule', 'unknown', 'not_acknowledged', '{due}', 'UTC', 1, '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')"),
            format!("INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, effect_identity, external_id, scheduled_for, created_at) VALUES ('schedule:{notification_id}', '{reminder_id}', 'schedule', 'pending', 'schedule:{notification_id}', '{notification_id}', '{due}', '2026-10-08T09:30:02Z')"),
        ] {
            transaction.execute(&statement, []).unwrap();
        }
    }
    transaction.commit().unwrap();
}

#[derive(Default)]
struct Center {
    pending: Vec<(String, DateTime<Utc>)>,
    schedules: Vec<String>,
    cancels: Vec<String>,
    lists: usize,
    abandoned: usize,
    deny: bool,
    fail_list: bool,
    silent: bool,
    unanswered: Vec<u64>,
}

struct NativeSide {
    handle: AtomicU64,
    center: Mutex<Center>,
}

impl NativeSide {
    fn new() -> Box<NativeSide> {
        Box::new(NativeSide {
            handle: AtomicU64::new(0),
            center: Mutex::new(Center::default()),
        })
    }

    fn context(&self) -> *mut c_void {
        self as *const NativeSide as *mut c_void
    }

    fn center(&self) -> std::sync::MutexGuard<'_, Center> {
        lock(&self.center)
    }
}

fn answer(native: &NativeSide, effect_id: u64, body: Value) {
    let bytes = serde_json::to_vec(&body).unwrap();
    let outcome = consume(unsafe {
        ohand_core_complete_notification_effect(
            native.handle.load(Ordering::SeqCst),
            effect_id,
            bytes.as_ptr(),
            bytes.len(),
        )
    });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK, "answer accepted");
}

unsafe extern "C" fn native_command(
    context: *mut c_void,
    effect_id: u64,
    command: u32,
    data: *const u8,
    len: usize,
) {
    let native = &*(context as *const NativeSide);
    let body: Value = if len == 0 {
        Value::Null
    } else {
        serde_json::from_slice(std::slice::from_raw_parts(data, len)).unwrap()
    };
    let reply = {
        let mut center = native.center();
        match command {
            OHAND_NOTIFICATION_COMMAND_ABANDON => {
                center.abandoned += 1;
                return;
            }
            _ if center.silent => {
                center.unanswered.push(effect_id);
                return;
            }
            OHAND_NOTIFICATION_COMMAND_SCHEDULE => {
                assert_eq!(body["payload_kind"], "generic");
                if center.deny {
                    json!({"error": {"class": "unauthorized", "code": "permission_denied"}})
                } else {
                    let id = body["notification_id"].as_str().unwrap().to_string();
                    let fire_at = DateTime::parse_from_rfc3339(body["fire_at"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc);
                    center.pending.retain(|(existing, _)| existing != &id);
                    center.pending.push((id.clone(), fire_at));
                    center.schedules.push(id);
                    json!({})
                }
            }
            OHAND_NOTIFICATION_COMMAND_CANCEL => {
                let id = body["notification_id"].as_str().unwrap().to_string();
                center.pending.retain(|(existing, _)| existing != &id);
                center.cancels.push(id);
                json!({})
            }
            OHAND_NOTIFICATION_COMMAND_LIST_PENDING => {
                center.lists += 1;
                if center.fail_list {
                    json!({"error": {"class": "transient", "code": "notification_center_failed"}})
                } else {
                    let pending: Vec<Value> = center
                        .pending
                        .iter()
                        .map(|(id, at)| {
                            json!({
                                "notification_id": id,
                                "fire_at": at.to_rfc3339_opts(SecondsFormat::Secs, true)
                            })
                        })
                        .collect();
                    json!({ "pending": pending })
                }
            }
            other => panic!("unexpected command {other}"),
        }
    };
    answer(native, effect_id, reply);
}

struct Session {
    handle: u64,
    events: Receiver<Event>,
    native: Box<NativeSide>,
    _recorder: Box<crate::ffi::core_handle::tests::Recorder>,
    directory: std::path::PathBuf,
    next_operation: u64,
}

impl Session {
    fn open(name: &str, reminders: usize) -> Session {
        let (directory, path) = temporary_store(name);
        seed_reminders(&path, reminders);
        Session::open_prepared(directory, path)
    }

    fn open_prepared(directory: std::path::PathBuf, path: String) -> Session {
        let mut handle = 0;
        let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        let native = NativeSide::new();
        native.handle.store(handle, Ordering::SeqCst);
        let registered = consume(ohand_core_set_notification_effects(
            handle,
            Some(native_command),
            native.context(),
        ));
        assert_eq!(registered.status, OHAND_CORE_STATUS_OK);
        Session {
            handle,
            events,
            native,
            _recorder: recorder,
            directory,
            next_operation: 1,
        }
    }

    fn start_raw(&mut self, request: &[u8]) -> (u64, Outcome) {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let outcome = consume(unsafe {
            ohand_core_start_reconcile_reminders(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        });
        (operation_id, outcome)
    }

    fn event(&self, operation_id: u64) -> Event {
        let event = self.events.recv_timeout(WAIT).expect("an event");
        assert_eq!(event.operation_id, operation_id);
        event
    }

    fn pass(&mut self, request: Value) -> Value {
        let (operation_id, started) = self.start_raw(&serde_json::to_vec(&request).unwrap());
        assert_eq!(started.status, OHAND_CORE_STATUS_OK);
        let event = self.event(operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK, "pass failed");
        serde_json::from_slice(&event.payload).unwrap()
    }

    fn failing_pass(&mut self, request: Value) -> (u32, Value) {
        let (operation_id, started) = self.start_raw(&serde_json::to_vec(&request).unwrap());
        assert_eq!(started.status, OHAND_CORE_STATUS_OK);
        let event = self.event(operation_id);
        (
            event.status,
            serde_json::from_slice(&event.payload).unwrap(),
        )
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
        let _ = ohand_core_set_notification_effects(self.handle, None, std::ptr::null_mut());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn arm() -> Value {
    json!({"rearm_rejected_schedules": true, "allow_optional_prompts": true})
}

fn plain() -> Value {
    json!({"rearm_rejected_schedules": false, "allow_optional_prompts": true})
}

fn count(outcome: &Value, key: &str) -> u64 {
    outcome[key].as_u64().unwrap_or_else(|| panic!("{key}"))
}

fn states(outcome: &Value, key: &str) -> Vec<String> {
    outcome["reminders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reminder| reminder[key].as_str().unwrap_or("null").to_string())
        .collect()
}

fn prompts(number: usize) -> Vec<Value> {
    (0..number)
        .map(|index| {
            json!({
                "notification_id": format!("prompt-{index}"),
                "fire_at": (Utc::now() + ChronoDuration::hours(1 + index as i64))
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            })
        })
        .collect()
}

#[test]
fn a_pass_installs_each_reminder_once_and_a_repeat_changes_nothing() {
    let _guard = serial();
    let mut session = Session::open("install", 3);

    let first = session.pass(plain());
    assert_eq!(count(&first, "installed"), 3);
    assert!(first["converged"].as_bool().unwrap());
    assert_eq!(states(&first, "schedule_state"), ["scheduled"; 3]);
    assert_eq!(session.native.center().pending.len(), 3);

    let again = session.pass(plain());
    assert_eq!(count(&again, "installed"), 0);
    assert_eq!(count(&again, "already_installed"), 3);
    assert_eq!(
        session.native.center().schedules.len(),
        3,
        "no duplicate install"
    );
    session.close();
}

#[test]
fn a_rejected_install_is_withheld_until_a_regained_permission_re_arms_it() {
    let _guard = serial();
    let mut session = Session::open("rearm", 2);
    session.native.center().deny = true;

    let denied = session.pass(plain());
    assert_eq!(count(&denied, "installed"), 0);
    assert_eq!(states(&denied, "schedule_state"), ["schedule_failed"; 2]);
    assert_eq!(states(&denied, "delivery_state"), ["unknown"; 2]);
    assert_eq!(
        states(&denied, "acknowledgment_state"),
        ["not_acknowledged"; 2]
    );
    assert!(session.native.center().schedules.is_empty());

    let still_denied = session.pass(plain());
    assert_eq!(count(&still_denied, "withheld"), 2);
    assert_eq!(count(&still_denied, "rearmed"), 0);
    assert_eq!(
        states(&still_denied, "schedule_state"),
        ["schedule_failed"; 2]
    );

    session.native.center().deny = false;
    let not_rearmed = session.pass(plain());
    assert_eq!(count(&not_rearmed, "installed"), 0, "recovery is explicit");

    let regained = session.pass(arm());
    assert_eq!(count(&regained, "rearmed"), 2);
    assert_eq!(count(&regained, "installed"), 2);
    assert_eq!(states(&regained, "schedule_state"), ["scheduled"; 2]);

    let repeated = session.pass(arm());
    assert_eq!(count(&repeated, "rearmed"), 0);
    assert_eq!(count(&repeated, "installed"), 0);
    assert_eq!(
        session.native.center().schedules.len(),
        2,
        "one install per reminder"
    );
    session.close();
}

#[test]
fn a_re_arm_while_permission_is_still_denied_fails_again_without_duplicating_state() {
    let _guard = serial();
    let mut session = Session::open("rearm-denied", 1);
    session.native.center().deny = true;
    session.pass(plain());

    let retried = session.pass(arm());
    assert_eq!(count(&retried, "rearmed"), 1);
    assert_eq!(count(&retried, "installed"), 0);
    assert_eq!(states(&retried, "schedule_state"), ["schedule_failed"]);
    assert!(session.native.center().schedules.is_empty());
    session.close();
}

#[test]
fn capacity_overflow_is_held_back_and_reminders_win_over_prompts() {
    let _guard = serial();
    let mut session = Session::open("capacity", 62);

    let outcome = session.pass(json!({
        "allow_optional_prompts": true,
        "prompts": prompts(3),
    }));
    assert_eq!(count(&outcome, "installed"), 60);
    assert_eq!(count(&outcome, "demoted"), 2);
    assert_eq!(count(&outcome, "prompts_granted"), 0);
    assert_eq!(count(&outcome, "prompts_deferred"), 3);
    let held = states(&outcome, "unschedulable_reason")
        .iter()
        .filter(|reason| reason.as_str() == "capacity_exceeded")
        .count();
    assert_eq!(held, 2);
    assert_eq!(session.native.center().pending.len(), 60);

    let repeat = session.pass(plain());
    assert_eq!(count(&repeat, "installed"), 0);
    assert_eq!(count(&repeat, "demoted"), 0);
    assert_eq!(session.native.center().schedules.len(), 60);
    session.close();
}

#[test]
fn optional_prompts_use_the_slots_left_and_are_withheld_without_permission() {
    let _guard = serial();
    let mut session = Session::open("prompts", 10);

    let allowed = session.pass(json!({
        "allow_optional_prompts": true,
        "prompts": prompts(9),
    }));
    assert_eq!(count(&allowed, "installed"), 10);
    assert_eq!(count(&allowed, "prompts_granted"), 7);
    assert_eq!(count(&allowed, "prompts_deferred"), 2);
    assert_eq!(count(&allowed, "prompts_withheld"), 0);

    let withheld = session.pass(json!({
        "allow_optional_prompts": false,
        "prompts": prompts(9),
    }));
    assert_eq!(count(&withheld, "prompts_granted"), 0);
    assert_eq!(count(&withheld, "prompts_withheld"), 9);
    assert_eq!(
        states(&withheld, "schedule_state"),
        ["scheduled"; 10],
        "reminders do not depend on prompts"
    );
    assert_eq!(session.native.center().schedules.len(), 10);
    session.close();
}

#[test]
fn facts_are_readable_without_native_effects_and_a_pass_is_refused() {
    let _guard = serial();
    let mut session = Session::open("facts", 2);
    let cleared = consume(ohand_core_set_notification_effects(
        session.handle,
        None,
        std::ptr::null_mut(),
    ));
    assert_eq!(cleared.status, OHAND_CORE_STATUS_OK);

    let facts = session.pass(json!({"facts_only": true}));
    assert_eq!(states(&facts, "request_state"), ["resolved"; 2]);
    assert_eq!(count(&facts, "installed"), 0);

    let (_, refused) = session.start_raw(&serde_json::to_vec(&plain()).unwrap());
    assert_eq!(refused.status, OHAND_CORE_STATUS_UNSUPPORTED);
    assert_eq!(refused.code(), "notification_effects_not_registered");
    session.close();
}

#[test]
fn a_failed_pending_listing_aborts_the_pass_before_any_change() {
    let _guard = serial();
    let mut session = Session::open("list-failure", 2);
    session.native.center().fail_list = true;

    let (status, failure) = session.failing_pass(plain());
    assert_eq!(status, OHAND_CORE_STATUS_TRANSIENT);
    assert_eq!(failure["code"], "pending_list_unavailable");
    assert!(session.native.center().schedules.is_empty());

    session.native.center().fail_list = false;
    let recovered = session.pass(plain());
    assert_eq!(count(&recovered, "installed"), 2);
    session.close();
}

#[test]
fn an_unanswered_command_is_abandoned_and_a_late_answer_is_ignored() {
    let _guard = serial();
    let mut session = Session::open("deadline", 1);
    session.native.center().silent = true;

    let operation_id = session.next_operation;
    session.next_operation += 1;
    let job = reconcile_job(
        session.handle,
        operation_id,
        ValidatedRequest {
            rearm_rejected_schedules: false,
            allow_optional_prompts: true,
            prompts: Vec::new(),
            facts_only: false,
        },
        lock(&REGISTRATIONS).get(&session.handle).cloned(),
        Duration::from_millis(100),
    );
    instance::lookup(session.handle)
        .unwrap()
        .submit(operation_id, job)
        .unwrap();
    let event = session.event(operation_id);
    assert_eq!(event.status, OHAND_CORE_STATUS_TRANSIENT);
    let failure: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(failure["code"], "pending_list_unavailable");

    let late = {
        let center = session.native.center();
        assert_eq!(center.abandoned, 1);
        center.unanswered[0]
    };
    let body = b"{}";
    let outcome = consume(unsafe {
        ohand_core_complete_notification_effect(session.handle, late, body.as_ptr(), body.len())
    });
    assert_eq!(outcome.code(), "not_found");
    assert!(lock(&WAITING)
        .keys()
        .all(|(handle, _)| *handle != session.handle));
    session.close();
}

#[test]
fn bad_requests_and_answers_are_refused_with_normalized_failures() {
    let _guard = serial();
    let mut session = Session::open("invalid", 1);

    for request in [
        &b"{"[..],
        br#"{"unknown":true}"#,
        br#"{"prompts":[{"notification_id":"","fire_at":"2040-01-01T00:00:00Z"}]}"#,
        br#"{"prompts":[{"notification_id":"p","fire_at":"tomorrow"}]}"#,
    ] {
        let (_, outcome) = session.start_raw(request);
        assert_eq!(outcome.code(), "invalid_request");
    }
    let (_, empty) = session.start_raw(b"");
    assert_eq!(empty.code(), "invalid_request");
    let oversized = vec![b' '; OHAND_CORE_MAX_RECONCILE_REQUEST_BYTES + 1];
    let (_, large) = session.start_raw(&oversized);
    assert_eq!(large.code(), "request_too_large");
    let null = consume(unsafe {
        ohand_core_start_reconcile_reminders(session.handle, 90, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let unknown_handle =
        consume(unsafe { ohand_core_start_reconcile_reminders(u64::MAX, 91, b"{}".as_ptr(), 2) });
    assert_eq!(unknown_handle.code(), "invalid_handle");

    for bytes in [
        &b"{"[..],
        br#"{"unexpected":1}"#,
        br#"{"pending":[{"notification_id":""}]}"#,
    ] {
        let outcome = consume(unsafe {
            ohand_core_complete_notification_effect(session.handle, 1, bytes.as_ptr(), bytes.len())
        });
        assert_eq!(outcome.code(), "invalid_request");
    }
    let unknown = consume(unsafe {
        ohand_core_complete_notification_effect(session.handle, u64::MAX, b"{}".as_ptr(), 2)
    });
    assert_eq!(unknown.code(), "not_found");
    session.close();
}

#[test]
fn native_failure_codes_are_reduced_to_fixed_labels() {
    assert_eq!(sanitized_code("permission_denied"), "permission_denied");
    assert_eq!(
        sanitized_code("Contains Text 123 private"),
        "native_failure"
    );
    assert_eq!(sanitized_code(""), "native_failure");
    assert_eq!(
        sanitized_code(&"a".repeat(MAX_CODE_BYTES + 1)),
        "native_failure"
    );
}

#[test]
fn clearing_a_replaced_registration_by_context_leaves_the_replacement_active() {
    let _guard = serial();
    let mut session = Session::open("replaced", 1);
    let first_context = session.native.context();

    let replacement = NativeSide::new();
    replacement.handle.store(session.handle, Ordering::SeqCst);
    let registered = consume(ohand_core_set_notification_effects(
        session.handle,
        Some(native_command),
        replacement.context(),
    ));
    assert_eq!(registered.status, OHAND_CORE_STATUS_OK);

    let stale = consume(ohand_core_set_notification_effects(
        session.handle,
        None,
        first_context,
    ));
    assert_eq!(stale.status, OHAND_CORE_STATUS_OK);

    let outcome = session.pass(plain());
    assert_eq!(count(&outcome, "installed"), 1);
    assert_eq!(replacement.center().schedules.len(), 1);
    assert!(session.native.center().schedules.is_empty());

    let released = consume(ohand_core_set_notification_effects(
        session.handle,
        None,
        replacement.context(),
    ));
    assert_eq!(released.status, OHAND_CORE_STATUS_OK);
    let (_, refused) = session.start_raw(&serde_json::to_vec(&plain()).unwrap());
    assert_eq!(refused.code(), "notification_effects_not_registered");
    session.close();
}

#[test]
fn the_waiting_registry_is_empty_after_every_scenario() {
    let _guard = serial();
    assert!(lock(&WAITING).is_empty());
}

#[test]
fn reminders_seeded_by_plain_sql_are_installed_and_re_armed_like_core_written_ones() {
    let _guard = serial();
    let (directory, path) = temporary_store("sql-seed");
    seed_reminders_by_sql(&path, 2);
    let mut session = Session::open_prepared(directory, path);
    session.native.center().deny = true;

    let denied = session.pass(plain());
    assert_eq!(states(&denied, "schedule_state"), ["schedule_failed"; 2]);
    session.native.center().deny = false;
    let regained = session.pass(arm());
    assert_eq!(count(&regained, "rearmed"), 2);
    assert_eq!(count(&regained, "installed"), 2);
    assert_eq!(states(&regained, "schedule_state"), ["scheduled"; 2]);
    let pending = session.native.center().pending.clone();
    assert!(pending
        .iter()
        .all(|(id, _)| id.ends_with("#1") && id.starts_with("4a2b8c1e")));
    session.close();
}
