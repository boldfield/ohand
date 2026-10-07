use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let output_file = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ohand_core.h");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_parse_expand(&["ohand_core"])
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(&output_file);

    // Copy the generated header to a known location for iOS projects
    let ios_bridge_dir = PathBuf::from(&crate_dir)
        .parent()
        .unwrap()
        .join("ios")
        .join("OhAndCoreBridge");

    if ios_bridge_dir.exists() {
        let dest = ios_bridge_dir.join("ohand_core.h");
        std::fs::copy(&output_file, &dest)
            .unwrap_or_else(|e| panic!("Failed to copy header to iOS bridge: {}", e));
    }

    println!(
        "cargo:warning=Generated header at: {}",
        output_file.display()
    );
}
