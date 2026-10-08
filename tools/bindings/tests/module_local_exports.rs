//! A module under `ffi/` of the core crate is exported by adding only its own file and the
//! `pub mod` line in `ffi/mod.rs`; the binding crate and generator configuration stay untouched.
//!
//! The fixture mirrors the real layout (a core crate named `ohand-core` with an `ffi` module, a
//! binding crate named `ohand-bindings`) in a temporary directory, so no test symbol is ever part
//! of the shipped ABI.

use ohand_bindgen::{
    compare_header_with_library, declared_functions, exported_functions, generate_header,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
    std::fs::write(path, contents).expect("write fixture file");
}

fn create_fixture(name: &str, declare_module: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ohand-bindgen-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    write_file(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"core\", \"bindings\"]\nresolver = \"2\"\n",
    );
    write_file(
        &root.join("core/Cargo.toml"),
        "[package]\nname = \"ohand-core\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    write_file(
        &root.join("core/src/lib.rs"),
        "pub mod ffi;\n/// Unrelated core constant.\npub const FIXTURE_CORE_CONSTANT: u32 = 7;\npub const FIXTURE_CORE_EXPRESSION: usize = 64 * 1024;\npub const FIXTURE_CORE_TEXT: &str = \"text\";\npub const FIXTURE_CORE_ARRAY: [u8; 2] = [1, 2];\npub fn link_anchor() -> u32 { FIXTURE_CORE_CONSTANT }\n",
    );
    write_file(
        &root.join("core/src/ffi/mod.rs"),
        if declare_module {
            "pub mod fixture_module;\n"
        } else {
            ""
        },
    );
    write_file(
        &root.join("core/src/ffi/fixture_module.rs"),
        "/// Exported because of its prefix.\npub const OHAND_FIXTURE_MODULE_CONSTANT: u32 = 9;\n\n#[no_mangle]\npub extern \"C\" fn ohand_fixture_module_value(input: u32) -> u32 {\n    input + 1\n}\n",
    );
    write_file(
        &root.join("bindings/Cargo.toml"),
        "[package]\nname = \"ohand-bindings\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"staticlib\", \"rlib\"]\n\n[dependencies]\nohand-core = { path = \"../core\" }\n",
    );
    write_file(
        &root.join("bindings/src/lib.rs"),
        "#[no_mangle]\npub extern \"C\" fn ohand_fixture_central_value() -> u32 {\n    ohand_core::link_anchor()\n}\n",
    );
    let lock_status = Command::new(cargo_program())
        .args(["generate-lockfile", "--offline"])
        .current_dir(&root)
        .status()
        .expect("run cargo");
    assert!(lock_status.success(), "fixture lock file generation failed");
    root
}

fn cargo_program() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn production_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cbindgen.toml")
}

fn build_library(root: &Path) -> PathBuf {
    let mut command = Command::new(cargo_program());
    command
        .args([
            "build",
            "--quiet",
            "--offline",
            "--package",
            "ohand-bindings",
        ])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"));
    assert!(
        command.status().expect("run cargo").success(),
        "fixture build failed"
    );
    root.join("target/debug/libohand_bindings.a")
}

fn library_exports(library: &Path) -> std::collections::BTreeSet<String> {
    let output = Command::new("nm")
        .arg("-g")
        .arg(library)
        .output()
        .expect("run nm");
    assert!(output.status.success());
    exported_functions(&String::from_utf8_lossy(&output.stdout))
}

#[test]
fn module_local_export_reaches_header_and_library_without_central_edits() {
    let root = create_fixture("declared", true);
    let header = generate_header(&root.join("bindings"), &production_config()).expect("generate");
    let declared = declared_functions(&header);
    assert!(declared.contains("ohand_fixture_module_value"), "{header}");
    assert!(declared.contains("ohand_fixture_central_value"), "{header}");
    assert!(
        header.contains("#define OHAND_FIXTURE_MODULE_CONSTANT 9"),
        "{header}"
    );
    assert!(
        header.contains("Exported because of its prefix."),
        "{header}"
    );
    assert!(
        !header.contains("FIXTURE_CORE") && !header.contains("Unrelated core constant"),
        "unrelated core constants must not leak into the header: {header}"
    );

    let exported = library_exports(&build_library(&root));
    compare_header_with_library(&declared, &exported).expect("header and library agree");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn undeclared_module_is_not_exported_anywhere() {
    let root = create_fixture("undeclared", false);
    let header = generate_header(&root.join("bindings"), &production_config()).expect("generate");
    assert!(!declared_functions(&header).contains("ohand_fixture_module_value"));
    assert!(!library_exports(&build_library(&root)).contains("ohand_fixture_module_value"));
    std::fs::remove_dir_all(&root).ok();
}
