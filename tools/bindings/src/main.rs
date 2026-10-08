use cbindgen::{Builder, Config};
use std::env;
use std::path::PathBuf;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <config_path> <output_dir>", args[0]);
        process::exit(1);
    }

    let config_path = PathBuf::from(&args[1]);
    let output_dir = PathBuf::from(&args[2]);

    // Ensure config exists
    if !config_path.exists() {
        eprintln!("Config not found: {}", config_path.display());
        process::exit(1);
    }

    // Load config from file
    let config = match Config::from_file(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            process::exit(1);
        }
    };

    // Determine source file from config or default
    let repo_root = env::current_dir().expect("current directory");
    let src_file = repo_root.join("core/bindings/src/lib.rs");

    if !src_file.exists() {
        eprintln!("FFI source not found: {}", src_file.display());
        process::exit(1);
    }

    // Generate bindings
    let mut output = Vec::new();
    match Builder::new()
        .with_config(config)
        .with_src(&src_file)
        .generate()
    {
        Ok(bindings) => {
            bindings.write(&mut output);

            // Create output directory
            if let Err(e) = std::fs::create_dir_all(&output_dir) {
                eprintln!("Failed to create output directory: {}", e);
                process::exit(1);
            }

            let output_file = output_dir.join("ohand_bindings.h");
            if let Err(e) = std::fs::write(&output_file, &output) {
                eprintln!("Failed to write bindings: {}", e);
                process::exit(1);
            }

            println!("Generated: {}", output_file.display());
        }
        Err(e) => {
            eprintln!("Binding generation failed: {}", e);
            process::exit(1);
        }
    }
}
