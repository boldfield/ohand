//! Process-level capture durability (T02a). Each test hard-kills (SIGKILL) a real child process
//! at a durable acknowledgment boundary of the actual foreground ingress import, reopens the
//! database in the parent as a restart would, and checks that acknowledged captures survive with
//! stable identity, that redelivery never duplicates, and that a sole uncommitted source (staged
//! record, incomplete write, audio file) is never deleted. All content is synthetic.

mod child;
mod harness;

use harness::*;
use ohand_core::ingress::{
    import_foreground_ingress, CommitStatus, ImportDisposition, IngressErrorKind, IngressRejection,
};
use std::fs;
use std::time::Duration;

struct Acknowledged {
    capture_id: String,
    item_id: String,
    saved_at: String,
}

fn parse_acknowledged(message: &str) -> Acknowledged {
    let mut parts = message.split(' ');
    assert_eq!(parts.next(), Some("acknowledged"));
    Acknowledged {
        capture_id: parts.next().unwrap().to_string(),
        item_id: parts.next().unwrap().to_string(),
        saved_at: parts.next().unwrap().to_string(),
    }
}

fn assert_one_consistent_row_set(workspace: &Workspace, expected_captures: i64) {
    for table in ["captures", "items", "search_index"] {
        assert_eq!(
            workspace.count(table),
            expected_captures,
            "{table} row count"
        );
    }
}

/// Audio imports have no text yet, so they create a capture and an item but no index row.
fn assert_audio_import_rows(workspace: &Workspace, expected_captures: i64) {
    assert_eq!(workspace.count("captures"), expected_captures);
    assert_eq!(workspace.count("items"), expected_captures);
    assert_eq!(workspace.count("search_index"), 0);
}

fn assert_acknowledgment_survives(workspace: &Workspace, acknowledged: &Acknowledged) {
    let database = workspace.open_database();
    assert_eq!(
        item_ids_for(&database, &acknowledged.capture_id),
        vec![acknowledged.item_id.clone()],
        "an acknowledged capture keeps exactly its acknowledged item identity"
    );
    let saved_at: String = database
        .conn()
        .query_row(
            "SELECT created_at FROM items WHERE item_id = ?",
            [acknowledged.item_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved_at, acknowledged.saved_at);
}

#[test]
fn kill_after_staging_before_import_leaves_the_record_and_it_imports_once_on_restart() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("stage_then_wait", workspace.root(), 1);
    let capture_id = child
        .expect_message_starting_with("staged ")
        .trim_start_matches("staged ")
        .to_string();
    child.kill();

    assert_eq!(workspace.count("captures"), 0);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![capture_id.clone()]
    );

    let report = workspace.drain_staging();
    assert!(report.kept_after_failure.is_empty());
    assert_eq!(report.acknowledgments.len(), 1);
    assert_eq!(report.acknowledgments[0].capture_id, capture_id);
    assert_eq!(
        report.acknowledgments[0].disposition,
        ImportDisposition::Imported
    );
    assert_one_consistent_row_set(&workspace, 1);
    assert!(workspace.staging().list().complete_capture_ids.is_empty());
}

#[test]
fn kill_while_import_waits_for_the_write_lock_commits_nothing_and_keeps_the_record() {
    let workspace = Workspace::new();
    let blocker = workspace.open_database();
    blocker.conn().execute_batch("BEGIN IMMEDIATE").unwrap();

    let child = ChildProcess::spawn("import_blocked_by_writer", workspace.root(), 1);
    child.expect_message_starting_with("importing ");
    std::thread::sleep(Duration::from_millis(300));
    child.kill();
    blocker.conn().execute_batch("ROLLBACK").unwrap();

    assert_one_consistent_row_set(&workspace, 0);
    let capture_id = synthetic_capture_id(0);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![capture_id.clone()]
    );

    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_eq!(
        report.acknowledgments[0].disposition,
        ImportDisposition::Imported
    );
    assert_one_consistent_row_set(&workspace, 1);
}

#[test]
fn kill_after_acknowledgment_before_staging_removal_keeps_identity_and_redelivery_converges() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("acknowledged_then_wait", workspace.root(), 1);
    let acknowledged = parse_acknowledged(&child.expect_message_starting_with("acknowledged "));
    child.kill();

    assert_acknowledgment_survives(&workspace, &acknowledged);
    assert_one_consistent_row_set(&workspace, 1);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![acknowledged.capture_id.clone()],
        "removal had not happened, so the record is still there"
    );

    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    let redelivered = &report.acknowledgments[0];
    assert_eq!(redelivered.disposition, ImportDisposition::AlreadyImported);
    assert_eq!(redelivered.item_id, acknowledged.item_id);
    assert_eq!(redelivered.saved_at, acknowledged.saved_at);
    assert_one_consistent_row_set(&workspace, 1);
    assert!(workspace.staging().list().complete_capture_ids.is_empty());
}

#[test]
fn kill_after_staging_removal_keeps_the_acknowledged_capture() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("removed_then_wait", workspace.root(), 1);
    let acknowledged = parse_acknowledged(&child.expect_message_starting_with("acknowledged "));
    child.expect_message_starting_with("removed ");
    child.kill();

    assert!(workspace.staging().list().complete_capture_ids.is_empty());
    assert_acknowledgment_survives(&workspace, &acknowledged);
    assert_one_consistent_row_set(&workspace, 1);
    assert!(workspace.drain_staging().acknowledgments.is_empty());
    assert_one_consistent_row_set(&workspace, 1);
}

#[test]
fn killing_an_unpaused_import_stream_never_loses_an_acknowledged_capture() {
    for acknowledgments_before_kill in [1_usize, 6, 25] {
        let workspace = Workspace::new();
        let child = ChildProcess::spawn("burst", workspace.root(), 400);
        let mut acknowledged = Vec::new();
        let mut staged = Vec::new();
        while acknowledged.len() < acknowledgments_before_kill {
            let message = child.next_message();
            if message.starts_with("staged ") {
                staged.push(message.trim_start_matches("staged ").to_string());
            } else {
                acknowledged.push(parse_acknowledged(&message));
            }
        }
        child.kill();

        for entry in &acknowledged {
            assert_acknowledgment_survives(&workspace, entry);
        }

        // Whatever the kill interrupted (staged but not yet removed, or not yet imported)
        // recovers exactly once; nothing staged is lost and nothing is duplicated.
        workspace.drain_staging();
        let second_pass = workspace.drain_staging();
        assert!(second_pass.acknowledgments.is_empty());
        assert!(workspace.staging().list().complete_capture_ids.is_empty());

        let database = workspace.open_database();
        let stored = stored_capture_ids(&database);
        for capture_id in &staged {
            assert!(
                stored.contains(capture_id),
                "{capture_id} was staged and must be imported"
            );
            assert_eq!(item_ids_for(&database, capture_id).len(), 1);
        }
        for entry in &acknowledged {
            assert_acknowledgment_survives(&workspace, entry);
        }
        let total = stored.len() as i64;
        assert_one_consistent_row_set(&workspace, total);
    }
}

#[test]
fn a_write_killed_before_its_rename_is_never_imported_or_deleted() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("partial_stage_then_wait", workspace.root(), 1);
    let capture_id = child
        .expect_message_starting_with("partial ")
        .trim_start_matches("partial ")
        .to_string();
    child.kill();

    let staging = workspace.staging();
    let incomplete_path = staging.incomplete_path(&capture_id);
    let truncated_bytes = fs::read(&incomplete_path).unwrap();
    assert!(!truncated_bytes.is_empty());

    for _ in 0..3 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert!(report.kept_after_failure.is_empty());
    }
    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(fs::read(&incomplete_path).unwrap(), truncated_bytes);
    let listing = staging.list();
    assert!(listing.complete_capture_ids.is_empty());
    assert_eq!(listing.incomplete_file_names.len(), 1);

    // The capture is re-delivered in full: the same write now completes and imports once.
    let capture = synthetic_text_capture(0);
    fs::remove_file(&incomplete_path).unwrap();
    assert_eq!(staging.stage(&capture), Ok(StageOutcome::Created));
    assert_eq!(workspace.drain_staging().acknowledgments.len(), 1);
    assert_one_consistent_row_set(&workspace, 1);
}

#[test]
fn repeated_delivery_across_restarts_yields_one_capture_with_one_identity() {
    let workspace = Workspace::new();
    let capture = synthetic_text_capture(7);
    let staging = workspace.staging();

    assert_eq!(staging.stage(&capture), Ok(StageOutcome::Created));
    assert_eq!(staging.stage(&capture), Ok(StageOutcome::AlreadyStaged));
    let first = workspace.drain_staging();
    assert_eq!(first.acknowledgments.len(), 1);
    let identity = first.acknowledgments[0].clone();
    assert_eq!(identity.disposition, ImportDisposition::Imported);

    for _ in 0..3 {
        assert_eq!(staging.stage(&capture), Ok(StageOutcome::Created));
        let again = workspace.drain_staging();
        assert_eq!(again.acknowledgments.len(), 1);
        assert_eq!(
            again.acknowledgments[0].disposition,
            ImportDisposition::AlreadyImported
        );
        assert_eq!(again.acknowledgments[0].item_id, identity.item_id);
        assert_eq!(again.acknowledgments[0].saved_at, identity.saved_at);
    }
    assert_one_consistent_row_set(&workspace, 1);
    assert_eq!(
        item_ids_for(&workspace.open_database(), &capture.capture_id),
        vec![identity.item_id]
    );
}

#[test]
fn a_different_record_under_a_staged_capture_id_is_refused_and_the_original_stays() {
    let workspace = Workspace::new();
    let staging = workspace.staging();
    let original = synthetic_text_capture(3);
    staging.stage(&original).unwrap();
    let original_bytes = fs::read(staging.record_path(&original.capture_id)).unwrap();

    let mut different = original.clone();
    different.text = Some("different synthetic content".to_string());
    assert_eq!(
        staging.stage(&different),
        Err(StageError::DifferentRecordUnderSameCaptureId)
    );
    assert_eq!(
        fs::read(staging.record_path(&original.capture_id)).unwrap(),
        original_bytes
    );
    assert_eq!(workspace.drain_staging().acknowledgments.len(), 1);
    assert_eq!(
        workspace
            .open_database()
            .conn()
            .query_row(
                "SELECT text FROM captures WHERE capture_id = ?",
                [original.capture_id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "synthetic capture number 3"
    );
}

#[test]
fn a_failed_or_conflicting_import_never_removes_the_sole_staged_source_or_its_audio() {
    let workspace = Workspace::new();
    let staging = workspace.staging();
    let audio_bytes = workspace.write_audio(5);

    // Unknown route: refused before any write; the record and audio stay for a later retry.
    let mut unroutable = synthetic_audio_capture(5);
    unroutable.route_id = "route-added-later".to_string();
    staging.stage(&unroutable).unwrap();
    for _ in 0..2 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert_eq!(report.kept_after_failure.len(), 1);
    }
    assert_one_consistent_row_set(&workspace, 0);
    assert!(staging.record_path(&unroutable.capture_id).exists());
    assert_eq!(fs::read(workspace.audio_path(5)).unwrap(), audio_bytes);

    // Once the route exists the very same staged record imports; the audio file is the only
    // copy of the recording and the import leaves it alone.
    workspace
        .open_database()
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES ('route-added-later', 'route-added-later', 'personal', '[]', '2026-10-08T09:00:00Z')",
            [],
        )
        .unwrap();
    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_audio_import_rows(&workspace, 1);
    assert_eq!(fs::read(workspace.audio_path(5)).unwrap(), audio_bytes);

    // Same ID, different content: rejected by core, the stored capture is untouched and the
    // staged record that could not be imported is kept rather than discarded.
    let mut conflicting = synthetic_audio_capture(5);
    conflicting.route_id = "route-added-later".to_string();
    conflicting.audio_reference = Some("audio/clip-other.m4a".to_string());
    let mut database = workspace.open_database();
    let error = import_foreground_ingress(&mut database, &conflicting).unwrap_err();
    assert!(matches!(
        error.kind,
        IngressErrorKind::ConflictingReuse { .. }
    ));
    assert_eq!(error.commit_status, CommitStatus::NotCommitted);
    drop(database);
    staging.stage(&conflicting).unwrap();
    let report = workspace.drain_staging();
    assert!(report.acknowledgments.is_empty());
    assert_eq!(report.kept_after_failure.len(), 1);
    assert!(staging.record_path(&conflicting.capture_id).exists());
    assert_eq!(
        workspace
            .open_database()
            .conn()
            .query_row(
                "SELECT audio_reference FROM captures WHERE capture_id = ?",
                [conflicting.capture_id.as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "audio/clip-5.m4a"
    );
    assert_eq!(fs::read(workspace.audio_path(5)).unwrap(), audio_bytes);
    assert_audio_import_rows(&workspace, 1);
}

#[test]
fn validation_rejections_are_specific_and_keep_the_staged_record() {
    let workspace = Workspace::new();
    let mut mismatched = synthetic_text_capture(9);
    mismatched.route_id = WORK_ROUTE.to_string();
    let mut database = workspace.open_database();
    let error = import_foreground_ingress(&mut database, &mismatched).unwrap_err();
    assert!(matches!(
        error.kind,
        IngressErrorKind::Validation(IngressRejection::RouteScopeMismatch { .. })
    ));
    drop(database);

    workspace.staging().stage(&mismatched).unwrap();
    assert!(workspace.drain_staging().acknowledgments.is_empty());
    assert!(workspace
        .staging()
        .record_path(&mismatched.capture_id)
        .exists());
    assert_one_consistent_row_set(&workspace, 0);
}
