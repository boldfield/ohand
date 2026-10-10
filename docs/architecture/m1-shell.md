# M1 management shell selection

Status: decision recorded from P09 feasibility evidence on 2026-10-10.

## Decision

**Tauri 2 is selected as the M1 production management shell.**

The native iPhone capture remains independent. Rust core, SQLite durable state, and native Swift capture/services are preserved. The management UI (settings, retrieval, reminders) runs in a Tauri webview accessed via deep-link handoff from the native capture surface.

## Evidence basis

P09 executed the documented probe matrix on the target device (iPhone 16 Pro, iOS 26.6.2) and recorded evidence in `docs/validation/device-feasibility.md`. Relevant findings:

### Handoff and identity preservation (P07)

- **Cold handoff** (shell not running): Tauri probe launched and received the capture identifier. Record shows `webviewReady == false` and handoff interval 4.961 s. Shell remains responsive while capture saves independently.
- **Warm handoff** (shell backgrounded): Second capture handed off; shell lists both identifiers. Record shows `webviewReady == true`.
- **Hostile route rejection**: Three invalid URLs (`ohand-tauri://` scheme variations) submitted while shell running; each rejected and counter incremented, no entries added.
- **Secret isolation**: Each handoff file contains only `captureId`, `receivedAtUnixMs`, and `webviewReady`. Capture text never appears in Tauri container. Native entry saves durably before "Review in management app" button is enabled, so shell absence does not block save.

All evidence: `docs/validation/device-feasibility.md` P07 section and `p09-probe-matrix-sweep.json` cells for P07 (3 cold, warm, hostile; 5 VoiceOver and large text).

### Accessibility (P07 latency session)

- **VoiceOver**: Handoff button announced as button with hint "Opens the management app. Only this entry's identifier is sent." Tauri probe headings navigable by rotor; summary read in full; identifiers read digit-by-digit.
- **Large text**: Tauri probe scales without clipping at maximum accessibility text size. Button remains reachable by scrolling. (Probe-level defects in CaptureProbe and other probes noted; Tauri itself passes.)
- **Keyboard**: Not exercised on device (no hardware keyboard available for phone); deferred to U10.

Evidence: `device-feasibility.md` Accessibility section and latency session P07_accessibility record.

### Build and configuration

- **Tauri 2.12.1** pinned in `probes/tauri/src-tauri/Cargo.toml`.
- **tao 0.37.1** is the underlying windowing library (via Tauri).
- **Scene URL hook** provided in `probes/tauri/src-tauri/src/scene_urls.rs` to receive handoff URLs before tao's problematic normalisation.
- **iOS deployment target and SDK** specified in `probes/tauri/ios/project.yml` via XcodeGen; validated by CI job `tauri-probe` in `.github/workflows/ios.yml`.

## Remediation: known tao integration issues

The probe uses workarounds for two known tao/Tauri issues that affect URL reception on iOS:

1. **Cold launch URL not delivered by tao delegate**: tao's `TaoSceneDelegate` ignores `UISceneConnectionOptions.URLContexts`, so the app-launch URL never reaches `RunEvent::Opened`. Workaround in `scene_urls.rs`: hook `scene:willConnectToSession:options:` (via Objective-C runtime) and extract URLs directly. The iOS scene path is tested in CI; the fallback `RunEvent::Opened` path (macOS/Android) remains unexercised.

2. **Warm URL normalisation by tao**: tao parses URLs using the `url` crate before emitting `RunEvent::Opened`, which lower-cases the scheme and strips tabs/newlines. Validating `Url::as_str()` would accept input the grammar must reject (e.g., `OHAND-TAURI://` lower-cased to `ohand-tauri://`). Workaround in `scene_urls.rs`: hook `scene:openURLContexts:` (via Objective-C runtime) and apply the literal grammar to the raw string before tao parses it.

Both hooks depend on private tao selectors (`TaoSceneDelegate.scene(_:willConnectToSession:options:)` and `scene(_:openURLContexts:)`). A tao upgrade can silently disable these hooks.

**Remediation steps for production:**

1. **Verify tao upgrade compatibility**: Before updating tao or Tauri, verify that the selectors in `scene_urls.rs` still exist and are called at the expected points. CI verification requires `run-events.log` to contain `scene-hook installed=true`, `scene-connect urls=N` and `scene-open urls=N` lines; a missing scene hook fails the build. CI test `tauri-probe` in `.github/workflows/ios.yml` explicitly checks `scene-hook installed` in the logged event kinds.

2. **Fix upstream or document workaround**: 
   - File issue with tao/Tauri for `UISceneConnectionOptions.URLContexts` delivery and URL normalisation.
   - If upstream does not fix by M2, document the Objective-C hook dependency and ensure any future tao maintenance includes a selector-existence check in CI.

3. **Add integration test for scene-hook resilience**: Include `ios/scripts/verify_handoff_evidence.py` in the signed build CI to ensure the hook logged successfully; this is part of the `tauri-probe` job in `.github/workflows/ios.yml` and will be promoted to the main release build in U01.

## Rust core and native boundary preservation

The management shell choice does not affect the core architecture:

- **Rust core** continues to own domain state (SQLite), provider adapters, orchestration, job execution, and deterministic state transitions. No Tauri or webview dependency is introduced to the core.
- **Native (Swift) capture** remains the entry point. `CaptureProbe` (and the production equivalent in C03/C04) saves locally via native protection classes before any handoff. Silent and voice capture do not depend on the shell.
- **Handoff boundary**: Only a stable identifier crosses the process boundary; capture content remains in the native capture container until explicitly retrieved via Rust-backed queries.
- **Settings and retrieval** (management UI) run in Tauri and are reached via `ohand-tauri://` scheme registered in `probes/tauri/src-tauri/Info.ios.plist` (merged into the app plist by tauri-cli).
- **Native effect interfaces** (notification delivery, credential storage, audio playback) remain in Swift; Tauri calls them through native bindings if needed, or Rust handles scheduling/persistence and Swift executes effects.

## Integration steps for U01

1. Move `probes/tauri/` to the production location `app/` and integrate into the signed build pipeline.
2. Ensure the scene-hook workaround is in place and CI-verified as described in Remediation steps above.
3. Wire the handoff receiver to production Rust services (item lookup, settings persistence).
4. Remove or isolate probe stubs (dummy capture lists, test credentials) from the release target.
5. Validate that the shell launches to capture on first install (no management/settings list shown first).
6. Verify `ohand-tauri://` scheme is registered in the production app plist.

## Deferred accessibility and edge cases

The following cells from the P07 probe were deferred and remain unmet until the receiving tasks observe them:

- **Keyboard navigation** (amendment 1, U10): The device-procedure step for hardware keyboard Tab/Space/Return was not run because no keyboard was available for the phone. This is a deferral, not a pass; the production shell must be validated with a keyboard before first release.
- **Live VoiceOver summary** (amendment 3, U10): Whether the capture-count summary announces automatically when the count changes (a live status announcement) was not exercised. The static reading is recorded; dynamic announcement must be verified in U10.

Both are management-shell UI concerns (accessibility checklist U10) and do not block the shell selection or U01 integration.

## No accepted alternate

The DESIGN.md stated "Evaluate Tauri 2 with Swift integrations against the actual phone flow before choosing it. If necessary, use a SwiftUI phone shell and consider Tauri for Mac." P09 and P07 measured Tauri on the actual phone. The evidence supports Tauri for M1; no separate SwiftUI shell is built or maintained in parallel. A future Mac management shell (M2+) may use Tauri on macOS or another approach, but that is outside M1 scope.

## Source retention and citations

- Tauri probe specification and build: `probes/tauri/` and `docs/validation/tauri-build.md`.
- Handoff specification and test: `probes/tauri-handoff/` and `docs/validation/tauri-handoff.md`.
- Device evidence and evidence manifest: `docs/validation/device-feasibility.md`, `docs/validation/evidence/device-feasibility/`, and the probe-matrix sweep at `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`.
- CI job verifying scene-hook installation: `.github/workflows/ios.yml` job `tauri-probe`.

Co-Authored-By: Claude Haiku 4.5 <noreply@anthropic.com>
