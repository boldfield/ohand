//! The scenarios a spawned child runs. Outside a child (no scenario variable) the entry point
//! returns at once, so a plain `cargo test` run passes over it.

use crate::harness::{
    child_scenario_from_environment, open_database_at, synthetic_text_capture, StagingStore,
    PROTOCOL_PREFIX,
};
use ohand_core::ingress::import_foreground_ingress;
use std::time::{Duration, Instant};

const HANG_LIMIT: Duration = Duration::from_secs(90);

fn say(message: &str) {
    println!("\n{PROTOCOL_PREFIX}{message}");
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
    let capture = synthetic_text_capture(0);
    match scenario.name.as_str() {
        "stage_then_wait" => {
            staging.stage(&capture).unwrap();
            say(&format!("staged {}", capture.capture_id));
            wait_to_be_killed();
        }
        "partial_stage_then_wait" => {
            staging.write_partial_only(&capture);
            say(&format!("partial {}", capture.capture_id));
            wait_to_be_killed();
        }
        "import_blocked_by_writer" => {
            staging.stage(&capture).unwrap();
            let mut database = open_database_at(&scenario.root);
            say(&format!("importing {}", capture.capture_id));
            let outcome = import_foreground_ingress(&mut database, &capture);
            say(&format!("import returned ok={}", outcome.is_ok()));
            wait_to_be_killed();
        }
        "acknowledged_then_wait" | "removed_then_wait" => {
            staging.stage(&capture).unwrap();
            let mut database = open_database_at(&scenario.root);
            let acknowledgment = import_foreground_ingress(&mut database, &capture).unwrap();
            say(&format!(
                "acknowledged {} {} {}",
                acknowledgment.capture_id, acknowledgment.item_id, acknowledgment.saved_at
            ));
            if scenario.name == "removed_then_wait" {
                staging.remove(&capture.capture_id);
                say(&format!("removed {}", capture.capture_id));
            }
            wait_to_be_killed();
        }
        "burst" => {
            let mut database = open_database_at(&scenario.root);
            for index in 0..scenario.count {
                let capture = synthetic_text_capture(index);
                staging.stage(&capture).unwrap();
                say(&format!("staged {}", capture.capture_id));
                let acknowledgment = import_foreground_ingress(&mut database, &capture).unwrap();
                say(&format!(
                    "acknowledged {} {} {}",
                    acknowledgment.capture_id, acknowledgment.item_id, acknowledgment.saved_at
                ));
                staging.remove(&capture.capture_id);
            }
            say("finished");
            wait_to_be_killed();
        }
        other => panic!("unknown child scenario {other}"),
    }
}
