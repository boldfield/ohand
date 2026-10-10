# P09: Actual-device feasibility record

This document transcribes what was observed on the target phone during the two P09 sessions. It adds no measurement of its own. Every result cites a sanitized evidence record committed under `docs/validation/evidence/device-feasibility/`, the build that was installed, and a collection time. It is graded against `docs/features/m1-plan.md#p09`, as stated in the P09 scope amendment in `docs/features/m1-task-refinement.md`.

**Status: not a full certification of the documented probe matrix.** Every category in the first acceptance criterion has device evidence, but a number of documented checks were not run, were not observable, or were deferred. [Unmet checks](#unmet-checks) lists each one precisely. Nothing below should be read as covering them.

## Evidence records

| Short name | Committed file | Collected |
| --- | --- | --- |
| `matrix` | `2026-10-09-device-matrix.json` (`device-feasibility-2026-10-09`) | 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z (as stated in the record) |
| `latency` | `2026-10-10-latency-accessibility.json` (`device-feasibility-2026-10-10-latency-accessibility`) | 2026-10-10T05:18:00Z to 2026-10-10T05:31:00Z |
| `manifest` | `2026-10-09-private-evidence-manifest.json` | generated 2026-10-10T05:49:30Z |

All three are in `docs/validation/evidence/device-feasibility/`.

Only these three JSON files are committed. The screenshots, WAV files, app-container files, logs and screen recordings that the results cite by name are **private** and stay on the maintainer's Mac under `~/.ohand-private-evidence/p09/`. Several show the home or lock screen and must never be published. The `manifest` record is the auditable reference: for every private artifact it lists the relative path, size in bytes, SHA-256, modification time and a `personal` flag. Artifact names below are paths relative to that root, and each appears in the manifest.

Artifact modification times are the finest per-result time available (`manifest.timestamps`: on-device creation time where AirDrop preserved it, otherwise the copy time). Two caveats:

- The eight `audio-probe/recording-*.wav` files have modification times from 2026-10-09T17:11:51Z to 17:40:27Z, and `matrix.collected_at` starts at 17:30:00Z. The record does not explain the difference and it is not reconciled here.
- The `latency/ScreenRecording_*` files carry the copy time (05:32:43Z). Their device-clock start times are in the file names (22-18-54, 22-19-54, 22-20-51 and 22-27-08 America/Los_Angeles, which is 05:18:54Z, 05:19:54Z, 05:20:51Z and 05:27:08Z).

## Device, OS and builds

| Item | Observed | Source |
| --- | --- | --- |
| Device | iPhone 16 Pro (label `trial-phone`) | `matrix.device`, `latency.device` |
| iOS | 26.6.2 (build 23G90), read via `devicectl` | `matrix.ios_version`; `latency.ios_version` states it is unchanged |
| Device time zone | America/Los_Angeles | both records |
| Toolchain (AudioProbe signing record) | Xcode 26.6 (17F113) | `apple-signing-20261009T164701Z-5b3f5c34.json` |

| Probe | Revision | Signing record |
| --- | --- | --- |
| BridgeProbe | 6ac3d1f8 | `evidence/apple-signing/apple-signing-20261009T000932Z-c2cb6485.json` |
| AudioProbe | 257343f3 | `evidence/apple-signing/apple-signing-20261009T164701Z-5b3f5c34.json` |
| TranscriptionProbe | 257343f3 | `evidence/apple-signing/apple-signing-20261009T164718Z-e2dc1dae.json` |
| NotificationProbe | 257343f3 | `evidence/apple-signing/apple-signing-20261009T164732Z-b11e7a0d.json` |
| CredentialProbe | 257343f3 | none: `matrix.builds` says "manual archive + export with the development profile; no tooling record" |
| CaptureProbe (app and control extension) | 257343f3 | none: "manual archive + export; app + control extension; no tooling record" |
| TauriProbe | 257343f3 | none: "tauri ios build --debug, development route; no tooling record" |

The second session (`latency`) reports CaptureProbe, AudioProbe and TauriProbe as the same installs as the first.

Signing behaviour observed: the four tooling records show manual signing, route `development`, export method `debugging`, identity class Apple Development, a device-scoped profile expiring 2027-10-09T00:08:01Z, and a successful install. For the three probes without a tooling record, `manifest.signing_of_probes_without_sign_probe_records` describes the manual procedure used (same development profile, installed with `devicectl`). That description is the maintainer's account; there is no tool-generated record for those three, so their signing is **not** established by a tooling record.

## Results by area

### Capture entry (P02): cold, warm, unlocked, locked

Build CaptureProbe 257343f3. Doc: `docs/validation/capture-entry.md`.

- **Unlocked, Control Center press to entry.** The first session recorded "about 2 s (maintainer estimate)", which is not a measurement and is superseded by the timed trials in [Latency](#latency).
- **From the lock screen.** The control was pressed from the lock screen. No Face ID prompt was observed. The app opened showing only the new entry, and the entry reported `Protected data: available`. The record's reading is that the device was already unlocked by the time the app foregrounded, and that passive unlock was not instrumented. The `latency` record adds that locked-device capture cannot be measured by screen recording, because iOS stops the recording when the device locks. Net result: with this entry mechanism the app opened only after unlock; **capture while the device remained locked was not observed**. Artifacts: `capture-probe/p02-01-control-from-lock-screen-entry.png` (2026-10-09T19:09:57Z).
- **Entry and records.** Entry text `Entry: controlIntent, warm launch; Protected data: available; Result: Saved`. Four control presses produced four distinct records (two cold, two warm), all `protectedDataAvailable` true, with no duplicate `perform()` observed. Artifacts: `capture-probe/container/CaptureProbe/records/*.json` (mtimes 2026-10-09T18:39:50Z to 19:08:41Z in the manifest) and `capture-probe/container/CaptureProbe/last-presented.json`.

### Latency

Collected 2026-10-10T05:18:00Z to 05:31:00Z, builds as above. Record: `latency` (`sections.P02_trigger_to_ready`, `sections.P03_end_of_input_to_save`).

**Method.** iOS screen recordings (1206x2622, nominal 60 fps, variable frame rate) were extracted frame by frame with their presentation timestamps and classified by region brightness; each detected frame was checked by eye. Resolution is one frame (16.7 ms). Trigger is touch-down, the first frame in which the pressed control is highlighted. iOS screen recording does not draw touches, so touch-up cannot be seen and every figure is measured from touch-down, which makes it an upper bound on release-to-ready.

**Trigger-to-ready, Control Center capture control.** Two ready points are reported per trial: `first_visible` (app content first visible inside the launch animation) and `fully_drawn` (animation finished, frame identical to steady state). The conservative figure is `fully_drawn`. Seconds, n = trial count; with n of 5 and 6, p95 by nearest rank is the maximum.

| Launch | n | Metric | min | median | p95 (= max) |
| --- | --- | --- | --- | --- | --- |
| Cold (app force-quit first) | 6 | press to first visible | 0.750 | 0.750 | 0.967 |
| Cold | 6 | press to fully drawn | 0.933 | 0.942 | 1.150 |
| Warm (backgrounded, not force-quit) | 5 | press to first visible | 0.683 | 0.700 | 0.733 |
| Warm | 5 | press to fully drawn | 0.867 | 0.883 | 0.916 |

Per-trial values are in `latency.sections.P02_trigger_to_ready.cold_trials` and `warm_trials`. Artifacts: `latency/ScreenRecording_10-09-2026 22-18-54_1.MP4` (six cold trials), `latency/ScreenRecording_10-09-2026 22-19-54_1.MP4` (five warm trials), `latency/frames/strip-cold-t4.png`, `latency/frames/strip-warm-t4.png`, `latency/frames/cold-entries.png`, `latency/frames/warm-entries.png`. The entry text in the recordings reads `controlIntent, cold launch` for all six cold trials and `warm launch` for all five warm trials.

**End-of-input-to-durable-save, text-style entry via the control.** Not separable from trigger-to-ready on this path. In all eleven trials the first visible app frame already shows the entry with `Result: Saved`, so the record is written before the first frame is drawn. The bound is: durable save is at most `press_to_first_visible` (cold median 0.750 s, max 0.967 s; warm median 0.700 s, max 0.733 s).

**End-of-input-to-durable-save, voice (AudioProbe Stop button).** Stop reports Saved only after the recorder has finished and the file has been read back (`docs/validation/audio-probe.md`, Finalization), so the first frame showing the Saved line is the durable point. Five cycles of about three seconds of speech.

| n | min | median | max (= p95) |
| --- | --- | --- | --- |
| 5 | 0.150 s | 0.150 s | 0.183 s |

Touch-down to Saved per trial: 0.183, 0.150, 0.150, 0.150, 0.150 s. Release lies between touch-down and the Saved frame, so release-to-durable-save is at most these figures. Artifacts: `latency/ScreenRecording_10-09-2026 22-20-51_1.MP4`, `latency/frames/strip-audio-t1.png`.

**Not measured:** trigger-to-ready for a locked device (see above); a text-capture save latency distinct from the control path (no such measurement exists in either record); trigger-to-ready for voice (the AudioProbe Start button was not timed); trigger-to-ready from the home screen, Action button or Shortcuts.

### Voice recording, permission and interruptions (P03)

Build AudioProbe 257343f3. Doc: `docs/validation/audio-probe.md`. Record: `matrix.sections.P03_audio`. Files: `audio-probe/recording-*.wav` (eight files, 16 kHz mono Int16, inspected with `afinfo` and a header/RMS script), each listed with size and SHA-256 in the manifest.

| Check | Observed (as recorded) | Artifact (size from manifest) |
| --- | --- | --- |
| Permission granted | "Recording..."; recording saved (3.33 s file) | not individually identified in the record |
| Permission revoked in Settings, then Start | "Failed to start recording: Microphone permission denied" | not individually identified |
| Normal stop | Saved, 6.92 s file, size consistent with 32000 B/s, plays | not individually identified |
| Cancel | "Partial: 3.86s, 127732 bytes, Cancelled"; protection class shown; file matches and plays | `recording-A35AC340-...wav`, 127732 bytes, 2026-10-09T17:19:38Z |
| Interruption | "Partial: 5.86s, 191764 bytes, Audio interrupted"; protection class shown; file matches and plays. The record does not state what caused the interruption | `recording-6E0C20DF-...wav`, 191764 bytes, 2026-10-09T17:22:24Z |
| 10 s in background | "Recording was not interrupted. Saved: 9.71s, 314676 bytes"; signal present across the span; no `UIBackgroundModes: audio` was needed for 10 s | `recording-795A6E61-...wav`, 314676 bytes, 2026-10-09T17:28:24Z |
| 10 s with screen locked | "Recording was not interrupted. Saved: 8.19s, 266100 bytes"; protection class shown; signal continuous, quieter mid-file | `recording-49906AFF-...wav`, 266100 bytes, 2026-10-09T17:29:38Z |
| Upper bound | UI timer kept running past 500 s; Stop reported "Saved: 300.00s, 9604096 bytes"; header consistent | `recording-EA86CCD8-...wav`, 9604096 bytes, 2026-10-09T17:35:27Z |
| Process death (force-quit while recording) | A WAV exists (54594 bytes) with an unfinalized header (data length 0; 1.58 s of PCM present); `afinfo` duration 0.000, not playable as-is | `recording-AC879EC1-...wav`, 54594 bytes, 2026-10-09T17:40:27Z |

Not tested: low-free-space refusal. Survival of the earlier fixture after microphone permission revoke is covered under P04.

### Offline transcription (P04)

Build TranscriptionProbe 257343f3. Doc: `docs/validation/transcription-probe.md`. Record: `matrix.sections.P04_transcription`. Artifacts: `transcription-probe/p04-01-...png` through `p04-10-...png` (mtimes 2026-10-09T17:51:00Z to 18:02:11Z).

- **Availability snapshot.** Before the request: "Pending: speech recognition permission not requested yet." After grant: en-US "Ready: on-device recognition is available"; fr-FR "Pending: on-device recognition unavailable for fr-FR (model not installed or device unsupported...)"; zz-ZZ "Pending: language zz-ZZ is not supported by the speech framework". Settings shows no Speech Recognition toggle until the first request. (`p04-01`, `p04-02`, `p04-03`, `p04-04`, `p04-05`.)
- **Offline.** Airplane mode with all radios off: "Transcribed on device in 0.32s, mean confidence 0.98" for the synthetic fixture sentence "Remind me to call the dentist tomorrow at nine". With the network on: same transcript in 0.15 s. (`p04-06`, `p04-07`, 2026-10-09T17:55:51Z.) The record does not state a difference attributable to the network and this document draws no conclusion beyond the two measured values.
- **Fail closed.** fr-FR with the network on: Pending, no transcript. (`p04-08`.)
- **Permission revoked while backgrounded.** iOS relaunched the app ("No audio loaded"); after Generate: "Pending: speech recognition permission denied or revoked. Audio kept; no transcription was attempted." Whether the previous fixture file survived was **not checked**. (`p04-09`, `p04-10`.)
- **Time to transcript, seven runs (seconds).** 0.32, 0.15, 0.28, 0.15, 0.15, 0.15, 0.15. The record does not say which runs were offline beyond the 0.32 s run.
- **Interruption and cancel.** Recorded as **not observable**: the fixture completes in 0.15 to 0.32 s, faster than a screen lock or app switch can be performed. Offline speech with an immediate screen lock was therefore not exercised (see [Unmet checks](#unmet-checks)).
- **Saved audio versus transcription.** The probe reports "Audio kept; no transcription was attempted" separately from "Transcribed on device", so those states are distinguishable in the observed screens. Installed reminders are not part of this probe; see P05.

### Notifications with the UI closed (P05)

Build NotificationProbe 257343f3. Doc: `docs/validation/notification-probe.md`. Record: `matrix.sections.P05_notifications`. Artifacts: `notification-probe/p05-01-...png` through `p05-09-...png` (mtime 2026-10-09T18:24:49Z for all; `p05-04` and `p05-06` are PERSONAL and unpublished).

- **Authorization.** `granted` true; alert, badge, sound, lock screen and notification center enabled; alert style banner; a second request granted in 2 ms. (`p05-01`, `p05-02`.)
- **Foreground delivery.** Banner shown; `willPresentCalled` true; `deliveredContainsRequest` true. (`p05-03`.)
- **App closed, screen on.** Scheduled 5 s out and the app force-quit: the banner appeared. After relaunch `reportDelivered` showed `closedAppDeliveredDateUTC` 2026-10-09T18:20:47Z, `closedAppDeliveredPresent` true, `stillPending` false and `willPresentCallbackCountSinceLaunch` 0. (`p05-04` PERSONAL, `p05-05`.)
- **App closed, device locked.** Scheduled, then locked at once: the notification showed on the lock screen; `reportDelivered` gave a delivered time of 2026-10-09T18:21:50Z, present true. (`p05-06` PERSONAL, `p05-07`.)
- **Pending capacity.** 100 requests added, `addErrorCount` 0, `pendingCount` 64. `retainedAmongFirstAdded64` 28, `retainedAmongLastAdded64` 64, `retainedAmongSoonest64` 41; retained identifiers begin at `probe.cap.036`. The OS kept the 64 most recently added requests and dropped the oldest silently. (`p05-08`.)
- **Calendar trigger.** Pacific/Auckland target 2026-10-13 09:30: expected, `nextTrigger` and `pendingNextTrigger` all 2026-10-12T20:30:00Z, delta 0 s, no add error. (`p05-09`.)
- **Pending versus delivered.** The recorded `reportDelivered` fields (`stillPending`, delivered time, present) are reported separately from the scheduling call, so scheduled, still-pending and delivered are distinct observations.

Not tested (the record's own list): Focus and summaries, Low Power Mode, reboot, time-zone change after scheduling, provisional and ephemeral authorization, reinstall or OS-upgrade survival.

### Handoff to the Tauri probe (P07)

Builds CaptureProbe and TauriProbe 257343f3. Doc: `docs/validation/tauri-handoff.md`. Record: `matrix.sections.P07_handoff`. Artifacts: `tauri-handoff/p07-01-...png` through `p07-08-...png` (mtimes 2026-10-09T18:44:12Z to 18:49:59Z), plus the `container-capture/` and `container-tauri-probe/` trees listed in the manifest.

- **Cold.** Tauri probe force-quit; the handoff listed the identifier (prefix CB02991B). The handoff file shows `webviewReady` false (the URL arrived at scene connect before the web view was ready) and the identifier was still listed. CaptureProbe committed at 18:38:51.727Z and the Tauri probe received it at 18:38:56.688Z, a handoff interval of 4.961 s. (Handoff latency is not trigger-to-ready.)
- **Warm.** A second handoff listed both identifiers, `webviewReady` true.
- **Hostile URL.** `ohand-tauri://capture?captureId=not-a-uuid` gave "Handoffs rejected: 1" and the list stayed at four.
- **Secret isolation.** Each `handoffs/<id>.json` contains exactly `captureId`, `receivedAtUnixMs` and `webviewReady`. The capture text appears nowhere in the Tauri container. Every listed identifier exists in both containers; an entry that was not handed off exists only in CaptureProbe.
- **Side observation.** Opening the CaptureProbe scheme with a bogus `captureId` query minted a fresh valid entry; the query is ignored.
- **Keyboard navigation (step 4).** Not run on the device. Deferred out of P09 to U10 by the recorded scope amendment (no hardware keyboard is available for the phone). This is a deferral, not a pass.

### Accessibility

Records: `matrix.sections.P07_handoff.6_large_text` (2026-10-09 session) and `latency.sections.P07_accessibility` (2026-10-10 session, VoiceOver with Caption Panel on, text read from the caption band of the screen recording `latency/ScreenRecording_10-09-2026 22-27-08_1.MP4`, tiles `accessibility/vo/tile-0.png` to `tile-4.png`, 2026-10-10T05:41:42Z to 05:41:43Z).

VoiceOver (`latency` record):

- CaptureProbe "Review in management app" button with an entry present reads "Review in management app, Button, , Opens the management app. Only this entry's identifier is sent." With no entry it reads dimmed, matching its disabled state.
- CaptureProbe heading reads "Capture Probe, Heading"; the description and capture identifier line are read in full.
- Tauri probe headings, with the rotor on Headings, reached "ohand-tauri-probe, Oh And Tauri Probe, Heading", "Native Round-Trip Test, Heading level 2" and "Received captures, Heading level 2".
- Tauri probe summary read as "Captures received: 6. Handoffs rejected: 1." Whether it announces automatically when the count changes was **not exercised**.
- Tauri probe identifiers are read in full, digit groups spelled out. The "Hello from Tauri" text field and the "Echo from Rust" result are reachable by swiping.

Largest text size (`matrix`, artifacts `large-text/lt-01-audioprobe.png` through `lt-07-tauriprobe.png`, all 2026-10-09T18:55:27Z, `lt-05` PERSONAL): the Tauri probe scales with nothing clipped. **Defects observed:** CaptureProbe's "Review in management app" label overflows its button; AudioProbe Start/Stop labels truncate; CredentialProbe "Arm Locked Retrieval" label truncates; TranscriptionProbe Cancel falls below the fold but stays reachable. BridgeProbe and NotificationProbe were fine.

These are probe-level findings; the shipped UI is covered by U10.

### Secure credential behaviour (P11)

Build CredentialProbe 257343f3. Doc: `docs/validation/credential-probe.md`. Record: `matrix.sections.P11_credentials`. Artifacts: `credential-probe/container/locked-retrieval.log` (2026-10-09T18:56:47Z) and `credential-probe/p11-01-...png` through `p11-04-...png` (2026-10-09T19:01:43Z).

- **Locked retrieval.** Armed 18:56:18Z, backgrounded 18:56:22Z. At offsets 1, 3 and 6 s (protected data available) all five accessibility classes returned status 0. At offsets 10, 15 and 20 s (protected data unavailable, device locked): WhenUnlocked, WhenUnlockedThisDeviceOnly and WhenPasscodeSetThisDeviceOnly returned -25308 (`errSecInteractionNotAllowed`); AfterFirstUnlock and AfterFirstUnlockThisDeviceOnly returned status 0 with matching values. The background task expired 25 s after backgrounding.
- **Unlocked.** All five classes status 0, protected data available.
- **Relaunch.** Force-quit, relaunch, retrieve without storing: all five status 0.
- **Reboot.** Store, reboot, first unlock, retrieve: all five status 0.
- **Passcode removal.** Not tested.

The AfterFirstUnlock classes therefore remain readable while the device is locked after first unlock.

## Unmet checks

These documented checks have no device evidence, either because they were not run, could not be observed, or were deferred. None is certified.

1. **Hardware keyboard navigation of the Tauri probe** (`tauri-handoff.md` step 4): not run, no keyboard. Deferred to U10 by the recorded scope amendment.
2. **Capture while the device stays locked** and **trigger-to-ready for a locked device**: the control opened the app only after passive unlock, and screen recording stops on lock.
3. **Offline speech with an immediate screen lock, and P04 interruption and cancel**: not observable with the 0.15 to 0.32 s fixture.
4. **Capture-entry (`capture-entry.md`)**: file write behaviour before first unlock after reboot, including survival of the pending-entry file; behaviour when the system terminates the app between handoff and commit; the Shortcuts or Action-button entry (a shortcut URL warm entry appears in the P07 artifacts but its latency was not measured).
5. **P03**: low-free-space refusal.
6. **P04**: whether the earlier fixture survived a permission revoke.
7. **P05**: Focus and summaries, Low Power Mode, reboot survival of scheduled notifications, time-zone change after scheduling, provisional and ephemeral authorization, reinstall or OS-upgrade survival.
8. **P07**: whether the Tauri summary announces automatically under VoiceOver.
9. **P11**: passcode removal.
10. **Signing**: no tooling record for CredentialProbe, CaptureProbe or TauriProbe.
11. **Latency**: touch-up is not observable, so all latencies are touch-down upper bounds; voice Start and text-field trigger-to-ready were not timed; sample sizes are 5 or 6 per condition.

## Acceptance mapping

| Criterion (`m1-plan.md#p09`) | Where recorded | Status |
| --- | --- | --- |
| 1. Measured cold/warm/locked/unlocked capture, permission states, interruptions, offline ASR, notifications with UI closed, handoff, accessibility, secure credential behaviour | Capture entry, Latency, P03, P04, P05, P07, Accessibility, P11 | Device evidence exists for each category; gaps are items 1 to 9 above (locked capture and ASR-under-lock in particular) |
| 2. Trigger-to-ready and end-of-input-to-durable-save measured separately; no simulator or checklist substitution | Latency | Measured on the device for the control path (cold, warm) and voice Stop; locked, voice Start and text field are not measured |
| 3. Record the precise unmet check and block rather than certify | Unmet checks | Unmet checks are listed; the matrix is not certified |
| 4. Observed OS build, signing and credential-lock behaviour in sanitized evidence | Device, OS and builds; P11 | OS build 26.6.2 (23G90) observed; tooling records for four probes, none for three |
| 5. Offline speech and time to transcript on the device, including immediate screen lock; saved audio distinguished from transcription and installed reminders | P04, P05 | Offline transcription and time measured; immediate screen lock not exercised |
| 6. Every result cites a named sanitized artifact, build/revision and collection time; raw private evidence preserved with an auditable reference | Evidence records, per-section citations | Sections cite artifacts, builds and times; private files are referenced by path, size, SHA-256 and mtime in `manifest`. Items without an individually named artifact are marked as such |
