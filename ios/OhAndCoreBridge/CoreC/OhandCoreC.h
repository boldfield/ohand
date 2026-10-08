// Clang module shim for Swift. A framework cannot use a bridging header, so Swift imports the
// generated C header (tools/bindings, written to OHAND_RUST_OUTPUT_DIR at build time) through
// this module. Targets that import it must put this directory on SWIFT_INCLUDE_PATHS and the
// Rust output directory on HEADER_SEARCH_PATHS.
#include "ohand_bindings.h"
