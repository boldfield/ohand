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

A durable capture ingress record is created each time the capture screen is entered. The ViewController creates a stable capture ID on initialization and persists it using the Rust boundary:

- **Capture ID**: A UUID generated once per ViewController instance, stable across retries within the same app launch.
- **Entry timestamp**: ISO8601 timestamp captured at record creation.
- **Lock state**: Device lock state from `UIApplication.shared.isProtectedDataAvailable`.
- **Timezone and locale**: System TimeZone and Locale identifiers.
- **Scope**: Item scope defaults to `personal`; privacy route is `route-local` (no networking).
- **Session topic**: Optional, nil in P02.

The record is passed to `ProbeStore.save()` via the Rust boundary:

```swift
let record = CaptureRecord(
    captureId: captureId,
    // ... other fields ...
)
let result = try store.save(record)
```

This ensures:
- **Idempotency**: Saving the same captureId twice returns `idempotentReplay: true` on the second call.
- **Durability**: After successful save, the record is persisted in the Rust SQLite store and survives app restart.

## Tests

Unit tests in `ios/CaptureProbe/Tests/CaptureProbeBoundaryTests.swift` verify:

1. **Save and retrieve**: A record can be created and retrieved by its capture ID.
2. **Idempotent save**: Saving the same ID twice; the second returns `idempotentReplay: true`.
3. **Lock state capture**: Records correctly capture `entry_locked: true` and `entry_locked: false`.
4. **Multiple captures**: Different capture IDs are independent and retrievable.

These tests run in the `CaptureProbeTests` target on the simulator via Xcode, with results in `ios-evidence/CaptureProbeTests.xcresult`.

## Simulator evidence

macOS CI (`.github/workflows/ios.yml` simulator job):

1. **Build**: `./scripts/build-simulator.sh CaptureProbe` compiles the app with Rust bindings.
2. **Unit tests**: `xcodebuild test -scheme CaptureProbeTests` runs boundary tests.
3. **Launch smoke test**: `./scripts/smoke-capture-simulator.sh` boots the simulator, installs CaptureProbe, launches it, waits 3 seconds, verifies the process is running, captures a screenshot.

Evidence artifacts:
- `CaptureProbe-build.log` — build output
- `CaptureProbeTests.log` — unit test output and `CaptureProbeTests.xcresult`
- `captureprobe-launch.png` — screenshot of the entry screen
- `smoke-capture.log` — simulator launch verification

## Device evidence (not yet collected)

Physical device testing is required for complete validation:

- **System control handoff**: iOS 18+ device with the signed app installed; exercise the control from lock screen and Control Center to launch the capture screen.
- **Cold/warm launch**: Capture multiple times; verify the app stays running and each entry generates a unique, stable ingress record.
- **Lock state**: Test capture on a locked device and after unlock; verify `entry_locked` reflects the actual state.
- **Persistence across relaunch**: Force-kill the app, relaunch, retrieve the ingress records created before the kill; confirm all persisted.
- **No background microphone**: The control and capture screen do not request microphone permissions; audio is not part of this probe.

Device evidence will include device metadata, logs confirming ingress records persisted, and screenshots of the control and entry screen.

## Not covered

- Audio recording and transcription are in P03.
- Text entry, editing, and cleanup are deferred to the full capture UI.
- Handoff to a management/detail screen is in P07.
- Voice-triggered entry and accessibility are separate probes.
- Background recording, Siri integration, or always-listening are not part of M1.

## Findings for B01 (Production capture bridge)

On simulator:
- The Rust boundary (`ProbeStore`) can persist and retrieve CaptureRecord values idempotently.
- Lock state is available at app launch through `UIApplication.shared.isProtectedDataAvailable`.
- A stable capture ID can be maintained across app launches if stored durably.

On device (pending):
- System controls (iOS 18+) can reliably launch the app and call OpenIntent handlers.
- Cold and warm launches both create new ingress records with stable, unique IDs.
- The control surface can be compiled into both the app and extension targets without namespace conflicts.

The production bridge (B01) will extend this with actual audio input, UI state restoration, and the full handoff contract to a detail screen or management view. The Rust boundary and ingress persistence mechanism proved here are reusable.
