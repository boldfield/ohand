//! Exercises the exported C ABI exactly as the Swift probe does, plus exact-checkpoint
//! cancellation through the internal hook that the C ABI cannot express.

use ohand_bindings::probe::{
    save_capture_json, CancelToken, Checkpoint, ErrorClass, Failure, ProbeStore,
};
use ohand_bindings::*;
use serde_json::{json, Value};
use std::sync::Mutex;

// The live-allocation counter is process-wide, so tests that assert on it run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn capture_json(capture_id: &str, text: &str) -> Value {
    json!({
        "capture_id": capture_id,
        "text": text,
        "capture_instant": "2026-03-01T09:30:00Z",
        "timezone_id": "Europe/Berlin",
        "utc_offset_minutes": 60,
        "locale": "de_DE",
        "calendar": "gregorian",
        "item_scope": "personal",
        "route_id": "route-default",
        "entry_locked": false,
        "created_at": "2026-03-01T09:30:00Z"
    })
}

struct Outcome {
    status: u32,
    body: Value,
}

unsafe fn take(mut result: OhandResult) -> Outcome {
    let bytes = std::slice::from_raw_parts(result.data, result.len).to_vec();
    ohand_result_free(&mut result);
    assert!(
        result.data.is_null() && result.len == 0,
        "free clears the result"
    );
    ohand_result_free(&mut result); // second free is a no-op
    Outcome {
        status: result.status,
        body: serde_json::from_slice(&bytes).expect("response is JSON"),
    }
}

unsafe fn save_bytes(
    store: *const OhandProbeStore,
    token: *const OhandCancelToken,
    bytes: &[u8],
) -> Outcome {
    take(ohand_probe_save_capture(
        store,
        token,
        bytes.as_ptr(),
        bytes.len(),
    ))
}

unsafe fn save(
    store: *const OhandProbeStore,
    token: *const OhandCancelToken,
    request: &Value,
) -> Outcome {
    save_bytes(store, token, request.to_string().as_bytes())
}

unsafe fn get(store: *const OhandProbeStore, capture_id: &str) -> Outcome {
    take(ohand_probe_get_capture(
        store,
        std::ptr::null(),
        capture_id.as_ptr(),
        capture_id.len(),
    ))
}

#[test]
fn round_trips_capture_shaped_value_and_unicode() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        assert!(!store.is_null());
        let text = "Café ☕ 日本語 \u{202E}rtl\u{202C} 👩‍👩‍👧 e\u{301} nul:\u{0} end";
        let saved = save(store, std::ptr::null(), &capture_json("cap-1", text));
        assert_eq!(saved.status, OHAND_STATUS_OK);
        assert_eq!(saved.body["capture"]["text"], text);
        assert_eq!(saved.body["idempotent_replay"], false);
        let fetched = get(store, "cap-1");
        assert_eq!(fetched.status, OHAND_STATUS_OK);
        assert_eq!(fetched.body["text"], text);
        assert_eq!(fetched.body["timezone_id"], "Europe/Berlin");
        ohand_probe_store_free(store);
    }
}

#[test]
fn identical_retry_is_idempotent_and_conflicting_reuse_fails_without_replacing_source() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        let original = capture_json("cap-1", "original words");
        assert_eq!(
            save(store, std::ptr::null(), &original).status,
            OHAND_STATUS_OK
        );
        let replay = save(store, std::ptr::null(), &original);
        assert_eq!(replay.status, OHAND_STATUS_OK);
        assert_eq!(replay.body["idempotent_replay"], true);
        let conflict = save(
            store,
            std::ptr::null(),
            &capture_json("cap-1", "different words"),
        );
        assert_eq!(conflict.status, OHAND_STATUS_PERMANENT);
        assert_eq!(conflict.body["code"], "capture_conflict");
        assert_eq!(get(store, "cap-1").body["text"], "original words");
        ohand_probe_store_free(store);
    }
}

#[test]
fn malformed_inputs_return_normalized_failures() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        let code_of = |outcome: Outcome| {
            assert_eq!(outcome.status, OHAND_STATUS_PERMANENT);
            assert_eq!(outcome.body["class"], "permanent");
            outcome.body["code"].as_str().unwrap().to_string()
        };

        assert_eq!(
            code_of(save_bytes(store, std::ptr::null(), &[0xFF, 0xFE, b'{'])),
            "invalid_utf8"
        );
        assert_eq!(
            code_of(save_bytes(store, std::ptr::null(), b"not json")),
            "invalid_request"
        );
        assert_eq!(
            code_of(save_bytes(store, std::ptr::null(), b"")),
            "invalid_request"
        );

        let mut missing_field = capture_json("cap-1", "x");
        missing_field.as_object_mut().unwrap().remove("timezone_id");
        assert_eq!(
            code_of(save(store, std::ptr::null(), &missing_field)),
            "invalid_request"
        );

        let mut unknown_field = capture_json("cap-1", "x");
        unknown_field["surprise"] = json!(1);
        assert_eq!(
            code_of(save(store, std::ptr::null(), &unknown_field)),
            "invalid_request"
        );

        let mut no_source = capture_json("cap-1", "x");
        no_source.as_object_mut().unwrap().remove("text");
        assert_eq!(
            code_of(save(store, std::ptr::null(), &no_source)),
            "invalid_request"
        );

        assert_eq!(
            code_of(save(store, std::ptr::null(), &capture_json("", "x"))),
            "invalid_request"
        );

        let null_request = take(ohand_probe_save_capture(
            store,
            std::ptr::null(),
            std::ptr::null(),
            5,
        ));
        assert_eq!(code_of(null_request), "null_argument");
        let null_store = take(ohand_probe_save_capture(
            std::ptr::null(),
            std::ptr::null(),
            b"{}".as_ptr(),
            2,
        ));
        assert_eq!(code_of(null_store), "null_argument");

        assert_eq!(code_of(get(store, "missing")), "not_found");
        let invalid_id = take(ohand_probe_get_capture(
            store,
            std::ptr::null(),
            [0xC0u8, 0x80].as_ptr(),
            2,
        ));
        assert_eq!(code_of(invalid_id), "invalid_utf8");

        // Failure payloads never echo caller content.
        let secret_text = "do-not-echo-this-private-phrase";
        let rejected = save_bytes(
            store,
            std::ptr::null(),
            format!("{{\"text\":\"{secret_text}\"}}").as_bytes(),
        );
        assert!(!rejected.body.to_string().contains(secret_text));
        ohand_probe_store_free(store);
    }
}

#[test]
fn size_bound_is_enforced_exactly() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        let limit = ohand_bindings_max_request_bytes();
        assert_eq!(limit, 1_048_576);

        let overhead = capture_json("cap-limit", "").to_string().len();
        let text = "a".repeat(limit - overhead);
        let at_limit = capture_json("cap-limit", &text).to_string();
        assert_eq!(at_limit.len(), limit);
        let accepted = save_bytes(store, std::ptr::null(), at_limit.as_bytes());
        assert_eq!(accepted.status, OHAND_STATUS_OK);
        assert_eq!(
            accepted.body["capture"]["text"].as_str().unwrap().len(),
            text.len()
        );

        let over_limit = capture_json("cap-overx", &format!("{text}a")).to_string();
        assert_eq!(over_limit.len(), limit + 1);
        let rejected = save_bytes(store, std::ptr::null(), over_limit.as_bytes());
        assert_eq!(rejected.status, OHAND_STATUS_PERMANENT);
        assert_eq!(rejected.body["code"], "request_too_large");
        assert_eq!(get(store, "cap-overx").body["code"], "not_found");

        // Length alone decides: a bogus pointer with an oversized length is never dereferenced.
        let bogus = take(ohand_probe_save_capture(
            store,
            std::ptr::null(),
            std::ptr::dangling::<u8>(),
            limit + 1,
        ));
        assert_eq!(bogus.body["code"], "request_too_large");

        // The bound counts UTF-8 bytes, not characters.
        let multibyte = "é".repeat((limit - overhead) / 2 + 1);
        let multibyte_request = capture_json("cap-multibyte", &multibyte).to_string();
        assert!(multibyte_request.len() > limit);
        assert_eq!(
            save_bytes(store, std::ptr::null(), multibyte_request.as_bytes()).body["code"],
            "request_too_large"
        );
        ohand_probe_store_free(store);
    }
}

#[test]
fn pre_cancelled_call_changes_nothing_and_late_cancel_changes_nothing() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        let token = ohand_cancel_token_new();
        ohand_cancel_token_cancel(token);
        let cancelled = save(store, token, &capture_json("cap-1", "x"));
        assert_eq!(cancelled.status, OHAND_STATUS_CANCELLED);
        assert_eq!(cancelled.body["class"], "cancelled");
        assert_eq!(get(store, "cap-1").body["code"], "not_found");
        ohand_cancel_token_free(token);

        let late_token = ohand_cancel_token_new();
        assert_eq!(
            save(store, late_token, &capture_json("cap-2", "kept")).status,
            OHAND_STATUS_OK
        );
        ohand_cancel_token_cancel(late_token);
        ohand_cancel_token_cancel(late_token);
        assert_eq!(get(store, "cap-2").body["text"], "kept");
        ohand_cancel_token_free(late_token);
        ohand_probe_store_free(store);
    }
}

#[test]
fn cancellation_at_each_checkpoint_rolls_back_without_mutation() {
    let store = ProbeStore::open_in_memory().unwrap();
    let request = capture_json("cap-1", "text").to_string();

    for checkpoint in [Checkpoint::AfterValidation, Checkpoint::BeforeCommit] {
        let token = CancelToken::default();
        let failure = save_capture_json(&store, Some(&token), &request, &|reached| {
            if reached == checkpoint {
                token.cancel();
            }
        })
        .unwrap_err();
        assert_eq!(failure, Failure::CANCELLED);
        assert_eq!(failure.class, ErrorClass::Cancelled);
        let lookup = ohand_bindings::probe::get_capture_json(&store, None, "cap-1");
        assert_eq!(
            lookup.unwrap_err(),
            Failure::NOT_FOUND,
            "cancel at {checkpoint:?} left a record"
        );
    }

    let token = CancelToken::default();
    save_capture_json(&store, Some(&token), &request, &|_| {}).expect("uncancelled call commits");
    assert!(ohand_bindings::probe::get_capture_json(&store, None, "cap-1").is_ok());
}

#[test]
fn concurrent_cancellation_is_all_or_nothing() {
    let _guard = serial();
    unsafe {
        let store = ohand_probe_store_open_in_memory();
        let store_address = store as usize;
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                std::thread::spawn(move || {
                    let store = store_address as *const OhandProbeStore;
                    let mut committed = Vec::new();
                    for index in 0..40 {
                        let capture_id = format!("cap-{worker}-{index}");
                        let token = ohand_cancel_token_new();
                        let token_address = token as usize;
                        let canceller = std::thread::spawn(move || {
                            ohand_cancel_token_cancel(token_address as *const OhandCancelToken)
                        });
                        let outcome = save(store, token, &capture_json(&capture_id, "text"));
                        canceller.join().unwrap();
                        ohand_cancel_token_free(token);
                        match outcome.status {
                            OHAND_STATUS_OK => committed.push((capture_id, true)),
                            OHAND_STATUS_CANCELLED => committed.push((capture_id, false)),
                            other => panic!("unexpected status {other}"),
                        }
                    }
                    committed
                })
            })
            .collect();
        for worker in workers {
            for (capture_id, expected_stored) in worker.join().unwrap() {
                let stored = get(store, &capture_id).status == OHAND_STATUS_OK;
                assert_eq!(
                    stored, expected_stored,
                    "{capture_id}: result disagrees with store"
                );
            }
        }
        ohand_probe_store_free(store);
    }
}

#[test]
fn ownership_is_released_on_every_path() {
    let _guard = serial();
    unsafe {
        let baseline = ohand_bindings_live_allocations();
        let store = ohand_probe_store_open_in_memory();
        let token = ohand_cancel_token_new();
        assert_eq!(ohand_bindings_live_allocations(), baseline + 2);

        let mut held = save_bytes_unfreed(store, &capture_json("cap-1", "x"));
        assert_eq!(held.status, OHAND_STATUS_OK);
        assert_eq!(
            ohand_bindings_live_allocations(),
            baseline + 3,
            "result buffer is live until freed"
        );
        ohand_result_free(&mut held);
        assert_eq!(ohand_bindings_live_allocations(), baseline + 2);

        for index in 0..100 {
            save(store, token, &capture_json(&format!("cap-{index}"), "ok"));
            save_bytes(store, token, b"garbage");
            get(store, "missing");
            take(ohand_probe_trigger_panic());
        }
        ohand_cancel_token_cancel(token);
        save(store, token, &capture_json("cap-x", "cancelled"));

        ohand_probe_store_free(std::ptr::null_mut());
        ohand_cancel_token_free(std::ptr::null_mut());
        ohand_cancel_token_cancel(std::ptr::null());
        ohand_result_free(std::ptr::null_mut());

        ohand_cancel_token_free(token);
        ohand_probe_store_free(store);
        assert_eq!(
            ohand_bindings_live_allocations(),
            baseline,
            "every object was released"
        );
    }
}

unsafe fn save_bytes_unfreed(store: *const OhandProbeStore, request: &Value) -> OhandResult {
    let bytes = request.to_string();
    ohand_probe_save_capture(store, std::ptr::null(), bytes.as_ptr(), bytes.len())
}

#[test]
fn stores_are_isolated_from_each_other() {
    let _guard = serial();
    unsafe {
        let first = ohand_probe_store_open_in_memory();
        let second = ohand_probe_store_open_in_memory();
        save(
            first,
            std::ptr::null(),
            &capture_json("cap-1", "only in first"),
        );
        assert_eq!(get(second, "cap-1").body["code"], "not_found");
        ohand_probe_store_free(first);
        ohand_probe_store_free(second);
    }
}

#[test]
fn panics_are_contained_as_internal_failures() {
    let _guard = serial();
    unsafe {
        let outcome = take(ohand_probe_trigger_panic());
        assert_eq!(outcome.status, OHAND_STATUS_PERMANENT);
        assert_eq!(outcome.body["code"], "internal");
        assert_eq!(ohand_bindings_abi_version(), OHAND_BINDINGS_ABI_VERSION);
    }
}
