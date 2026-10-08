use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:\n  ohand-bindgen generate <binding-crate-dir> <cbindgen.toml> <output-header>\n  ohand-bindgen check-library <header> <static-library>";

fn run(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [command, binding_crate, config_file, output_header] if command == "generate" => {
            let header = ohand_bindgen::generate_header(
                &PathBuf::from(binding_crate),
                &PathBuf::from(config_file),
            )?;
            let output_header = PathBuf::from(output_header);
            if let Some(parent) = output_header.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let temporary_header = output_header.with_extension("h.tmp");
            std::fs::write(&temporary_header, header).map_err(|error| error.to_string())?;
            std::fs::rename(&temporary_header, &output_header).map_err(|error| error.to_string())
        }
        [command, header, library] if command == "check-library" => {
            ohand_bindgen::check_library_matches_header(
                &PathBuf::from(header),
                &PathBuf::from(library),
            )
        }
        _ => Err(USAGE.to_string()),
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("ohand-bindgen: {message}");
            ExitCode::FAILURE
        }
    }
}
