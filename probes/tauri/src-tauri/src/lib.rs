use std::fs;

use tauri::Manager;
use ohand_tauri_handoff::HandoffValidator;

const ROUNDTRIP_RECORD_FILE: &str = "roundtrip.json";
const HANDOFF_RECORD_FILE: &str = "handoff.json";

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

#[tauri::command]
fn handle_handoff_url(
    app: tauri::AppHandle,
    url: String,
) -> Result<serde_json::Value, String> {
    match HandoffValidator::validate(&url) {
        Ok(handoff_request) => {
            let response = serde_json::json!({
                "success": true,
                "captureId": handoff_request.capture_id.to_string(),
                "route": handoff_request.route.as_str(),
                "timestamp": handoff_request.timestamp,
            });
            let data_dir = app.path().app_data_dir().map_err(|error| error.to_string())?;
            fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
            let bytes = serde_json::to_vec_pretty(&response).map_err(|error| error.to_string())?;
            fs::write(data_dir.join(HANDOFF_RECORD_FILE), bytes).map_err(|error| error.to_string())?;
            Ok(response)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![echo_message, record_roundtrip, handle_handoff_url])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{echo, HandoffValidator};

    #[test]
    fn echo_prefixes_input() {
        assert_eq!(echo("Hello from Tauri"), "Echo from Rust: Hello from Tauri");
    }

    #[test]
    fn handoff_url_validator_accepts_valid_url() {
        let valid_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let url = format!("ohand-tauri://capture?captureId={}", valid_uuid);
        let result = HandoffValidator::validate(&url);
        assert!(result.is_ok());
        let request = result.unwrap();
        assert_eq!(request.capture_id.to_string(), valid_uuid);
        assert_eq!(request.route.as_str(), "capture");
    }

    #[test]
    fn handoff_url_validator_rejects_invalid_url() {
        let url = "ohand-tauri://capture?captureId=not-a-uuid";
        let result = HandoffValidator::validate(url);
        assert!(result.is_err());
    }

    #[test]
    fn handoff_url_validator_rejects_missing_capture_id() {
        let url = "ohand-tauri://capture";
        let result = HandoffValidator::validate(url);
        assert!(result.is_err());
    }

    #[test]
    fn handoff_url_validator_rejects_wrong_scheme() {
        let valid_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let url = format!("ohand://capture?captureId={}", valid_uuid);
        let result = HandoffValidator::validate(&url);
        assert!(result.is_err());
    }

    #[test]
    fn handoff_url_validator_rejects_duplicate_capture_id() {
        let valid_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let url = format!(
            "ohand-tauri://capture?captureId={}&captureId=evil",
            valid_uuid
        );
        let result = HandoffValidator::validate(&url);
        assert!(result.is_err());
    }
}
