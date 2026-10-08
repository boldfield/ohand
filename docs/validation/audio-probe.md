# Recording interruption and partial-audio recovery (P03)

P03 probes native recording and durable partial-file handling independently of classification or networking. It exercises interruption handling, permission denial, and file recovery to verify that interrupted captures can be preserved as partially-complete records.

This document reports what the probe implements and what the automated checks assert. It records no result for any commit; the `ios.yml` run for the submitted commit is the evidence, and it is linked from the pull request. Everything under "Needs a physical device" is **not measured**.

## What exists

| Path | Role |
| --- | --- |
| `ios/AudioProbe/Sources/Recording.swift` | `AudioRecorder` (recording lifecycle, interruption handling, route changes) and `AudioRecordingResult` (durable success/failure/partial outcome). Compiled into the probe app and unit tests. |
| `ios/AudioProbe/Sources/AppDelegate.swift` | Scene delegate (foreground state) and the recording test screen with start/stop/cancel controls. |
| `ios/AudioProbe/Info.plist` | Probe app metadata and required permissions. |
| `ios/AudioProbe/Tests/AudioProbeRecordingTests.swift` | XCTest suite (`AudioProbeRecordingTests`, part of `OhAndTests` scheme). |
| `ios/project.yml` | Xcode project configuration for `AudioProbe` app and `AudioProbeTests` unit-test target. |

## Design

- **Synthetic recording with durable file handling.** `AudioRecorder` creates PCM-format audio files in the app's Documents directory. The recorder creates a unique file path for each recording session and writes atomically using Foundation's file I/O.
- **Interruption lifecycle.** The probe observes `AVAudioSession.interruptionNotification` and `AVAudioSession.routeChangeNotification` to capture system interruptions (phone calls, audio route changes, app backgrounding). Recording can be interrupted, paused, or resumed depending on the interruption type and the app's response.
- **Partial-file recovery.** When recording is cancelled (interrupted or explicitly stopped), the recorder preserves the partial audio file and returns its path along with duration and file size, allowing recovery of partially-recorded audio if needed.
- **Permission handling.** On startup, the probe requests microphone permission using `AVAudioApplication.requestRecordPermission`. Recording start fails gracefully with a permission-denied status if permission is not granted; the UI displays the permission state.
- **Result tracking.** `AudioRecordingResult` distinguishes three outcomes:
  - **Success:** Recording completed normally; file was fully saved with final duration and size.
  - **Partial:** Recording was cancelled or interrupted; partial file exists and can be recovered; duration and interruption reason are reported.
  - **Failure:** Recording never started or failed before writing a file; no file path is available; interruption reason explains the failure.
- **UI controls.** The probe provides three buttons:
  - **Start Recording:** Begins recording to a timestamped file in Documents.
  - **Stop Recording:** Ends recording and returns a success result.
  - **Cancel:** Interrupts recording, preserves the partial file, and returns a partial/failure result.
  - The screen displays real-time recording duration and the final result (success/partial/failure).

## Audio format and constraints

- **Sample rate:** 16 kHz (industry standard for voice; compatible with ASR).
- **Channels:** Mono (single microphone input).
- **Bit depth:** 16-bit signed integer (CD-quality audio).
- **Codec:** Linear PCM (WAV format), uncompressed.
- **File location:** `{App Documents}/recording-{UUID}.wav`.
- **File size:** Approximately 32 kB per second of audio (16 kHz × 1 channel × 2 bytes per sample).
- **Maximum duration:** System-dependent; on iOS, foreground recording is limited by available storage and device power. No hard limit is enforced in the probe; interrupted recording before system limit preserves the partial file.

## What the automated checks assert

Unit tests (`AudioProbeRecordingTests`, simulator, no host app):

- Recording starts successfully and records audio to the designated file.
- Recording stops and returns a successful result with non-zero duration and file size.
- Cancelling recording returns a partial result with file path, duration, and interruption reason preserved.
- Stopping without starting returns a failure result with no file path.
- Multiple sequential recording sessions can be performed; each session can start and stop independently.
- Audio session is configured in the record category with appropriate options.
- Recording file is created when recording starts and contains audio data when stopped.
- Recorded duration matches the elapsed time (within 0.2 seconds tolerance for timing variance).

The tests do not instantiate the scene delegate or view controller (the unit-test bundle has no host app); `AudioRecorder` is tested in isolation. Interruption and route-change notifications are observed but not simulated in the test bundle.

Simulator smoke test: The probe UI runs and allows the user to start, stop, and cancel recording. Recording files are created in the app's Documents directory with predictable names.

| Phase | Action | Expected result |
| --- | --- | --- |
| 1 | Launch app | Probe screen shows "Ready" with Start/Stop/Cancel buttons |
| 2 | Tap Start Recording | Screen shows "Recording..." and duration counter increments |
| 3 | Wait ~2 seconds | Duration counter shows approximately 2.0s |
| 4 | Tap Stop Recording | Screen shows "Saved: 2.0s, ~64000 bytes" |
| 5 | Tap Start Recording again | New recording session begins |
| 6 | Wait ~1 second | Duration counter shows approximately 1.0s |
| 7 | Tap Cancel | Screen shows "Partial: 1.0s, ~32000 bytes, Cancelled" |

No phase may lose a recording file or fail to update the UI. After each phase, the app remains responsive and ready for the next action.

## Limits of the simulator evidence

- The simulator has full microphone access and does not enforce iOS data-protection classes. Recording always succeeds in the simulator if permissions are granted (simulated).
- Interruption notifications (phone calls, Bluetooth route changes, system audio alerts) are not simulated. The code to handle these notifications is present but not exercised in the simulator.
- Background recording is not tested; the simulator does not enforce background-execution time limits.
- The simulator does not enforce API permissions at the OS level; permission denial must be tested on a real device.

## Needs a physical device (not measured)

- Whether microphone permission requests appear and can be granted/denied.
- Whether recording succeeds when permissions are denied and fails with a permission-denied status.
- Whether recording continues if the device is locked or the app is backgrounded (foreground-only behavior).
- Whether a phone call, Bluetooth audio route change, or system audio alert interrupts recording, and whether partial audio is preserved.
- Actual recording file size and duration accuracy on a real microphone and system audio stack.
- Whether a killed app (force-quit or system termination) leaves a partial file in a recoverable state.
- Audio quality and compression behavior with the chosen sample rate and bit depth.
- Whether the document-directory file is persisted across app restarts and whether it survives a device reboot (backup and data-protection interaction).

## Reuse notes

- Keep `AudioRecorder` separate from the UI; it is a simple AVAudioRecorder wrapper with interruption observation and can be reused in production capture flows.
- Store partial files with unique UUIDs to avoid collisions and allow recovery of multiple interrupted recordings.
- Distinguish permission denial, interruption, and cancellation in the result so the UI can guide the user appropriately.
- On a physical device, test recording from the lock screen and verify that partial files are preserved even if the app is terminated.
