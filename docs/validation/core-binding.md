# P01: Rust-to-Swift FFI Boundary Validation

Status: Implementation validated on iOS simulator and Linux core harness. Ready for device testing.

## Boundary Contract

The Rust core (`ohand_core`) exposes a C-compatible FFI for capture operations. Swift imports this through the `OhAndCoreC` module defined by `ios/OhAndCoreBridge/module.modulemap`.

### C FFI Types and Functions

**OhAndCapture**: Opaque structure representing a capture with all required metadata and optional fields.
- All pointer fields are allocated by Rust and owned by the caller until freed with `ohand_capture_free`.
- String fields use NUL-terminated C strings (allocated via CString).

**ohand_capture_new**: Create a new capture from Swift-provided strings.
- Returns a non-null `OhAndCapture*` on success, null on failure.
- Sets `*out_error` to an allocated `OhAndError*` on failure; must be freed with `ohand_error_free`.
- Validates all required fields are present (at least text or audio_reference).
- Rejects strings with interior NUL bytes.

**ohand_error_message**: Extract error message from OhAndError.
- Returned pointer is valid only during the lifetime of the error.

**ohand_error_free**: Deallocate error returned from FFI.

### Validation Evidence

#### Rust Unit Tests
Located in `core/src/bindings/capture.rs`:
- `test_capture_round_trip_ascii`: Basic capture creation with ASCII text
- `test_capture_unicode`: Full Unicode support including emoji
- `test_capture_optional_fields`: Optional field behavior (None when not provided)
- `test_capture_locked_entry`: Boolean and numeric field preservation
- `test_capture_large_input`: 1 MB input acceptance test
- `test_capture_error_memory_cleanup`: Error handling and pointer validity
- `test_error_message_lifetime`: Error message extraction and lifetime

#### Swift Integration Tests
Located in `ios/Tests/ProjectSmoke/OhAndCoreBridgeTests.swift`:
- `testBasicCaptureCreation`: Round-trip value preservation via FFI
- `testUnicodeCapture`: Unicode string handling through FFI boundary
- `testLargeInput1MB`: 1 MB acceptance demonstrated
- `testOptionalFieldsNone` / `testOptionalFieldsPresent`: Optional field contract
- `testLockedEntry`: Boolean and numeric types
- `testMissingRequiredText`: Error condition handling
- `testMemoryCleanuponSuccess`: Multiple captures without leaks

### Build Integration

**Linux checks** (`make check` / `make test`):
- Validates compilation, format, lint
- Runs Rust unit tests
- Confirms C header is valid C

**iOS CI** (`.github/workflows/ios.yml`):
- Calls `ios/scripts/build-rust-core.sh` to cross-compile for simulator
- Compiles Swift targets against the generated framework
- Runs `xcodebuild test` on OhAndTests including OhAndCoreBridgeTests
- Records evidence in `OhAndTests.xcresult`

### Known Limitations

**Cancellation/Timeout**: Not implemented in M1. All capture operations are synchronous. This is intentional for the initial probe.

**Panic Safety**: FFI functions do not use `catch_unwind`. This is acceptable for the probe; production code should add guards at FFI boundaries.

### Testing Across Platforms

| Platform | Test Target | Evidence |
|----------|-------------|----------|
| Linux (CI) | `cargo test --all --locked` | Test output with pass count |
| iOS Simulator (CI) | `xcodebuild test -scheme OhAndTests` | `OhAndTests.xcresult` |
| iOS Device (Future) | Same scheme on device | Device test logs |

The simulator tests verify the actual boundary works on iOS. Linux tests verify the Rust side independently.
