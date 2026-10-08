# Tauri 2 iOS management-shell probe

Minimal Tauri 2 app used to decide whether a webview is acceptable for the
management and settings screens. It has one native round-trip
(`echo_message`) and no product logic, provider credentials or network access.

Layout:

- `dist/` static webview assets (committed, loaded locally, strict CSP)
- `src-tauri/` Rust crate, `tauri.conf.json`, icons; excluded from the root Cargo workspace
- `package.json` / `package-lock.json` pin `@tauri-apps/cli` (run through `npx tauri`)

Build, simulator launch, CI assertions, pinned versions, evidence and known
limitations are in [`docs/validation/tauri-build.md`](../../docs/validation/tauri-build.md).
