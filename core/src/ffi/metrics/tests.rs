//! Drives metrics the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, open_memory, recorder, register, serial, Event, Recorder, WAIT,
};

struct Session {
    handle: OhandCoreHandle,
    events: std::sync::mpsc::Receiver<Event>,
    _recorder: Box<Recorder>,
    next_operation: u64,
}

fn start(
    handle: OhandCoreHandle,
    operation_id: u64,
    request: &Value,
) -> crate::ffi::core_handle::tests::Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe { ohand_core_start_metrics(handle, operation_id, bytes.as_ptr(), bytes.len()) })
}

impl Session {
    fn open() -> Session {
        let handle = open_memory();
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
            next_operation: 100,
        }
    }

    fn event(&mut self, request: &Value) -> Event {
        let operation_id = self.next_operation;
        self.next_operation += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn succeed(&mut self, request: &Value) -> Value {
        let event = self.event(request);
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

    fn fail(&mut self, request: &Value) -> (u32, String) {
        let event = self.event(request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        let value: Value = serde_json::from_slice(&event.payload).unwrap();
        (event.status, value["code"].as_str().unwrap().to_string())
    }

    fn close(self) {
        let cleared = consume(ohand_core_set_event_callback(
            self.handle,
            None,
            std::ptr::null_mut(),
        ));
        assert_eq!(cleared.status, OHAND_CORE_STATUS_OK);
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn export(session: &mut Session) -> Value {
    session.succeed(&json!({"op": "export", "read_authentication": "authenticated"}))
}

#[test]
fn recorded_samples_are_exported_as_a_content_free_report() {
    let _guard = serial();
    let mut session = Session::open();
    let recorded = session.succeed(&json!({
        "op": "record_timing", "series": "trigger_to_ready", "dimension": "cold", "duration_ms": 420
    }));
    assert_eq!(recorded["outcome"], "recorded");
    session.succeed(&json!({
        "op": "record_timing", "series": "end_of_input_to_save", "dimension": "voice", "duration_ms": 80
    }));
    session.succeed(&json!({
        "op": "record_failure", "series": "scheduling_failure", "dimension": "permission_denied"
    }));

    let exported = export(&mut session);
    assert_eq!(exported["outcome"], "report");
    let report = &exported["report"];
    assert_eq!(report["schema"], "ohand.metrics.v1");
    assert_eq!(report["recording_enabled"], true);
    let timings = report["timings"].as_array().unwrap();
    assert_eq!(timings.len(), 2);
    assert_eq!(timings[0]["series"], "trigger_to_ready");
    assert_eq!(timings[0]["median_ms"], 420);
    assert_eq!(timings[1]["series"], "end_of_input_to_save");
    assert_eq!(timings[1]["median_ms"], 80);
    assert_eq!(report["failures"][0]["dimension"], "permission_denied");
    assert_eq!(report["failures"][0]["occurrences"], 1);
    session.close();
}

#[test]
fn capture_only_export_enable_and_clear_are_refused_and_recording_needs_no_authentication() {
    let _guard = serial();
    let mut session = Session::open();
    let recorded = session.succeed(&json!({
        "op": "record_failure", "series": "processing_failure", "dimension": "timeout"
    }));
    assert_eq!(recorded["outcome"], "recorded");
    let (status, code) =
        session.fail(&json!({"op": "export", "read_authentication": "capture_only"}));
    assert_eq!(status, OHAND_CORE_STATUS_UNAUTHORIZED);
    assert_eq!(code, "metrics_not_authorized");
    for request in [
        json!({"op": "set_enabled", "enabled": false, "read_authentication": "capture_only"}),
        json!({"op": "set_enabled", "enabled": true, "read_authentication": "capture_only"}),
        json!({"op": "clear", "read_authentication": "capture_only"}),
    ] {
        let (status, code) = session.fail(&request);
        assert_eq!(status, OHAND_CORE_STATUS_UNAUTHORIZED, "{request}");
        assert_eq!(code, "metrics_not_authorized", "{request}");
    }
    let report = export(&mut session);
    assert_eq!(report["report"]["recording_enabled"], true);
    assert_eq!(report["report"]["failures"][0]["occurrences"], 1);
    session.close();
}

#[test]
fn disabling_stops_recording_and_clearing_removes_what_was_recorded() {
    let _guard = serial();
    let mut session = Session::open();
    let timing = json!({
        "op": "record_timing", "series": "trigger_to_ready", "dimension": "warm", "duration_ms": 10
    });
    session.succeed(&timing);
    assert_eq!(
        session.succeed(
            &json!({"op": "set_enabled", "enabled": false, "read_authentication": "authenticated"})
        )["outcome"],
        "ok"
    );
    assert_eq!(session.succeed(&timing)["outcome"], "disabled");
    let report = export(&mut session);
    assert_eq!(report["report"]["recording_enabled"], false);
    assert_eq!(report["report"]["timings"][0]["retained_samples"], 1);

    assert_eq!(
        session.succeed(&json!({"op": "clear", "read_authentication": "authenticated"}))["outcome"],
        "ok"
    );
    let report = export(&mut session);
    assert!(report["report"]["timings"].as_array().unwrap().is_empty());
    assert_eq!(report["report"]["recording_enabled"], false);
    session.close();
}

#[test]
fn requests_outside_the_closed_vocabulary_are_refused_before_queueing() {
    let _guard = serial();
    let handle = open_memory();
    let rejected = |request: Value| {
        let outcome = start(handle, 1, &request);
        assert_ne!(outcome.status, OHAND_CORE_STATUS_OK, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    };
    rejected(
        json!({"op": "record_timing", "series": "trigger_to_ready", "dimension": "text", "duration_ms": 1}),
    );
    rejected(
        json!({"op": "record_timing", "series": "what I said", "dimension": "cold", "duration_ms": 1}),
    );
    rejected(
        json!({"op": "record_timing", "series": "trigger_to_ready", "dimension": "cold", "duration_ms": 1, "note": "free text"}),
    );
    rejected(json!({"op": "record_timing", "series": "trigger_to_ready", "dimension": "cold"}));
    rejected(
        json!({"op": "record_timing", "series": "trigger_to_ready", "dimension": "cold", "duration_ms": -1}),
    );
    rejected(
        json!({"op": "record_failure", "series": "processing_failure", "dimension": "capacity_exceeded"}),
    );
    rejected(
        json!({"op": "record_failure", "series": "processing_failure", "dimension": "timeout", "detail": "x"}),
    );
    rejected(json!({"op": "export", "read_authentication": "maybe"}));
    rejected(json!({"op": "set_enabled", "enabled": true}));
    rejected(json!({"op": "clear"}));
    rejected(json!({"op": "clear", "read_authentication": "maybe"}));
    rejected(json!({"op": "send_somewhere", "url": "example"}));
    let bytes: &[u8] = b"not json";
    let outcome =
        consume(unsafe { ohand_core_start_metrics(handle, 1, bytes.as_ptr(), bytes.len()) });
    assert_eq!(outcome.code(), "invalid_request");
    let outcome = consume(unsafe { ohand_core_start_metrics(handle, 1, std::ptr::null(), 0) });
    assert_eq!(outcome.code(), "invalid_request");
    let outcome = consume(unsafe { ohand_core_start_metrics(handle, 1, std::ptr::null(), 4) });
    assert_eq!(outcome.code(), "null_argument");
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}

#[test]
fn an_implausible_duration_is_refused_by_the_worker_and_stores_nothing() {
    let _guard = serial();
    let mut session = Session::open();
    let (_, code) = session.fail(&json!({
        "op": "record_timing", "series": "trigger_to_ready", "dimension": "cold",
        "duration_ms": crate::metrics::MAX_DURATION_MS + 1
    }));
    assert_eq!(code, "invalid_request");
    let report = export(&mut session);
    assert!(report["report"]["timings"].as_array().unwrap().is_empty());
    session.close();
}

#[test]
fn an_export_names_the_build_the_app_supplies() {
    let _guard = serial();
    let mut session = Session::open();
    let revision = "0123456789abcdef0123456789abcdef01234567";
    let exported = session.succeed(&json!({
        "op": "export", "read_authentication": "authenticated",
        "build": {"version": "0.1.0", "build_number": "412", "revision": revision, "dirty": true}
    }));
    assert_eq!(
        exported["report"]["build"],
        json!({"version": "0.1.0", "build_number": "412", "revision": revision, "dirty": true})
    );
    let unnamed = export(&mut session);
    assert!(unnamed["report"].get("build").is_none());
    session.close();

    let handle = open_memory();
    for build in [
        json!({"version": "a note", "build_number": "412", "revision": revision, "dirty": true}),
        json!({"version": "0.1.0", "build_number": "1", "revision": revision, "dirty": false, "note": "x"}),
    ] {
        let request =
            json!({"op": "export", "read_authentication": "authenticated", "build": build});
        let outcome = start(handle, 1, &request);
        assert_ne!(outcome.status, OHAND_CORE_STATUS_OK, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}
