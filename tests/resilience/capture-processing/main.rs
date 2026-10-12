//! Process-level capture durability (T02a). Each test hard-kills (SIGKILL) a real child process
//! at a durable acknowledgment boundary of the actual foreground ingress import, reopens the
//! database in the parent as a restart would, and checks that acknowledged captures survive with
//! stable identity, that redelivery never duplicates, and that a sole uncommitted source (staged
//! record, incomplete write, in-progress or finalized audio file) is never deleted. The audio
//! handoff follows the native in-progress to finalized move; see `harness.rs` for what the
//! native staging model does and does not reproduce. `processing_tests` (T02b) covers the job
//! runner side: provider timeouts, invalid output, policy changes, crash and duplicate delivery,
//! and deletion or correction racing a provider result. All content is synthetic.

mod child;
mod harness;
mod processing;
mod processing_tests;

use harness::*;
use ohand_core::ingress::{
    import_foreground_ingress, CommitStatus, ImportDisposition, IngressErrorKind, IngressRejection,
};
use std::fs;
use std::time::Duration;

const NO_MESSAGE_EXPECTED: Duration = Duration::from_millis(400);

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
fn kill_before_the_write_transaction_starts_commits_nothing_and_keeps_the_record() {
    let workspace = Workspace::new();
    let blocker = workspace.open_database();
    blocker.conn().execute_batch("BEGIN IMMEDIATE").unwrap();

    let child = ChildProcess::spawn("import_blocked_by_writer", workspace.root(), 1);
    child.expect_message_starting_with("importing ");
    assert_eq!(
        child.poll_message(NO_MESSAGE_EXPECTED),
        None,
        "the import must still be waiting for the write lock when it is killed"
    );
    child.kill();
    blocker.conn().execute_batch("ROLLBACK").unwrap();

    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![synthetic_capture_id(0)]
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
fn kill_after_rows_are_written_before_commit_rolls_back_and_the_record_imports_once() {
    let workspace = Workspace::new();
    // An open reader holds a shared lock: the child's write transaction starts and writes its
    // rows, but its COMMIT cannot take the exclusive lock until the reader finishes.
    let reader = workspace.open_database();
    reader
        .conn()
        .execute_batch("BEGIN; SELECT COUNT(*) FROM captures;")
        .unwrap();

    let child = ChildProcess::spawn("import_blocked_at_commit", workspace.root(), 1);
    child.expect_message_starting_with("importing ");
    child.expect_message_starting_with("blocked at commit");
    assert_eq!(
        child.poll_message(NO_MESSAGE_EXPECTED),
        None,
        "the import must not have returned: it is stuck at COMMIT"
    );
    child.kill();

    let journal = workspace.root().join("ohand.db-journal");
    assert!(
        fs::metadata(&journal).map(|m| m.len() > 0).unwrap_or(false),
        "the killed transaction left a hot rollback journal, so it had begun writing"
    );
    reader.conn().execute_batch("ROLLBACK").unwrap();
    drop(reader);

    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![synthetic_capture_id(0)],
        "the record outlives an import that never committed"
    );

    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_eq!(
        report.acknowledgments[0].disposition,
        ImportDisposition::Imported
    );
    assert_one_consistent_row_set(&workspace, 1);
    assert!(workspace.staging().list().complete_capture_ids.is_empty());
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
    let record = synthetic_text_record(0);
    fs::remove_file(&incomplete_path).unwrap();
    assert_eq!(staging.stage(&record), Ok(StageOutcome::Created));
    assert_eq!(workspace.drain_staging().acknowledgments.len(), 1);
    assert_one_consistent_row_set(&workspace, 1);
}

#[test]
fn repeated_delivery_across_restarts_yields_one_capture_with_one_identity() {
    let workspace = Workspace::new();
    let record = synthetic_text_record(7);
    let staging = workspace.staging();

    assert_eq!(staging.stage(&record), Ok(StageOutcome::Created));
    assert_eq!(staging.stage(&record), Ok(StageOutcome::AlreadyStaged));
    let first = workspace.drain_staging();
    assert_eq!(first.acknowledgments.len(), 1);
    let identity = first.acknowledgments[0].clone();
    assert_eq!(identity.disposition, ImportDisposition::Imported);

    for _ in 0..3 {
        assert_eq!(staging.stage(&record), Ok(StageOutcome::Created));
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
        item_ids_for(&workspace.open_database(), &record.capture_id),
        vec![identity.item_id]
    );
}

#[test]
fn a_different_record_under_a_staged_capture_id_is_refused_and_the_original_stays() {
    let workspace = Workspace::new();
    let staging = workspace.staging();
    let original = synthetic_text_record(3);
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

fn assert_sole_audio_copy(workspace: &Workspace, record: &StagedRecord, bytes: &[u8]) {
    let audio = record.audio.as_ref().unwrap();
    let in_progress = workspace.in_progress_audio_path(&audio.in_progress_file_name);
    let finalized = workspace.finalized_audio_path(&audio.finalized_file_name);
    assert_ne!(
        in_progress.exists(),
        finalized.exists(),
        "exactly one file holds the recording"
    );
    let holder = if finalized.exists() {
        finalized
    } else {
        in_progress
    };
    assert_eq!(fs::read(holder).unwrap(), bytes);
}

fn assert_audio_capture_references_finalized_file(
    workspace: &Workspace,
    record: &StagedRecord,
    bytes: &[u8],
) {
    let audio = record.audio.as_ref().unwrap();
    assert_eq!(
        workspace.stored_audio_reference(&record.capture_id),
        Some(format!("FinalizedAudio/{}", audio.finalized_file_name))
    );
    assert!(!workspace
        .in_progress_audio_path(&audio.in_progress_file_name)
        .exists());
    assert_eq!(
        fs::read(workspace.finalized_audio_path(&audio.finalized_file_name)).unwrap(),
        bytes
    );
    assert_audio_import_rows(workspace, 1);
}

#[test]
fn kill_after_an_audio_record_is_staged_before_the_handoff_keeps_the_only_recording() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("audio_staged_then_wait", workspace.root(), 1);
    child.expect_message_starting_with("staged ");
    child.kill();

    let record = synthetic_audio_record(0);
    let bytes = synthetic_audio_bytes(0);
    assert_one_consistent_row_set(&workspace, 0);
    assert_sole_audio_copy(&workspace, &record, &bytes);
    assert!(workspace
        .in_progress_audio_path(&record.audio.as_ref().unwrap().in_progress_file_name)
        .exists());

    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
    assert!(workspace.staging().list().complete_capture_ids.is_empty());
}

#[test]
fn kill_after_the_audio_move_before_import_converges_on_one_capture_referencing_the_finalized_file()
{
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("audio_moved_then_wait", workspace.root(), 1);
    child.expect_message_starting_with("moved ");
    child.kill();

    let record = synthetic_audio_record(0);
    let bytes = synthetic_audio_bytes(0);
    let audio = record.audio.as_ref().unwrap();
    assert!(!workspace
        .in_progress_audio_path(&audio.in_progress_file_name)
        .exists());
    assert_sole_audio_copy(&workspace, &record, &bytes);
    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![record.capture_id.clone()],
        "the record is what keeps the moved recording reachable"
    );

    // Redelivery finds the finalized file with the source gone: ready without moving again.
    assert_eq!(
        workspace.staging().finalize_audio(audio),
        Ok(AudioFinalization::Ready { moved: false })
    );
    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_eq!(
        report.acknowledgments[0].disposition,
        ImportDisposition::Imported
    );
    assert!(workspace.drain_staging().acknowledgments.is_empty());
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
    assert_eq!(
        item_ids_for(&workspace.open_database(), &record.capture_id).len(),
        1
    );
}

#[test]
fn kill_after_an_audio_import_before_record_removal_redelivers_to_the_same_item() {
    let workspace = Workspace::new();
    let child = ChildProcess::spawn("audio_acknowledged_then_wait", workspace.root(), 1);
    let acknowledged = parse_acknowledged(&child.expect_message_starting_with("acknowledged "));
    child.kill();

    let record = synthetic_audio_record(0);
    let bytes = synthetic_audio_bytes(0);
    assert_acknowledgment_survives(&workspace, &acknowledged);
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
    assert_eq!(
        workspace.staging().list().complete_capture_ids,
        vec![record.capture_id.clone()]
    );

    let report = workspace.drain_staging();
    assert_eq!(report.acknowledgments.len(), 1);
    assert_eq!(
        report.acknowledgments[0].disposition,
        ImportDisposition::AlreadyImported
    );
    assert_eq!(report.acknowledgments[0].item_id, acknowledged.item_id);
    assert_eq!(report.acknowledgments[0].saved_at, acknowledged.saved_at);
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
    assert!(workspace.staging().list().complete_capture_ids.is_empty());
}

#[test]
fn redelivering_an_audio_record_after_its_source_is_gone_adds_no_capture_and_deletes_nothing() {
    let workspace = Workspace::new();
    let staging = workspace.staging();
    let record = synthetic_audio_record(2);
    let bytes = workspace.write_in_progress_audio(&record, 2);

    assert_eq!(staging.stage(&record), Ok(StageOutcome::Created));
    assert_eq!(staging.stage(&record), Ok(StageOutcome::AlreadyStaged));
    let first = workspace.drain_staging();
    assert_eq!(first.acknowledgments.len(), 1);
    let identity = first.acknowledgments[0].clone();

    for _ in 0..3 {
        assert_eq!(staging.stage(&record), Ok(StageOutcome::Created));
        let again = workspace.drain_staging();
        assert_eq!(again.acknowledgments.len(), 1);
        assert_eq!(
            again.acknowledgments[0].disposition,
            ImportDisposition::AlreadyImported
        );
        assert_eq!(again.acknowledgments[0].item_id, identity.item_id);
        assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
    }
}

#[test]
fn when_both_audio_files_exist_the_handoff_conflicts_touching_neither_and_importing_nothing() {
    let workspace = Workspace::new();
    let record = synthetic_audio_record(4);
    let audio = record.audio.clone().unwrap();
    let in_progress = workspace.in_progress_audio_path(&audio.in_progress_file_name);
    let finalized = workspace.finalized_audio_path(&audio.finalized_file_name);
    fs::write(&in_progress, b"recording still in progress").unwrap();
    fs::write(&finalized, b"a different finalized recording").unwrap();
    workspace.staging().stage(&record).unwrap();

    for _ in 0..3 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert_eq!(report.kept_after_failure.len(), 1);
        assert!(report.kept_after_failure[0]
            .1
            .contains("DestinationConflict"));
    }
    assert_eq!(
        workspace.staging().finalize_audio(&audio),
        Err(AudioFailure::DestinationConflict)
    );
    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(
        fs::read(&in_progress).unwrap(),
        b"recording still in progress"
    );
    assert_eq!(
        fs::read(&finalized).unwrap(),
        b"a different finalized recording"
    );
    assert!(workspace.staging().record_path(&record.capture_id).exists());
}

#[test]
fn a_missing_or_empty_recording_keeps_the_record_and_whatever_file_exists() {
    let workspace = Workspace::new();
    let staging = workspace.staging();

    let missing = synthetic_audio_record(5);
    staging.stage(&missing).unwrap();
    let empty = synthetic_audio_record(6);
    staging.stage(&empty).unwrap();
    let empty_audio = empty.audio.as_ref().unwrap();
    let empty_path = workspace.in_progress_audio_path(&empty_audio.in_progress_file_name);
    fs::write(&empty_path, b"").unwrap();

    for _ in 0..2 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert_eq!(report.kept_after_failure.len(), 2);
    }
    assert_one_consistent_row_set(&workspace, 0);
    assert!(staging.record_path(&missing.capture_id).exists());
    assert!(staging.record_path(&empty.capture_id).exists());
    assert!(empty_path.exists());

    // The recording turns up later: the same records now import.
    workspace.write_in_progress_audio(&missing, 5);
    fs::write(&empty_path, synthetic_audio_bytes(6)).unwrap();
    assert_eq!(workspace.drain_staging().acknowledgments.len(), 2);
    assert_audio_import_rows(&workspace, 2);
}

#[test]
fn a_failed_import_never_removes_the_record_or_the_recording() {
    let workspace = Workspace::new();
    let staging = workspace.staging();
    let mut record = synthetic_audio_record(8);
    record.context.route_id = "route-added-later".to_string();
    let bytes = workspace.write_in_progress_audio(&record, 8);
    staging.stage(&record).unwrap();

    // Unknown route: refused before any write. The recording was already moved to the
    // finalized store, where it stays reachable from the kept record.
    for _ in 0..2 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert_eq!(report.kept_after_failure.len(), 1);
        assert_one_consistent_row_set(&workspace, 0);
        assert!(staging.record_path(&record.capture_id).exists());
        assert_sole_audio_copy(&workspace, &record, &bytes);
    }

    workspace.insert_route(&workspace.open_database(), "route-added-later", "personal");
    assert_eq!(workspace.drain_staging().acknowledgments.len(), 1);
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);

    // Same capture ID, different content: core rejects it, the stored capture is untouched and
    // the staged record and its recording are kept rather than discarded.
    let mut conflicting = record.clone();
    let conflicting_audio = AudioHandoff {
        in_progress_file_name: "recording-8-other.m4a".to_string(),
        finalized_file_name: format!("{}.wav", record.capture_id),
    };
    conflicting.audio = Some(conflicting_audio.clone());
    let mut database = workspace.open_database();
    let error = import_foreground_ingress(&mut database, &conflicting.core_capture()).unwrap_err();
    assert!(matches!(
        error.kind,
        IngressErrorKind::ConflictingReuse { .. }
    ));
    assert_eq!(error.commit_status, CommitStatus::NotCommitted);
    drop(database);
    let other_bytes = b"a second, different recording".to_vec();
    fs::write(
        workspace.in_progress_audio_path(&conflicting_audio.in_progress_file_name),
        &other_bytes,
    )
    .unwrap();
    staging.stage(&conflicting).unwrap();
    let report = workspace.drain_staging();
    assert!(report.acknowledgments.is_empty());
    assert_eq!(report.kept_after_failure.len(), 1);
    assert!(staging.record_path(&conflicting.capture_id).exists());
    assert_eq!(
        fs::read(workspace.finalized_audio_path(&conflicting_audio.finalized_file_name)).unwrap(),
        other_bytes
    );
    assert_audio_capture_references_finalized_file(&workspace, &record, &bytes);
}

#[test]
fn an_unreadable_or_newer_format_record_is_kept_untouched() {
    let workspace = Workspace::new();
    let staging = workspace.staging();

    let corrupt_id = synthetic_capture_id(11);
    fs::write(staging.record_path(&corrupt_id), b"{ not a record").unwrap();
    let mut newer = synthetic_text_record(12);
    newer.format_version = 2;
    fs::write(
        staging.record_path(&newer.capture_id),
        StagingStore::serialized(&newer),
    )
    .unwrap();

    for _ in 0..2 {
        let report = workspace.drain_staging();
        assert!(report.acknowledgments.is_empty());
        assert_eq!(report.kept_after_failure.len(), 2);
    }
    assert_one_consistent_row_set(&workspace, 0);
    assert_eq!(
        fs::read(staging.record_path(&corrupt_id)).unwrap(),
        b"{ not a record"
    );
    assert!(staging.record_path(&newer.capture_id).exists());
}

#[test]
fn validation_rejections_are_specific_and_keep_the_staged_record() {
    let workspace = Workspace::new();
    let mut mismatched = synthetic_text_record(9);
    mismatched.context.route_id = WORK_ROUTE.to_string();
    let mut database = workspace.open_database();
    let error = import_foreground_ingress(&mut database, &mismatched.core_capture()).unwrap_err();
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
