# M1 management shell selection

Status: decision recorded from P09 feasibility evidence on 2026-10-11.

## Decision

**SwiftUI is selected as the M1 production management shell.** One production shell is built; there is no parallel Tauri implementation.

The deciding reason is a gap in the evidence, not a measured advantage of SwiftUI. P09 measured Tauri only as a separate app (TauriProbe) that CaptureProbe hands an identifier to over a URL. Nothing in P09 builds or runs Tauri inside the one installed app that M1 requires (`docs/features/m1-plan.md`, "M1 decisions and limits"). The repository's production host, `OhAndApp` (`ios/project.yml`, bundle identifier `com.boldfield.ohand.app`), is a Swift `UIApplication` that owns its own `AppDelegate`/`SceneDelegate` (`ios/AppAssembly/`) and depends on the `OhAndCaptureControl` extension. The Tauri probe is a `#[tauri::mobile_entry_point]` application (`probes/tauri/src-tauri/src/lib.rs`) whose runtime, tao, installs its own scene delegate. P09 did not test whether the two can be combined into one app and this record does not claim they can. DESIGN.md (Platforms and architecture) says to evaluate Tauri against the actual phone flow and, if necessary, use a SwiftUI phone shell. With no evidence of a supported one-app Tauri host, the fallback applies.

The Rust core and the native capture boundary are preserved either way: the core owns SQLite state and provider orchestration, native Swift owns capture, App Intents, the control extension, notifications and credential storage, and the management shell holds no domain state.

## Evidence considered

All device results come from P09 (iPhone 16 Pro, iOS 26.6.2) in `docs/validation/device-feasibility.md`, with the cell-by-cell sweep at `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`. Figures are transcribed from that document and are not reinterpreted.

### Capture entry latency (P02, CaptureProbe via Control Center)

Touch-down to ready, from screen recordings at one-frame resolution (16.7 ms). Touch-up cannot be observed, so these are upper bounds. Sample sizes are small; p95 equals the maximum.

| Launch | n | Metric | min | median | max |
| --- | --- | --- | --- | --- | --- |
| Cold | 6 | press to first visible | 0.750 s | 0.750 s | 0.967 s |
| Cold | 6 | press to fully drawn | 0.933 s | 0.942 s | 1.150 s |
| Warm | 5 | press to first visible | 0.683 s | 0.700 s | 0.733 s |
| Warm | 5 | press to fully drawn | 0.867 s | 0.883 s | 0.916 s |

In all eleven trials the first visible frame already showed the entry with `Result: Saved`, so durable save is at most the press-to-first-visible figure. Voice (AudioProbe Stop button, n = 5): touch-down to Saved 0.150 s median, 0.183 s maximum. No acceptance budget has been declared for these figures; judging them is outside P10. None of them involve the management shell.

### Handoff to the Tauri probe (P07)

- Cold: the identifier (prefix CB02991B) was listed. CaptureProbe committed at 18:38:51.727Z and TauriProbe received it at 18:38:56.688Z, a handoff interval of 4.961 s. The record states that this commit-to-receipt interval is not trigger-to-ready; no trigger-to-visible-ready time was measured for the Tauri probe, so no judgement about its launch speed is made here.
- Warm: a second handoff listed both identifiers.
- Hostile URL: one URL, `ohand-tauri://capture?captureId=not-a-uuid`, gave "Handoffs rejected: 1" and the list stayed at four.
- Secret isolation: each handoff file holds only `captureId`, `receivedAtUnixMs` and `webviewReady`; the capture text appears nowhere in the Tauri container. The Tauri probe UI exposes identifiers and a rejected-handoff count, not capture content and not a detail view.
- URL delivery needed workarounds: tao 0.37.1 mishandles scene URLs twice, and `probes/tauri/src-tauri/src/scene_urls.rs` patches both through tao's private `TaoSceneDelegate` class and selectors, which `docs/validation/tauri-handoff.md` says a tao upgrade can silently disable.

This is the only measured Tauri result. It shows Tauri can receive an identifier across two apps. It does not measure an in-app transition, and it does not measure content retrieval through Rust.

### Accessibility

- VoiceOver: the "Review in management app" button read with its hint, the Tauri headings were reachable with the rotor, and the Tauri summary read "Captures received: 6. Handoffs rejected: 1."
- Largest text size: the Tauri probe scaled with nothing clipped. CaptureProbe's "Review in management app" label overflowed its button (`device-feasibility.md`, Accessibility). This is the entry point to the management shell, and it is the one measured result that favours Tauri's rendering over the native probe UI. It does not decide the choice, because the overflow is a layout defect in a probe button, not a limit of native UI, and the Tauri probe's screen was a single static page. The fix and its owner are given under Remediation.
- Not exercised and not passed: hardware keyboard navigation and whether the Tauri summary announces count changes as a live status (deferred to U10, amendments 1 and 3).

### Build identity

DESIGN.md (M1 feasibility) requires reproducible signed builds. P09 does not show them. TauriProbe, CaptureProbe and CredentialProbe were signed by hand and no record was written at build time; their working-tree state was not recorded. TranscriptionProbe and NotificationProbe records show `working_tree_clean` false, so revision 257343f3 does not provably identify what was installed (`device-feasibility.md`, build-identity caveat). Only AudioProbe and BridgeProbe state a clean tree. The sanitized signing record deliberately carries no certificate or profile detail and no hash (`docs/validation/signing.md`, Output handling).

## Weighing the options

| | SwiftUI in `OhAndApp` | Tauri in the one installed app |
| --- | --- | --- |
| Measured in P09 | Native capture and control entry (latency above); management screens not measured | Cross-app identifier receipt only; no one-app host built |
| One-app host | `OhAndApp` already exists with its own delegates and the capture control dependency | Unproven: would need Tauri's tao-owned app lifecycle to coexist with, or replace, `AppAssembly`, plus a build and signing path for both |
| Runtime stacks in the app | Swift only, plus the Rust core | Swift capture, App Intents and control, plus tao, a webview, a JS toolchain and private-selector URL hooks |
| Known cost | Large-text layout must be built and validated (U10) | Private tao hooks that a tao upgrade can silently break; unresolved host model |
| Core boundary | Rust core reached through the native bridge (`ios/OhAndCoreBridge`) | Webview would reach the core through an additional bridge not exercised in P09 |

Unmeasured, and therefore not used as reasons: relative in-app transition speed, development velocity, and any code-reuse benefit. SwiftUI is not shown to be faster or more accessible than Tauri; it is the option that needs no unproven host integration.

If a later task wants to reopen the choice, it needs a spike that builds one app hosting Tauri and the existing Swift capture surface, signs it, and measures trigger-to-visible-ready for the management screen on the device. That is not scheduled in M1.

## Integration and build model (SwiftUI)

- **Host:** `OhAndApp` stays the single host, built by XcodeGen from `ios/project.yml`, with `OhAndCaptureControl` embedded as it is now. SwiftUI management screens (settings, retrieval, reminders) live in the same app target and are presented from the existing `AppAssembly` composition. U01 owns that wiring (`app/`, `ios/AppAssembly/`) and must not reimplement the composition root.
- **Capture to management:** the "Review in management app" control becomes an in-app navigation that passes only the stable capture identifier. The P07 `ohand-tauri://` URL handoff is a probe mechanism and is not carried into production. Capture content is read from SQLite on demand.
- **Core and storage:** capture and management share one process, one app container, one SQLite database and one Rust core instance through the production bindings (B01) and service registration (B02). No second backend is created for the shell.
- **Native effects:** notifications, credentials and audio stay in Swift behind the effect interfaces, as now.
- **CI:** the hosted `simulator` job in `.github/workflows/ios.yml` builds and tests `OhAndApp` unsigned. Removing or isolating the probe targets from the release target is a U01 acceptance criterion (m1-plan.md, U01) and the simulator job does not establish it.

## Remediation and owners

1. **"Review in management app" overflow at the largest text size.** Implementation owner: U01, which builds the production control that opens management and must wrap or resize its label without clipping. Validation owner: U10, which must confirm it on the device with large text and VoiceOver. The probe button itself is not shipped. This is not passed by this record.
2. **Keyboard navigation and live VoiceOver announcement.** Owner: U10 only (amendments 1 and 3). Unmet until observed.
3. **Signed builds.** Current state: P08a/P08b delivered signing tooling and one observed signed BridgeProbe install, hosted `ios.yml` is credential-free and unsigned, and `docs/validation/signing.md` states that the tool supports only app-only probes and that schemes with embedded extensions would need one profile per bundle identifier. `OhAndApp` embeds `OhAndCaptureControl`, so the existing tooling cannot yet sign the production app. T10 owns producing the signed candidate with revision, version/build, checksum and signing route/validity metadata (m1-plan.md, T10), and it depends on P08. The multi-bundle-identifier signing support is a gap that must be assigned before T10 runs: the coordinator should create a follow-up task extending the P08b tooling, or give it to T10 explicitly. Until then no production-app signed build exists, and the hand-signed, unrecorded-tree state of the P09 probes is a recorded gap and not a pass.
4. **Mac shell.** Out of scope for M1 (DESIGN.md, M2). The Tauri probe, its documents and its evidence are retained only as an input to that later evaluation. Every probe ran on iPhone, so there is no macOS evidence for or against Tauri.

## Source retention

- Keep `probes/tauri/` (including `src-tauri/src/scene_urls.rs`), `docs/validation/tauri-build.md` and `docs/validation/tauri-handoff.md` as reference. They are not a production dependency and U01 must keep them out of the release target.
- Device evidence: `docs/validation/device-feasibility.md`, `docs/validation/evidence/device-feasibility/` and the sweep file named above.
- Production host: `ios/project.yml`, `ios/AppAssembly/`, `ios/Capture/`.
- CI: `.github/workflows/ios.yml` jobs `simulator` and `tauri-probe` (the latter exercises the probe only).

## Integration steps for U01

1. Add the SwiftUI management screens to the `OhAndApp` composition without replacing the composition root.
2. Wire in-app navigation from the capture surface to the management screens, passing only the capture identifier.
3. Resolve real production services, not probe stubs, and share the single Rust core and SQLite store.
4. Remove or isolate probe targets from the release target.
5. Keep the launch criterion: the app launches to capture; retrieval and settings are secondary.
6. Build the management entry control so its label does not clip at the largest text size (Remediation 1).
