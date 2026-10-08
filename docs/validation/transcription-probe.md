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

The probe loads audio files (WAV format, typically from the AudioProbe P03) and attempts transcription using `SFSpeechRecognizer` initialized for en-US locale. Results report:

- `Transcribed: <text>` — on-device recognition succeeded; text is the `bestTranscription.formattedString` and confidence is reported as a percentage of the first recognized segment.
- `Partial: <text>` — intermediate result (displayed during active recognition before final result).
- `Speech recognizer not available` — the recognizer is unavailable (may need model download on-device; simulator models are frequently outdated or missing).
- `Error: <reason>` — recognition failed with a specific error (timeout, audio engine failure, unreadable file, or unsupported format).
- `Speech recognition permission denied` — user denied the speech recognition permission.
- `Speech recognition restricted` — system has restricted speech recognition access.
- `No audio file loaded` — no WAV file was loaded before transcription was attempted.
- `Recognizer not available` — recognizer is `nil` or not `isAvailable`.

Duration is measured from transcription start to final result, excluding partial results.

### Model availability and language support

`SFSpeechRecognizer.isAvailable` returns false when:

- The device does not support on-device speech recognition (very rare on current iOS devices, but possible on older hardware).
- The language model for the requested locale has not been downloaded on the device.
- The system is in a state that does not permit on-device recognition (e.g., setup/recovery mode).

A missing model is reported as `Speech recognizer not available` and requires explicit user download via Settings > General > Keyboard > Dictation on physical devices. Simulators inherit the host macOS model status.

### Offline verification

The probe intentionally does NOT disable network access. Because Apple's Speech framework is designed to silently fall back to cloud recognition, the probe documentation states what **can** be verified on simulator and what requires physical-device testing:

- **Simulator:** Build and app launch work; recognizer availability is checkable; permission prompts and denials work.
- **Physical device:** Actual offline recognition with network unavailable (airplane mode), language/model availability after explicit download, confidence scoring, interruption handling.

A cloud-fallback-detection test would require network interception (proxy, VPN, or real network isolation), which is outside the scope of the probe; it is noted as evidence requirement for later integration.

## Audio format and constraints

- **Format:** 16 kHz, mono, 16-bit signed little-endian Linear PCM in a WAV file. The AudioProbe (P03) produces this format natively.
- **Locale:** en-US (hardcoded for this probe; additional locales can be added to support language model availability testing).
- **Audio path:** Load any `.wav` file from the app's Documents directory. The Load Test Audio button loads a WAV file from the app container (typically from AudioProbe).
- **Duration bound:** No enforced maximum; `SFSpeechURLRecognitionRequest` has platform-dependent timeouts.

### Permission and model state

- **Speech recognition permission:** Requested at app launch. Denial is reported immediately; revocation in Settings appears only after app relaunch.
- **Language model:** Checked via `SFSpeechRecognizer.isAvailable` and `supportsOnDeviceRecognition` after initialization. If unavailable or on-device recognition is not supported, the status is reported with a visible UI indication. The simulator may report unavailable or unsupported even if the OS version ostensibly supports it.

## What the automated checks assert

The `ios.yml` build compiles the probe app and runs a smoke test on a simulator, which validates:

- The app builds without error with the Speech framework and `SFSpeechRecognizer` integration.
- Recognizer initialization with `supportsOnDeviceRecognition` check and permission request flow work (no crashes).
- The app launches and displays the probe UI.
- The Xcode build environment and iOS SDK are functional for Speech framework use.

The simulator checks do NOT prove:

- Actual offline recognition (simulator may lack model data or have outdated models).
- Language/model availability for locales other than en-US.
- Cloud-fallback prevention (requires network isolation; the probe sets `requiresOnDeviceRecognition = true` but network availability is not controlled in CI).
- Button interactions or transcription end-to-end (smoke test only checks launch).
- Actual microphone recording (the probe loads pre-recorded audio files, not live capture).

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
