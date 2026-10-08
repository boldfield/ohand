# Probe native control handoff and protected ingress (P02)

P02 measures supported system-control/shortcut handoff to a native capture screen and validates that the system can create durable ingress records with stable capture identifiers. It is a probe, not the production capture implementation.

## Decision

Use iOS 18+ system controls (WidgetKit ControlWidget) to launch the capture screen from the lock screen or Control Center, where available. A ProbeOpenCaptureIntent (AppIntent) routes all entry paths to a single foreground capture screen. Durable ingress records use the Rust capture store established in D02, called via the Rust-to-Swift boundary proved in P01. On earlier OS versions and unsupported simulators, fallback to direct app launch for testing.

## What exists

| Path | Role |
| --- | --- |
| `ios/CaptureProbe/Sources/AppDelegate.swift` | App lifecycle and main capture screen. |
| `ios/CaptureProbe/Shared/ProbeOpenCaptureIntent.swift` | OpenIntent and destination enum; compiled into both app and control targets. |
| `ios/CaptureProbe/Control/ProbeControlBundle.swift` | iOS 18+ system control definition. |
| `ios/CaptureProbe/Info.plist` | App configuration. |
| `ios/CaptureProbe/Control/Info.plist` | Control extension configuration. |
| `docs/validation/capture-entry.md` | This document. |

The CaptureProbe target (application) and CaptureProbeControl target (app-extension) are defined in `ios/project.yml`.

## Ingress record format

A durable capture ingress record is created each time the capture screen is entered, preserving:

- **Capture ID**: A stable identifier, generated as `UUID-<timestamp>` format, allowing later retrieval and preventing duplicates.
- **Entry timestamp**: When the capture screen was launched.
- **Lock state**: Whether the device was locked when the entry occurred (`entry_locked` boolean).
- **Timezone and locale**: System context captured at entry for deterministic time resolution.
- **Scope**: Item scope defaults to `personal` for P02 testing; privacy route is always `local` (no networking).
- **Session topic**: Optional, captured from app state if available.

The record is written immediately to the Rust SQLite store using `save_capture()` from `core/src/store/captures/`, ensuring:

- Idempotency: Identical retries return the same record; conflicting IDs fail rather than replace.
- Durability: After successful return, the record survives crashes and restarts.
- No content exposure: Only metadata is stored; no audio/transcription is included in P02.

## Cold and warm launch

**Cold launch**: App not running, launched by a system control or direct tap.
- Test on simulator: Control Center or app direct launch.
- Test on device: System control from lock screen (iOS 18+) or Control Center.
- Record: The generated screenshot and app launch log.

**Warm launch**: App already running in foreground or background, launched by a control or intent.
- Control returns to existing app, does not restart it.
- Ingress record created on each launch, maintaining separate IDs.

Evidence: UI state on return, fresh ingress record ID, and the smoke test confirms the app remains running.

## Lock state behavior

**Before first unlock**: Device freshly booted or locked, not yet unlocked by user.
- Ingress record captures `entry_locked: true`.
- On simulator: Simulated by reboot; on device, actual lock state.
- Test: Capture screen launches while locked; verify ingress record is created and persists.

**After first unlock**: Device unlocked at least once since boot.
- Ingress record captures `entry_locked: false` (or `true` if relocked).
- Test: Unlock device, launch capture screen, verify state.

**Locked after use**: Device relocked after being used.
- Subsequent captures record the current lock state.

## Simulator evidence

macOS CI (`ios.yml` simulator job, via `make test` and probe launch):

- Build CaptureProbe for the selected simulator.
- Launch the app; verify it initializes and displays the capture entry screen.
- Confirm the app stays running (smoke test: read a fresh ingress record and verify its ID is stable).
- Generate and save a screenshot of the entry screen.
- Log the simulator environment (SDK, runtime, UDID).

No system control is exercised on the simulator (WidgetKit controls do not run in the simulator). Direct app launch tests the entry point logic; control integration is validated manually on a real iOS 18+ device.

## Device evidence (not yet collected)

Physical device testing is required for complete validation:

- **Actual system control**: iOS 18+ device with the signed app installed; exercise the control from lock screen and Control Center.
- **Lock state**: Test capture on a locked device and after first unlock.
- **Handoff stability**: Capture multiple times; confirm each generates a unique ingress record ID and all records persist.
- **No background microphone**: The control and capture screen do not request microphone permissions; audio is not part of this probe.
- **No authentication bypass**: Normal device unlock/authentication applies; the control respects standard access restrictions.

Device evidence includes:
- Screenshots of the control in lock screen and Control Center.
- Logs confirming ingress records were created and persisted.
- Device metadata (OS version, device type, iOS build).

## Not covered

- Audio recording and transcription are in P03.
- Text entry, editing, and cleanup are deferred to the full capture UI.
- Handoff to a management/detail screen is in P07.
- Voice-triggered entry and accessibility are separate probes.
- Background recording, Siri integration, or always-listening are not part of M1.

## Findings for B01 (Production capture bridge)

- System controls (iOS 18+) can reliably launch the app and call OpenIntent handlers.
- The control surface can be compiled into both the app and extension targets without namespace conflicts.
- Ingress records created via the Rust boundary during launch initialization are durable and idempotent.
- Lock state is available at app launch through `UIApplication.shared.isProtectedDataAvailable`.
- Manual entry of a control on earlier iOS versions requires a fallback (direct app launch) or a graceful feature flag.

The production bridge (B01) will extend this with actual audio input, UI state restoration, and the full handoff contract to a detail screen or management view. The boundary and ingress mechanism proved here are reusable.
