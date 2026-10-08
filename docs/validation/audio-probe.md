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

- **Synthetic recording with durable file handling.** `AudioRecorder` creates PCM-format audio files in the app's Documents directory. The recorder creates a unique file path for each recording session. `AVAudioRecorder` writes in place; recording finalization is verified by opening the file with `AVAudioFile` and confirming a non-zero frame length.
- **Interruption lifecycle.** The probe observes `AVAudioSession.interruptionNotification` to capture audio interruptions (phone calls, competing audio). When an interruption begins, recording is flagged as interrupted. `AVAudioSession.routeChangeNotification` is registered but does not block recording; route changes do not independently interrupt a recording in progress.
- **Partial-file recovery.** When recording is cancelled or interrupted, the recorder preserves the partial audio file and verifies it is readable before returning its path. Duration and file size are reported. If the partial file is not recoverable (size zero or unreadable), no file path is returned but the interruption reason is preserved.
- **Permission handling.** On startup, the probe requests microphone permission using `AVAudioApplication.requestRecordPermission` (iOS 17+) or `AVAudioSession.requestRecordPermission` (iOS 16). Recording start fails gracefully with a permission-denied status if permission is not granted; the UI displays the permission state.
- **Result tracking.** `AudioRecordingResult` distinguishes three outcomes:
  - **Success:** Recording completed normally and file was finalized and verified readable with final duration and size.
  - **Partial:** Recording was cancelled or interrupted; partial file is recoverable and readable; duration and interruption reason are reported.
  - **Failure:** Recording never started, failed before writing a file, or the partial file is not recoverable; no file path is available; interruption reason explains the failure.
- **UI controls.** The probe provides three buttons:
  - **Start Recording:** Begins recording to a timestamped file in Documents.
  - **Stop Recording:** Ends recording and returns a success result if file finalization succeeds, or a failure result if interrupted.
  - **Cancel:** Explicitly interrupts recording, preserves the partial file if recoverable, and returns a partial result with reason "Cancelled" or a failure result.
  - The screen displays real-time recording duration and the final result (success/partial/failure).
- **Background constraints.** Recording is designed for foreground use. The app does not include `UIBackgroundModes: audio` in its entitlements, so recording will pause if the app is backgrounded and cannot resume automatically. Background audio recording on a real device must be tested separately.

## Audio format and constraints

- **Sample rate:** 16 kHz (industry standard for voice; compatible with ASR).
- **Channels:** Mono (single microphone input).
- **Bit depth:** 16-bit signed integer (CD-quality audio).
- **Codec:** Linear PCM (WAV format), uncompressed, written in place by `AVAudioRecorder`.
- **File location:** `{App Documents}/recording-{UUID}.wav`.
- **File size:** Approximately 32 kB per second of audio (16 kHz × 1 channel × 2 bytes per sample).
- **Duration limits:** The probe enforces no maximum duration limit. On the simulator, recording is limited by available storage (typically gigabytes, sufficient for the test scope). On a real device, foreground recording without `UIBackgroundModes: audio` is limited by microphone availability and system resource constraints (battery, RAM, storage). The probe is tested with recordings up to 2 seconds on the simulator. Device limits (maximum safe duration and minimum required free space) should be measured independently and enforced in production use.
- **Partial file size bounds:** A partial file of 0.3 seconds is approximately 9.6 kB; 1.0 second is approximately 32 kB. These are representative sizes used in test assertions to verify that partial files accumulate audio data during active recording.
- **Background and lock-screen constraints:** The probe does not include `UIBackgroundModes: audio` in its entitlements, so recording will pause and cannot resume automatically if the app is backgrounded. On a locked device without background audio mode, recording will pause when the device locks. Background recording on a real device requires `UIBackgroundModes: audio` and must be tested separately with the app running in the background.

## What the automated checks assert

Unit tests (`AudioProbeRecordingTests`, simulator, hosted in app):

- Recording starts successfully and records audio to the designated file.
- Recording stops and returns a successful result only after the file is finalized and verified readable with non-zero duration and file size (using `AVAudioFile(forReading:)` to confirm `length > 0`).
- Cancelling recording returns a partial result with recoverable file path, duration, and the interruption reason "Cancelled".
- Cancelling recording verifies the partial file is recoverable using `AVAudioFile(forReading:)` before returning its path, or returns a failure if the file is unreadable.
- Recording interrupted by `AVAudioSession.interruptionNotification` (type: began) stops with a partial result; the interruption reason is "Audio interrupted".
- Interrupted recordings are verified readable using `AVAudioFile` before being reported as recoverable; unreadable partial files return an honest failure.
- Interruption followed by `.ended` with `.shouldResume` resumes recording and marks the session with `Interrupted and resumed` to preserve the fact that a gap occurred; the later Stop returns a partial result, not a clean success.
- Partial files from interruptions and cancellations are verified to be readable audio before their paths are returned.
- Stopping without starting returns a failure result with no file path.
- Calling stop twice does not report success twice; recorder and start-time state are cleared after the first stop, and the second stop fails with "No active recording".
- Failed start (e.g., invalid destination path) clears recorder and start-time state; a subsequent stop fails without re-attempting to start.
- Multiple sequential recording sessions can be performed; each session can start and stop independently.
- Audio session is configured in the record category with appropriate options.
- Recording file is created when recording starts and contains audio data when stopped.
- Recorded duration matches the elapsed time (within 0.2 seconds tolerance for timing variance).

The tests run in the hosted `OhAndTests` scheme with `TEST_HOST: AudioProbe.app`. `AudioRecorder` behavior is tested through unit tests with synthetic file I/O and simulated interruption notifications via `NotificationCenter.post()`.

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

- **Microphone permission:** The hosted test target (`OhAndTests` with `TEST_HOST: AudioProbe.app`) runs the app in the test process. The app's `AVAudioApplication.requestRecordPermission` prompt may block or timeout in CI, but the tests are designed to skip if permission is not granted or handle denial gracefully. Recording will be denied with `.denied` permission, and that path is tested. The simulator does not fully enforce iOS data-protection classes.
- **Interruption notifications:** Real system interruptions (phone calls, Bluetooth route changes) are not automatically simulated. Synthetic interruption tests explicitly post `AVAudioSession.interruptionNotification` with `.began` and `.ended(.shouldResume)` to verify the probe's handling. Real interruptions require testing on a physical device.
- **Background recording:** Background recording is not tested on the simulator; the simulator does not enforce background-execution time limits or the lack of `UIBackgroundModes: audio` entitlement. Background recording behavior must be verified on a real device.
- **Data protection:** The simulator does not enforce iOS data-protection classes. Lock-screen and device-lock behavior must be tested on a physical device.

## Measurement procedure on a real device

To verify the probe on a real device:

1. **Permission flow:** Install the app, tap Start Recording, and verify that a system permission dialog appears (if permissions were not previously granted). Grant microphone permission and verify that recording starts. Revoke permission in Settings, then tap Start Recording again and verify that the UI shows "Microphone permission denied" and recording fails.
2. **Recording lifecycle:** Start recording, wait ~2 seconds, then tap Stop. Verify that the UI shows "Saved: 2.0s, ~64000 bytes" (or a similar wall-clock duration and file size) and a WAV file appears in the app's Documents directory (visible via Xcode's File Sharing, iTunes file sharing, or a file browser). Verify the file is playable.
3. **Partial file recovery:** Start recording, wait ~1 second, then tap Cancel. Verify that the UI shows "Partial: 1.0s, ~32000 bytes, Cancelled" and a recoverable WAV file is preserved in the Documents directory. Verify the partial file is playable and contains valid audio.
4. **Interruption handling:** Start recording, then initiate a phone call or system alert (or toggle Bluetooth if a device is connected). Verify that the app reacts to the interruption (stops recording and shows an interruption result with "Audio interrupted"). Verify that a partial file is preserved and contains valid audio. After the interruption ends and if the app resumes, a later Stop should show "Interrupted and resumed" to reflect that a gap occurred.
5. **Background behavior:** Start recording, immediately background the app (home button or swipe), and wait ~2 seconds. Return to the app and tap Stop. On a device without `UIBackgroundModes: audio`, verify that recording paused and the final result shows a partial file with a duration less than the wall-clock time (e.g., 0.1 to 0.3 seconds if backgrounded quickly) and the interruption reason "Audio interrupted". The file should be playable and contain the short pre-pause audio.
6. **Lock screen behavior:** Start recording, immediately lock the device (sleep button), and wait ~2 seconds. Unlock the device and tap Stop. On a device without `UIBackgroundModes: audio`, recording should have paused when the device locked. Verify the final result shows a partial file with reduced duration and the interruption reason. The file should be playable.
7. **File persistence:** After several recording sessions, use Xcode's File Sharing view to examine the app's Documents directory. Verify that each WAV file can be opened in a media player and plays the recorded audio (full for normal stops, partial for cancellations and interruptions).

Record the results of each step (permission state, file creation, file size, duration, audio playability, and whether gaps are preserved) as evidence.

## Reuse notes

- Keep `AudioRecorder` separate from the UI; it is a simple AVAudioRecorder wrapper with interruption observation and can be reused in production capture flows.
- Store partial files with unique UUIDs to avoid collisions and allow recovery of multiple interrupted recordings.
- Distinguish permission denial, interruption, and cancellation in the result so the UI can guide the user appropriately.
- On a physical device, test recording from the lock screen and verify that partial files are preserved even if the app is terminated.
