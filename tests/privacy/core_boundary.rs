//! A synthetic mixed-scope store and a driver for the real exported C functions, so the read and
//! export tests cross the same boundary Swift does. Only the store contents are synthetic.

use ohand_core::ffi::core_handle::exports::{
    ohand_core_cancel, ohand_core_close, ohand_core_open, ohand_core_result_free,
    ohand_core_set_event_callback, OhandCoreHandle, OhandCoreResult,
};
use ohand_core::ffi::core_handle::failure::OHAND_CORE_STATUS_OK;
use ohand_core::ffi::item_corrections::ohand_core_start_item_correction;
use ohand_core::ingress::import_foreground_ingress;
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Database, SystemClock};
use rusqlite::types::ValueRef;
use serde_json::Value;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

pub const WORK_ROUTE: &str = "route-work";
pub const PERSONAL_ROUTE: &str = "route-personal";
/// Distinct synthetic content per scope, so a leak names its source.
pub const PERSONAL_CANARY: &str = "CANARYPERSONAL4d2a";
pub const WORK_CANARY: &str = "CANARYWORK81bc";
/// Replaced by a correction; the replaced original stays searchable only inside its own scope.
pub const PERSONAL_ORIGINAL_CANARY: &str = "CANARYORIGINAL07ef";
pub const PERSONAL_CORRECTED_CANARY: &str = "CANARYCORRECTED93aa";
/// Appears in the session-topic of a personal item.
pub const PERSONAL_TOPIC_CANARY: &str = "CANARYTOPIC5e10";
const WAIT: Duration = Duration::from_secs(10);

pub struct Seeded {
    pub personal_item: String,
    pub work_item: String,
    pub corrected_item: String,
}

pub struct MixedStore {
    _directory: TempDir,
    pub root: PathBuf,
    pub store_path: String,
    pub seeded: Seeded,
}

fn capture(capture_id: &str, text: &str, scope: &str, topic: Option<&str>) -> Capture {
    let route = if scope == "work" {
        WORK_ROUTE
    } else {
        PERSONAL_ROUTE
    };
    Capture {
        capture_id: capture_id.to_string(),
        text: Some(text.to_string()),
        audio_reference: None,
        capture_instant: "2026-10-08T09:30:00+00:00".to_string(),
        timezone_id: "UTC".to_string(),
        utc_offset_minutes: 0,
        locale: "en_US".to_string(),
        calendar: "gregorian".to_string(),
        item_scope: scope.to_string(),
        route_id: route.to_string(),
        entry_locked: false,
        created_at: "2026-10-08T09:30:01+00:00".to_string(),
        session_topic: topic.map(str::to_string),
    }
}

impl MixedStore {
    pub fn new() -> MixedStore {
        let directory = TempDir::new().unwrap();
        let root = directory.path().join("app");
        std::fs::create_dir(&root).unwrap();
        let store_path = root.join("ohand.db").display().to_string();
        let mut database = Database::open(&store_path, Arc::new(SystemClock)).unwrap();
        for (route, scope) in [(PERSONAL_ROUTE, "personal"), (WORK_ROUTE, "work")] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                         created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-01T00:00:00Z')",
                    rusqlite::params![route, route, scope],
                )
                .unwrap();
        }
        let mut import = |capture: &Capture| {
            import_foreground_ingress(&mut database, capture)
                .unwrap()
                .item_id
        };
        let personal_item = import(&capture(
            "personal-1",
            &format!("call the clinic about {PERSONAL_CANARY}"),
            "personal",
            Some(PERSONAL_TOPIC_CANARY),
        ));
        let work_item = import(&capture(
            "work-1",
            &format!("quarterly review notes {WORK_CANARY}"),
            "work",
            None,
        ));
        let corrected_item = import(&capture(
            "personal-2",
            &format!("draft {PERSONAL_ORIGINAL_CANARY}"),
            "personal",
            None,
        ));
        // The correction goes through the real correction command, so the stored rows and the
        // search index are what production writes, not hand-written approximations.
        let revision: i64 = database
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [&corrected_item],
                |row| row.get(0),
            )
            .unwrap();
        drop(database);
        {
            let mut core = OpenCore::open(&store_path);
            let outcome = core.run(
                ohand_core_start_item_correction,
                &serde_json::json!({
                    "command": "correct_text",
                    "command_id": "correction-1",
                    "item_id": corrected_item,
                    "expected_revision": revision,
                    "text": format!("final {PERSONAL_CORRECTED_CANARY}"),
                }),
            );
            assert!(outcome.is_ok(), "{}", outcome.text());
            assert_eq!(outcome.json()["outcome"], "corrected");
        }
        MixedStore {
            _directory: directory,
            root,
            store_path,
            seeded: Seeded {
                personal_item,
                work_item,
                corrected_item,
            },
        }
    }

    pub fn database(&self) -> Database {
        Database::open(&self.store_path, Arc::new(SystemClock)).unwrap()
    }

    /// Every value of every table rendered as text, for asserting that content is absent from
    /// durable state altogether.
    pub fn dump_state(&self) -> String {
        let database = self.database();
        let conn = database.conn();
        let mut tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        let names: Vec<String> = tables
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut dump = String::new();
        for name in names {
            let Ok(mut statement) = conn.prepare(&format!("SELECT * FROM \"{name}\"")) else {
                continue;
            };
            let columns = statement.column_count();
            let Ok(mut rows) = statement.query([]) else {
                continue;
            };
            while let Ok(Some(row)) = rows.next() {
                for column in 0..columns {
                    match row.get_ref(column) {
                        Ok(ValueRef::Text(text)) => dump.push_str(&String::from_utf8_lossy(text)),
                        Ok(ValueRef::Blob(bytes)) => dump.push_str(&String::from_utf8_lossy(bytes)),
                        Ok(ValueRef::Integer(value)) => dump.push_str(&value.to_string()),
                        _ => {}
                    }
                    dump.push('\n');
                }
            }
        }
        dump
    }
}

/// Names of the files directly inside `directory`, sorted.
pub fn file_names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---- The exported C functions ----

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

/// What a finished operation told native.
pub struct Outcome {
    pub status: u32,
    pub payload: Vec<u8>,
}

impl Outcome {
    pub fn is_ok(&self) -> bool {
        self.status == OHAND_CORE_STATUS_OK
    }

    /// The raw JSON text, for asserting that something never appears in it.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.payload).unwrap()
    }

    pub fn code(&self) -> String {
        self.json()["code"].as_str().unwrap().to_string()
    }
}

/// An open core over a store file, with every outcome recorded.
pub struct OpenCore {
    pub handle: OhandCoreHandle,
    // Boxed so the callback context keeps a stable address until the handle is closed.
    _recorder: Box<Recorder>,
    receiver: Receiver<(u64, u32, Vec<u8>)>,
    next_operation: u64,
    closed: bool,
}

impl OpenCore {
    pub fn open(store_path: &str) -> OpenCore {
        let mut handle = 0;
        let (status, _) =
            consume(unsafe { ohand_core_open(store_path.as_ptr(), store_path.len(), &mut handle) });
        assert_eq!(status, OHAND_CORE_STATUS_OK);
        let (sender, receiver) = channel();
        let recorder = Box::new(Recorder {
            sender: Mutex::new(sender),
        });
        let (status, _) = consume(ohand_core_set_event_callback(
            handle,
            Some(record_event),
            &*recorder as *const Recorder as *mut c_void,
        ));
        assert_eq!(status, OHAND_CORE_STATUS_OK);
        OpenCore {
            handle,
            _recorder: recorder,
            receiver,
            next_operation: 1,
            closed: false,
        }
    }

    /// Start an operation with `start` and wait for its outcome: either the immediate refusal
    /// or the event for the operation.
    pub fn run(
        &mut self,
        start: unsafe extern "C" fn(OhandCoreHandle, u64, *const u8, usize) -> OhandCoreResult,
        request: &Value,
    ) -> Outcome {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let bytes = serde_json::to_vec(request).unwrap();
        let (status, payload) =
            consume(unsafe { start(self.handle, operation_id, bytes.as_ptr(), bytes.len()) });
        if status != OHAND_CORE_STATUS_OK {
            return Outcome { status, payload };
        }
        loop {
            let (id, status, payload) = self.receiver.recv_timeout(WAIT).expect("core outcome");
            if id == operation_id {
                return Outcome { status, payload };
            }
        }
    }
}

impl OpenCore {
    /// Queue an operation without waiting for its outcome.
    pub fn submit(
        &mut self,
        start: unsafe extern "C" fn(OhandCoreHandle, u64, *const u8, usize) -> OhandCoreResult,
        request: &Value,
    ) -> Outcome {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let bytes = serde_json::to_vec(request).unwrap();
        let (status, payload) =
            consume(unsafe { start(self.handle, operation_id, bytes.as_ptr(), bytes.len()) });
        Outcome { status, payload }
    }

    /// Cancel everything queued or running, as native does when the session ends.
    pub fn cancel(&self) {
        let (status, _) = consume(ohand_core_cancel(self.handle));
        assert_eq!(status, OHAND_CORE_STATUS_OK);
    }

    /// Close the handle (which waits for its worker) and return every outcome native was told.
    pub fn close_and_collect(&mut self) -> Vec<Outcome> {
        if !self.closed {
            self.closed = true;
            let _ = consume(ohand_core_close(self.handle));
        }
        self.receiver
            .try_iter()
            .map(|(_, status, payload)| Outcome { status, payload })
            .collect()
    }
}

impl Drop for OpenCore {
    fn drop(&mut self) {
        if !self.closed {
            let _ = consume(ohand_core_close(self.handle));
        }
    }
}
