//! Cancelled exports leave nothing readable behind, and deleted text does not survive in the
//! store files (T04b findings F41). Fixtures are synthetic.

use ohand_core::ffi::core_handle::exports::{
    ohand_core_cancel, ohand_core_close, ohand_core_open, ohand_core_set_event_callback,
    OhandCoreResult,
};
use ohand_core::ffi::export_snapshot::ohand_core_start_export;
use ohand_core::ffi::item_deletion::record_item_deletion;
use ohand_core::retrieval::index::sync_item_in_tx;
use ohand_core::store::captures::{save_capture, Capture};
use ohand_core::store::schema::{Database, SystemClock};
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// One lowercase token, so the search index's tokenizer stores it byte for byte and the raw scan
/// can see it there as well as in the item and capture rows.
const CANARY: &str = "rutabagacanary4471";
const RACE_RUNS: u64 = 220;
const ITEM_COUNT: usize = 150;

fn insert_item(database: &mut Database, index: usize, text: &str) -> String {
    let capture_id = format!("capture-{index}");
    let item_id = format!("item-{index}");
    let capture = Capture::new(
        capture_id.clone(),
        Some(text.to_string()),
        None,
        "2026-10-10T09:30:00+00:00".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-personal".to_string(),
        false,
        "2026-10-10T09:30:01+00:00".to_string(),
        None,
    )
    .unwrap();
    save_capture(database, &capture).unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type,
                                save_state, sync_state, processing_state,
                                transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, 'active', 'note', 'saved', 'not_configured', 'pending',
                     'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
            [&item_id, &capture_id],
        )
        .unwrap();
    let transaction = database.immediate_transaction().unwrap();
    sync_item_in_tx(&transaction, &item_id).unwrap();
    transaction.commit().unwrap();
    item_id
}

fn open_store(path: &str) -> Database {
    Database::open(path, Arc::new(SystemClock)).unwrap()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn store_files_contain_canary(store_path: &str) -> bool {
    ["", "-journal", "-wal"].iter().any(|suffix| {
        std::fs::read(format!("{store_path}{suffix}")).is_ok_and(|bytes| contains(&bytes, CANARY))
    })
}

fn must_succeed(result: OhandCoreResult) {
    assert_eq!(result.status, 0, "ABI call failed");
}

struct Observed {
    succeeded: AtomicBool,
    cancel_handle: AtomicU32,
}

unsafe extern "C" fn record_event(
    context: *mut c_void,
    _operation_id: u64,
    status: u32,
    _data: *const u8,
    _len: usize,
) {
    let observed = &*(context as *const Observed);
    if status == 0 {
        observed.succeeded.store(true, Ordering::SeqCst);
        // Cancelling from inside the success callback must leave the export alone.
        let handle = observed.cancel_handle.load(Ordering::SeqCst);
        if handle != 0 {
            let _ = ohand_core_cancel(u64::from(handle));
        }
    }
}

fn start_export(handle: u64, output_path: &Path) -> OhandCoreResult {
    let request = serde_json::json!({
        "read_authentication": "authenticated",
        "scopes": ["personal"],
        "audio": "excluded",
        "output_path": output_path.display().to_string(),
    })
    .to_string();
    unsafe { ohand_core_start_export(handle, 1, request.as_ptr(), request.len()) }
}

fn export_fixture() -> (TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let store_path = directory.path().join("core.sqlite").display().to_string();
    let mut database = open_store(&store_path);
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')",
            [],
        )
        .unwrap();
    for index in 0..ITEM_COUNT {
        insert_item(&mut database, index, &format!("{CANARY} note {index}"));
    }
    (directory, store_path)
}

#[test]
fn cancel_racing_an_export_leaves_no_canary_file_unless_success_was_reported() {
    let (directory, store_path) = export_fixture();
    let mut completed = 0;
    for run in 0..RACE_RUNS {
        let output_path = directory.path().join(format!("export-{run}.jsonl"));
        let observed = Observed {
            succeeded: AtomicBool::new(false),
            cancel_handle: AtomicU32::new(0),
        };
        let mut handle = 0;
        must_succeed(unsafe {
            ohand_core_open(store_path.as_ptr(), store_path.len(), &mut handle)
        });
        must_succeed(ohand_core_set_event_callback(
            handle,
            Some(record_event),
            &observed as *const Observed as *mut c_void,
        ));
        must_succeed(start_export(handle, &output_path));
        std::thread::sleep(Duration::from_micros(
            (run % 40) * 150 * if run % 2 == 0 { 1 } else { 40 },
        ));
        must_succeed(ohand_core_cancel(handle));
        must_succeed(ohand_core_close(handle));

        let reported = observed.succeeded.load(Ordering::SeqCst);
        match std::fs::read(&output_path) {
            Ok(bytes) => {
                assert!(
                    reported,
                    "run {run}: export file left behind without a success report"
                );
                assert!(contains(&bytes, CANARY));
                completed += 1;
            }
            Err(_) => assert!(!reported, "run {run}: success reported but no file"),
        }
        let leftovers = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(&format!("export-{run}.")) && name != format!("export-{run}.jsonl")
            })
            .count();
        assert_eq!(leftovers, 0, "run {run}: temporary file left behind");
    }
    eprintln!("{completed} of {RACE_RUNS} runs completed before cancel");
}

#[test]
fn cancel_racing_an_export_never_removes_a_file_that_already_existed() {
    let (directory, store_path) = export_fixture();
    for run in 0..60u64 {
        let output_path = directory.path().join(format!("precious-{run}.txt"));
        std::fs::write(&output_path, b"caller-owned").unwrap();
        let mut handle = 0;
        must_succeed(unsafe {
            ohand_core_open(store_path.as_ptr(), store_path.len(), &mut handle)
        });
        must_succeed(start_export(handle, &output_path));
        std::thread::sleep(Duration::from_micros(run * 200));
        must_succeed(ohand_core_cancel(handle));
        must_succeed(ohand_core_close(handle));
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            b"caller-owned",
            "run {run}: a pre-existing destination was removed or replaced"
        );
    }
}

#[test]
fn cancel_after_success_is_reported_leaves_the_export() {
    let (directory, store_path) = export_fixture();
    let output_path = directory.path().join("export-complete.jsonl");
    let observed = Observed {
        succeeded: AtomicBool::new(false),
        cancel_handle: AtomicU32::new(0),
    };
    let mut handle = 0;
    must_succeed(unsafe { ohand_core_open(store_path.as_ptr(), store_path.len(), &mut handle) });
    observed
        .cancel_handle
        .store(u32::try_from(handle).unwrap(), Ordering::SeqCst);
    must_succeed(ohand_core_set_event_callback(
        handle,
        Some(record_event),
        &observed as *const Observed as *mut c_void,
    ));
    must_succeed(start_export(handle, &output_path));
    for _ in 0..500 {
        if observed.succeeded.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    must_succeed(ohand_core_close(handle));
    assert!(observed.succeeded.load(Ordering::SeqCst));
    assert!(contains(&std::fs::read(&output_path).unwrap(), CANARY));
}

fn delete_canary_item(store_path: &str, secure: bool) {
    let mut database = open_store(store_path);
    if !secure {
        database
            .conn()
            .execute_batch(
                "PRAGMA secure_delete = OFF;
                 INSERT INTO search_index(search_index, rank) VALUES('secure-delete', 0);",
            )
            .unwrap();
    }
    // Neighbouring rows keep the pages in use, so freed content would otherwise stay readable.
    for index in 0..40 {
        insert_item(&mut database, 1000 + index, "neighbouring synthetic note");
    }
    let item_id = insert_item(&mut database, 2000, &format!("{CANARY} to be deleted"));
    assert!(store_files_contain_canary(store_path));
    record_item_deletion(&mut database, &item_id, 0).unwrap();
    drop(database);
}

fn deletion_store() -> (TempDir, String) {
    let directory = tempfile::tempdir().unwrap();
    let store_path = directory.path().join("core.sqlite").display().to_string();
    (directory, store_path)
}

#[test]
fn deleted_text_is_absent_from_every_store_file() {
    let (_directory, store_path) = deletion_store();
    delete_canary_item(&store_path, true);
    assert!(!store_files_contain_canary(&store_path));
}

#[test]
fn deleted_words_are_absent_from_the_search_index_segments() {
    let (_directory, store_path) = deletion_store();
    delete_canary_item(&store_path, true);
    let database = open_store(&store_path);
    let segments_holding_canary: i64 = database
        .conn()
        .query_row(
            "SELECT count(*) FROM search_index_data WHERE instr(block, CAST(?1 AS BLOB)) > 0",
            [CANARY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(segments_holding_canary, 0);
}

#[test]
fn the_scan_detects_the_canary_when_secure_delete_is_off() {
    let (_directory, store_path) = deletion_store();
    delete_canary_item(&store_path, false);
    assert!(
        store_files_contain_canary(&store_path),
        "without secure_delete the deleted text remains, so the scan can see it"
    );
}

#[test]
fn an_export_with_no_callback_registered_leaves_no_file() {
    let (directory, store_path) = export_fixture();
    for run in 0..20 {
        let output_path = directory.path().join(format!("silent-{run}.jsonl"));
        let mut handle = 0;
        must_succeed(unsafe {
            ohand_core_open(store_path.as_ptr(), store_path.len(), &mut handle)
        });
        must_succeed(start_export(handle, &output_path));
        std::thread::sleep(Duration::from_millis(run * 5));
        must_succeed(ohand_core_close(handle));
        assert!(
            !output_path.exists(),
            "run {run}: an export nobody was told about left a file"
        );
    }
}

#[test]
fn a_connection_opened_directly_on_the_store_also_sets_secure_delete() {
    // The audio-removal fence opens its own connection with `Connection::open` on the store path
    // rather than through `Database::open`; it must get the same pragmas.
    let (_directory, store_path) = deletion_store();
    drop(open_store(&store_path));

    let direct = rusqlite::Connection::open(&store_path).unwrap();
    let secure_delete: i64 = direct
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = direct
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(secure_delete, 1);
    assert_eq!(foreign_keys, 1);

    direct
        .execute_batch("CREATE TABLE synthetic_scratch (text TEXT NOT NULL)")
        .unwrap();
    for _ in 0..40 {
        direct
            .execute(
                "INSERT INTO synthetic_scratch (text) VALUES ('neighbouring synthetic note')",
                [],
            )
            .unwrap();
    }
    direct
        .execute(
            "INSERT INTO synthetic_scratch (text) VALUES (?1)",
            [format!("{CANARY} to be deleted")],
        )
        .unwrap();
    assert!(store_files_contain_canary(&store_path));
    direct
        .execute(
            "DELETE FROM synthetic_scratch WHERE text LIKE 'rutabagacanary%'",
            [],
        )
        .unwrap();
    drop(direct);
    assert!(!store_files_contain_canary(&store_path));
}
