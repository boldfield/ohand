use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ohand_tauri_handoff::{HandoffInbox, HandoffSnapshot};
use tauri::Manager;

const ROUNDTRIP_RECORD_FILE: &str = "roundtrip.json";
const HANDOFF_DIRECTORY: &str = "handoffs";

/// Set once the web UI has asked for the handoff list. Stored with each record to show whether a handoff
/// was written before any web UI existed.
struct WebviewReady(AtomicBool);

fn echo(input: &str) -> String {
    format!("Echo from Rust: {input}")
}

#[tauri::command]
fn echo_message(input: String) -> String {
    echo(&input)
}

/// The UI reports what it rendered after the echo returned. Writing it to the
/// app data directory gives the simulator test a deterministic artifact.
#[tauri::command]
fn record_roundtrip(
    app: tauri::AppHandle,
    status: String,
    rendered_result: String,
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|error| error.to_string())?;
    fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
    let record = serde_json::json!({
        "status": status,
        "rendered_result": rendered_result,
    });
    let bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
    fs::write(data_dir.join(ROUNDTRIP_RECORD_FILE), bytes).map_err(|error| error.to_string())
}

fn handoff_inbox(app: &tauri::AppHandle) -> Result<HandoffInbox, String> {
    let data_dir = app.path().app_data_dir().map_err(|error| error.to_string())?;
    Ok(HandoffInbox::new(data_dir.join(HANDOFF_DIRECTORY)))
}

/// Read-only view for the management UI: capture identifiers and a rejected-handoff count, nothing else.
#[tauri::command]
fn list_handoffs(
    app: tauri::AppHandle,
    webview_ready: tauri::State<'_, WebviewReady>,
) -> Result<HandoffSnapshot, String> {
    webview_ready.0.store(true, Ordering::SeqCst);
    handoff_inbox(&app)?
        .snapshot()
        .map_err(|error| error.to_string())
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Native URL receiver. It runs from the application's open-URL event, not from a webview command, so a handoff is
/// stored whether or not the web UI has loaded.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
fn record_opened_urls(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    use ohand_tauri_handoff::{receive_urls, ReceiveOutcome};

    let inbox = match handoff_inbox(app) {
        Ok(inbox) => inbox,
        Err(error) => {
            eprintln!("handoff: no inbox directory: {error}");
            return;
        }
    };
    let webview_ready = app.state::<WebviewReady>().0.load(Ordering::SeqCst);
    let outcomes = receive_urls(
        &inbox,
        urls.iter().map(|url| url.as_str()),
        webview_ready,
        now_unix_ms(),
    );
    for outcome in outcomes {
        match outcome {
            ReceiveOutcome::Recorded(id) => eprintln!("handoff: recorded {id}"),
            ReceiveOutcome::Duplicate(id) => eprintln!("handoff: duplicate {id}"),
            ReceiveOutcome::Rejected(reason) => eprintln!("handoff: rejected {reason}"),
            ReceiveOutcome::Failed(error) => eprintln!("handoff: storage failure: {error}"),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .manage(WebviewReady(AtomicBool::new(false)))
        .invoke_handler(tauri::generate_handler![
            echo_message,
            record_roundtrip,
            list_handoffs
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| match event {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
        tauri::RunEvent::Opened { urls } => record_opened_urls(app_handle, &urls),
        _ => {
            let _ = app_handle;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::echo;

    #[test]
    fn echo_prefixes_input() {
        assert_eq!(echo("Hello from Tauri"), "Echo from Rust: Hello from Tauri");
    }
}
