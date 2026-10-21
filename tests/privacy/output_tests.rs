//! Outward presentations (T04b): generic notification payloads and exports, plus the files
//! application code leaves behind. A canary may be readable only where the user asked to read it.

use crate::core_boundary::{
    file_names, MixedStore, OpenCore, PERSONAL_CANARY, PERSONAL_CORRECTED_CANARY,
    PERSONAL_ORIGINAL_CANARY, PERSONAL_ROUTE, WORK_CANARY,
};
use crate::fixtures::{runner_config, setup_time, SteppedClock};
use chrono::{DateTime, TimeZone, Utc};
use ohand_core::export::{export_core_snapshot, AudioInclusion, ExportRequest, ReadAuthentication};
use ohand_core::ffi::core_handle::exports::ohand_core_cancel;
use ohand_core::ffi::export_snapshot::ohand_core_start_export;
use ohand_core::ingress::import_foreground_ingress;
use ohand_core::interpretation::dispatch::{AdapterRegistry, InterpretationDispatcher};
use ohand_core::jobs::queue::enqueue_job;
use ohand_core::jobs::runner::{InterpretationCapability, JobCapabilities, JobRunner};
use ohand_core::lifecycle::delete_intent::mark_deletion_intent;
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::providers::contracts::{CancelToken, ManualClock};
use ohand_core::reminders::coordinator::ReminderCoordinator;
use ohand_core::reminders::reconcile::{
    NotificationEffectError, NotificationEffects, PayloadKind, PendingRequest, ScheduleRequest,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::events::ItemScope;
use serde_json::{json, Value};
use std::path::Path;
use uuid::Uuid;

const REMINDER_CANARY: &str = "CANARYREMINDER6c0d";

// ---- Notifications ----

#[derive(Default)]
struct RecordingCenter {
    scheduled: Vec<ScheduleRequest>,
    pending: Vec<PendingRequest>,
}

impl NotificationEffects for RecordingCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.scheduled.push(request.clone());
        self.pending.push(PendingRequest {
            notification_id: request.notification_id.clone(),
            fire_at: Some(request.fire_at),
        });
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.pending
            .retain(|request| request.notification_id != notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        Ok(self.pending.clone())
    }
}

/// Saves a personal reminder command with a canary session topic and installs its reminder.
fn install_reminder(store: &MixedStore, center: &mut RecordingCenter) -> String {
    let mut database = store.database();
    let capture = Capture {
        capture_id: Uuid::new_v4().to_string(),
        text: Some("Remind me 2026-10-20 14:30:00 to call mom".to_string()),
        audio_reference: None,
        capture_instant: "2026-10-08T09:30:00Z".to_string(),
        timezone_id: "UTC".to_string(),
        utc_offset_minutes: 0,
        locale: "en_US".to_string(),
        calendar: "gregorian".to_string(),
        item_scope: "personal".to_string(),
        route_id: PERSONAL_ROUTE.to_string(),
        entry_locked: false,
        created_at: "2026-10-08T09:30:01Z".to_string(),
        // The offline grammar is closed, so the canary rides in the topic, which is stored with
        // the item and must not reach the OS either.
        session_topic: Some(REMINDER_CANARY.to_string()),
    };
    let item_id = import_foreground_ingress(&mut database, &capture)
        .unwrap()
        .item_id;
    // Only the reminder command is processed; the other seeded items stay untouched.
    database.conn().execute("DELETE FROM jobs", []).unwrap();
    enqueue_job(
        &mut database,
        Uuid::new_v4().to_string(),
        item_id.clone(),
        JOB_TYPE_INTERPRET.to_string(),
        0,
        None,
        Some(Uuid::new_v4().to_string()),
        1,
        setup_time(),
    )
    .unwrap();

    let clock = SteppedClock::after_setup();
    let registry = AdapterRegistry::new();
    let provider_clock = ManualClock::new();
    let capabilities = JobCapabilities::new().with(InterpretationCapability::new(
        InterpretationDispatcher::new(&registry, &provider_clock),
    ));
    let runner = JobRunner::new(capabilities, clock.as_ref(), runner_config());
    let outcome = ReminderCoordinator::new(clock.as_ref()).drain_and_settle(
        &mut database,
        &runner,
        &CancelToken::new(),
        center,
        &[],
    );
    assert!(outcome.settled.is_ok(), "{:?}", outcome.settled);
    item_id
}

#[test]
fn every_notification_request_is_generic_and_carries_no_capture_text() {
    let store = MixedStore::new();
    let mut center = RecordingCenter::default();
    let item_id = install_reminder(&store, &mut center);

    assert_eq!(center.scheduled.len(), 1, "the reminder must be installed");
    for request in &center.scheduled {
        assert_eq!(request.payload_kind, PayloadKind::Generic);
        assert_eq!(request.opaque_target_id, item_id);
        let rendered = format!("{request:?}");
        for canary in [REMINDER_CANARY, PERSONAL_CANARY, WORK_CANARY, "call mom"] {
            assert!(!rendered.contains(canary), "{canary} in {rendered}");
        }
    }
}

// ---- Exports ----

fn exported_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, 8, 0, 0).unwrap()
}

fn export_request(path: &Path, authentication: &str, scopes: &[&str]) -> Value {
    json!({
        "read_authentication": authentication,
        "scopes": scopes,
        "audio": "excluded",
        "output_path": path.display().to_string(),
    })
}

#[test]
fn an_export_holds_only_the_requested_scope_and_no_replaced_text_of_another() {
    let store = MixedStore::new();
    let mut database = store.database();
    let mut output = Vec::new();
    let summary = export_core_snapshot(
        &mut database,
        &ExportRequest {
            authentication: ReadAuthentication::Authenticated,
            scopes: vec![ItemScope::Work],
            audio: AudioInclusion::Excluded,
        },
        exported_at(),
        &mut output,
    )
    .unwrap();
    let text = String::from_utf8(output).unwrap();

    assert_eq!(summary.item_count, 1);
    assert!(text.contains(WORK_CANARY));
    for canary in [
        PERSONAL_CANARY,
        PERSONAL_ORIGINAL_CANARY,
        PERSONAL_CORRECTED_CANARY,
    ] {
        assert!(!text.contains(canary), "{canary} in a work-only export");
    }
}

#[test]
fn a_locked_session_export_is_refused_and_leaves_nothing_behind() {
    let store = MixedStore::new();
    let exports = store.root.join("exports");
    std::fs::create_dir(&exports).unwrap();
    let target = exports.join("snapshot.jsonl");
    let mut core = OpenCore::open(&store.store_path);

    let refused = core.run(
        ohand_core_start_export,
        &export_request(&target, "capture_only", &["personal", "work"]),
    );
    assert!(!refused.is_ok());
    assert_eq!(refused.code(), "export_not_authorized");
    assert!(
        file_names(&exports).is_empty(),
        "a refused export wrote a file"
    );
    for canary in [PERSONAL_CANARY, WORK_CANARY] {
        assert!(!refused.text().contains(canary));
    }
    assert!(
        !refused.text().contains(&target.display().to_string()),
        "the failure echoed the output path"
    );
}

#[test]
fn a_failed_or_cancelled_export_leaves_no_readable_file() {
    let store = MixedStore::new();
    let exports = store.root.join("exports");
    std::fs::create_dir(&exports).unwrap();
    let mut core = OpenCore::open(&store.store_path);

    // A destination that cannot be created.
    let missing = exports.join("absent").join("snapshot.jsonl");
    let unwritable = core.run(
        ohand_core_start_export,
        &export_request(&missing, "authenticated", &["personal"]),
    );
    assert!(!unwritable.is_ok());
    assert!(!unwritable.text().contains(PERSONAL_CANARY));
    assert!(file_names(&exports).is_empty());

    // An existing file is never replaced or read back.
    let existing = exports.join("existing.jsonl");
    std::fs::write(&existing, b"prior").unwrap();
    let collided = core.run(
        ohand_core_start_export,
        &export_request(&existing, "authenticated", &["personal"]),
    );
    assert!(!collided.is_ok());
    assert_eq!(std::fs::read(&existing).unwrap(), b"prior");
    std::fs::remove_file(&existing).unwrap();

    // Cancelling the core before the export is queued discards it.
    let cancelled_target = exports.join("cancelled.jsonl");
    let cancel = ohand_core_cancel(core.handle);
    assert_eq!(cancel.status, 0);
    let request = serde_json::to_vec(&export_request(
        &cancelled_target,
        "authenticated",
        &["personal", "work"],
    ))
    .unwrap();
    let late =
        unsafe { ohand_core_start_export(core.handle, 900, request.as_ptr(), request.len()) };
    assert_ne!(late.status, 0, "work was accepted after cancel");
    assert!(
        file_names(&exports).is_empty(),
        "{:?}",
        file_names(&exports)
    );
}

#[test]
fn a_deleted_item_is_absent_from_every_export_and_from_the_application_directory() {
    let store = MixedStore::new();
    let personal = store.seeded.personal_item.clone();
    let mut database = store.database();
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [&personal],
            |row| row.get(0),
        )
        .unwrap();
    mark_deletion_intent(&mut database, &personal, revision, exported_at()).unwrap();

    let exports = store.root.join("exports");
    std::fs::create_dir(&exports).unwrap();
    let target = exports.join("snapshot.jsonl");
    let mut core = OpenCore::open(&store.store_path);
    let done = core.run(
        ohand_core_start_export,
        &export_request(&target, "authenticated", &["personal", "work"]),
    );
    assert!(done.is_ok(), "{}", done.text());
    let exported = std::fs::read_to_string(&target).unwrap();
    assert!(!exported.contains(PERSONAL_CANARY));
    assert!(!exported.contains(&personal), "a tombstone was exported");
    assert!(exported.contains(WORK_CANARY));

    // No other file application code wrote holds the deleted text. The store's own file is
    // covered by the logical dump in the read tests: SQLite may keep freed pages until reuse.
    drop(core);
    drop(database);
    for name in file_names(&store.root) {
        let path = store.root.join(&name);
        if path.is_file() && !name.starts_with("ohand.db") {
            let bytes = std::fs::read(&path).unwrap();
            let readable = String::from_utf8_lossy(&bytes);
            assert!(
                !readable.contains(PERSONAL_CANARY),
                "{name} still holds deleted text"
            );
        }
    }
}
