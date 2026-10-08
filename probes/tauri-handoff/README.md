# Native-to-Tauri handoff probe

Probe for connecting the native capture entry (P02) to the candidate Tauri management shell (P06).
Uses stable capture identifiers and rejects unknown or malicious routes.

## Layout

- `src/lib.rs` — Rust library with `HandoffValidator`, `HandoffRoute` and `HandoffRequest` types. Validates handoff URLs (`ohand-tauri://capture?captureId=...`) and rejects invalid routes.
- `Cargo.toml` — Library manifest.
- `Sources/HandoffCore.swift` — Swift equivalent for potential native integration and unit tests.
- `tests/` — Swift unit tests for handoff validation.

## Design

The handoff URL scheme is `ohand-tauri://route?captureId=<uuid>`.
The only supported route in P07 is `capture`.
Requests with invalid routes, missing capture IDs, or wrong schemes are rejected.
The native capture (P02) saves the entry immediately; the handoff to Tauri is optional and does not block the save.

## Build and test

Rust tests:
```bash
cargo test --package ohand-tauri-handoff
```

Swift tests are part of the `OhAndTests` scheme in the iOS project.

## Known limitations

- The probe validates URLs only; it does not test the full Tauri webview UI or the actual handoff from system controls.
- Accessibility features (keyboard, VoiceOver, large-text) are measured on a physical device using the procedure in `docs/validation/tauri-handoff.md`.
- The probe does not implement persistent storage or cross-app handoff; those are production decisions for C-series and B-series tasks.
