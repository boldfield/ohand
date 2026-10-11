# M1 management shell selection

Status: decision recorded from P09 feasibility evidence on 2026-10-10.

## Decision

**Tauri 2 is selected as the M1 production management shell.**

The native iPhone capture remains independent. Rust core, SQLite durable state, and native Swift capture/services are preserved. The management UI (settings, retrieval, reminders) runs in a Tauri webview within the same app.

### Production architecture model

The M1 production app is a **single installed iPhone app** (OhAndApp, as built by XcodeGen in `ios/project.yml`), composed of:
1. **Native Swift capture surface** (built with App Intents): invoked via Control Center, Shortcuts, or Action button. Saves captures to SQLite via the Rust core.
2. **Tauri webview management shell** (settings, retrieval, reminders): embedded as a view in the same app. Navigation from capture to management shell is in-app view switching, not inter-process communication.

Both surfaces share the same app container, SQLite database, and Rust core backend. U01 will embed the Tauri webview into OhAndApp and wire the handoff from native capture UI to the webview view controller. The probes (CaptureProbe and TauriProbe as separate apps) used separate containers and a URL scheme to demonstrate that each component works independently; production unifies them into one app bundle with both surfaces integrated and no inter-app handoff.

## Evidence basis

P09 executed the documented probe matrix on the target device (iPhone 16 Pro, iOS 26.6.2) and recorded evidence in `docs/validation/device-feasibility.md`. Relevant findings:

### Handoff and identity preservation (P07)

P07 measured URL-scheme handoff between two separate probe apps (CaptureProbe and TauriProbe) to verify that each component works independently:

- **Cold handoff** (shell not running): TauriProbe launched and received the capture identifier via URL scheme. Record shows `webviewReady == false` (the URL arrived at scene connect before the webview was ready) and commit-to-receipt interval 4.961 s. Shell remained responsive while capture saved independently. This interval is not trigger-to-ready; it includes inter-app launch and webview initialization on a separate app's first run. Production in-app navigation will not use this inter-app path.
- **Warm handoff** (shell backgrounded): Second capture handed off; shell listed both identifiers. Record shows `webviewReady == true`.
- **Hostile route rejection**: One invalid URL (`ohand-tauri://capture?captureId=not-a-uuid`) submitted while shell running; rejected and counter incremented ("Handoffs rejected: 1"), no entries added. This validates scheme parsing and input validation.
- **Secret isolation**: Each handoff file contained only `captureId`, `receivedAtUnixMs`, and `webviewReady`. Capture text never appeared in TauriProbe container. Native entry saved durably before "Review in management app" button was enabled, so shell absence did not block save.

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

0. **Establish reproducible signed builds** (owned by P08): TauriProbe was hand-signed without a build-time signing record (`device-feasibility.md:49`), and its working-tree state was not recorded (`device-feasibility.md:210`). DESIGN.md:118 requires "reproducible signed builds". P08 will establish a CI-integrated signing process that records the build input (source commit hash, Cargo.lock hash, Xcode version), the signing identity (team/certificate hash), the provisioning profile, and the resulting app bundle hash. This record is kept per release so builds can be audited and reproduced.

1. **Verify tao upgrade compatibility**: Before updating tao or Tauri, verify that the selectors in `scene_urls.rs` still exist and are called at the expected points. CI verification requires `run-events.log` to contain `scene-hook installed=true`, `scene-connect urls=N` and `scene-open urls=N` lines; a missing scene hook fails the build. CI test `tauri-probe` in `.github/workflows/ios.yml` explicitly checks `scene-hook installed` in the logged event kinds.

2. **Fix upstream or document workaround**: 
   - File issue with tao/Tauri for `UISceneConnectionOptions.URLContexts` delivery and URL normalisation.
   - If upstream does not fix by M2, document the Objective-C hook dependency and ensure any future tao maintenance includes a selector-existence check in CI.

3. **Verify scene-hook in U01 integration**: The current `tauri-probe` job in `.github/workflows/ios.yml` is an unsigned simulator build and must verify `scene-hook installed=true`. During U01 integration, ensure the webview code in the embedded Tauri component includes the scene-hook workaround and that the unsigned simulator build verifies it. After P08 establishes signed-build CI (step 0), the signed pipeline will inherit this verification.

## Rust core and native boundary preservation

The management shell choice does not affect the core architecture:

- **Rust core** continues to own domain state (SQLite), provider adapters, orchestration, job execution, and deterministic state transitions. No Tauri or webview dependency is introduced to the core.
- **Native (Swift) capture** remains the entry point. `CaptureProbe` (and the production equivalent in C03/C04) saves locally via native protection classes before any shell interaction. Silent and voice capture do not depend on the management shell.
- **In-app handoff**: The native capture surface has a "Review in management app" button that navigates to the webview management shell within the same app. Only the stable capture identifier is passed; capture content is retrieved from SQLite on demand by the management UI via Rust-backed queries.
- **Settings and retrieval** (management UI) run in Tauri and are presented as a view within the single OhAndApp. The `ohand-tauri://` scheme registered in the probe's plist is not used for production in-app navigation but may remain as an external entry point if another app links to Oh And.
- **Native effect interfaces** (notification delivery, credential storage, audio playback) remain in Swift; Tauri calls them through native bindings if needed, or Rust handles scheduling/persistence and Swift executes effects.

## Integration steps for U01

1. Integrate the Tauri webview component from `probes/tauri/` into the OhAndApp host (as part of `app/` per U01's ownership in m1-plan.md). Embed it as a view/view controller within the existing Swift UIApplication composition.
2. Ensure the scene-hook workaround (from `probes/tauri/src-tauri/src/scene_urls.rs`) is in place and CI-verified as described in Remediation step 1 and 3 above.
3. Wire the in-app handoff from the native capture "Review in management app" button to the webview view controller, passing the stable capture identifier.
4. Wire the handoff receiver to production Rust services (item lookup, settings persistence).
5. Remove or isolate probe stubs (dummy capture lists, test credentials) from the release target.
6. Validate that the app launches to the native capture surface on first install (no management/settings list shown first; U01 requirement from m1-plan.md:19).

## Deferred accessibility and edge cases

The following cells from the P07 probe and the capture-to-shell handoff path were deferred and remain unmet until the receiving tasks observe them:

- **Keyboard navigation** (amendment 1, U10): The device-procedure step for hardware keyboard Tab/Space/Return was not run because no keyboard was available for the phone. This is a deferral, not a pass; the production shell must be validated with a keyboard before first release.
- **Live VoiceOver summary** (amendment 3, U10): Whether the capture-count summary announces automatically when the count changes (a live status announcement) was not exercised. The static reading is recorded; dynamic announcement must be verified in U10.
- **Review button label overflow at maximum text size** (U10): CaptureProbe's "Review in management app" button label overflows at the largest accessibility text size, as recorded in `device-feasibility.md:160`. This button is the entry point to the management shell and must be fixed before first release. Remediation is part of U10 integration.

All three are accessibility checklist U10 concerns and do not block the shell selection or U01 integration.

## No accepted alternate

The DESIGN.md stated "Evaluate Tauri 2 with Swift integrations against the actual phone flow before choosing it. If necessary, use a SwiftUI phone shell and consider Tauri for Mac." P09 and P07 measured Tauri on the actual phone. The evidence supports Tauri for M1; no separate SwiftUI shell is built or maintained in parallel.

**Tauri versus SwiftUI tradeoff:** Production ships the same Swift code either way: native capture (Control Center, Shortcuts, Action button entry points), App Intents, credential storage, notification delivery, and audio playback. Under the Tauri choice, the management UI (settings, retrieval, reminders) runs in an embedded webview with private tao selector hooks (`scene_urls.rs`), creating a maintenance dependency on tao's internal implementation. Under a SwiftUI choice, the management UI would extend the existing OhAndApp with new view controllers and SwiftUI screens.

Tradeoffs:
- **Tauri**: Requires tao selector hooks that depend on private APIs. Decouples the management UI codebase from the app's composition root. Offers web-based development velocity and potential code reuse on Mac.
- **SwiftUI**: Avoids private API dependencies and uses standard scene delegate patterns. Requires building and maintaining management UI in Swift alongside the native capture code. Adds complexity to the app's composition and view hierarchy.

The P07 probe evidence demonstrates that Tauri's core components (URL reception, webview rendering, accessibility) work on the actual device. The measured 4.961 s interval was inter-app cold launch; in-app view switching will be significantly faster and was not separately measured. The tao selector hooks are explicitly verified in CI (remediation steps above), making the dependency visible. This tradeoff is accepted for M1.

A future Mac management shell (M2+) may use Tauri on macOS or another approach, but that is outside M1 scope.

## Source retention and citations

- Tauri probe specification and build: `probes/tauri/` and `docs/validation/tauri-build.md`.
- Handoff specification and test: `probes/tauri-handoff/` and `docs/validation/tauri-handoff.md`.
- Device evidence and evidence manifest: `docs/validation/device-feasibility.md`, `docs/validation/evidence/device-feasibility/`, and the probe-matrix sweep at `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`.
- CI job verifying scene-hook installation: `.github/workflows/ios.yml` job `tauri-probe`.
