use std::fs;

use tauri::Manager;

const ROUNDTRIP_RECORD_FILE: &str = "roundtrip.json";

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![echo_message, record_roundtrip])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::echo;

    #[test]
    fn echo_prefixes_input() {
        assert_eq!(echo("Hello from Tauri"), "Echo from Rust: Hello from Tauri");
    }
}
