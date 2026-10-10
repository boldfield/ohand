# M1 management shell selection

Status: decision recorded from P09 feasibility evidence on 2026-10-10.

## Decision

**Tauri 2 is selected as the M1 production management shell.**

The native iPhone capture remains independent. Rust core, SQLite durable state, and native Swift capture/services are preserved. The management UI (settings, retrieval, reminders) runs in a Tauri webview accessed via deep-link handoff from the native capture surface.

### Production architecture model

The M1 production app is a **single installed iPhone app** with two UI surfaces:
1. **Native Swift capture surface** (built with App Intents): invoked via Control Center, Shortcuts, or Action button. Saves captures to SQLite via the Rust core.
2. **Tauri webview management shell** (settings, retrieval, reminders): invoked via `ohand-tauri://` deep-link scheme from the native capture surface's "Review in management app" button.

Both surfaces share the same app container, SQLite database, and Rust core backend. The handoff is internal to the app: a deep link from native UI to webview, not inter-process communication. The probes (CaptureProbe and TauriProbe as separate apps) use separate containers and demonstrate the viability of each component; production unifies them into one app bundle with both surfaces integrated.

## Evidence basis

P09 executed the documented probe matrix on the target device (iPhone 16 Pro, iOS 26.6.2) and recorded evidence in `docs/validation/device-feasibility.md`. Relevant findings:

### Handoff and identity preservation (P07)

- **Cold handoff** (shell not running): Tauri probe launched and received the capture identifier. Record shows `webviewReady == false` and handoff interval 4.961 s. Shell remains responsive while capture saves independently. **Assessment**: The 4.961 s launch is within typical Tauri cold-launch bounds on iOS and acceptable for a user flow where the shell is not normally running; the user captures first (< 1 s) and then reviews the result. This is faster than opening Settings or Reminders from within the app.
- **Warm handoff** (shell backgrounded): Second capture handed off; shell lists both identifiers. Record shows `webviewReady == true`.
- **Hostile route rejection**: One invalid URL (`ohand-tauri://capture?captureId=not-a-uuid`) submitted while shell running; rejected and counter incremented ("Handoffs rejected: 1"), no entries added.
- **Secret isolation**: Each handoff file contains only `captureId`, `receivedAtUnixMs`, and `webviewReady`. Capture text never appears in Tauri container. Native entry saves durably before "Review in management app" button is enabled, so shell absence does not block save.

All evidence: `docs/validation/device-feasibility.md` P07 section and `p09-probe-matrix-sweep.json` cells for P07 (cold, warm, hostile route rejection, secret isolation, VoiceOver and large text; keyboard navigation deferred to U10).

### Accessibility (P07 latency session)

- **VoiceOver**: Handoff button announced as button with hint "Opens the management app. Only this entry's identifier is sent." Tauri probe headings navigable by rotor; summary read in full; identifiers read digit-by-digit.
- **Large text**: Tauri probe scales without clipping at maximum accessibility text size. Button remains reachable by scrolling. **Defect on the handoff path**: At maximum accessibility text size, CaptureProbe's "Review in management app" label overflows its button (`device-feasibility.md:160`). This label is the entry point to the chosen shell and is deferred as a remediation item to U10. (Other probe-level defects in AudioProbe, CredentialProbe, TranscriptionProbe noted; production remediation is part of U10.)
- **Keyboard**: Not exercised on device (no hardware keyboard available for phone); deferred to U10.

Evidence: `device-feasibility.md` Accessibility section and latency session P07_accessibility record.

### Build and configuration

- **Tauri 2.12.1** pinned in `probes/tauri/src-tauri/Cargo.toml`.
- **tao 0.37.1** is the underlying windowing library (via Tauri).
- **Scene URL hook** provided in `probes/tauri/src-tauri/src/scene_urls.rs` to receive handoff URLs before tao's problematic normalisation.
- **iOS deployment target and SDK** configured via XcodeGen and Tauri's iOS build (`tauri ios build`); validated by CI job `tauri-probe` in `.github/workflows/ios.yml` and the signed-build CI after U01 integration.

## Remediation: known tao integration issues

The probe uses workarounds for two known tao/Tauri issues that affect URL reception on iOS:

1. **Cold launch URL not delivered by tao delegate**: tao's `TaoSceneDelegate` ignores `UISceneConnectionOptions.URLContexts`, so the app-launch URL never reaches `RunEvent::Opened`. Workaround in `scene_urls.rs`: hook `scene:willConnectToSession:options:` (via Objective-C runtime) and extract URLs directly. The iOS scene path is tested in CI; the fallback `RunEvent::Opened` path (macOS/Android) remains unexercised.

2. **Warm URL normalisation by tao**: tao parses URLs using the `url` crate before emitting `RunEvent::Opened`, which lower-cases the scheme and strips tabs/newlines. Validating `Url::as_str()` would accept input the grammar must reject (e.g., `OHAND-TAURI://` lower-cased to `ohand-tauri://`). Workaround in `scene_urls.rs`: hook `scene:openURLContexts:` (via Objective-C runtime) and apply the literal grammar to the raw string before tao parses it.

Both hooks depend on private tao selectors (`TaoSceneDelegate.scene(_:willConnectToSession:options:)` and `scene(_:openURLContexts:)`). A tao upgrade can silently disable these hooks.

**Remediation steps for production:**

0. **Establish reproducible signed builds**: TauriProbe was hand-signed without a build-time signing record (`device-feasibility.md:49`), and its working-tree state was not recorded (`device-feasibility.md:210`). DESIGN.md:118 requires "reproducible signed builds". The U01 integration must establish a CI-integrated signing process that records the build input (source commit hash, Cargo.lock hash, Xcode version), the signing identity (team/certificate hash), the provisioning profile, and the resulting app bundle hash. This record is kept per release so builds can be audited and reproduced.

1. **Verify tao upgrade compatibility**: Before updating tao or Tauri, verify that the selectors in `scene_urls.rs` still exist and are called at the expected points. CI verification requires `run-events.log` to contain `scene-hook installed=true`, `scene-connect urls=N` and `scene-open urls=N` lines; a missing scene hook fails the build. CI test `tauri-probe` in `.github/workflows/ios.yml` explicitly checks `scene-hook installed` in the logged event kinds.

2. **Fix upstream or document workaround**: 
   - File issue with tao/Tauri for `UISceneConnectionOptions.URLContexts` delivery and URL normalisation.
   - If upstream does not fix by M2, document the Objective-C hook dependency and ensure any future tao maintenance includes a selector-existence check in CI.

3. **Verify scene-hook in signed build CI**: The current `tauri-probe` job in `.github/workflows/ios.yml` is an unsigned simulator build. After U01 integration, add the same scene-hook resilience check (verifying `scene-hook installed=true` in run-events.log) to the production signed-build pipeline so every release verifies the hook is working.

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
5. Validate that the app launches to the native capture surface on first install (no management/settings list shown first; U01 requirement from m1-plan.md:19).
6. Verify `ohand-tauri://` scheme is registered in the production app plist.

## Deferred accessibility and edge cases

The following cells from the P07 probe and the capture-to-shell handoff path were deferred and remain unmet until the receiving tasks observe them:

- **Keyboard navigation** (amendment 1, U10): The device-procedure step for hardware keyboard Tab/Space/Return was not run because no keyboard was available for the phone. This is a deferral, not a pass; the production shell must be validated with a keyboard before first release.
- **Live VoiceOver summary** (amendment 3, U10): Whether the capture-count summary announces automatically when the count changes (a live status announcement) was not exercised. The static reading is recorded; dynamic announcement must be verified in U10.
- **Review button label overflow at maximum text size** (U10): CaptureProbe's "Review in management app" button label overflows at the largest accessibility text size, as recorded in `device-feasibility.md:160`. This button is the entry point to the management shell and must be fixed before first release. Remediation is part of U10 integration.

All three are accessibility checklist U10 concerns and do not block the shell selection or U01 integration.

## No accepted alternate

The DESIGN.md stated "Evaluate Tauri 2 with Swift integrations against the actual phone flow before choosing it. If necessary, use a SwiftUI phone shell and consider Tauri for Mac." P09 and P07 measured Tauri on the actual phone. The evidence supports Tauri for M1; no separate SwiftUI shell is built or maintained in parallel.

**Tauri versus SwiftUI tradeoff:** Tauri requires private tao selector hooks (`scene_urls.rs`) for reliable URL reception on iOS, creating a maintenance dependency on tao's internal implementation. SwiftUI would avoid this dependency and use standard scene delegate APIs. However, Tauri's self-contained webview provides stronger separation between the management shell and native capture, avoids shipping Swift UI code as production correctness-critical, and offers faster feature velocity through web-based development. The 4.961 s cold-launch latency is acceptable given the user flow (capture happens first; shell is accessed afterwards). The hooks are explicitly verified in CI (remediation steps above), making the dependency visible and maintainable. This tradeoff is accepted for M1.

A future Mac management shell (M2+) may use Tauri on macOS or another approach, but that is outside M1 scope.

## Source retention and citations

- Tauri probe specification and build: `probes/tauri/` and `docs/validation/tauri-build.md`.
- Handoff specification and test: `probes/tauri-handoff/` and `docs/validation/tauri-handoff.md`.
- Device evidence and evidence manifest: `docs/validation/device-feasibility.md`, `docs/validation/evidence/device-feasibility/`, and the probe-matrix sweep at `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`.
- CI job verifying scene-hook installation: `.github/workflows/ios.yml` job `tauri-probe`.
