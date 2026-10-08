#[tauri::command]
fn echo_message(input: String) -> String {
    eprintln!("[tauri-probe] Echo command called with input: {}", input);
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
