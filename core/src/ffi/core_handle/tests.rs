//! Drives the exported C functions the way Swift does, against the real core store.

use super::exports::*;
use super::failure::*;
use super::instance::{self, JobFn};
use crate::providers::contracts::{ErrorClass, FailureKind, ProviderFailure};
use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::Duration;

pub(crate) const WAIT: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(150);

static SERIAL: Mutex<()> = Mutex::new(());

/// Counters are process-wide, so every test runs one at a time.
pub(crate) fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Event {
    pub(crate) operation_id: u64,
    pub(crate) status: u32,
    pub(crate) payload: Vec<u8>,
    pub(crate) thread: ThreadId,
}

pub(crate) struct Recorder {
    sender: Mutex<Sender<Event>>,
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
    let _ = recorder.sender.lock().unwrap().send(Event {
        operation_id,
        status,
        payload,
        thread: std::thread::current().id(),
    });
}

pub(crate) fn recorder() -> (Box<Recorder>, Receiver<Event>) {
    let (sender, receiver) = channel();
    (
        Box::new(Recorder {
            sender: Mutex::new(sender),
        }),
        receiver,
    )
}

fn context_of(recorder: &Recorder) -> *mut c_void {
    recorder as *const Recorder as *mut c_void
}

pub(crate) struct Outcome {
    pub(crate) status: u32,
    pub(crate) payload: Vec<u8>,
}

impl Outcome {
    pub(crate) fn code(&self) -> String {
        let value: serde_json::Value = serde_json::from_slice(&self.payload).unwrap();
        value["code"].as_str().unwrap().to_string()
    }
    pub(crate) fn class(&self) -> String {
        let value: serde_json::Value = serde_json::from_slice(&self.payload).unwrap();
        value["class"].as_str().unwrap().to_string()
    }
}

pub(crate) fn consume(mut result: OhandCoreResult) -> Outcome {
    let payload = if result.data.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(result.data, result.len).to_vec() }
    };
    let status = result.status;
    unsafe {
        ohand_core_result_free(&mut result);
        ohand_core_result_free(&mut result);
    }
    Outcome { status, payload }
}

pub(crate) fn open_memory() -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(b":memory:".as_ptr(), 8, &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    assert_ne!(handle, 0);
    handle
}

pub(crate) fn assert_rejected(outcome: &Outcome, status: u32, code: &str) {
    assert_eq!(outcome.status, status);
    assert_eq!(outcome.code(), code);
}

pub(crate) fn register(handle: OhandCoreHandle, recorder: &Recorder) {
    let outcome = consume(ohand_core_set_event_callback(
        handle,
        Some(record_event),
        context_of(recorder),
    ));
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
}

#[test]
fn repeated_open_and_close_return_every_counter_to_its_baseline() {
    let _guard = serial();
    let handles_before = ohand_core_live_handles();
    let buffers_before = ohand_core_live_result_buffers();
    for _ in 0..40 {
        let handle = open_memory();
        assert_eq!(ohand_core_live_handles(), handles_before + 1);
        assert_eq!(
            consume(ohand_core_close(handle)).status,
            OHAND_CORE_STATUS_OK
        );
    }
    assert_eq!(ohand_core_live_handles(), handles_before);
    assert_eq!(ohand_core_live_result_buffers(), buffers_before);
}

#[test]
fn handles_are_never_reused() {
    let _guard = serial();
    let first = open_memory();
    consume(ohand_core_close(first));
    let second = open_memory();
    assert_ne!(first, second);
    consume(ohand_core_close(second));
}

#[test]
fn closed_never_issued_and_zero_handles_are_rejected_by_every_export() {
    let _guard = serial();
    let closed = open_memory();
    assert_eq!(
        consume(ohand_core_close(closed)).status,
        OHAND_CORE_STATUS_OK
    );
    let (recorder, _events) = recorder();
    for handle in [closed, 0, 1 << 40, u64::MAX] {
        for outcome in [
            consume(ohand_core_close(handle)),
            consume(ohand_core_cancel(handle)),
            consume(ohand_core_start_store_check(handle, 1)),
            consume(ohand_core_set_event_callback(
                handle,
                Some(record_event),
                context_of(&recorder),
            )),
            consume(ohand_core_set_event_callback(
                handle,
                None,
                std::ptr::null_mut(),
            )),
        ] {
            assert_rejected(&outcome, OHAND_CORE_STATUS_PERMANENT, "invalid_handle");
            assert_eq!(outcome.class(), "permanent");
        }
    }
}

#[test]
fn invalid_open_input_is_rejected_without_leaking_or_touching_caller_memory() {
    let _guard = serial();
    let handles_before = ohand_core_live_handles();
    let buffers_before = ohand_core_live_result_buffers();
    let mut handle = 99;

    let null_path = consume(unsafe { ohand_core_open(std::ptr::null(), 5, &mut handle) });
    assert_rejected(&null_path, OHAND_CORE_STATUS_PERMANENT, "null_argument");
    assert_eq!(handle, 0);

    let empty = consume(unsafe { ohand_core_open(std::ptr::null(), 0, &mut handle) });
    assert_rejected(&empty, OHAND_CORE_STATUS_PERMANENT, "invalid_path");

    // The oversized length is refused from the length alone; this pointer is never read.
    let bogus = std::ptr::NonNull::<u8>::dangling().as_ptr();
    let oversized =
        consume(unsafe { ohand_core_open(bogus, OHAND_CORE_MAX_PATH_BYTES + 1, &mut handle) });
    assert_rejected(&oversized, OHAND_CORE_STATUS_PERMANENT, "request_too_large");

    let invalid_utf8 = consume(unsafe { ohand_core_open([0xff, 0xfe].as_ptr(), 2, &mut handle) });
    assert_rejected(&invalid_utf8, OHAND_CORE_STATUS_PERMANENT, "invalid_utf8");

    let no_out = consume(unsafe { ohand_core_open(b":memory:".as_ptr(), 8, std::ptr::null_mut()) });
    assert_rejected(&no_out, OHAND_CORE_STATUS_PERMANENT, "null_argument");

    let missing_directory = b"/nonexistent-directory-for-ohand/core.db";
    let unavailable = consume(unsafe {
        ohand_core_open(
            missing_directory.as_ptr(),
            missing_directory.len(),
            &mut handle,
        )
    });
    assert_rejected(
        &unavailable,
        OHAND_CORE_STATUS_PERMANENT,
        "store_unavailable",
    );
    assert!(!String::from_utf8_lossy(&unavailable.payload).contains("nonexistent"));
    assert_eq!(handle, 0);

    assert_eq!(ohand_core_live_handles(), handles_before);
    assert_eq!(ohand_core_live_result_buffers(), buffers_before);
}

#[test]
fn freeing_a_result_twice_or_null_is_harmless() {
    let _guard = serial();
    let buffers_before = ohand_core_live_result_buffers();
    let mut result = ohand_core_close(0);
    assert_eq!(ohand_core_live_result_buffers(), buffers_before + 1);
    unsafe {
        ohand_core_result_free(&mut result);
        ohand_core_result_free(&mut result);
        ohand_core_result_free(std::ptr::null_mut());
    }
    assert!(result.data.is_null());
    assert_eq!(ohand_core_live_result_buffers(), buffers_before);
}

#[test]
fn store_check_runs_on_a_background_thread_in_submission_order() {
    let _guard = serial();
    let handle = open_memory();
    let (recorder, events) = recorder();
    register(handle, &recorder);
    for operation_id in 1..=50 {
        assert_eq!(
            consume(ohand_core_start_store_check(handle, operation_id)).status,
            OHAND_CORE_STATUS_OK
        );
    }
    let expected_schema_version = crate::store::schema::MIGRATIONS
        .last()
        .unwrap()
        .target_version;
    for operation_id in 1..=50 {
        let event = events.recv_timeout(WAIT).unwrap();
        assert_eq!(event.operation_id, operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK);
        assert_ne!(event.thread, std::thread::current().id());
        let value: serde_json::Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(value["operation_id"], operation_id);
        assert_eq!(value["schema_version"], expected_schema_version);
        assert_eq!(value["capture_count"], 0);
    }
    consume(ohand_core_close(handle));
}

#[test]
fn events_without_a_registered_callback_are_discarded_and_clearing_stops_delivery() {
    let _guard = serial();
    let handle = open_memory();
    let (recorder, events) = recorder();
    consume(ohand_core_start_store_check(handle, 1));
    register(handle, &recorder);
    consume(ohand_core_start_store_check(handle, 2));
    let event = events.recv_timeout(WAIT).unwrap();
    assert!(event.operation_id == 1 || event.operation_id == 2);
    let clear = consume(ohand_core_set_event_callback(
        handle,
        None,
        std::ptr::null_mut(),
    ));
    assert_eq!(clear.status, OHAND_CORE_STATUS_OK);
    while events.recv_timeout(SETTLE).is_ok() {}
    consume(ohand_core_start_store_check(handle, 3));
    assert!(events.recv_timeout(SETTLE).is_err());
    consume(ohand_core_close(handle));
}

#[test]
fn cancel_before_start_refuses_work_and_delivers_nothing() {
    let _guard = serial();
    let handle = open_memory();
    let (recorder, events) = recorder();
    register(handle, &recorder);
    assert_eq!(
        consume(ohand_core_cancel(handle)).status,
        OHAND_CORE_STATUS_OK
    );
    assert_eq!(
        consume(ohand_core_cancel(handle)).status,
        OHAND_CORE_STATUS_OK
    );
    let refused = consume(ohand_core_start_store_check(handle, 1));
    assert_rejected(&refused, OHAND_CORE_STATUS_CANCELLED, "cancelled");
    assert_eq!(refused.class(), "cancelled");
    let refused_registration = consume(ohand_core_set_event_callback(
        handle,
        Some(record_event),
        context_of(&recorder),
    ));
    assert_rejected(
        &refused_registration,
        OHAND_CORE_STATUS_CANCELLED,
        "cancelled",
    );
    assert!(events.recv_timeout(SETTLE).is_err());
    assert_eq!(
        consume(ohand_core_close(handle)).status,
        OHAND_CORE_STATUS_OK
    );
    assert_rejected(
        &consume(ohand_core_cancel(handle)),
        OHAND_CORE_STATUS_PERMANENT,
        "invalid_handle",
    );
}

#[test]
fn no_callback_starts_after_cancel_returns() {
    let _guard = serial();
    for _ in 0..8 {
        let handle = open_memory();
        let (recorder, events) = recorder();
        register(handle, &recorder);
        for operation_id in 0..400 {
            consume(ohand_core_start_store_check(handle, operation_id));
        }
        events.recv_timeout(WAIT).unwrap();
        assert_eq!(
            consume(ohand_core_cancel(handle)).status,
            OHAND_CORE_STATUS_OK
        );
        while events.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            events.try_recv().is_err(),
            "a callback ran after cancel returned"
        );
        consume(ohand_core_close(handle));
    }
}

struct Gate {
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

unsafe extern "C" fn block_in_callback(
    context: *mut c_void,
    _operation_id: u64,
    _status: u32,
    _data: *const u8,
    _len: usize,
) {
    let gate = &*(context as *const Gate);
    gate.entered.send(()).unwrap();
    gate.release.lock().unwrap().recv_timeout(WAIT).unwrap();
}

#[test]
fn cancel_waits_for_a_running_callback_and_close_stops_the_worker() {
    let _guard = serial();
    let handle = open_memory();
    let (entered_sender, entered) = channel();
    let (release_sender, release_receiver) = channel();
    let gate = Gate {
        entered: entered_sender,
        release: Mutex::new(release_receiver),
    };
    let register_outcome = consume(ohand_core_set_event_callback(
        handle,
        Some(block_in_callback),
        &gate as *const Gate as *mut c_void,
    ));
    assert_eq!(register_outcome.status, OHAND_CORE_STATUS_OK);
    consume(ohand_core_start_store_check(handle, 1));
    consume(ohand_core_start_store_check(handle, 2));
    entered.recv_timeout(WAIT).unwrap();

    let (done_sender, done) = channel();
    let canceller = std::thread::spawn(move || {
        let outcome = consume(ohand_core_cancel(handle));
        done_sender.send(outcome.status).unwrap();
    });
    assert!(
        done.recv_timeout(SETTLE).is_err(),
        "cancel returned while a callback was still running"
    );
    release_sender.send(()).unwrap();
    assert_eq!(done.recv_timeout(WAIT).unwrap(), OHAND_CORE_STATUS_OK);
    canceller.join().unwrap();
    assert!(
        entered.recv_timeout(SETTLE).is_err(),
        "the queued second event was delivered after cancel"
    );
    let handles_before_close = ohand_core_live_handles();
    assert_eq!(
        consume(ohand_core_close(handle)).status,
        OHAND_CORE_STATUS_OK
    );
    assert_eq!(ohand_core_live_handles(), handles_before_close - 1);
}

#[test]
fn replacing_the_callback_waits_for_a_running_callback() {
    let _guard = serial();
    let handle = open_memory();
    let (entered_sender, entered) = channel();
    let (release_sender, release_receiver) = channel();
    let gate = Gate {
        entered: entered_sender,
        release: Mutex::new(release_receiver),
    };
    consume(ohand_core_set_event_callback(
        handle,
        Some(block_in_callback),
        &gate as *const Gate as *mut c_void,
    ));
    consume(ohand_core_start_store_check(handle, 1));
    entered.recv_timeout(WAIT).unwrap();
    let (done_sender, done) = channel();
    let clearer = std::thread::spawn(move || {
        let outcome = consume(ohand_core_set_event_callback(
            handle,
            None,
            std::ptr::null_mut(),
        ));
        done_sender.send(outcome.status).unwrap();
    });
    assert!(done.recv_timeout(SETTLE).is_err());
    release_sender.send(()).unwrap();
    assert_eq!(done.recv_timeout(WAIT).unwrap(), OHAND_CORE_STATUS_OK);
    clearer.join().unwrap();
    consume(ohand_core_close(handle));
}

struct Reentrant {
    handle: OhandCoreHandle,
    outcomes: Mutex<Vec<(String, u32, Option<String>)>>,
    finished: Sender<()>,
}

unsafe extern "C" fn call_back_into_core(
    context: *mut c_void,
    _operation_id: u64,
    _status: u32,
    _data: *const u8,
    _len: usize,
) {
    let reentrant = &*(context as *const Reentrant);
    let mut outcomes = reentrant.outcomes.lock().unwrap();
    let closed = consume(ohand_core_close(reentrant.handle));
    outcomes.push((
        "close".to_string(),
        closed.status,
        (closed.status != 0).then(|| closed.code()),
    ));
    let replaced = consume(ohand_core_set_event_callback(
        reentrant.handle,
        None,
        std::ptr::null_mut(),
    ));
    outcomes.push((
        "set_callback".to_string(),
        replaced.status,
        (replaced.status != 0).then(|| replaced.code()),
    ));
    let cancelled = consume(ohand_core_cancel(reentrant.handle));
    outcomes.push(("cancel".to_string(), cancelled.status, None));
    reentrant.finished.send(()).unwrap();
}

#[test]
fn close_and_replace_from_inside_a_callback_are_rejected_but_cancel_is_allowed() {
    let _guard = serial();
    let handle = open_memory();
    let (finished_sender, finished) = channel();
    let reentrant = Reentrant {
        handle,
        outcomes: Mutex::new(Vec::new()),
        finished: finished_sender,
    };
    consume(ohand_core_set_event_callback(
        handle,
        Some(call_back_into_core),
        &reentrant as *const Reentrant as *mut c_void,
    ));
    consume(ohand_core_start_store_check(handle, 1));
    finished.recv_timeout(WAIT).unwrap();
    {
        let outcomes = reentrant.outcomes.lock().unwrap();
        assert_eq!(
            *outcomes,
            vec![
                (
                    "close".to_string(),
                    OHAND_CORE_STATUS_PERMANENT,
                    Some("reentrant_call".to_string())
                ),
                (
                    "set_callback".to_string(),
                    OHAND_CORE_STATUS_PERMANENT,
                    Some("reentrant_call".to_string())
                ),
                ("cancel".to_string(), OHAND_CORE_STATUS_OK, None),
            ]
        );
    }
    assert_rejected(
        &consume(ohand_core_start_store_check(handle, 2)),
        OHAND_CORE_STATUS_CANCELLED,
        "cancelled",
    );
    assert_eq!(
        consume(ohand_core_close(handle)).status,
        OHAND_CORE_STATUS_OK
    );
}

#[test]
fn a_panicking_operation_becomes_an_internal_failure_and_the_worker_survives() {
    let _guard = serial();
    let handle = open_memory();
    let (recorder, events) = recorder();
    register(handle, &recorder);
    let instance = instance::lookup(handle).unwrap();
    let panicking: JobFn = Box::new(|_| panic!("intentional test panic"));
    instance.submit(7, panicking).unwrap();
    consume(ohand_core_start_store_check(handle, 8));
    let failed = events.recv_timeout(WAIT).unwrap();
    assert_eq!(failed.operation_id, 7);
    assert_eq!(failed.status, OHAND_CORE_STATUS_PERMANENT);
    let value: serde_json::Value = serde_json::from_slice(&failed.payload).unwrap();
    assert_eq!(value["code"], "internal");
    let survived = events.recv_timeout(WAIT).unwrap();
    assert_eq!(
        (survived.operation_id, survived.status),
        (8, OHAND_CORE_STATUS_OK)
    );
    consume(ohand_core_close(handle));
}

#[test]
fn a_full_queue_is_reported_as_transient_busy() {
    let _guard = serial();
    let handle = open_memory();
    let instance = instance::lookup(handle).unwrap();
    let (started_sender, started) = channel();
    let (release_sender, release_receiver) = channel::<()>();
    let blocker: JobFn = Box::new(move |_| {
        started_sender.send(()).unwrap();
        release_receiver.recv_timeout(WAIT).unwrap();
        Ok(Vec::new())
    });
    instance.submit(0, blocker).unwrap();
    started.recv_timeout(WAIT).unwrap();
    for operation_id in 1..=instance::QUEUE_CAPACITY as u64 {
        assert_eq!(
            consume(ohand_core_start_store_check(handle, operation_id)).status,
            OHAND_CORE_STATUS_OK
        );
    }
    let busy = consume(ohand_core_start_store_check(handle, u64::MAX));
    assert_rejected(&busy, OHAND_CORE_STATUS_TRANSIENT, "busy");
    assert_eq!(busy.class(), "transient");
    release_sender.send(()).unwrap();
    assert_eq!(
        consume(ohand_core_close(handle)).status,
        OHAND_CORE_STATUS_OK
    );
}

#[test]
fn concurrent_cancel_close_and_submission_never_deadlock_or_deliver_after_close() {
    let _guard = serial();
    let handles_before = ohand_core_live_handles();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..8 {
                    let handle = open_memory();
                    let (recorder, events) = recorder();
                    register(handle, &recorder);
                    for operation_id in 0..40 {
                        consume(ohand_core_start_store_check(handle, operation_id));
                    }
                    let contender = std::thread::spawn(move || {
                        consume(ohand_core_cancel(handle));
                        consume(ohand_core_close(handle)).status
                    });
                    let closed = consume(ohand_core_close(handle)).status;
                    let contender_status = contender.join().unwrap();
                    assert!(
                        (closed == OHAND_CORE_STATUS_OK)
                            != (contender_status == OHAND_CORE_STATUS_OK),
                        "exactly one close succeeds"
                    );
                    while events.try_recv().is_ok() {}
                    std::thread::sleep(Duration::from_millis(2));
                    assert!(events.try_recv().is_err());
                    drop(recorder);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(ohand_core_live_handles(), handles_before);
}

#[test]
fn provider_failures_keep_their_typed_class_and_fixed_message() {
    let kinds = [
        FailureKind::Timeout,
        FailureKind::Cancelled,
        FailureKind::Unavailable,
        FailureKind::RateLimited,
        FailureKind::Unauthorized,
        FailureKind::InvalidOutput,
        FailureKind::OutputTooLarge,
        FailureKind::InputTooLarge,
        FailureKind::CapabilityUnavailable,
        FailureKind::ProfileMismatch,
        FailureKind::Rejected,
    ];
    let mut codes = std::collections::BTreeSet::new();
    for kind in kinds {
        let provider_failure = ProviderFailure::new(kind);
        let abi_failure = AbiFailure::from_provider(&provider_failure);
        assert_eq!(abi_failure.class, kind.class());
        assert_eq!(abi_failure.status(), status_for(kind.class()));
        assert_eq!(abi_failure.message, provider_failure.message);
        assert!(codes.insert(abi_failure.code), "codes are unique per kind");
        let json: serde_json::Value = serde_json::from_slice(&abi_failure.to_json()).unwrap();
        assert_eq!(json["code"], abi_failure.code);
    }
    assert_eq!(
        status_for(ErrorClass::Transient),
        OHAND_CORE_STATUS_TRANSIENT
    );
    assert_eq!(
        status_for(ErrorClass::Permanent),
        OHAND_CORE_STATUS_PERMANENT
    );
    assert_eq!(
        status_for(ErrorClass::Unauthorized),
        OHAND_CORE_STATUS_UNAUTHORIZED
    );
    assert_eq!(
        status_for(ErrorClass::Cancelled),
        OHAND_CORE_STATUS_CANCELLED
    );
    assert_eq!(
        status_for(ErrorClass::Unsupported),
        OHAND_CORE_STATUS_UNSUPPORTED
    );
}

#[test]
fn store_failures_are_classified_by_sqlite_code_and_never_by_message_text() {
    use rusqlite::ffi::{Error as SqliteError, ErrorCode};
    let sqlite_failure = |code: ErrorCode, extended: i32| {
        rusqlite::Error::SqliteFailure(
            SqliteError {
                code,
                extended_code: extended,
            },
            Some("timeout permission denied network".to_string()),
        )
    };
    let busy = anyhow::Error::from(sqlite_failure(ErrorCode::DatabaseBusy, 5))
        .context("while saving a capture");
    let converted = AbiFailure::from_store(&busy, AbiFailure::STORAGE_ERROR);
    assert_eq!(converted, AbiFailure::STORE_BUSY);
    assert_eq!(converted.class, ErrorClass::Transient);

    let locked = anyhow::Error::from(sqlite_failure(ErrorCode::DatabaseLocked, 6));
    assert_eq!(
        AbiFailure::from_store(&locked, AbiFailure::STORAGE_ERROR),
        AbiFailure::STORE_BUSY
    );
    let cannot_open = anyhow::Error::from(sqlite_failure(ErrorCode::CannotOpen, 14));
    assert_eq!(
        AbiFailure::from_store(&cannot_open, AbiFailure::STORAGE_ERROR),
        AbiFailure::STORE_UNAVAILABLE
    );
    let corrupt = anyhow::Error::from(sqlite_failure(ErrorCode::DatabaseCorrupt, 11));
    assert_eq!(
        AbiFailure::from_store(&corrupt, AbiFailure::STORAGE_ERROR),
        AbiFailure::STORAGE_ERROR
    );
    let untyped = anyhow::anyhow!("Network timeout: permission denied");
    assert_eq!(
        AbiFailure::from_store(&untyped, AbiFailure::STORAGE_ERROR),
        AbiFailure::STORAGE_ERROR
    );
}

struct CrossCancel {
    peer: OhandCoreHandle,
    barrier: std::sync::Arc<std::sync::Barrier>,
    outcome: Mutex<Option<(u32, String)>>,
    finished: Sender<()>,
}

unsafe extern "C" fn cancel_the_peer(
    context: *mut c_void,
    _operation_id: u64,
    _status: u32,
    _data: *const u8,
    _len: usize,
) {
    let cross = &*(context as *const CrossCancel);
    cross.barrier.wait();
    let cancelled = consume(ohand_core_cancel(cross.peer));
    let code = if cancelled.status == OHAND_CORE_STATUS_OK {
        String::new()
    } else {
        cancelled.code()
    };
    *cross.outcome.lock().unwrap() = Some((cancelled.status, code));
    cross.finished.send(()).unwrap();
}

#[test]
fn callbacks_on_two_handles_cannot_deadlock_by_cancelling_each_other() {
    let _guard = serial();
    let first = open_memory();
    let second = open_memory();
    let (finished_sender, finished) = channel();
    // Both callbacks meet at this barrier, so each holds its delivery lock before either
    // tries to cancel the other.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_cross = Box::new(CrossCancel {
        peer: second,
        barrier: std::sync::Arc::clone(&barrier),
        outcome: Mutex::new(None),
        finished: finished_sender.clone(),
    });
    let second_cross = Box::new(CrossCancel {
        peer: first,
        barrier,
        outcome: Mutex::new(None),
        finished: finished_sender,
    });
    consume(ohand_core_set_event_callback(
        first,
        Some(cancel_the_peer),
        &*first_cross as *const CrossCancel as *mut c_void,
    ));
    consume(ohand_core_set_event_callback(
        second,
        Some(cancel_the_peer),
        &*second_cross as *const CrossCancel as *mut c_void,
    ));
    consume(ohand_core_start_store_check(first, 1));
    consume(ohand_core_start_store_check(second, 1));
    finished
        .recv_timeout(WAIT)
        .expect("cross-handle cancel deadlocked");
    finished
        .recv_timeout(WAIT)
        .expect("cross-handle cancel deadlocked");
    for cross in [&first_cross, &second_cross] {
        assert_eq!(
            *cross.outcome.lock().unwrap(),
            Some((OHAND_CORE_STATUS_PERMANENT, "reentrant_call".to_string()))
        );
    }
    assert_eq!(
        consume(ohand_core_close(first)).status,
        OHAND_CORE_STATUS_OK
    );
    assert_eq!(
        consume(ohand_core_close(second)).status,
        OHAND_CORE_STATUS_OK
    );
}

#[test]
fn a_provider_failure_with_inconsistent_or_arbitrary_fields_is_normalized_by_kind() {
    let malformed = ProviderFailure {
        kind: FailureKind::Timeout,
        class: ErrorClass::Unauthorized,
        retriable: false,
        message: "synthetic-sensitive-marker".to_string(),
    };
    let abi_failure = AbiFailure::from_provider(&malformed);
    assert_eq!(abi_failure.class, ErrorClass::Transient);
    assert_eq!(abi_failure.status(), OHAND_CORE_STATUS_TRANSIENT);
    assert_eq!(abi_failure.code, "timeout");
    assert_eq!(
        abi_failure.message,
        ProviderFailure::new(FailureKind::Timeout).message
    );
    let json = String::from_utf8(abi_failure.to_json()).unwrap();
    assert!(!json.contains("synthetic-sensitive-marker"));

    let deserialized: ProviderFailure = serde_json::from_str(
        r#"{"kind":"rejected","class":"transient","retriable":true,"message":"synthetic-sensitive-marker"}"#,
    )
    .unwrap();
    let abi_failure = AbiFailure::from_provider(&deserialized);
    assert_eq!(abi_failure.class, ErrorClass::Permanent);
    assert_eq!(abi_failure.code, "rejected");
    assert!(!String::from_utf8(abi_failure.to_json())
        .unwrap()
        .contains("synthetic-sensitive-marker"));
}
