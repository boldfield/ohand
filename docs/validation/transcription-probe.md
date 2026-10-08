# Offline on-device transcription capabilities (P04)

P04 probes native on-device speech recognition and language model availability independently of cloud providers. It records no result for any commit: the `ios.yml` run for the submitted commit is the automated evidence and is linked from the pull request. Everything under "Needs a physical device" is **not measured**.

## What exists

| Path | Role |
| --- | --- |
| `ios/TranscriptionProbe/Sources/AppDelegate.swift` | Probe screen with Load Test Audio / Transcribe / Cancel, live transcription status, duration measurement, and result text. Uses `SFSpeechRecognizer` for on-device recognition with `requiresOnDeviceRecognition = true` to prevent cloud fallback. |
| `ios/TranscriptionProbe/Info.plist` | App metadata, `NSSpeechRecognitionUsageDescription` and `NSMicrophoneUsageDescription`. |
| `ios/project.yml` | `TranscriptionProbe` app target and build configuration. |
| `.github/workflows/ios.yml` | Builds and smoke-tests the `TranscriptionProbe` app target on simulator to verify Speech framework integration compiles and the UI initializes. |

## Outcome contract

The probe loads audio files (WAV format) and attempts transcription using `SFSpeechRecognizer` initialized for en-US locale with `requiresOnDeviceRecognition = true`. This setting prevents cloud fallback; if on-device recognition is unavailable or unsupported, the request fails and returns an error. Results report:

- `Transcribed: <text>` — on-device recognition succeeded; text is the `bestTranscription.formattedString` and confidence is reported as a percentage of the first recognized segment.
- `Partial: <text>` — intermediate result (displayed during active recognition before final result).
- `Recognizer not available` — the recognizer is unavailable (may need model download on-device).
- `On-device speech recognition not supported on this device` — the device does not support on-device speech recognition.
- `Error: <reason>` — recognition failed with a specific error (timeout, audio read failure, model not available, or internal service error).
- `Speech recognition permission denied` — user denied the speech recognition permission.
- `Speech recognition restricted` — system has restricted speech recognition access.
- `No audio file loaded` — no WAV file was loaded before transcription was attempted.
- `Speech recognition permission not authorized` — transcription was attempted without authorization (permission not yet requested or explicitly denied).

Duration is measured from transcription start to final result, excluding partial results.

### Model availability and language support

`SFSpeechRecognizer.isAvailable` returns false when:

- The device does not support on-device speech recognition.
- The language model for the requested locale has not been downloaded on the device.
- The system is in a state that does not permit on-device recognition (e.g., setup/recovery mode).

When a recognizer is unavailable, transcription fails with an error. Model availability on simulators is unpredictable and may not match host system state; physical-device testing is required to verify actual model availability.

### On-device recognition guarantee

The probe sets `requiresOnDeviceRecognition = true`, which instructs the Speech framework to fail the request if on-device recognition is unavailable or not supported on the device. This prevents silent fallback to cloud recognition; if on-device is not available, the probe reports an error instead of sending audio to a remote service.

**Simulator verification (CI):**
- Build and app launch work.
- Recognizer availability and on-device support checks are functional.
- Permission prompts and denials work.
- The UI responds to button interactions and displays transcription/error states.

**Physical device verification (P09):**
- Actual offline recognition with network unavailable (airplane mode).
- Language/model availability for non-en-US locales.
- Confidence scoring and interruption handling.

## Audio format and constraints

- **Format:** 16 kHz, mono, 16-bit signed little-endian Linear PCM in a WAV file.
- **Locale:** en-US (hardcoded for this probe).
- **Audio path:** Load any `.wav` file from the app's Documents directory. Test files can be created independently or transferred via airdrop or other mechanisms.
- **Duration bound:** No enforced maximum; `SFSpeechURLRecognitionRequest` has platform-dependent timeouts.

### Permission and model state

- **Speech recognition permission:** Requested at app launch. Denial is reported immediately; revocation in Settings appears only after app relaunch.
- **Language model:** Checked via `SFSpeechRecognizer.isAvailable` and `supportsOnDeviceRecognition` after initialization. If unavailable or on-device recognition is not supported, the status is reported with a visible UI indication. On simulators, model availability is not guaranteed and should not be relied upon.

## What the automated checks assert

The `ios.yml` workflow compiles the probe app and runs a smoke test on a simulator, which validates:

- The app builds without error with the Speech framework and `SFSpeechRecognizer` integration.
- The app launches on a simulator without crashing.
- The probe UI is visible and responsive.

The simulator checks do NOT prove:

- Actual offline recognition or transcription functionality (simulator may lack model data or have outdated models).
- Language/model availability for locales other than en-US.
- Button interactions or transcription end-to-end flows.
- Behavior with no audio loaded or permission denial.
- Recovery from interruption (background/foreground transitions).

## Needs a physical device (not measured)

Run on a phone with the app installed, then record the values listed for each step.

1. **Model availability:** record whether `SFSpeechRecognizer(locale: Locale(identifier: "en-US")).isAvailable` is true or false at app launch. If false, go to Settings > General > Keyboard > Dictation and check whether the en-US language pack is downloadable/installed.

2. **Offline recognition (airplane mode):** enable Airplane Mode, launch the app, load a WAV from the Documents container (or use a WAV from an earlier recording), tap Transcribe, and record the result: success (text and confidence), timeout, or error. Note the elapsed time. Re-enable network.

3. **Network fallback test (optional, requires setup):** with network available and flight mode off, perform the same recognition. If the result differs significantly from airplane mode (e.g., "offline recognizer not available" vs. "transcribed"), the fallback to cloud is confirmed and should be noted as a future test requirement. This probe does not prevent fallback; it only documents whether it occurs.

4. **Partial results:** record whether intermediate results appear in the UI before the final result, and whether confidence scoring is reported for the best transcription.

5. **Permission states:** deny speech recognition permission in Settings before launching the app; record the permission-denied message. Revoke the permission and relaunch to confirm the revised state is shown.

6. **Long audio:** record 60+ seconds of speech (from AudioProbe or another source), load it, and transcribe. Record the final result text, duration, and whether a timeout occurred (expected if the audio exceeds platform limits).

7. **Interrupt:** start a transcription, then background the app (via home button or app switcher). Record whether recognition continues or stops, and what state is reported when the app returns to foreground.

8. **Corrupt/empty audio:** save an empty or corrupted WAV file to Documents, load it, and transcribe. Record the error message.

## Reuse notes

- Keep the probe focused on language-model and offline availability; do not duplicate capture or encoding logic from AudioProbe.
- The SFSpeechRecognizer initialization and `requestAuthorization` pattern is suitable for production but should be wrapped in a provider adapter for flexibility (future V-series tasks).
- Do not assume simulator success proves device success; model availability and actual offline behavior must be validated on a real device.
- The missing-model case is not recoverable in the app itself; it requires Settings access and a user action. The probe documents this requirement clearly so production can advise users.
