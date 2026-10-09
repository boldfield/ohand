//! Drives the health export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::OHAND_CORE_STATUS_OK;
use crate::ffi::core_handle::tests::{
    assert_rejected, consume, open_memory, recorder, register, serial, WAIT,
};
use serde_json::Value;

fn read_health(handle: OhandCoreHandle, operation_id: u64, stall: u32, grace: u32) -> Value {
    let (recorder, events) = recorder();
    register(handle, &recorder);
    let outcome = consume(ohand_core_start_processing_health(
        handle,
        operation_id,
        stall,
        grace,
    ));
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    let event = events.recv_timeout(WAIT).expect("a health event");
    assert_eq!(event.operation_id, operation_id);
    assert_eq!(event.status, OHAND_CORE_STATUS_OK);
    assert_ne!(event.thread, std::thread::current().id());
    serde_json::from_slice(&event.payload).unwrap()
}

#[test]
fn an_empty_store_reports_idle_health_with_the_operation_id() {
    let _guard = serial();
    let handle = open_memory();
    let health = read_health(handle, 7, 0, 0);
    assert_eq!(health["operation_id"], 7);
    assert_eq!(health["summary"], "idle");
    assert_eq!(health["stalled"], false);
    assert_eq!(health["pending_jobs"], 0);
    assert!(health["errors"].as_array().unwrap().is_empty());
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}

#[test]
fn due_work_that_nothing_drains_is_reported_as_a_stall() {
    let _guard = serial();
    let directory = std::env::temp_dir().join(format!("ohand-ffi-health-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    {
        let mut database = Database::open(
            &path,
            std::sync::Arc::new(crate::store::schema::SystemClock),
        )
        .unwrap();
        let tx = database.transaction().unwrap();
        tx.execute(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes,
                locale, calendar, item_scope, route_id, entry_locked, created_at)
             VALUES ('capture-1', 'synthetic', '2020-01-01T00:00:00Z', 'UTC', 0, 'en', 'gregorian',
                     'personal', 'route-1', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state,
                processing_state, transcription_state, created_at, updated_at)
             VALUES ('item-1', 'capture-1', 0, 'active', 'saved_local', 'not_configured',
                     'unprocessed', 'not_applicable', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, status,
                attempt_count, created_at)
             VALUES ('job-1', 1, 'item-1', 'interpret', 0, 'queued', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let mut handle = 0;
    let opened = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(opened.status, OHAND_CORE_STATUS_OK);

    let health = read_health(handle, 9, 60, 30);
    assert_eq!(health["summary"], "stalled");
    assert_eq!(health["stalled"], true);
    assert_eq!(health["stall_reasons"][0], "overdue");
    let text = health.to_string();
    assert!(!text.contains("item-1") && !text.contains("job-1") && !text.contains("synthetic"));
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn an_unreasonable_threshold_is_refused_before_anything_is_queued() {
    let _guard = serial();
    let handle = open_memory();
    let outcome = consume(ohand_core_start_processing_health(
        handle,
        1,
        OHAND_CORE_MAX_HEALTH_THRESHOLD_SECONDS + 1,
        0,
    ));
    assert_rejected(&outcome, 2, "invalid_request");
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}
