//! FFI Module — Rust-to-Swift boundary interfaces
//!
//! This module organizes the typed bindings, ABI contracts, thread safety guarantees, and error
//! conversion for the native-to-core boundary. Generated C headers are produced from the
//! bindings crate and are never committed to the repository; they are build artifacts.
//!
//! # Generated Bindings
//!
//! The canonical C header is generated from the `ohand-bindings` crate using cbindgen.
//! The generator runs during `make check` and `make test`, and separately for iOS builds.
//! Generated files are build output (`build/ohand_bindings.h` or platform equivalents),
//! never committed.
//!
//! # Module-Local Export Pattern
//!
//! FFI exports are declared within the `ohand-bindings` crate as `#[no_mangle] pub extern "C"`
//! functions. To export a C function:
//!
//! 1. Define the function in the `ohand-bindings` crate (e.g., in `core/bindings/src/lib.rs`
//!    or a submodule like `core/bindings/src/probe.rs`)
//! 2. The function is automatically discovered by cbindgen and included in the generated header
//! 3. cbindgen parses all modules declared in `core/bindings/src/lib.rs`
//!
//! Later-owned modules (e.g., B01b, B01c) that need FFI exports should:
//! - Define their exports in a module they own in the `ohand-bindings` crate (by adding a
//!   `mod` declaration in `core/bindings/src/lib.rs`), OR
//! - Define logic in `core/src/ffi/` and re-export from `ohand-bindings` via a wrapper function
//!
//! This pattern ensures:
//! - Exports are discovered without scanning dependencies
//! - Bindings remain reproducible and cacheable build artifacts
//! - Serialized ownership: later tasks depend on this task and know how to extend
//! - No concurrent edits to shared binding logic
