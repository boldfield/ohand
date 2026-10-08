# On-device transcription probe (P04)

P04 probes whether `SFSpeechRecognizer` can transcribe synthetic audio **on the device, with no cloud fallback**, and how the probe behaves when it cannot. It records no result for any commit: the `ios.yml` run for the submitted commit is the automated evidence and is linked from the pull request. Everything under "Needs a physical device" is **not measured**, and per the M1 plan offline recognition, language/model availability and timing are certified only by P09.

## What exists

| Path | Role |
| --- | --- |
| `ios/TranscriptionProbe/Shared/TranscriptionModel.swift` | Testable core (Foundation only): `SpeechPermission`, `RecognizerSnapshot`, `evaluateReadiness` (authorization, recognizer existence, on-device support and availability to a `PendingReason` or nil), `TranscriptionStatus`, `TranscriptionCoordinator` (state machine, cancellation, duration, interruption log), `TranscriptionFixture` (deterministic fixture lookup). |
| `ios/TranscriptionProbe/Shared/SpeechFrameworkEngine.swift` | The only code that touches `SFSpeechRecognizer`. Compiled into both the probe app and the test bundle. |
| `ios/TranscriptionProbe/Sources/` | Probe screen (`AppDelegate.swift`) and `SyntheticSpeechFixtureGenerator`, which renders a fixed phrase with `AVSpeechSynthesizer`. |
| `ios/TranscriptionProbe/Tests/TranscriptionProbeTests.swift` | XCTest suite with a fake engine, plus tests of the real engine that need no permission or model. |
| `ios/TranscriptionProbe/Info.plist` | `NSSpeechRecognitionUsageDescription` only. The probe transcribes files and never records, so it has no microphone string. |
| `ios/project.yml` | `TranscriptionProbeTests` unit-test bundle (compiles `Shared` and `Tests`, no host app, so no prompt can appear), included in the `OhAndTests` scheme and with its own scheme. |
| `.github/workflows/ios.yml` | Runs the suite through the `OhAndTests` scheme, builds the `TranscriptionProbe` app (`Build TranscriptionProbe`) and launch-smokes it (`Launch TranscriptionProbe smoke test`). |

## On-device requirement (no cloud fallback)

Every request the probe makes is built with `requiresOnDeviceRecognition = true` and `shouldReportPartialResults = false`. Before a request is built, two gates run:

1. `TranscriptionCoordinator.startTranscription` re-reads a fresh `RecognizerSnapshot` and refuses to call the engine unless `evaluateReadiness` returns nil.
2. `SpeechFrameworkEngine.recognize` independently refuses to create a request unless the recognizer exists and `supportsOnDeviceRecognition` is true, and completes with a failure saying no request was made.

When either gate fails nothing is sent anywhere: the status becomes `Pending`, the audio file is untouched, and no transcript exists. The probe never retries with the flag off and has no setting that turns it off. Whether Apple's framework honours the flag on a given OS build is not something this repo can prove; P09 checks it with the network actually disabled.

## Visible states

The status label shows exactly one of these (text in `TranscriptionStatus.description`):

- `No audio loaded`
- `Ready: on-device recognition is available`
- `Pending: <reason>. Audio kept; no transcription was attempted.` with reason one of: permission not requested yet; permission denied or revoked; restricted on this device; language `<locale>` not supported by the speech framework (no recognizer could be created); on-device recognition unavailable for `<locale>` (model not installed or device unsupported); recognizer for `<locale>` currently unavailable.
- `Transcribing on device...`
- `Transcribed on device in <s>s: "<text>"` with the mean segment confidence when available.
- `Failed after <s>s: <reason>. Audio kept; retry is possible.` (includes an empty transcript, reported as `No speech recognized`, and any framework error shown as `domain code: description`).
- `Cancelled. Audio kept.`

"Model not installed" and "device unsupported" are one state because the public API exposes only `supportsOnDeviceRecognition` before a request; the probe does not claim to distinguish them. A mid-recognition framework error is shown with its raw domain and code and is not mapped to a named cause. Permission is re-read before every run and when the app returns to the foreground, and `SFSpeechRecognizerDelegate` availability changes refresh a pending or ready state (never a running or finished one).

The language selector offers `en-US`, `fr-FR` and `zz-ZZ`. `zz-ZZ` is intentionally not a real locale and always exercises the unsupported-language state; `fr-FR` exercises a language whose on-device model may be absent on a given phone.

## Fixture

The probe needs speech audio without personal content. **Generate Synthetic Fixture** renders the phrase "Remind me to call the dentist tomorrow at nine" with `AVSpeechSynthesizer` to `Documents/transcription-fixture.caf` and loads it. The fixture is also usable without generation: **Load Fixture** looks for exactly `transcription-fixture.wav` and then `transcription-fixture.caf` in this app's `Documents` and loads the first that exists, so the choice is deterministic; no other file is ever picked. A WAV from elsewhere, including the P03 `AudioProbe`, lives in a different app container and is not visible here; to use one, copy it into this app's `Documents` under that name (for example through the Files app or `xcrun devicectl device copy to`). Whether `AVSpeechSynthesizer` produces audio on a hosted simulator is not asserted by any check.

## What the automated checks assert

`TranscriptionCoordinatorTests` uses a fake engine and a fake clock, and checks file bytes after each outcome:

- Readiness table: every permission state, missing recognizer, no on-device support and unavailable recognizer map to their own `PendingReason`; permission is reported first.
- For each pending reason: loading shows `Pending`, a start attempt keeps it `Pending`, the engine's `recognize` is never called, and the audio bytes and attachment are unchanged.
- An unsupported language selection, a permission revoked after `Ready`, a permission request that is granted or denied, and availability changes move the state as described; availability changes leave a running or finished transcription alone.
- A successful run passes the loaded URL and locale to the engine, reports a trimmed transcript and the duration measured by the injected clock; an empty transcript and an engine failure both end as `Failed` with the audio preserved, and a retry works.
- Cancel sets `Cancelled`, cancels the handle, and later error or result callbacks from that run, including after a new run has started, cannot overwrite any state. Duplicate completions and synchronous completion are handled.
- Loading rejects missing or empty files and keeps earlier audio; loading and language changes are refused while running; background and lock notifications are recorded only during a run.
- Fixture lookup picks only the exact names in the fixed order.

`SpeechFrameworkEngineAvailabilityTests` calls the real framework: `zz-ZZ` has no recognizer and is reported as an unsupported language, and `recognize` for `zz-ZZ` fails closed with "no request was made". A third test prints (does not assert) the host's `en-US` snapshot as `OBSERVED host speech availability ...` in `OhAndTests.log`.

The smoke step only proves the app installs and stays running for three seconds. No automated check drives the buttons or performs a real recognition.

Limits of this evidence: simulator results say nothing about the iPhone's installed models, offline behaviour, timing, lock/background interruption, or whether the framework honours the on-device flag.

## Setup requirements (known from the code; device behaviour unverified)

- Speech recognition permission is required (`NSSpeechRecognitionUsageDescription`); the system prompt appears on **Request Permission**.
- iOS 16 is the deployment floor; `supportsOnDeviceRecognition` and `requiresOnDeviceRecognition` exist from iOS 13.
- Which languages have an on-device model, how a missing model gets installed, its size and whether installation needs network are **not documented here**. P09 must record what the target phone shows and how, rather than rely on any claim in this document.

## Needs a physical device (not measured)

Run on the target phone with synthetic content only, and record the values each step lists. Each result needs a named sanitized artifact (screenshot or exported log) with the build revision and collection time.

1. **Snapshot:** OS build, for `en-US`, `fr-FR` and `zz-ZZ`: the status text before and after **Request Permission**.
2. **Offline recognition:** generate the fixture, enable airplane mode (no Wi-Fi, no cellular), press **Transcribe On Device**. Record the status text and whether a transcript appears. Repeat with the network on and compare. If `en-US` shows `Pending: on-device recognition unavailable`, record that and do not claim offline recognition.
3. **Fail-closed check:** with a language whose model is absent (try `fr-FR`), network on, press Transcribe. The expected result is `Pending`, not a transcript. A transcript here would mean the on-device flag was not honoured and must be reported as a failure of this probe.
4. **Permission:** deny, then revoke in Settings, and record the `Pending: ... denied or revoked` text and that the fixture file is still there. Record whether revocation while backgrounded is seen on return.
5. **Timing:** record the duration shown for at least five runs of the fixture and the OS build; do not generalize beyond the observed values.
6. **Interruption:** start a run and immediately lock the phone, then repeat by switching apps. Record the interruption line, the final status and the duration, and whether recognition finished, failed or was suspended.
7. **Cancel:** record that Cancel shows `Cancelled` and that no later status replaces it.
