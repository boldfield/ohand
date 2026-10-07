# Tauri 2 iOS Management-Shell Probe

Minimal Tauri 2 iOS probe for evaluating webview-based management UI feasibility.

## What this probe does

- Tests Tauri 2 iOS compilation and simulator launch
- Demonstrates Rust ↔ JavaScript IPC communication
- Validates that secrets are not exposed to the JavaScript layer
- Documents compatibility issues or blockers for production use

## What this probe does NOT do

- Implement production features (see Oh And core modules for actual functionality)
- Use any provider credentials or keys
- Access native capture surfaces or system integrations (separate probes P02, P03, etc.)

## Quick start

See [`docs/validation/tauri-build.md`](../../docs/validation/tauri-build.md) for full build and validation procedures.

### Build

```bash
cd probes/tauri
tauri build --target aarch64-apple-ios-sim
```

### Test on simulator

```bash
xcrun simctl boot "iPhone 16"
tauri ios dev --target aarch64-apple-ios-sim
```

### Expected result

- App launches on simulator
- UI shows "Oh And Tauri Probe" title
- Tapping "Echo" button sends text to Rust backend and displays response
- Status shows "Success" after round-trip

## Files

- `src-tauri/` — Rust backend (Tauri command handlers)
- `index.html` — UI structure (minimal, no forms)
- `style.css` — iOS-friendly styling
- `main.js` — Tauri IPC communication
- `tauri.conf.json` — Tauri configuration
- `package.json` — Frontend build setup

## Scope and constraints

**DO:** Test framework capability and validate security boundaries.

**DON'T:** Add production features, credentials, or integrations to this probe.

**Security:** No API keys, secrets, or provider credentials are committed here. Credentials in production use native Keychain or equivalent, never the JavaScript layer.

## Next steps

- **P07** probes native ↔ Tauri handoff
- **P10** records the Tauri vs. SwiftUI decision
- **U01** assembles the selected production shell
