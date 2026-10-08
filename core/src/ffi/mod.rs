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
//! To expose a function as part of the C ABI boundary, it must be declared as
//! `#[no_mangle] pub extern "C"` in a module that cbindgen can discover.
//!
//! **For functions defined in `core/src/ffi/`:**
//! Create a re-export wrapper in `core/bindings/src/lib.rs` or a new module there.
//! cbindgen will discover the `#[no_mangle] pub extern "C"` function and include it
//! in the generated header. This requires adding a single `mod` declaration in
//! `core/bindings/src/lib.rs`, which is a "named module declaration" allowed per
//! the refinement overlay.
//!
//! **Alternative (explicit registration):**
//! Define the C function directly in a submodule of the binding crate
//! (e.g., `core/bindings/src/ffi.rs`, with `mod ffi;` in `lib.rs`).
//! This works for functions that naturally belong in the binding-crate's ABI layer.
//!
//! **Pattern for B01b/B01c:**
//! Later-owned modules can add their FFI exports by:
//! 1. Creating a new module in `core/bindings/src/` (e.g., `core/bindings/src/b01b.rs`)
//! 2. Adding a `mod b01b;` declaration to `core/bindings/src/lib.rs`
//! 3. Defining their C functions there, potentially wrapping logic from `core/src/ffi/`
//!
//! This serialized approach ensures:
//! - Exports are discovered without scanning dependencies
//! - Bindings remain reproducible and cacheable build artifacts
//! - Clear ownership chain: later tasks depend on this pattern and know how to extend
//! - Ordered, non-concurrent edits to the binding crate
