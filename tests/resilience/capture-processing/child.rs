//! The scenarios a spawned child runs. Outside a child (no scenario variable) the entry point
//! returns at once, so a plain `cargo test` run passes over it.

use crate::harness::{
    child_scenario_from_environment, open_database_at, synthetic_audio_bytes,
    synthetic_audio_record, synthetic_text_record, StagedRecord, StagingStore, PROTOCOL_PREFIX,
};
use ohand_core::ingress::import_foreground_ingress;
use std::path::Path;
use std::time::{Duration, Instant};

const HANG_LIMIT: Duration = Duration::from_secs(90);

fn say(message: &str) {
    println!("\n{PROTOCOL_PREFIX}{message}");
}

/// Runs inside SQLite while the import's COMMIT waits for a reader to release its lock. The
/// import has by then written its rows (and the rollback journal) and cannot finish.
fn busy_at_commit(attempt: i32) -> bool {
    if attempt == 0 {
        say("blocked at commit");
    }
    std::thread::sleep(Duration::from_millis(10));
    attempt < 6000
}

fn write_recording(root: &Path, record: &StagedRecord, index: usize) {
    let audio = record.audio.as_ref().unwrap();
    std::fs::write(
        root.join("IngressAudio").join(&audio.in_progress_file_name),
        synthetic_audio_bytes(index),
    )
    .unwrap();
}

fn acknowledged_message(
    acknowledgment: &ohand_core::ingress::IngressImportAcknowledgment,
) -> String {
    format!(
        "acknowledged {} {} {}",
        acknowledgment.capture_id, acknowledgment.item_id, acknowledgment.saved_at
    )
}

/// Waits to be killed; exits non-zero if nobody does, so a failed parent cannot leak a child.
fn wait_to_be_killed() -> ! {
    let started = Instant::now();
    while started.elapsed() < HANG_LIMIT {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::process::exit(3);
}

#[test]
fn child_entry() {
    let Some(scenario) = child_scenario_from_environment() else {
        return;
    };
    let staging = StagingStore::at(&scenario.root);
    let record = synthetic_text_record(0);
    let capture = record.core_capture();
    match scenario.name.as_str() {
        "stage_then_wait" => {
            staging.stage(&record).unwrap();
            say(&format!("staged {}", record.capture_id));
            wait_to_be_killed();
        }
        "partial_stage_then_wait" => {
            staging.write_partial_only(&record);
            say(&format!("partial {}", capture.capture_id));
            wait_to_be_killed();
        }
        "audio_staged_then_wait" => {
            let audio_record = synthetic_audio_record(0);
            write_recording(&scenario.root, &audio_record, 0);
            staging.stage(&audio_record).unwrap();
            say(&format!("staged {}", audio_record.capture_id));
            wait_to_be_killed();
        }
        "audio_moved_then_wait" => {
            let audio_record = synthetic_audio_record(0);
            write_recording(&scenario.root, &audio_record, 0);
            staging.stage(&audio_record).unwrap();
            staging
                .finalize_audio(audio_record.audio.as_ref().unwrap())
                .unwrap();
            say(&format!("moved {}", audio_record.capture_id));
            wait_to_be_killed();
        }
        "audio_acknowledged_then_wait" => {
            let audio_record = synthetic_audio_record(0);
            write_recording(&scenario.root, &audio_record, 0);
            staging.stage(&audio_record).unwrap();
            staging
                .finalize_audio(audio_record.audio.as_ref().unwrap())
                .unwrap();
            let mut database = open_database_at(&scenario.root);
            let acknowledgment =
                import_foreground_ingress(&mut database, &audio_record.core_capture()).unwrap();
            say(&acknowledged_message(&acknowledgment));
            wait_to_be_killed();
        }
        "import_blocked_by_writer" => {
            staging.stage(&record).unwrap();
            let mut database = open_database_at(&scenario.root);
            say(&format!("importing {}", capture.capture_id));
            let outcome = import_foreground_ingress(&mut database, &capture);
            say(&format!("import returned ok={}", outcome.is_ok()));
            wait_to_be_killed();
        }
        "import_blocked_at_commit" => {
            staging.stage(&record).unwrap();
            let mut database = open_database_at(&scenario.root);
            database.conn().busy_handler(Some(busy_at_commit)).unwrap();
            say(&format!("importing {}", capture.capture_id));
            let outcome = import_foreground_ingress(&mut database, &capture);
            say(&format!("import returned ok={}", outcome.is_ok()));
            wait_to_be_killed();
        }
        "acknowledged_then_wait" | "removed_then_wait" => {
            staging.stage(&record).unwrap();
            let mut database = open_database_at(&scenario.root);
            let acknowledgment = import_foreground_ingress(&mut database, &capture).unwrap();
            say(&acknowledged_message(&acknowledgment));
            if scenario.name == "removed_then_wait" {
                staging.remove(&record.capture_id);
                say(&format!("removed {}", record.capture_id));
            }
            wait_to_be_killed();
        }
        "burst" => {
            let mut database = open_database_at(&scenario.root);
            for index in 0..scenario.count {
                let burst_record = synthetic_text_record(index);
                staging.stage(&burst_record).unwrap();
                say(&format!("staged {}", burst_record.capture_id));
                let acknowledgment =
                    import_foreground_ingress(&mut database, &burst_record.core_capture()).unwrap();
                say(&acknowledged_message(&acknowledgment));
                staging.remove(&burst_record.capture_id);
            }
            say("finished");
            wait_to_be_killed();
        }
        "interpret_in_flight" | "interpret_applied" => crate::processing::run_child_drain(
            &scenario.root,
            scenario.name == "interpret_in_flight",
            say,
            || wait_to_be_killed(),
        ),
        other => panic!("unknown child scenario {other}"),
    }
}
