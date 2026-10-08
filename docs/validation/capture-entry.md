# Capture entry handoff and protected ingress (P02)

P02 probes the path from a system control to a native capture screen and the durable write of a synthetic ingress record. It is a probe, not the production capture flow: C06/C-series tasks own the production `Capture/Entry/` surface and reuse what is established here.

This document reports what the probe implements and what the automated checks assert. It records no result for any commit; the `ios.yml` run for the submitted commit is the evidence, and it is linked from the pull request. Everything under "Needs a physical device" is **not measured**.

## What exists

| Path | Role |
| --- | --- |
| `ios/CaptureProbe/Shared/IngressCore.swift` | `IngressStore` (file-backed records) and `IngressFlow` (per-entry capture ID policy). Foundation only; compiled into the app, the control extension and the tests. |
| `ios/CaptureProbe/Shared/ProbeOpenCaptureIntent.swift` | The `OpenIntent` the control runs. Its `perform()` registers the handoff and posts a notification. |
| `ios/CaptureProbe/Sources/AppDelegate.swift` | Scene delegate (foreground events, cold/warm classification) and the capture screen. |
| `ios/CaptureProbe/Control/` | iOS 18 `ControlWidget` whose button runs `ProbeOpenCaptureIntent`. |
| `ios/CaptureProbe/Tests/IngressFlowTests.swift` | XCTest suite (`CaptureProbeTests`, also part of the `OhAndTests` scheme). |
| `ios/scripts/smoke-capture-simulator.sh`, `verify_capture_records.py` | Simulator smoke test that reads the persisted files. |

## Design

- **Durable store.** Records are JSON files in `Application Support/CaptureProbe/records/<captureId>.json`, written atomically with file protection `completeUntilFirstUserAuthentication` (the directory is created with the same class). This is a Swift-side probe store, not the Rust `save_capture` path: the P01 boundary only opens an in-memory SQLite database, and a file-backed Rust store is outside P02's owned paths. Moving the record onto the Rust store is a production decision for B-series/C-series tasks.
- **Why `completeUntilFirstUserAuthentication`.** `complete` protection makes the file unwritable while the device is locked, which defeats capture from the lock screen. `completeUntilFirstUserAuthentication` allows writes while locked after the first unlock. Before the first unlock after boot the write is expected to fail; the probe reports `Failed: write error ...` and keeps the ID for retry rather than claiming success. Whether iOS behaves that way for this app on a device is unmeasured.
- **One capture ID per entry.** `IngressFlow` keeps a pending entry (durable `pending-entry.json`, and in memory if that write fails). The control's intent calls `registerHandoff`, the scene calls `enter`; both resolve the same pending entry, so the ID is the same whichever happens first. A failed commit leaves the pending entry, so retries and a relaunch reuse the ID. A successful commit clears it, so the next entry gets a new ID. Committing an ID that already has a record is a replay and never overwrites the first record.
- **Cold and warm.** The scene delegate classifies the first `sceneWillEnterForeground` after connecting as cold and later ones as warm, and commits one record per foreground entry. A handoff that arrives while the app is already foreground commits immediately; one that arrives in the background is consumed by the next foreground entry.
- **No private history.** The screen shows only the current entry's ID, source, launch kind, protected-data flag and result. It never lists earlier records.
- **No background microphone and no authentication bypass.** The probe records no audio, requests no permissions and runs nothing while the app is not foreground.

## What the automated checks assert

Unit tests (`CaptureProbeTests`, simulator, no host app):

- the handoff ID is identical across repeated registrations and across a simulated relaunch (new `IngressFlow` over the same directory);
- entering commits the handoff ID with source `controlIntent`; a direct entry gets its own ID; a second entry after a commit gets a new ID;
- retry after a failed record write, after a failed write of everything, and after a relaunch reuses the same ID and ends with exactly one record;
- commit is idempotent and keeps the first record;
- records read back identically from a reopened store;
- the protected-data flag passed by the caller is stored with the record;
- the displayed lines contain the current ID and not an earlier entry's ID;
- the presented-outcome file matches the outcome;
- invoking `ProbeOpenCaptureIntent.perform()` registers the pending entry, posts the notification, and the following entry commits that same ID.

Simulator smoke test (`smoke-capture-simulator.sh`, then `verify_capture_records.py` on the files in the app's data container):

1. cold launch: one record with `launchKind` cold, a UUID `captureId`, and `last-presented.json` reporting `Saved` for a persisted ID;
2. terminate and cold launch again: two records, and the first record file is byte-identical to before the restart;
3. launch Settings to background the app, then launch the app again: three records, kinds cold, cold, warm, all IDs distinct, and the rendered outcome matches a persisted record.

The verifier has Linux unit tests (`test_verify_capture_records.py`, run by `make check`). They test the verifier, not the app.

Artifacts uploaded in `ios-evidence`: `capture-smoke.log`, `capture-records/` (the persisted files), `capture-1-cold.png`, `capture-2-cold-after-restart.png`, `capture-3-warm.png`, `CaptureProbe-build.log`.

## Limits of the simulator evidence

- The simulator does not enforce data protection classes. The tests show that the protected write path works and that failures are handled; they do not show that iOS blocks or allows a write in a protection state.
- `simctl` cannot press a Control Center control, lock the device or simulate before-first-unlock. The intent is exercised by calling `perform()` in a unit test, not by the system running it from a control.
- Because the smoke test launches the app directly, its records have source `directLaunch`.

## Needs a physical device (not measured)

- Whether a Control Center control (iOS 18) and, separately, a Shortcuts/Action-button style entry opens the app, on cold and warm launch, and the observed latency.
- Whether the control works from the lock screen, what authentication the system requires before the app opens, and that the screen exposes no earlier captures in that state.
- Whether `perform()` runs before or after `sceneWillEnterForeground` on warm and cold launches. The code handles both orders, but if the scene enters the foreground first it commits a `directLaunch` record and the intent then commits a second `controlIntent` record. Whether that second record occurs on a device is unknown.
- File write behavior before first unlock, after first unlock while locked, and while unlocked, with the chosen protection class, including whether the pending-entry file survives a reboot before first unlock.
- Behavior of an app that is terminated by the system between the handoff and the commit.

## Reuse notes

- Keep the intent in a `Shared/` directory that is a member of both the app and the control extension (checked by `ios/scripts/check_project_config.py`). It must be an `OpenIntent`, not `openAppWhenRun`.
- Pass protected-data state in from the app; `UIApplication` is unavailable to the extension target that also compiles the shared sources.
