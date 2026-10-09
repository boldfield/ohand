use ohand_tauri_handoff::{
    build_handoff_url, parse_handoff_url, receive_urls, CaptureId, HandoffError, HandoffInbox,
    HandoffRecord, ReceiveOutcome, RecordOutcome,
};
use serde::Deserialize;
use tempfile::TempDir;

const FIRST_ID: &str = "0F8FAD5B-D9CB-469F-A165-70867728950E";
const SECOND_ID: &str = "6BA7B810-9DAD-41D1-80B4-00C04FD430C8";

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    description: String,
    url: String,
    expect: String,
    #[serde(rename = "captureId")]
    capture_id: Option<String>,
}

fn handoff_url(capture_id: &str) -> String {
    format!("ohand-tauri://capture?captureId={capture_id}")
}

fn new_inbox() -> (TempDir, HandoffInbox) {
    let directory = TempDir::new().expect("temporary directory");
    let inbox = HandoffInbox::new(directory.path().join("handoffs"));
    (directory, inbox)
}

#[test]
fn shared_vectors_are_accepted_or_rejected_with_the_expected_code() {
    let vectors: Vectors =
        serde_json::from_str(include_str!("../fixtures/handoff-urls.json")).expect("vectors parse");
    assert!(vectors.cases.len() > 30, "vector file looks truncated");
    for case in vectors.cases {
        let outcome = parse_handoff_url(&case.url);
        if case.expect == "ok" {
            let capture_id = outcome.unwrap_or_else(|error| {
                panic!("{}: expected acceptance, got {error}", case.description)
            });
            assert_eq!(
                Some(capture_id.as_str().to_owned()),
                case.capture_id,
                "{}",
                case.description
            );
        } else {
            let error = outcome.expect_err(&case.description);
            assert_eq!(error.code(), case.expect, "{}", case.description);
        }
    }
}

/// Both iOS scene hooks (cold connect and warm open) pass the raw URL string straight to `receive_urls`, so running
/// every shared vector through it, in one delivery, is the receiver behaviour on both paths. A normalised parser
/// (scheme lower-cased, tab and newline stripped) would accept the upper-case-scheme and trailing-newline vectors.
#[test]
fn every_shared_vector_gets_the_same_verdict_through_the_receiver() {
    let vectors: Vectors =
        serde_json::from_str(include_str!("../fixtures/handoff-urls.json")).expect("vectors parse");
    let normalisation_sensitive = [
        "OHAND-TAURI://capture?captureId=0F8FAD5B-D9CB-469F-A165-70867728950E",
        "ohand-tauri://capture?captureId=0F8FAD5B-D9CB-469F-A165-70867728950E\n",
    ];
    for url in normalisation_sensitive {
        assert!(
            vectors
                .cases
                .iter()
                .any(|case| case.url == url && case.expect != "ok"),
            "fixture must hold the rejection vector {url:?}"
        );
    }
    for case in vectors.cases {
        let (_directory, inbox) = new_inbox();
        let outcome = receive_urls(&inbox, [case.url.as_str()], false, 1_000).remove(0);
        match (case.expect.as_str(), outcome) {
            ("ok", ReceiveOutcome::Recorded(capture_id)) => {
                assert_eq!(
                    Some(capture_id.as_str().to_owned()),
                    case.capture_id,
                    "{}",
                    case.description
                );
            }
            (expected, ReceiveOutcome::Rejected(reason)) => {
                assert_eq!(reason.code(), expected, "{}", case.description);
                assert!(
                    inbox.snapshot().unwrap().capture_ids.is_empty(),
                    "{}",
                    case.description
                );
            }
            (expected, outcome) => {
                panic!("{}: expected {expected}, got {outcome:?}", case.description)
            }
        }
    }
}

#[test]
fn built_urls_round_trip_to_the_same_identifier() {
    let capture_id: CaptureId = FIRST_ID.parse().expect("canonical id");
    let url = build_handoff_url(&capture_id);
    assert_eq!(url, handoff_url(FIRST_ID));
    assert_eq!(parse_handoff_url(&url).expect("round trip"), capture_id);
}

#[test]
fn cold_delivery_is_recorded_before_any_webview_exists() {
    let (_directory, inbox) = new_inbox();
    let outcomes = receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    assert!(
        matches!(outcomes.as_slice(), [ReceiveOutcome::Recorded(id)] if id.as_str() == FIRST_ID)
    );

    let records = inbox.records().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].capture_id, FIRST_ID);
    assert!(
        !records[0].webview_ready,
        "the record must show no webview was needed"
    );
}

#[test]
fn warm_delivery_keeps_earlier_records_and_adds_the_new_identifier() {
    let (_directory, inbox) = new_inbox();
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    receive_urls(&inbox, [handoff_url(SECOND_ID).as_str()], true, 2_000);

    let snapshot = inbox.snapshot().expect("snapshot");
    assert_eq!(snapshot.capture_ids, vec![FIRST_ID, SECOND_ID]);
    assert_eq!(snapshot.rejected_count, 0);
    let records = inbox.records().expect("records");
    assert!(!records[0].webview_ready);
    assert!(records[1].webview_ready);
}

#[test]
fn records_survive_a_process_restart() {
    let (directory, inbox) = new_inbox();
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    drop(inbox);

    let reopened = HandoffInbox::new(directory.path().join("handoffs"));
    assert_eq!(
        reopened.snapshot().expect("snapshot").capture_ids,
        vec![FIRST_ID]
    );
}

#[test]
fn replaying_an_identifier_keeps_the_first_record() {
    let (_directory, inbox) = new_inbox();
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    let outcomes = receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], true, 9_000);
    assert!(matches!(
        outcomes.as_slice(),
        [ReceiveOutcome::Duplicate(_)]
    ));

    let records = inbox.records().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].received_at_unix_ms, 1_000);
    assert!(!records[0].webview_ready);
}

#[test]
fn several_urls_in_one_delivery_are_each_handled() {
    let (_directory, inbox) = new_inbox();
    let first = handoff_url(FIRST_ID);
    let second = handoff_url(SECOND_ID);
    let outcomes = receive_urls(
        &inbox,
        [first.as_str(), "ohand-tauri://settings", second.as_str()],
        false,
        1_000,
    );
    assert_eq!(outcomes.len(), 3);
    assert!(matches!(
        outcomes[1],
        ReceiveOutcome::Rejected(HandoffError::UnknownRoute)
    ));
    assert_eq!(inbox.snapshot().expect("snapshot").capture_ids.len(), 2);
}

#[test]
fn rejected_urls_are_counted_by_reason_and_create_no_record() {
    let (_directory, inbox) = new_inbox();
    let hostile = [
        "ohand-tauri://capture/../../admin?captureId=0F8FAD5B-D9CB-469F-A165-70867728950E",
        "ohand-tauri://capture?captureId=<script>",
        "ohand-tauri://capture?private=true&captureId=0F8FAD5B-D9CB-469F-A165-70867728950E",
        "https://example.invalid/?captureId=0F8FAD5B-D9CB-469F-A165-70867728950E",
    ];
    let outcomes = receive_urls(&inbox, hostile, false, 1_000);
    assert!(outcomes
        .iter()
        .all(|outcome| matches!(outcome, ReceiveOutcome::Rejected(_))));

    let snapshot = inbox.snapshot().expect("snapshot");
    assert!(snapshot.capture_ids.is_empty());
    assert_eq!(snapshot.rejected_count, 4);

    let summary = std::fs::read_to_string(inbox.directory().join("rejections.json")).unwrap();
    assert!(summary.contains("bad_scheme"), "last reason is kept");
    assert!(!summary.contains("script"), "raw URL text is never stored");
}

#[test]
fn a_hostile_url_does_not_block_a_later_valid_handoff() {
    let (_directory, inbox) = new_inbox();
    receive_urls(
        &inbox,
        ["ohand-tauri://capture?captureId=evil"],
        false,
        1_000,
    );
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 2_000);
    let snapshot = inbox.snapshot().expect("snapshot");
    assert_eq!(snapshot.capture_ids, vec![FIRST_ID]);
    assert_eq!(snapshot.rejected_count, 1);
}

#[test]
fn storage_failure_is_reported_not_dropped() {
    let directory = TempDir::new().expect("temporary directory");
    let blocking_file = directory.path().join("handoffs");
    std::fs::write(&blocking_file, b"not a directory").expect("create blocker");
    let inbox = HandoffInbox::new(blocking_file);
    let outcomes = receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    assert!(matches!(outcomes.as_slice(), [ReceiveOutcome::Failed(_)]));
}

#[test]
fn the_web_ui_snapshot_carries_identifiers_and_a_count_only() {
    let (_directory, inbox) = new_inbox();
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    let json = serde_json::to_value(inbox.snapshot().expect("snapshot")).expect("serialise");
    let mut keys: Vec<&str> = json
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["captureIds", "rejectedCount"]);
}

#[test]
fn a_stored_record_holds_only_identifier_and_delivery_facts() {
    let (_directory, inbox) = new_inbox();
    let capture_id: CaptureId = FIRST_ID.parse().expect("canonical id");
    assert_eq!(
        inbox.record(&capture_id, 5, false).expect("record"),
        RecordOutcome::Recorded
    );
    let stored =
        std::fs::read_to_string(inbox.directory().join(format!("{FIRST_ID}.json"))).unwrap();
    let parsed: HandoffRecord = serde_json::from_str(&stored).expect("strict parse");
    assert_eq!(parsed.capture_id, FIRST_ID);
}

#[test]
fn non_record_files_in_the_inbox_are_ignored() {
    let (_directory, inbox) = new_inbox();
    receive_urls(&inbox, [handoff_url(FIRST_ID).as_str()], false, 1_000);
    std::fs::write(inbox.directory().join("notes.json"), b"{}").unwrap();
    std::fs::write(inbox.directory().join("..%2Fescape.json"), b"{}").unwrap();
    assert_eq!(
        inbox.snapshot().expect("snapshot").capture_ids,
        vec![FIRST_ID]
    );
}
