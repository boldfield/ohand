# Recording interruption and partial-audio recovery (P03)

P03 probes native recording and durable partial-file handling independently of classification or networking. It records no result for any commit: the `ios.yml` run for the submitted commit is the automated evidence and is linked from the pull request. Everything under "Needs a physical device" is **not measured**.

## What exists

| Path | Role |
| --- | --- |
| `ios/AudioProbe/Shared/Recording.swift` | `AudioRecorder` (session state machine, interruption handling, read-back verification), `AudioCaptureEngine` (capture seam), `AVAudioRecorderEngine` (real microphone engine), `AudioRecordingResult`, `AudioRecordingLimits`. Compiled into both the probe app and the test bundle. |
| `ios/AudioProbe/Sources/AppDelegate.swift` | Probe screen with Start / Stop / Cancel, live duration, and the last result text. |
| `ios/AudioProbe/Info.plist` | App metadata and `NSMicrophoneUsageDescription`. |
| `ios/AudioProbe/Tests/AudioProbeRecordingTests.swift` | XCTest suite plus `SyntheticCaptureEngine`. |
| `.github/workflows/ios.yml` | Builds the `AudioProbe` app target (`Build AudioProbe` step, `AudioProbe-build.log`) so `AppDelegate.swift` and the real engine compile in native CI, in addition to the tests. |
| `ios/project.yml` | `AudioProbe` app and `AudioProbeTests` unit-test bundle. The test bundle compiles `AudioProbe/Shared` plus `AudioProbe/Tests` directly (the `CaptureProbeTests` pattern); it has no `TEST_HOST`, so no app class is defined twice and no microphone permission prompt is involved. The bundle is in the `OhAndTests` scheme (run by `ios.yml`) and has its own `AudioProbeTests` scheme. |

## Outcome contract

`AudioRecordingResult.outcome` is one of:

- `saved` — the engine confirmed a successful finish **and** the closed file was read back with `AVAudioFile(forReading:)` as non-empty audio. Reported only by Stop on an uninterrupted session.
- `partial(reason)` — a recoverable, read-back-verified prefix exists at `filePath`. Reasons: `Audio interrupted`, `Cancelled`, `Recording finalization failed` (the engine reported a failed, errored or unconfirmed finish but the prefix is readable).
- `failed(reason)` — no file path is exposed: the recording never started, there is no active session, or the closed file is empty or unreadable (`<reason>; no recoverable audio`). Unreadable bytes are left on disk, not deleted, and not advertised.

Duration is computed from the verified file (frames / sample rate), never from the wall clock. Stop, Cancel and interruption all close the file first and inspect it second; none of them reports anything before the engine's `stopAndClose()` has returned.

### Finalization

`AVAudioRecorderEngine.stopAndClose()` calls `AVAudioRecorder.stop()`, keeps the recorder referenced, and then waits (bounded, default 1 s) for `audioRecorderDidFinishRecording(_:successfully:)`. The wait serves the main run loop in 10 ms slices instead of blocking it, so a callback queued on the main thread can arrive during the wait; there is no deadlock with a main-thread delegate. The engine reports a clean close only when a finish callback from the active recorder arrived **and** its flag was `true` **and** no encode error was reported. A `false` flag, an encode error, or no callback within the bound makes `stopAndClose()` return `false`, which `AudioRecorder` turns into `partial(Recording finalization failed)` when the prefix reads back, or `failed(...)` when it does not. Fail-closed on timeout is deliberate: success is never reported on the strength of the read-back alone. Which of the two cases a real phone produces (callback arriving, or timing out) is part of the device measurements below.

The recorder is cleared only after the wait, so failures delivered during `stop()` are honored. A 300 s auto-stop has already delivered its callback by the time Stop is tapped. Finalization callbacks from any other recorder are ignored.

After the engine closes, the closed file is read back with `AVAudioFile(forReading:)` and must contain frames; the read-back guards the prefix content, the callback guards finalization. Both must hold for `saved`.

### Interruption

On `AVAudioSession.interruptionNotification` `.began` the active session is closed and inspected immediately, the verified prefix is kept, and `onSessionEndedEarly` fires so the UI can show it without waiting for Stop. The next Stop returns that same result and does not touch the engine again. `.ended` (with or without `.shouldResume`) is deliberately ignored: restarting `AVAudioRecorder` on the same URL would truncate the saved prefix, so a gap is never papered over and a new capture needs a new file. iOS does not deliver the finish callback for an interrupted recorder, so closing after `.began` normally runs the full bounded wait and the result is `partial(Audio interrupted)` regardless; the UI therefore updates up to 1 s after the notification. Notifications arriving off the main thread are hopped to main. Route changes are not observed because they are not interruptions.

## Audio format, bounds and constraints

- **Format:** 16 kHz, mono, 16-bit signed little-endian Linear PCM in a WAV file, written in place by `AVAudioRecorder` (no atomic rename; the read-back is what guards a half-written file). About 32 kB per second plus a header.
- **File location:** `Documents/recording-<UUID>.wav` in the probe app's container.
- **Duration bound (enforced):** `AudioRecordingLimits.maxDurationSeconds = 300`, passed to `AVAudioRecorder.record(forDuration:)`, which stops the recorder itself after 300 s (about 9.6 MB). A later Stop then reports `saved` with the verified duration.
- **Free-space bound (enforced):** `minimumFreeBytes = 50_000_000` (about 50 MB, roughly 26 minutes of margin). Start is refused with `Insufficient free space` below it, and with `Free space unknown` if capacity cannot be read (fail closed).
- **Mid-recording failure and auto-stop:** a failure such as disk full, and the 300 s auto-stop, are not pushed to the UI. `isRecording` stays true and the on-screen timer keeps counting (it is wall-clock) until Stop is tapped; the Stop result then carries the verified duration, which is at most 300 s.
- **Background and lock:** the probe has no `UIBackgroundModes: audio`. Without it iOS can suspend the app when it is backgrounded or the device locks, which ends capture without a guaranteed interruption callback. Whatever happens, Stop reports what the read-back finds on disk. Adding the mode is a production decision to make after the device measurements below.
- **Data protection:** the probe does not set a file-protection class, so new files get the container default. The result text includes the file's `protectionKey` (when the platform returns one) so the device run records the real class and whether capture continues, and the file stays readable, while locked.

## What the automated checks assert

`AudioProbeRecordingTests` runs in the simulator job. Interruptions are synthetic: the suite posts `AVAudioSession.interruptionNotification` on a private `NotificationCenter`, and a `SyntheticCaptureEngine` writes a real 8000-frame WAV prefix (0.5 s) through `AVAudioFile` so read-back runs on genuine files. No microphone or permission prompt is needed. Assertions (all fail closed, using `XCTUnwrap`/`AVAudioFile` throws):

- Stop returns `saved` only after the engine closed once and the file reads back with 8000 frames and 0.5 s.
- `AVAudioRecorderEngineFinalizationTests` drive the real `AVAudioRecorderEngine` delegate path with an attached, never-started `AVAudioRecorder` (no microphone): a `true` finish delivered during the stop wait confirms the close; a `false` finish delivered before Stop or during the wait, an encode error, a missing callback within the bound, a callback from an unrelated recorder, and Stop with no recorder all return unclean.
- An engine failure at close yields `partial(Recording finalization failed)` with a readable prefix, never `saved`.
- A corrupt, truncated-to-empty or zero-frame file after close yields `failed(no recoverable audio)` with no path, for Stop, Cancel and interruption.
- Cancel closes first, then reports `partial(Cancelled)` with a readable prefix.
- Interruption `.began` closes immediately, fires `onSessionEndedEarly` once, keeps a readable prefix, and a following Stop returns the same result without a second close.
- `.began` then `.ended(.shouldResume)` does not restart the engine (`startCount == 1`) and Stop is `partial(Audio interrupted)`, not a clean success.
- An interruption while idle is ignored.
- Stop without Start fails; a second Stop fails with `No active recording` and does not close the engine again.
- Permission denial (engine start error) and other failed starts leave no session, no file, and allow a later recording; a second Start during a session is refused and keeps the first.
- Insufficient or unknown free space refuses Start; the 300 s limit reaches the engine; defaults equal the bounds above.
- Sessions run back to back independently.

One further test drives the real `AVAudioRecorderEngine` for 0.3 s. On the hosted simulator the permission is normally undetermined, so the test calls `XCTSkip` with the engine's reason; it is expected to be reported as skipped there and is not evidence of real microphone capture. If capture does start it asserts that `saved` implies a readable non-empty file and that any other result states a reason.

Limits of this evidence: the simulator does not exercise real interruptions, background suspension, device lock, data-protection classes, or the real route/permission behaviour of a phone.

## Needs a physical device (not measured)

Run on a phone, then record the values listed for each step.

1. **Permission:** first Start shows the system prompt; grant it and record `Recording...`. Deny or revoke in Settings, Start again, record the text `Failed to start recording: Microphone permission denied`.
2. **Normal stop:** record about 5 s, Stop. Record whether the result is `Saved` or `Partial ... Recording finalization failed` (the latter means the finish callback was missing or false within 1 s; note how long Stop took). Record the result text (duration should match the elapsed time within about 0.2 s; size about 160 kB), play the WAV from the container.
3. **Cancel:** record about 3 s, Cancel. Record `Partial: ... Cancelled`, duration, size, and that the file plays.
4. **Real interruption:** record, then trigger a phone call, Siri or an alarm. Record whether the UI updates immediately to `Partial: ... Audio interrupted`, the duration against the time the interruption began, and that the file plays. After the interruption ends, confirm nothing resumes and a new Start creates a new file.
5. **Background:** record, switch away for 10 s, return. Record whether an interruption result had already appeared, and otherwise what Stop reports (outcome, duration, size) and whether the audio covers the backgrounded span. This decides whether `UIBackgroundModes: audio` is needed.
6. **Lock:** record, lock for 10 s, unlock, Stop. Record the same fields plus the `protection` class shown in the result and whether audio recorded while locked is present.
7. **Bounds:** record to the 300 s limit once, record the auto-stop behaviour (the UI timer keeps running; note the Stop result's duration), final size and Stop result; with low free space (below 50 MB) record the refusal text.
8. **Process death:** start recording, force-quit the app, relaunch and list `Documents`. Record whether a WAV exists and whether it is playable (the probe does not yet repair an unfinalized WAV header).

## Reuse notes

- Keep the engine behind `AudioCaptureEngine` so production capture can reuse the verification and interruption logic and tests can inject faults without hardware.
- Do not resume into an existing URL after an interruption; open a new file and link the segments at a higher level.
- Keep distinguishing `saved`, `partial` and `failed` so the UI and later pipeline never treat a gap or an unverified file as a clean capture.
