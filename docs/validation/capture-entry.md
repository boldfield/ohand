# Capture entry handoff and protected ingress (P02)

P02 probes the path from a system control or shortcut to a native capture screen and the durable write of a synthetic ingress record. It is a probe, not the production capture flow: C06/C-series tasks own the production `Capture/Entry/` surface and reuse what is established here.

This document reports what the probe implements and what the automated checks assert. It records no result for any commit; the `ios.yml` run for the submitted commit is the evidence, and it is linked from the pull request. Everything under "Needs a physical device" is **not measured**.

## What exists

| Path | Role |
| --- | --- |
| `ios/CaptureProbe/Shared/IngressCore.swift` | `IngressStore` (file-backed records), `IngressFlow` (per-entry capture ID policy) and `IngressSession` (per-scene foreground state and commit decision). Foundation only; compiled into the app, the control extension and the tests. |
| `ios/CaptureProbe/Shared/ProbeOpenCaptureIntent.swift` | The `OpenIntent` the control runs. Its `perform()` registers the handoff and announces its capture ID. |
| `ios/CaptureProbe/Sources/AppDelegate.swift` | Scene delegate (foreground events, shortcut URL delivery) and the capture screen. |
| `ios/CaptureProbe/Info.plist` | Registers the `ohand-captureprobe` URL scheme used by the shortcut entry. |
| `ios/CaptureProbe/Control/` | iOS 18 `ControlWidget` whose button runs `ProbeOpenCaptureIntent`. |
| `ios/CaptureProbe/Tests/IngressFlowTests.swift` | XCTest suite (`CaptureProbeTests`, also part of the `OhAndTests` scheme). |
| `ios/scripts/smoke-capture-simulator.sh`, `verify_capture_records.py` | Simulator smoke test that drives shortcut handoffs and reads the persisted files. |

## Design

- **Only a handoff creates an entry.** Two handoffs exist: the control's `ProbeOpenCaptureIntent` and the shortcut URL `ohand-captureprobe://capture` (what a Shortcuts "Open URL" action or an Action-button shortcut would open). Launching the app from its icon is not a handoff: the screen shows "Ready" and nothing is written. The URL carries no data and grants nothing beyond opening the screen.
- **One capture ID per handoff, minted at the handoff.** `IngressFlow.registerHandoff` creates the ID and stores it as a pending entry (durable `pending-entry.json`, and in memory if that write fails). The intent's `perform()` then announces that ID to the scene; a URL is registered by the scene that receives it. The scene commits exactly the registered ID: immediately if it is foreground, otherwise on its next `sceneWillEnterForeground`. Because a plain foreground entry never commits anything, the order and the delay between `perform()` and the scene's foreground callbacks cannot produce a second ID or relabel another entry; there is no time window or other timing heuristic.
- **Retries and races.** A failed commit leaves the pending entry, so a retry, a later foreground entry and a relaunch all commit the same ID. A successful commit clears it, so the next handoff gets a new ID. If the foreground callback commits the pending ID before the announcement arrives, the announcement re-renders that same entry; an announcement for an ID committed earlier reports a replay. Committing an ID that already has a record never overwrites it.
- **Stranded entries.** A pending entry left by a failed commit (for example a write before first unlock) is adopted by the next handoff instead of minting another ID: it keeps its ID, source and `registeredAt`. That is the retry semantics; no record exists for it yet, so nothing is relabelled.
- **Durable store.** Records are JSON files in `Application Support/CaptureProbe/records/<captureId>.json`, written atomically with file protection `completeUntilFirstUserAuthentication` (the directory is created with the same class). This is a Swift-side probe store, not the Rust `save_capture` path: the P01 boundary only opens an in-memory SQLite database, and a file-backed Rust store is outside P02's owned paths. Moving the record onto the Rust store is a production decision for B-series/C-series tasks.
- **Why `completeUntilFirstUserAuthentication`.** `complete` protection makes the file unwritable while the device is locked, which defeats capture from the lock screen. `completeUntilFirstUserAuthentication` allows writes while locked after the first unlock. Before the first unlock after boot the write is expected to fail; the probe shows `Failed: write error ...` and keeps the ID for retry rather than claiming success. Whether iOS behaves that way for this app on a device is unmeasured.
- **Cold and warm.** `IngressSession` classifies the first foreground entry of the process as cold and later ones as warm. A record's `launchKind` is the kind of the foreground session in which it was committed, so a handoff while the app is already foreground is recorded with that session's kind.
- **No private history.** The screen shows only the current entry's ID, source, launch kind, protected-data flag and result, or the idle text. It never lists earlier records.
- **No background microphone and no authentication bypass.** The probe records no audio, requests no permissions and runs nothing while the app is not foreground.

## What the automated checks assert

Unit tests (`CaptureProbeTests`, simulator, no host app):

- the handoff ID is identical across repeated registrations and across a simulated relaunch (new `IngressFlow` over the same directory); committing keeps that ID and source; a foreground entry without a handoff creates no entry; a second handoff after a commit gets a new ID;
- retry after a failed record write, after a failed write of everything, and after a relaunch reuses the same ID and ends with exactly one record;
- commit is idempotent and keeps the first record; announcing an already committed ID is a replay; records read back identically from a reopened store;
- the protected-data flag passed by the caller is stored with the record and shown;
- the displayed lines contain the current ID and not an earlier entry's ID; the presented-outcome file matches the outcome and the idle screen;
- `ProbeOpenCaptureIntent.perform()` registers a pending `controlIntent` entry and announces its ID without committing it;
- `IngressSession` driven through the real intent and notification path: plain cold and warm launches render idle and write nothing; foreground then `perform()` and `perform()` then foreground, each on cold and warm entry, persist exactly one record whose ID is the one rendered; `perform()` ten minutes after the foreground callback still yields exactly one ID; a second control tap immediately after a committed entry gets its own ID and leaves the earlier record byte-for-byte unchanged; an announcement racing the foreground commit renders the same ID; a handoff while backgrounded neither renders nor commits until the next foreground entry; a cold shortcut URL is committed by the foreground callback and a warm one immediately; unrelated URLs are ignored; a write that fails while protected data is reported unavailable is retried with the same ID at the next foreground entry; a control handoff adopts an entry stranded by a failed commit.

The tests do not instantiate `CaptureProbeSceneDelegate` or `CaptureProbeViewController` (the unit-test bundle has no host app); "rendered" means the outcome the session hands to the view, whose lines the tests inspect.

Simulator smoke test (`smoke-capture-simulator.sh`, then `verify_capture_records.py` on the files in the app's data container after each phase; every phase also requires that no pending entry is left and that previously persisted records still exist):

1. plain cold launch: idle screen (`last-presented.json` has no capture ID and status `Ready`), no record;
2. terminate, then `simctl openurl ohand-captureprobe://capture` (cold): exactly one new record, source `shortcutURL`, kind cold, and the presented ID is that record with status `Saved`;
3. terminate and open the URL again (cold): exactly one more record, and the first record file is byte-identical to before the restart;
4. launch Settings to background the app, then open the URL (warm): exactly one more record, kind warm;
5. background the app, then launch it directly (warm): idle screen and no new record;
6. open the URL while the app is foreground: exactly one more record.

The verifier has Linux unit tests (`test_verify_capture_records.py`, run by `make check`). They test the verifier, not the app.

Artifacts uploaded in `ios-evidence`: `capture-smoke.log`, `capture-records/` (the persisted files), `capture-1-plain-cold.png` through `capture-6-url-foreground.png`, `CaptureProbe-build.log`.

## Limits of the simulator evidence

- The simulator does not enforce data protection classes. The tests show that the protected write path works and that failures are handled; they do not show that iOS blocks or allows a write in a protection state.
- `simctl` cannot press a Control Center control, lock the device or simulate before-first-unlock. The control's intent is exercised by calling `perform()` in unit tests, not by the system running it from a control.
- `simctl openurl` delivers the URL through the scene's URL callbacks; it does not run the Shortcuts app or an Action-button shortcut.

## Needs a physical device (not measured)

- Whether a Control Center control (iOS 18) and, separately, a Shortcuts/Action-button entry opening the URL opens the app, on cold and warm launch, and the observed latency.
- Whether the control works from the lock screen, what authentication the system requires before the app opens, and that the screen exposes no earlier captures in that state.
- Whether the system ever runs `perform()` more than once for one control press. Each run registers a handoff; a second run after the first was committed would create a second entry.
- File write behavior before first unlock, after first unlock while locked, and while unlocked, with the chosen protection class, including whether the pending-entry file survives a reboot before first unlock.
- Behavior of an app that is terminated by the system between the handoff and the commit.

## Reuse notes

- Keep the intent in a `Shared/` directory that is a member of both the app and the control extension (checked by `ios/scripts/check_project_config.py`). It must be an `OpenIntent`, not `openAppWhenRun`.
- Mint the capture ID at the handoff and commit only that ID. Committing on every foreground entry forces the app to guess which foreground entry a later `perform()` belongs to.
- Pass protected-data state in from the app; `UIApplication` is unavailable to the extension target that also compiles the shared sources.
