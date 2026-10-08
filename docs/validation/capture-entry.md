# Probe native control handoff and protected ingress (P02)

P02 measures supported system-control/shortcut handoff to a native capture screen and validates that the system can create durable ingress records with stable capture identifiers. It is a probe, not the production capture implementation.

## Implementation

Use iOS 18+ system controls (WidgetKit ControlWidget) to launch the capture screen from the lock screen or Control Center, where available. A ProbeOpenCaptureIntent (AppIntent) routes all entry paths to a single foreground capture screen. Durable ingress records use the Rust capture store established in D02, called via the Rust-to-Swift boundary proved in P01.

| Path | Role |
| --- | --- |
| `ios/CaptureProbe/Sources/AppDelegate.swift` | App lifecycle and main capture screen. |
| `ios/CaptureProbe/Shared/ProbeOpenCaptureIntent.swift` | OpenIntent and destination enum; compiled into both app and control targets. |
| `ios/CaptureProbe/Control/ProbeControlBundle.swift` | iOS 18+ system control definition. |
| `ios/CaptureProbe/Sources/Boundary/` | Rust boundary for durable store access (copied from P01 BridgeProbe). |
| `ios/CaptureProbe/Tests/` | Unit tests for ingress record persistence, idempotency and lock state. |

The CaptureProbe target (application) and CaptureProbeControl target (app-extension) are defined in `ios/project.yml`.

## Ingress record implementation

A capture ingress record is created each time the capture screen is entered. The ViewController generates a unique capture ID per entry, accepting one from the intent handoff if available. The record is saved via the Rust boundary for validation:

- **Capture ID**: A UUID generated per entry, or passed through the ProbeOpenCaptureIntent handoff if available. This single ID persists across retries and relaunch for the same entry (idempotency), but a new entry gets a new ID.
- **Entry timestamp**: ISO8601 timestamp captured at record creation.
- **Lock state**: Device lock state from `UIApplication.shared.isProtectedDataAvailable`.
- **Timezone and locale**: System TimeZone and Locale identifiers.
- **Scope**: Item scope defaults to `personal`; privacy route is `route-local` (no networking).
- **Text content**: Synthetic content "Capture probe ingress entry" for boundary validation.
- **Session topic**: Optional, nil in P02.

The record is passed to `ProbeStore.save()` via the Rust boundary:

```swift
let record = CaptureRecord(
    captureId: captureId,  // per-entry or from handoff
    text: "Capture probe ingress entry",
    // ... other fields ...
)
let result = try store.save(record)
```

This ensures:
- **Stable ID per entry**: Each entry (cold launch, warm launch, or control activation) gets a unique ID, or receives one from the handoff and reuses it for retries.
- **Idempotency**: Saving the same captureId twice returns `idempotentReplay: true` on the second call, but distinct entry IDs create distinct records.
- **File-backed durability**: The Rust capture store uses a file-backed SQLite database at `Application Support/captures.db` (protected with `.complete` file protection) for persistence across app relaunch.
- **Boundary validation**: The Rust capture store accepts and validates the record structure, enforcing idempotency and retrieval with durable storage.

## Tests

Unit tests in `ios/CaptureProbe/Tests/CaptureProbeBoundaryTests.swift` verify:

1. **Save and retrieve**: A record with valid text content can be created and retrieved by its capture ID.
2. **Idempotent save**: Saving the same ID twice within a session; the second returns `idempotentReplay: true`.
3. **Lock state capture**: Records correctly capture `entry_locked: true` and `entry_locked: false`.
4. **Capture ID stability**: A stable ID persists across save and retrieval within a session.
5. **Persistence across reopen**: A record saved to the durable store remains after the store is closed and reopened; verified by creating a new ProbeStore instance and retrieving the record.
6. **Retry idempotency across reopen**: After closing and reopening the store, saving the same record again returns `idempotentReplay: true` on the second call, proving durability and idempotency across app relaunch.

These tests run in the `CaptureProbeTests` target on the simulator via Xcode, with results in `ios-evidence/CaptureProbeTests.xcresult`.

## Simulator evidence

macOS CI (`.github/workflows/ios.yml` simulator job):

1. **Build**: `./scripts/build-simulator.sh CaptureProbe` compiles the app with Rust bindings and file-backed store.
2. **Unit tests**: `xcodebuild test -scheme CaptureProbeTests` runs boundary tests including save, retrieve, idempotency, persistence across reopen, and retry idempotency.
3. **Launch smoke test**: `./scripts/smoke-capture-simulator.sh` boots the simulator, installs CaptureProbe, launches it, waits 3 seconds, verifies the process is running, reads and verifies the ingress record ID from app UserDefaults (where it was stored as a reference), and confirms the record is persisted in the durable store.

Evidence artifacts:
- `CaptureProbe-build.log` — build output
- `CaptureProbeTests.log` — unit test output including persistence and reopen tests, linked to `CaptureProbeTests.xcresult`
- `captureprobe-launch.png` — screenshot of the entry screen
- `smoke-capture.log` — simulator launch, ingress record verification, and confirmation of durable persistence

## Device evidence (not yet collected)

Physical device testing is required for complete validation:

- **System control handoff**: iOS 18+ device with the signed app installed; exercise the control from lock screen and Control Center to launch the capture screen and verify ProbeOpenCaptureIntent routes to CaptureProbeViewController.
- **Cold launch**: Install signed app on device, launch it, verify ingress record is created with the stable ID stored in UserDefaults.
- **Warm launch**: Relaunch the app, verify it reuses the same stable capture ID from UserDefaults, and saves an idempotent record.
- **Lock state**: Test capture on a locked device and after unlock; verify `entry_locked` reflects the actual state at each launch.
- **Persistence across force-quit**: Force-kill the app after capture, relaunch, verify the stable ID is reused and a new idempotent record is created.
- **No background microphone**: The control and capture screen do not request microphone permissions; audio is not part of this probe.

Device evidence will include device metadata, logs confirming ingress records persisted, and screenshots of the control and entry screen.

## Not covered

- Audio recording and transcription are in P03.
- Text entry, editing, and cleanup are deferred to the full capture UI.
- Handoff to a management/detail screen is in P07.
- Voice-triggered entry and accessibility are separate probes.
- Background recording, Siri integration, or always-listening are not part of M1.

## Findings for B01 (Production capture bridge)

On simulator (verified):
- The Rust boundary (`ProbeStore`) accepts, validates, and retrieves CaptureRecord values with idempotency tracking; the file-backed store persists records across app relaunch.
- Lock state is available at app launch through `UIApplication.shared.isProtectedDataAvailable`.
- Each entry generates a unique capture ID, or accepts one from the intent handoff, and reuses it for retries until persisted.
- Unit tests verify save, retrieve, idempotency within a session, and persistence across store reopen with retry idempotency.
- The smoke test verifies that the app can launch, the ingress record ID is created and accessible, and the record is durable in the file-backed store.

On device (pending):
- System controls (iOS 18+) can reliably launch the app and route through ProbeOpenCaptureIntent.
- Cold and warm launches both reuse the same stable ID from UserDefaults.
- The control surface can be compiled into both the app and extension targets without namespace conflicts.
- Device lock state at various points in the app lifecycle is captured accurately.

The production bridge (B01) will extend this with actual audio input, UI state restoration, and the full handoff contract to a detail screen or management view. The Rust boundary call pattern and ingress record structure proved here are reusable, though production will use a file-backed or app-managed durable store rather than UserDefaults for sensitive capture metadata.
