# M1 management shell selection

Status: decision recorded from P09 feasibility evidence on 2026-10-10.

## Decision

**SwiftUI is selected as the M1 production management shell.**

The native iPhone capture remains the entry point. Rust core, SQLite durable state, and native Swift capture/services are preserved. The management UI (settings, retrieval, reminders) runs as SwiftUI screens within the same app.

### Production architecture model

The M1 production app is a **single installed iPhone app** (OhAndApp, as built by XcodeGen in `ios/project.yml`), composed of:
1. **Native Swift capture surface** (built with App Intents): invoked via Control Center, Shortcuts, or Action button. Saves captures to SQLite via the Rust core.
2. **SwiftUI management shell** (settings, retrieval, reminders): screens integrated into the existing OhAndApp composition. Navigation from capture to management shell is in-app view controller switching.

Both surfaces share the same app container, SQLite database, and Rust core backend. U01 will add SwiftUI view controllers for settings, retrieval, and reminders to the existing OhAndApp composition and wire the handoff from the native capture "Review in management app" button to the management screens. The probes (CaptureProbe and TauriProbe as separate apps) used separate containers and a URL scheme to demonstrate each component's feasibility; production unifies them into one app bundle using the platform patterns that the OhAndApp target already implements.

## Evidence basis

P09 executed the documented probe matrix on the target device (iPhone 16 Pro, iOS 26.6.2) and recorded evidence in `docs/validation/device-feasibility.md`. Relevant findings:

### Capture and handoff (P02, P07)

P09 measured the native capture entry and handoff path between separate apps:

- **Capture entry**: Control Center access launches CaptureProbe in 0.750–1.150 s (cold) and 0.683–0.916 s (warm). Voice capture from the Stop button saves in 0.150–0.183 s. Capture saves durably before the management button becomes enabled; the button press does not block the save.
- **Handoff to separate app**: CaptureProbe handoff file contains only `captureId`, `receivedAtUnixMs`, and `webviewReady`. TauriProbe received the identifier and listed it alongside prior captures. Inter-app URL delivery required a 4.961 s cold launch interval (app not running) and workarounds for tao's URL delivery bugs (`scene_urls.rs` hooks). Hostile URLs rejected without affecting state.
- **In-app retrieval**: TauriProbe rendered captured entries from a separate container accessed via the URL handoff. Summary and detail views rendered correctly; text scaled to maximum accessibility sizes without clipping.

All evidence: `docs/validation/device-feasibility.md` P07 section and `p09-probe-matrix-sweep.json` cells for P07 (cold, warm, hostile route rejection, secret isolation, VoiceOver and large text; keyboard navigation deferred to U10).

### Accessibility (P07 latency session)

- **VoiceOver**: Handoff button announced as button with hint "Opens the management app. Only this entry's identifier is sent." Tauri probe headings navigable by rotor; summary read in full; identifiers read digit-by-digit.
- **Large text**: Tauri probe scales without clipping at maximum accessibility text size. Button remains reachable by scrolling. **Defect on the handoff path**: At maximum accessibility text size, CaptureProbe's "Review in management app" label overflows its button (`device-feasibility.md:160`). This label is the entry point to the chosen shell and is deferred as a remediation item to U10. (Other probe-level defects in AudioProbe, CredentialProbe, TranscriptionProbe noted; production remediation is part of U10.)
- **Keyboard**: Not exercised on device (no hardware keyboard available for phone); deferred to U10.

Evidence: `device-feasibility.md` Accessibility section and latency session P07_accessibility record.

### Build and configuration

- **XcodeGen project** continues to build the single OhAndApp target (`ios/project.yml`).
- **SwiftUI framework** used for management screens, integrated alongside existing native capture UI.
- **Rust bindings** for query/persistence from Swift; management screens call Rust-backed queries to retrieve captures and settings.
- **CI coverage** provided by existing iOS simulator job in `.github/workflows/ios.yml` (no additional probe isolation needed).

## Remediation: SwiftUI integration and deferred accessibility items

SwiftUI management screens integrate into the existing OhAndApp composition without private-API workarounds or external URL schemes.

**Remediation steps for production:**

0. **Establish reproducible signed builds** (owned by P08): P09 used hand-signed probe builds without build-time signing records. DESIGN.md:118 requires "reproducible signed builds". P08 owns signing automation and will record the build input, signing identity, provisioning profile, and resulting app bundle hash per release for audit and reproduction.

1. **Fix accessibility defect on the handoff path**: CaptureProbe's "Review in management app" button label overflows at maximum text size (`device-feasibility.md:160`). This button is the entry point to the chosen shell. Fix the label wrapping or button layout to keep it reachable and readable. Owned by U01 during integration.

2. **Verify in-app handoff in U01**: Wire the capture "Review in management app" button to navigate to the management SwiftUI screens, passing the stable capture identifier. Confirm the handoff path is accessible (VoiceOver, large text, keyboard) and that capture and management share the Rust core and SQLite backend correctly.

## Rust core and native boundary preservation

The management shell choice does not affect the core architecture:

- **Rust core** continues to own domain state (SQLite), provider adapters, orchestration, job execution, and deterministic state transitions. No webview dependency is introduced to the core.
- **Native (Swift) capture** remains the entry point. `CaptureProbe` (and the production equivalent in C03/C04) saves locally via native protection classes before any shell interaction. Silent and voice capture do not depend on the management shell.
- **In-app handoff**: The native capture surface has a "Review in management app" button that navigates to the SwiftUI management shell within the same app. Only the stable capture identifier is passed; capture content is retrieved from SQLite on demand by the management UI via Rust-backed queries.
- **Settings and retrieval** (management UI) run as SwiftUI screens within OhAndApp and are presented alongside the native capture interface. No external URL scheme is used for production in-app navigation.
- **Native effect interfaces** (notification delivery, credential storage, audio playback) remain in Swift and are called directly or via Rust when scheduling/persistence is involved.

## Integration steps for U01

1. Add SwiftUI view controllers for management screens (settings, retrieval, reminders) to the OhAndApp composition (as part of `app/` per U01's ownership in m1-plan.md).
2. Wire the in-app handoff from the native capture "Review in management app" button to the management SwiftUI screens, passing the stable capture identifier.
3. Wire the management screens to production Rust services (item lookup, settings persistence, reminder scheduling).
4. Ensure both capture and management share the same SQLite backend and Rust core instance.
5. Remove or isolate probe stubs (dummy capture lists, test credentials) from the release target.
6. Validate that the app launches to the native capture surface on first install (no management/settings list shown first; U01 requirement from m1-plan.md:19).
7. Fix the accessibility defect on the handoff button (step 1 in remediation).

## Deferred accessibility and edge cases

The following cells from the P09 probe matrix were deferred and remain unmet until the receiving tasks observe them:

- **Keyboard navigation** (amendment 1, U10): The device-procedure step for hardware keyboard Tab/Space/Return was not run because no keyboard was available for the phone. This is a deferral, not a pass; the production capture and management UI must be validated with a keyboard before first release.
- **Live VoiceOver summary** (amendment 3, U10): Whether the capture-count summary announces automatically when the count changes (a live status announcement) was not exercised. The static reading is recorded; dynamic announcement must be verified in U10.
- **Review button label overflow at maximum text size** (U01 remediation): CaptureProbe's "Review in management app" button label overflows at the largest accessibility text size, as recorded in `device-feasibility.md:160`. This button is the entry point to the management shell and must be fixed during U01 integration.

These are accessibility and integration concerns and do not block the shell selection.

## Shell choice: SwiftUI for M1, Tauri for future Mac

The DESIGN.md stated "Evaluate Tauri 2 with Swift integrations against the actual phone flow before choosing it. If necessary, use a SwiftUI phone shell and consider Tauri for Mac."

P09 measured Tauri as a **separate app** with inter-app URL handoff. It did not measure or prove that Tauri 2 supports embedding as a webview component inside an existing Swift UIApplication host. Tauri 2's `#[tauri::mobile_entry_point]` macro installs a tao-owned `TaoAppDelegate`/`TaoSceneDelegate` and runs its own UIApplication event loop. The repository already provides `OhAndApp` with its own `AppDelegate`/`SceneDelegate` in `ios/AppAssembly/`. These two patterns cannot be combined without a distinct spike to prove the feasibility.

**SwiftUI for M1:** The existing OhAndApp target can host SwiftUI management screens alongside native capture UI using standard platform patterns: view controller composition, scene delegates, and shared app state. This choice:
- Uses architecture already present in the repository
- Requires no private-API workarounds or tao selector hooks
- Keeps the management UI codebase alongside native capture, simplifying shared state and Rust bindings
- Matches the fallback guidance in DESIGN.md

**Tauri for future Mac (M2+):** A Mac companion shell is required by M2. macOS has different UI patterns and a Tauri app as a native citizen is feasible on macOS. The Tauri probe evidence (working web rendering, accessible interactivity, URL routing) supports using Tauri on Mac.

No separate SwiftUI and Tauri implementations are built in parallel for M1.

## Source retention and citations

- Tauri probe specification and feasibility evidence: `probes/tauri/` and `docs/validation/tauri-build.md`, `docs/validation/tauri-handoff.md`. Retained as reference for future Mac shell evaluation.
- Device evidence and evidence manifest: `docs/validation/device-feasibility.md`, `docs/validation/evidence/device-feasibility/`, and the probe-matrix sweep at `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`.
- OhAndApp composition and AppAssembly: `ios/project.yml` (XcodeGen config), `ios/AppAssembly/` (app delegates and scene delegates).
- CI job for native iOS integration: `.github/workflows/ios.yml` job `simulator` and related lint checks.
