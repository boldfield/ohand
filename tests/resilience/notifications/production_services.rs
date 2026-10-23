//! T03, production boundary: the same lapse scenarios driven through the core handle exactly as
//! the native layer drives it (`ohand_core_open`, the persisted prompt schedule, the reminder
//! reconciliation pass with its explicit re-arm), over one file-backed store that is closed and
//! reopened. Nothing here composes the coordinators by hand.
//!
//! Evidence class: the operating system is the `Native` mock below. Its pending set outlives
//! every handle, as the real one outlives the app. This is simulator/mock evidence, not proof of
//! physical delivery. The handle reads the system clock, so reminders are due far in the future
//! and the scenarios do not depend on the date they run.

use ohand_core::ffi::core_handle::exports::{
    ohand_core_close, ohand_core_open, ohand_core_result_free, ohand_core_set_event_callback,
    OhandCoreResult,
};
use ohand_core::ffi::notification_reconciliation::{
    ohand_core_complete_notification_effect, ohand_core_set_notification_effects,
    ohand_core_start_reconcile_reminders, OHAND_NOTIFICATION_COMMAND_CANCEL,
    OHAND_NOTIFICATION_COMMAND_LIST_PENDING, OHAND_NOTIFICATION_COMMAND_SCHEDULE,
};
use ohand_core::ffi::prompt_schedule::{
    ohand_core_start_configure_prompt_schedule, ohand_core_start_read_prompt_schedule,
    ohand_core_start_refresh_prompt_schedule,
};
use ohand_core::store::schema::{Database, SystemClock};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(120);
const STATUS_OK: u32 = 0;
const PROMPT_DAYS: usize = 3;

// ---- Mock OS ----

#[derive(Default)]
struct Center {
    /// notification identifier -> fire_at (RFC 3339)
    pending: BTreeMap<String, String>,
    schedule_calls: Vec<String>,
    cancel_calls: Vec<String>,
    /// While set, every schedule request is refused as unauthorized.
    permission_denied: bool,
}

impl Center {
    fn mutations(&self) -> usize {
        self.schedule_calls.len() + self.cancel_calls.len()
    }

    fn reset_counters(&mut self) {
        self.schedule_calls.clear();
        self.cancel_calls.clear();
    }

    fn prompt_ids(&self) -> Vec<String> {
        self.pending
            .keys()
            .filter(|id| id.starts_with("prompt:"))
            .cloned()
            .collect()
    }

    fn reminder_ids(&self) -> Vec<String> {
        self.pending
            .keys()
            .filter(|id| !id.starts_with("prompt:"))
            .cloned()
            .collect()
    }
}

/// The native side. Shared by every handle opened over the store.
struct Native {
    handle: Mutex<u64>,
    center: Mutex<Center>,
}

impl Native {
    fn new() -> Arc<Native> {
        Arc::new(Native {
            handle: Mutex::new(0),
            center: Mutex::new(Center::default()),
        })
    }

    fn center(&self) -> std::sync::MutexGuard<'_, Center> {
        self.center.lock().unwrap()
    }
}

fn complete(handle: u64, effect_id: u64, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    let outcome = consume(unsafe {
        ohand_core_complete_notification_effect(handle, effect_id, bytes.as_ptr(), bytes.len())
    });
    assert_eq!(outcome.0, STATUS_OK, "answer accepted");
}

unsafe extern "C" fn native_command(
    context: *mut c_void,
    effect_id: u64,
    command: u32,
    data: *const u8,
    len: usize,
) {
    let native = &*(context as *const Native);
    let handle = *native.handle.lock().unwrap();
    let body: Value = if len == 0 {
        Value::Null
    } else {
        serde_json::from_slice(std::slice::from_raw_parts(data, len)).unwrap()
    };
    let answer = {
        let mut center = native.center();
        match command {
            OHAND_NOTIFICATION_COMMAND_SCHEDULE => {
                let id = body["notification_id"].as_str().unwrap().to_string();
                if center.permission_denied {
                    json!({"error": {"class": "unauthorized", "code": "notifications_denied"}})
                } else {
                    let fire_at = body["fire_at"].as_str().unwrap().to_string();
                    center.pending.insert(id.clone(), fire_at);
                    center.schedule_calls.push(id);
                    json!({})
                }
            }
            OHAND_NOTIFICATION_COMMAND_CANCEL => {
                let id = body["notification_id"].as_str().unwrap().to_string();
                center.pending.remove(&id);
                center.cancel_calls.push(id);
                json!({})
            }
            OHAND_NOTIFICATION_COMMAND_LIST_PENDING => {
                let pending: Vec<Value> = center
                    .pending
                    .iter()
                    .map(|(id, at)| json!({"notification_id": id, "fire_at": at}))
                    .collect();
                json!({ "pending": pending })
            }
            // An abandon needs no answer.
            _ => return,
        }
    };
    complete(handle, effect_id, &answer);
}

// ---- Handle plumbing ----

fn consume(mut result: OhandCoreResult) -> (u32, Vec<u8>) {
    let payload = if result.data.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(result.data, result.len).to_vec() }
    };
    let status = result.status;
    unsafe { ohand_core_result_free(&mut result) };
    (status, payload)
}

struct Recorder {
    sender: Mutex<Sender<(u64, u32, Vec<u8>)>>,
}

unsafe extern "C" fn record_event(
    context: *mut c_void,
    operation_id: u64,
    status: u32,
    data: *const u8,
    len: usize,
) {
    let recorder = &*(context as *const Recorder);
    let payload = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let _ = recorder
        .sender
        .lock()
        .unwrap()
        .send((operation_id, status, payload));
}

/// One open handle (one app launch) over the store.
struct Launch {
    handle: u64,
    events: Receiver<(u64, u32, Vec<u8>)>,
    _recorder: Box<Recorder>,
    next_operation: u64,
    closed: bool,
}

impl Launch {
    fn open(path: &str, native: &Arc<Native>) -> Launch {
        let mut handle = 0;
        let (status, _) =
            consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(status, STATUS_OK);
        let (sender, events) = channel();
        let recorder = Box::new(Recorder {
            sender: Mutex::new(sender),
        });
        let (status, _) = consume(ohand_core_set_event_callback(
            handle,
            Some(record_event),
            &*recorder as *const Recorder as *mut c_void,
        ));
        assert_eq!(status, STATUS_OK);
        *native.handle.lock().unwrap() = handle;
        let (status, _) = consume(ohand_core_set_notification_effects(
            handle,
            Some(native_command),
            Arc::as_ptr(native) as *mut c_void,
        ));
        assert_eq!(status, STATUS_OK);
        Launch {
            handle,
            events,
            _recorder: recorder,
            next_operation: 1,
            closed: false,
        }
    }

    /// The app is killed: the worker stops and the handle is released.
    fn close(&mut self) {
        if !self.closed {
            consume(ohand_core_close(self.handle));
            self.closed = true;
        }
    }

    fn run(&mut self, start: impl FnOnce(u64, u64) -> OhandCoreResult) -> Value {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let (status, payload) = consume(start(self.handle, operation_id));
        assert_eq!(status, STATUS_OK, "start refused: {payload:?}");
        let (event_operation, event_status, payload) =
            self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event_operation, operation_id);
        assert_eq!(
            event_status,
            STATUS_OK,
            "operation failed: {}",
            String::from_utf8_lossy(&payload)
        );
        serde_json::from_slice(&payload).unwrap()
    }

    fn call(&mut self, request: Value, starter: StartFn) -> Value {
        let bytes = serde_json::to_vec(&request).unwrap();
        self.run(|handle, operation_id| unsafe {
            starter(handle, operation_id, bytes.as_ptr(), bytes.len())
        })
    }

    fn configure_daily_nine(&mut self) -> Value {
        self.call(
            json!({"time_zone": "UTC", "settings": {"mode": "daily", "time": {"hour": 9, "minute": 0}}}),
            ohand_core_start_configure_prompt_schedule,
        )
    }

    fn refresh_on_launch(&mut self) -> Value {
        self.call(
            json!({"time_zone": "UTC", "trigger": "launch"}),
            ohand_core_start_refresh_prompt_schedule,
        )
    }

    fn read_schedule(&mut self) -> Value {
        self.run(|handle, operation_id| ohand_core_start_read_prompt_schedule(handle, operation_id))
    }

    fn reconcile(&mut self, allow_prompts: bool, rearm_rejected: bool) -> Value {
        self.call(
            json!({
                "allow_optional_prompts": allow_prompts,
                "rearm_rejected_schedules": rearm_rejected,
            }),
            ohand_core_start_reconcile_reminders,
        )
    }
}

type StartFn = unsafe extern "C" fn(u64, u64, *const u8, usize) -> OhandCoreResult;

impl Drop for Launch {
    fn drop(&mut self) {
        self.close();
        consume(ohand_core_set_notification_effects(
            self.handle,
            None,
            std::ptr::null_mut(),
        ));
        consume(ohand_core_set_event_callback(
            self.handle,
            None,
            std::ptr::null_mut(),
        ));
    }
}

// ---- Store ----

struct Store {
    path: String,
    native: Arc<Native>,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Store {
    /// A migrated file-backed store holding `count` active items, each with a resolved explicit
    /// reminder far in the future that has not been handed to the OS yet.
    fn with_reminders(count: usize) -> Store {
        let path = format!(
            "{}/test_notification_resilience_boundary_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let mut database = Database::open(&path, Arc::new(SystemClock)).unwrap();
        let transaction = database.transaction().unwrap();
        for index in 0..count {
            let capture_id = format!("capture-boundary-{index}");
            let item_id = format!("7d1c5a1e-0000-4000-8000-{index:012}");
            let reminder_id = format!("4a2b8c1e-5d4a-4e0b-9a77-{index:012}");
            let due = format!("2090-01-0{}T09:00:00Z", 1 + index);
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
        drop(database);
        Store {
            path,
            native: Native::new(),
        }
    }

    fn launch(&self) -> Launch {
        Launch::open(&self.path, &self.native)
    }

    fn prompt_schedule_rows(&self) -> i64 {
        let database = Database::open(&self.path, Arc::new(SystemClock)).unwrap();
        database
            .conn()
            .query_row("SELECT COUNT(*) FROM prompt_schedule", [], |row| row.get(0))
            .unwrap()
    }

    fn notification_ids(&self) -> Vec<String> {
        let database = Database::open(&self.path, Arc::new(SystemClock)).unwrap();
        let mut statement = database
            .conn()
            .prepare("SELECT reminder_id || '#' || schedule_generation FROM reminders ORDER BY reminder_id")
            .unwrap();
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        ids
    }
}

fn schedule_states(outcome: &Value) -> Vec<String> {
    outcome["reminders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reminder| reminder["schedule_state"].as_str().unwrap().to_string())
        .collect()
}

// ---- Restart through the persisted schedule ----

#[test]
fn the_prompt_schedule_survives_close_and_reopen_and_a_relaunch_installs_nothing_twice() {
    let store = Store::with_reminders(1);
    let mut first = store.launch();
    first.configure_daily_nine();
    let planned = first.reconcile(true, false);
    assert_eq!(planned["converged"], true);
    assert_eq!(planned["installed"], 1);
    assert_eq!(planned["prompts_installed"], PROMPT_DAYS);
    let schedule_before = first.read_schedule();
    assert_eq!(store.prompt_schedule_rows(), 1, "persisted by the core");
    let pending_before = store.native.center().pending.clone();
    assert_eq!(pending_before.len(), 1 + PROMPT_DAYS);
    first.close();

    // A new process: the schedule is read back from the store, not from anything in memory.
    let mut second = store.launch();
    let schedule_after = second.read_schedule();
    assert_eq!(schedule_after["enabled"], true);
    assert_eq!(schedule_after["time"], json!({"hour": 9, "minute": 0}));
    assert_eq!(schedule_after["next"], schedule_before["next"]);

    second.refresh_on_launch();
    store.native.center().reset_counters();
    for _ in 0..2 {
        let pass = second.reconcile(true, false);
        assert_eq!(pass["converged"], true);
        assert_eq!(pass["installed"], 0);
        assert_eq!(pass["prompts_installed"], 0);
    }
    assert_eq!(
        store.native.center().mutations(),
        0,
        "a relaunch neither duplicates nor reschedules what the OS holds"
    );
    assert_eq!(store.native.center().pending, pending_before);
}

#[test]
fn a_purged_pending_set_is_restored_once_per_request_after_a_relaunch() {
    let store = Store::with_reminders(1);
    let mut first = store.launch();
    first.configure_daily_nine();
    first.reconcile(true, false);
    first.close();

    // The OS lost its pending requests while the app was closed.
    store.native.center().pending.clear();
    store.native.center().reset_counters();
    let mut second = store.launch();
    second.refresh_on_launch();
    let restored = second.reconcile(true, false);
    let again = second.reconcile(true, false);

    assert_eq!(restored["installed"], 1);
    assert_eq!(again["installed"], 0);
    assert_eq!(again["prompts_installed"], 0);
    let center = store.native.center();
    assert_eq!(center.reminder_ids(), store.notification_ids());
    assert!(
        center.prompt_ids().len() <= PROMPT_DAYS,
        "prompts stay within their horizon"
    );
    assert_eq!(
        center.schedule_calls.len(),
        1 + center.prompt_ids().len(),
        "one install per request the OS now holds, none twice"
    );
}

// ---- Permission loss and regain through the production reconciliation ----

#[test]
fn a_regained_permission_rearms_a_rejected_reminder_exactly_once_across_relaunches() {
    let store = Store::with_reminders(2);
    store.native.center().permission_denied = true;
    let mut first = store.launch();

    let denied = first.reconcile(false, false);
    assert_eq!(denied["converged"], false);
    assert_eq!(schedule_states(&denied), vec!["schedule_failed"; 2]);
    assert!(store.native.center().pending.is_empty());

    // Permission returns, but nothing retries on its own and nothing is reported as scheduled.
    store.native.center().permission_denied = false;
    store.native.center().reset_counters();
    first.close();
    let mut second = store.launch();
    let waiting = second.reconcile(false, false);
    assert_eq!(waiting["rearmed"], 0);
    assert_eq!(waiting["installed"], 0);
    assert_eq!(schedule_states(&waiting), vec!["schedule_failed"; 2]);
    assert_eq!(store.native.center().mutations(), 0);

    // The re-arm the native layer requests on regained permission installs each reminder once,
    // under the identifier it always had.
    let rearmed = second.reconcile(false, true);
    assert_eq!(rearmed["rearmed"], 2);
    assert_eq!(rearmed["installed"], 2);
    assert_eq!(rearmed["converged"], true);
    assert_eq!(schedule_states(&rearmed), vec!["scheduled"; 2]);
    assert_eq!(
        store.native.center().reminder_ids(),
        store.notification_ids()
    );
    assert_eq!(store.native.center().schedule_calls.len(), 2);

    // Repeating the request, in this launch and in a later one, changes nothing.
    store.native.center().reset_counters();
    let repeated = second.reconcile(false, true);
    assert_eq!(repeated["rearmed"], 0);
    second.close();
    let mut third = store.launch();
    let relaunched = third.reconcile(false, true);
    assert_eq!(relaunched["rearmed"], 0);
    assert_eq!(relaunched["installed"], 0);
    assert_eq!(schedule_states(&relaunched), vec!["scheduled"; 2]);
    assert_eq!(store.native.center().mutations(), 0);
    assert_eq!(
        store.native.center().reminder_ids(),
        store.notification_ids()
    );
}

#[test]
fn losing_prompt_permission_withholds_prompts_only_and_regaining_it_restores_each_once() {
    let store = Store::with_reminders(1);
    let mut launch = store.launch();
    launch.configure_daily_nine();
    launch.reconcile(true, false);
    let reminder_ids = store.native.center().reminder_ids();
    assert_eq!(store.native.center().prompt_ids().len(), PROMPT_DAYS);

    // Optional prompting is not allowed any more: the prompts are cancelled, the installed
    // explicit reminder is untouched.
    let withheld = launch.reconcile(false, false);
    assert_eq!(withheld["prompts_withheld"], PROMPT_DAYS);
    assert_eq!(withheld["prompts_installed"], 0);
    assert!(store.native.center().prompt_ids().is_empty());
    assert_eq!(store.native.center().reminder_ids(), reminder_ids);
    assert_eq!(schedule_states(&withheld), vec!["scheduled"]);

    // The schedule is still the user's choice, and regaining the permission restores the
    // prompts once, after a relaunch as well.
    launch.close();
    let mut relaunched = store.launch();
    assert_eq!(relaunched.read_schedule()["enabled"], true);
    store.native.center().reset_counters();
    let restored = relaunched.reconcile(true, false);
    let repeated = relaunched.reconcile(true, false);
    assert_eq!(restored["prompts_installed"], PROMPT_DAYS);
    assert_eq!(repeated["prompts_installed"], 0);
    let center = store.native.center();
    assert_eq!(center.prompt_ids().len(), PROMPT_DAYS);
    assert_eq!(center.schedule_calls.len(), PROMPT_DAYS);
    assert_eq!(center.reminder_ids(), reminder_ids);
}
