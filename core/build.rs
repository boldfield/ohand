use std::env;
use std::path::PathBuf;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let output_file = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ohand_core.h");
    let config_file = PathBuf::from(&crate_dir).join("cbindgen.toml");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(
            cbindgen::Config::from_file(&config_file)
                .unwrap_or_else(|_| cbindgen::Config::default()),
        )
        .with_parse_expand(&["ohand_core"])
        .with_language(cbindgen::Language::C)
        .generate()
        .expect("Unable to generate bindings")
        .write_to_file(&output_file);

    println!(
        "cargo:warning=Generated header at: {}",
        output_file.display()
    );
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
