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
- Node.js 18+ (for Tauri 2.4.1 CLI)
- Target: iPhone simulator running iOS 16.0+

## Pinned Dependencies

This probe uses **pinned, reproducible versions**:
- Tauri 2.4.1 (Rust and CLI)
- @tauri-apps/api 2.4.1 and @tauri-apps/cli 2.4.1 (JavaScript)
- Cargo.lock (committed to repository)
- package-lock.json (committed to repository)

**Do not use `@latest` or `^` versions.** Clean checkout must use the pinned lockfiles.

## Build Steps

### 1. Navigate to probe directory

```bash
cd probes/tauri
```

### 2. Install pinned dependencies

Install dependencies using the committed lockfiles:

```bash
npm ci
cargo generate-lockfile --locked  # Verify Cargo.lock is present
```

### 3. Initialize iOS project (first time only)

If the `src-tauri/gen/apple` directory does not exist, initialize the Xcode project:

```bash
cd src-tauri
cargo tauri ios init
cd ..
```

This creates the iOS Xcode project under `src-tauri/gen/apple/`.

### 4. Build for iOS simulator

Build for the aarch64-sim target (simulator):

```bash
cargo tauri ios build --target aarch64-sim
```

For a physical device, use:
```bash
cargo tauri ios build --target aarch64
```

This generates:
- Rust backend compilation (tauri-build runs)
- iOS app bundle under `target/aarch64-sim/debug/` (simulator) or `target/aarch64/debug/` (device)
- Xcode integration files

### 5. Launch on simulator

Boot a simulator and launch the app:

```bash
# Boot simulator (if not running)
xcrun simctl boot "iPhone 16"  # or another available device

# Install the app (find the built bundle)
APP_PATH=$(find target -name "ohand_tauri_probe.app" -type d | head -1)
DEVICE_UDID="<simulator-udid>"
xcrun simctl install "$DEVICE_UDID" "$APP_PATH"

# Launch the app
xcrun simctl launch "$DEVICE_UDID" "com.boldfield.ohand.tauri-probe"

# Observe simulator - app should automatically run echo test on launch
```

## Simulator Validation Procedure

### Expected behavior

1. **App launches** without crash on simulator boot
2. **UI displays** with title "Oh And Tauri Probe"
3. **Automatic test runs** - the app automatically calls the echo function on launch
4. **Result displays** automatically: "Echo from Rust: Hello from Tauri"
5. **Status updates** to "Success" (or stays running to indicate completion)

### Test procedure

1. Build and launch per steps above
2. Observe the simulator:
   - App opens with title "Oh And Tauri Probe"
   - Status shows "Tauri ready - running automatic echo test..."
   - After ~500ms, result field updates with: "Echo from Rust: Hello from Tauri"
   - Status changes to "Success"
3. Manual testing (optional, for development):
   - Clear the input field and enter custom text
   - Tap "Echo" button
   - Observe result updates with the echoed text
4. Take a screenshot of the successful automatic test result

### Failure modes to document

- **Tauri configuration error** ("tauri.conf.json error")
- **Workspace resolution error** ("is not in the workspace")
- **Cargo.lock/package-lock.json mismatch** (lockfile out of date or missing)
- **Rust compilation error** (unsupported features, mismatched dependencies)
- **iOS app build error** (Xcode, Swift, or framework mismatch)
- **App crash on launch** (indicates native incompatibility or missing configuration)
- **Button tap produces no response** (IPC failure)
- **IPC returns error** (bridge communication broken, missing capability)

Document any failures with:
- Exact error message and stack trace
- Build log excerpt (especially configuration validation and Rust compiler output)
- Simulator logs (via `xcrun simctl spawn <device_id> log stream`)
- Tauri version and architecture used
- Pinned dependency versions from Cargo.lock and package-lock.json

## JavaScript Bridge (Tauri 2 API)

This probe uses **Tauri 2.x API**, not Tauri 1.x. Key differences:

**Tauri 2.x (correct):**
```javascript
// IPC invoke in Tauri 2
const response = await window.__TAURI__.core.invoke('echo_message', { input: 'Hello' });
```

**Tauri 1.x (incorrect):**
```javascript
// This is Tauri 1 API - do NOT use
const response = await window.__TAURI__.invoke('echo_message', { input: 'Hello' });
```

The probe's index.html and main.js use Tauri 2 API and include a strict Content-Security-Policy. Scripts are bundled locally; no remote URLs are loaded.

## Security validation

### Provider key exposure check

The production app must **never** pass provider API keys or secrets to JavaScript. This probe verifies:

1. **No keys in configuration files** (tauri.conf.json contains no secrets)
2. **No keys in JavaScript** (main.js has no hardcoded credentials or API keys)
3. **Rust backend handles secrets** (echo_message command does not transmit secrets; credentials remain in Rust domain only)

For production integrations, use native Keychain for credential storage and expose only opaque references across the JavaScript boundary.

### Test: Credential isolation

The echo_message command is a proof of concept that Rust ↔ JavaScript communication works without exposing secrets:

```rust
#[tauri::command]
fn echo_message(input: String) -> String {
    // Command succeeds; no secrets are involved
    format!("Echo from Rust: {}", input)
}
```

JavaScript invokes it but never receives or sends actual credentials:

```javascript
const response = await window.__TAURI__.core.invoke('echo_message', { input: 'Hello' });
// response = "Echo from Rust: Hello" — demonstrates IPC without secrets
```

## Clean Build and Full Cycle

If incremental builds fail:

```bash
cd probes/tauri
rm -rf src-tauri/target
rm -rf dist
cargo clean
npm ci
# Re-initialize if gen/apple was deleted
cd src-tauri && cargo tauri ios init && cd ..
# Rebuild
cargo tauri ios build --target aarch64-sim
```

## Troubleshooting

### "Could not find simulator"

Ensure a simulator is running:
```bash
xcrun simctl list devices
xcrun simctl boot "iPhone 16"
```

### "tauri.conf.json error"

Tauri 2 uses a different schema than Tauri 1. Ensure:
- `identifier` is at the root level, not inside `bundle`
- `bundle.targets` is `["app"]`, not `["ios"]` or `[{"ios": ["app"]}]`
- `build.devUrl` and `build.frontendDist` are set correctly
- No invalid Tauri 1 keys like `build.devPath` exist

Run `npx tauri info` to validate the configuration.

### "current package believes it's in a workspace when it's not"

Ensure the root Cargo.toml has:
```toml
[workspace]
exclude = ["probes/tauri/src-tauri"]
```

This keeps the probe's Rust code outside the workspace.

### "depends on tauri with feature X but tauri does not have that feature"

Remove Tauri 1 features like `shell-open` or `os-all` from Cargo.toml. Tauri 2 does not have these. Use pinned versions:

```toml
[dependencies]
tauri = { version = "=2.4.1" }
```

### "Compilation error: unknown module"

Ensure Rust toolchain matches (check `rust-toolchain.toml`):
```bash
rustup override set $(cat ../../rust-toolchain.toml | grep channel | cut -d'"' -f2)
```

### "IPC communication timeout or undefined __TAURI__"

- Check that `window.__TAURI__.core` is defined in JavaScript console
- Use Tauri 2 API: `window.__TAURI__.core.invoke()`, not Tauri 1's `window.__TAURI__.invoke()`
- Verify `tauri.conf.json` has a valid window definition (not an empty array)
- Ensure Rust command is registered via `generate_handler![echo_message]`
- Check CSP in index.html allows inline scripts

## Live Device Testing

Simulator validation passes and device testing is separate. To test on a physical iPhone 16:

1. Obtain Apple Developer signing credentials
2. Configure signing in Xcode (team/provisioning profile)
3. Plug iPhone 16 and select it as target
4. Run `cargo tauri ios build --target aarch64`

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
