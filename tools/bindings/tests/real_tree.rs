//! Generation from the real sources: reproducible, C-only, limited to the intended surface.

use ohand_bindgen::{declared_functions, generate_header};
use std::path::PathBuf;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn generate_real_header() -> String {
    generate_header(
        &repository_root().join("core/bindings"),
        &repository_root().join("tools/bindings/cbindgen.toml"),
    )
    .expect("header generation from the real sources")
}

#[test]
fn generation_is_reproducible_and_matches_the_command_line_tool() {
    let first = generate_real_header();
    assert_eq!(first, generate_real_header());

    let output_directory =
        std::env::temp_dir().join(format!("ohand-bindgen-cli-{}", std::process::id()));
    let output_header = output_directory.join("nested/ohand_bindings.h");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_ohand-bindgen"))
        .arg("generate")
        .arg(repository_root().join("core/bindings"))
        .arg(repository_root().join("tools/bindings/cbindgen.toml"))
        .arg(&output_header)
        .status()
        .expect("run generator");
    assert!(status.success());
    assert_eq!(
        first,
        std::fs::read_to_string(&output_header).expect("read header")
    );
    std::fs::remove_dir_all(&output_directory).ok();
}

#[test]
fn header_is_c_and_exports_only_the_boundary() {
    let header = generate_real_header();
    for forbidden in [
        "#include <ostream>",
        "#include <new>",
        "constexpr",
        "namespace",
    ] {
        assert!(
            !header.contains(forbidden),
            "C++ construct leaked: {forbidden}"
        );
    }
    for unrelated in [
        "DEFAULT_MAX_RESPONSE_BYTES",
        "PROFILE_SCHEMA_VERSION",
        "CORE_VERSION",
        "SUPPORTED_PROPOSAL_SCHEMA_VERSION",
        "DEFAULT_MAX_OUTPUT_TOKENS",
    ] {
        assert!(
            !header.contains(unrelated),
            "unrelated core item exported: {unrelated}"
        );
    }
    let functions = declared_functions(&header);
    for expected in [
        "ohand_probe_save_capture",
        "ohand_probe_get_capture",
        "ohand_result_free",
    ] {
        assert!(
            functions.contains(expected),
            "missing {expected}: {functions:?}"
        );
    }
}

#[test]
fn exports_are_target_independent() {
    for relative_directory in ["core/bindings/src", "core/src/ffi"] {
        let mut pending = vec![repository_root().join(relative_directory)];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read source directory") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let source = std::fs::read_to_string(&path).expect("read source");
                    assert!(
                        !source.contains("cfg(target_") && !source.contains("cfg(not(target_"),
                        "{} gates code on the target; the header must not depend on the target",
                        path.display()
                    );
                }
            }
        }
    }
}
