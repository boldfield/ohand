//! FFI exports module-local pattern: demonstrates how later-owned modules declare exports.
//! This module shows the extension mechanism: a new module in core/bindings with a single
//! `mod ffi_exports;` declaration in lib.rs allows later-owned modules to add FFI exports
//! without concurrent central-file edits.

/// Test marker: verifies that new modules added to core/bindings are discovered by cbindgen
/// and their exports are included in the generated header. Demonstrates the mechanism
/// that B01b/B01c will follow by adding their own modules here.
/// Removed once real production exports appear.
#[no_mangle]
pub extern "C" fn ohand_ffi_module_export_test_marker() -> u32 {
    42
}
