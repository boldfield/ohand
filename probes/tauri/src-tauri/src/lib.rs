use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ohand_tauri_handoff::{HandoffInbox, HandoffSnapshot};
use tauri::Manager;

#[cfg(target_os = "ios")]
mod scene_urls;

const ROUNDTRIP_RECORD_FILE: &str = "roundtrip.json";
const HANDOFF_DIRECTORY: &str = "handoffs";
const MAX_TRACE_BYTES: u64 = 16 * 1024;

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

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Native URL receiver. It runs from a native open-URL callback, not from a webview command, so a handoff is stored
/// whether or not the web UI has loaded. On iOS the callbacks are the raw scene hooks in `scene_urls`, because
/// `RunEvent::Opened` carries URLs that tao has already normalised.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
fn record_opened_urls<'a>(app: &tauri::AppHandle, urls: impl Iterator<Item = &'a str>) {
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
        urls,
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

    #[cfg(target_os = "ios")]
    {
        let handle = app.handle().clone();
        let installed = scene_urls::install(move |source, urls| {
            let label = match source {
                scene_urls::SceneUrlSource::Connect => "scene-connect",
                scene_urls::SceneUrlSource::Open => "scene-open",
            };
            trace_label(&handle, &format!("{label} urls={}", urls.len()));
            record_opened_urls(&handle, urls.iter().map(String::as_str));
        });
        trace_label(app.handle(), &format!("scene-hook installed={installed}"));
    }

    app.run(|app_handle, event| {
        trace_run_event(app_handle, &event);
        match event {
            // Not on iOS: there the scene hooks record the raw string, and these URLs are already normalised.
            #[cfg(any(target_os = "macos", target_os = "android"))]
            tauri::RunEvent::Opened { urls } => {
                record_opened_urls(app_handle, urls.iter().map(|url| url.as_str()))
            }
            _ => {
                let _ = app_handle;
            }
        }
    });
}

/// Delivery trace: event kinds and timestamps only, never URLs. Lets CI show that the cold-launch URL arrived.
fn trace_run_event(app: &tauri::AppHandle, event: &tauri::RunEvent) {
    let label = match event {
        tauri::RunEvent::Ready => "ready".to_string(),
        tauri::RunEvent::Resumed => "resumed".to_string(),
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
        tauri::RunEvent::Opened { urls } => format!("opened urls={}", urls.len()),
        _ => return,
    };
    trace_label(app, &label);
}

fn trace_label(app: &tauri::AppHandle, label: &str) {
    use std::io::Write;

    let Ok(data_dir) = app.path().app_data_dir() else { return };
    if fs::create_dir_all(&data_dir).is_err() {
        return;
    }
    let trace_path = data_dir.join("run-events.log");
    let trace_is_full = fs::metadata(&trace_path).is_ok_and(|metadata| metadata.len() > MAX_TRACE_BYTES);
    if trace_is_full {
        return;
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace_path)
    {
        let _ = writeln!(file, "{} {label}", now_unix_ms());
    }
}

#[cfg(test)]
mod tests {
    use super::echo;

    #[test]
    fn echo_prefixes_input() {
        assert_eq!(echo("Hello from Tauri"), "Echo from Rust: Hello from Tauri");
    }
}
