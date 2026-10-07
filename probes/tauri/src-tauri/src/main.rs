use tauri::Manager;

// Tauri iOS probe: minimal management-shell UI with one native round-trip command.
// This probe tests feasibility of Tauri 2 as an iOS management surface.
#[tauri::command]
fn echo_message(input: String) -> String {
    format!("Echo from Rust: {}", input)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![echo_message])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app_handle, _event| {});
}

#[cfg(not(mobile))]
fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![echo_message])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app_handle, _event| {});
}
