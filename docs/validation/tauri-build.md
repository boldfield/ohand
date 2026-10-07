# Tauri 2 iOS Management-Shell Probe Build and Validation

Status: Feasibility probe for Tauri 2 as an iOS management-shell candidate.

## Overview

This document describes building and testing the Tauri 2 iOS probe on the simulator. The probe validates:

1. Tauri 2 can build successfully on iOS
2. The probe launches on the simulator without crashing
3. Native Rust ↔ JavaScript round-trip communication works
4. No provider keys are exposed in the JavaScript layer

The probe does **not** implement production features; it tests framework feasibility only.

## Prerequisites

- macOS 12.0 or later
- Xcode 16.4 (pinned in `ios/project.yml`)
- Rust toolchain (installed via `rust-toolchain.toml`)
- Node.js 18+ (for Tauri CLI)
- Target: iPhone simulator running iOS 16.0+

## Build Steps

### 1. Install Tauri CLI

```bash
npm install -g @tauri-apps/cli@latest
```

### 2. Navigate to probe directory

```bash
cd probes/tauri
```

### 3. Install frontend dependencies (if needed)

The probe uses minimal dependencies (just Tauri API). If a package.json exists:

```bash
npm install
```

### 4. Build for iOS simulator

```bash
tauri build --target aarch64-apple-ios-sim
```

This generates:
- Rust backend compilation
- iOS app bundle
- Xcode project integration

### 5. Launch on simulator

```bash
xcrun simctl boot "iPhone 16"
tauri ios dev --target aarch64-apple-ios-sim
```

Or manually:
1. Open the generated Xcode project
2. Select "OhAndTauriProbe" target
3. Select "iPhone 16 Simulator"
4. Build and run (Cmd+R)

## Simulator Validation Procedure

### Expected behavior

1. **App launches** without crash on simulator boot
2. **UI displays** with title "Oh And Tauri Probe"
3. **Text input** accepts text (e.g., "Hello from Tauri")
4. **Echo button** invokes Rust backend
5. **Result displays** "Echo from Rust: [input text]"
6. **Status updates** to "Success"

### Test procedure

1. Build and launch per steps above
2. In the simulator app:
   - Read title: "Oh And Tauri Probe"
   - Verify input field shows default "Hello from Tauri"
   - Tap "Echo" button
   - Observe result field updates with echo response
   - Status changes to "Success"
3. Repeat with custom input to verify Rust communication works

### Failure modes to document

- Xcode compilation error (Rust, Swift, or framework mismatch)
- App crash on launch (indicates native incompatibility)
- Button tap produces no response (IPC failure)
- IPC returns error (bridge communication broken)

Document any failures with:
- Exact error message and stack trace
- Xcode build log excerpt
- Simulator logs (via `xcrun simctl spawn <device_id> log stream --predicate 'process == "OhAndTauriProbe"'`)
- Tauri version and architecture used

## Security validation

### Provider key exposure check

The production app must **never** pass provider API keys or secrets to JavaScript. This probe verifies:

1. **No keys in configuration files** (tauri.conf.json contains no secrets)
2. **No keys in JavaScript** (main.js has no hardcoded credentials)
3. **Rust backend handles secrets** (echo_message command does not echo secrets; credentials remain in Rust domain only)

For production integrations, use native Keychain for credential storage and expose only opaque references across the JavaScript boundary.

### Test: Credential isolation

Add a provider key to the Rust backend (synthetic only):

```rust
#[tauri::command]
fn get_api_endpoint() -> String {
    // NEVER: return secret; always return opaque reference only
    "ohand://provider/1".to_string()
}
```

The JavaScript can receive an opaque reference but never the actual key:

```javascript
const providerRef = await window.__TAURI__.invoke('get_api_endpoint');
// providerRef = "ohand://provider/1" — never exposes actual key
```

## Clean Build and Full Cycle

If incremental builds fail:

```bash
cd probes/tauri
rm -rf src-tauri/target
rm -rf dist
tauri build --target aarch64-apple-ios-sim
```

## Troubleshooting

### "Could not find simulator"

Ensure a simulator is running:
```bash
xcrun simctl list devices
xcrun simctl boot "iPhone 16"
```

### "Tauri plugin not found"

Verify Tauri CLI is installed and up to date:
```bash
tauri --version
npm install -g @tauri-apps/cli@latest
```

### "Compilation error: unknown module"

Ensure Rust toolchain matches (check `rust-toolchain.toml`):
```bash
rustup override set $(cat ../../rust-toolchain.toml | grep channel | cut -d'"' -f2)
```

### "IPC communication timeout"

- Check that `window.__TAURI__` is defined in browser console
- Verify `tauri.conf.json` security settings allow IPC
- Ensure Rust command handler is registered in `main.rs` via `generate_handler!`

## Live Device Testing

Simulator validation passes and device testing is separate. To test on a physical iPhone 16:

1. Obtain Apple Developer signing credentials
2. Configure signing in Xcode (team/provisioning profile)
3. Plug iPhone 16 and select it as target
4. Run `tauri ios dev --target aarch64-apple-ios`

Device testing is not part of this probe; see **P08** (signing) and **P09** (device feasibility) for production builds and physical validation.

## Next steps

If this probe passes simulator validation:
- P07 probes native-to-Tauri handoff
- P10 records the management-shell decision (Tauri vs. SwiftUI)
- U01 assembles the selected production shell

If this probe fails:
- Document the incompatibility in the failure modes above
- Consider SwiftUI as the management shell alternative
- Update DESIGN.md and P10 decision task as needed
