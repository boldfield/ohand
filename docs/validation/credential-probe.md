# P11: Credential Probe — Keychain Accessibility and Lock Behavior

## Overview

The Credential Probe establishes and tests credential accessibility using iOS Keychain with synthetic test data. This probe validates native credential isolation independently of the production schema/bridge dependencies before P09 device testing.

## Scope

- **Synthetic credentials only**: All test credentials are generated with predictable identifiers (e.g., `synthetic-credential-WhenUnlocked`).
- **Simulator-focused**: A launch-argument self-test runs in CI and exercises store, retrieve, repeated store, delete and service isolation for all five classes. Physical-device lock behavior (access while locked, passcode requirements, reboot) is deferred to P09, which uses the locked-retrieval log described below.
- **No production dependencies**: The probe builds and runs without the core bridge, service composition, or production schema.

## Keychain Accessibility Classes Tested

The probe evaluates five standard iOS Keychain accessibility classes:

### 1. `kSecAttrAccessibleWhenUnlocked`

**Accessibility**: Data is accessible only when the device is unlocked.

- **Simulator behavior**: Accessible during normal app execution.
- **Lock/relaunch on device**: Data is accessible only while the device is unlocked; becomes inaccessible while locked. Requires the device to be unlocked at the moment of access.
- **Use case in M1**: Not suitable for credentials needed while device is locked.
- **Simulator result**: see "Observed simulator results"; lock behavior is device-only and left to P09.

### 2. `kSecAttrAccessibleAfterFirstUnlock`

**Accessibility**: Data is accessible after the first device unlock each boot, then remains accessible until the device restarts.

- **Simulator behavior**: Store and retrieve succeed (the simulator does not enforce lock state).
- **Lock/relaunch on device**: Accessible after first unlock; remains accessible even after lock until the next device restart. After restart, requires unlock again.
- **Use case in M1**: The item stays stored across app relaunch, device lock and device restart; after a restart it is unavailable until the first user unlock, then readable again until the next restart. This is the migratable (backup/restore transferable) variant, so it is not the M1 choice for provider credentials; M1 uses `AfterFirstUnlockThisDeviceOnly` (class 3).
- **Simulator result**: see "Observed simulator results"; lock behavior is device-only and left to P09.

### 3. `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`

**Accessibility**: Like `AfterFirstUnlock`, but bound to the device and not transferable via backup/restore.

- **Simulator behavior**: Identical to `AfterFirstUnlock` on simulator.
- **Lock/relaunch on device**: Accessible after first unlock; remains accessible even after lock until the next device restart. Not transferred during backup/restore.
- **Use case in M1**: Recommended for device-specific credentials (e.g., locally created provider tokens).
- **Simulator result**: see "Observed simulator results"; lock behavior is device-only and left to P09.

### 4. `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`

**Accessibility**: Data is accessible only while the device is unlocked, bound to this device.

- **Simulator behavior**: Accessible during normal execution.
- **Lock/relaunch on device**: Becomes unreadable (inaccessible) while locked but is not deleted from storage. Readable again after unlock. Not transferred via backup.
- **Use case in M1**: High security for temporary session data or recently entered credentials.
- **Simulator result**: see "Observed simulator results"; lock behavior is device-only and left to P09.

### 5. `kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly`

**Accessibility**: Accessible only if the device has a passcode set, and only while unlocked.

- **Simulator behavior**: Accessible (no passcode requirement enforced).
- **Lock/relaunch on device**: Requires both a set passcode and device unlock. Items are deleted (not merely inaccessible) if the passcode is removed; changing the passcode does not affect accessibility.
- **Use case in M1**: Credentials that require strong device protection and are tied to the device's security configuration.
- **Simulator result**: see "Observed simulator results"; lock behavior is device-only and left to P09.

## Implementation Details

### Synthetic Credential Format

All test credentials follow a deterministic pattern:

```
Service: com.boldfield.ohand.probes.credential.test
Account: test-credential-{AccessibilityClassName}
Value: synthetic-credential-{AccessibilityClassName}
```

Example:
- Account: `test-credential-WhenUnlocked`
- Value: `synthetic-credential-WhenUnlocked`

No real API keys, passwords, or sensitive data are used. Test credentials are easily identifiable and removable.

### UI and Manual Test Controls

Launching the app never touches the Keychain. The buttons are:

1. **Store All Credentials**: stores (replacing any existing item) one synthetic credential per class and shows each `OSStatus`.
2. **Retrieve & Check (no store)**: reads the existing items without writing and shows `OSStatus` and whether the value matches. `-25308` (`errSecInteractionNotAllowed`) means the item is unreadable in the current lock state; `-25300` (`errSecItemNotFound`) means it was never stored or was deleted.
3. **Arm Locked Retrieval (stores, then lock device)**: stores all items, then arms the locked-state harness below.
4. **Show / Clear Locked-Retrieval Log**: displays or removes the log written by the harness.
5. **Clear Test Credentials**: deletes only items whose service is `com.boldfield.ohand.probes.credential.test`.

### Keychain Self-Test (`-runKeychainSelfTest`)

`xcrun simctl launch <udid> com.boldfield.ohand.probes.credential -runKeychainSelfTest` runs, for each of the five classes: store, retrieve (value must match), repeated store (must succeed, replacing the item) and retrieve again. It then stores one decoy item under a different service (`com.boldfield.ohand.probes.credential.decoy`), deletes all probe items, asserts every probe item is `errSecItemNotFound`, asserts the decoy item is untouched, and removes the decoy. Each step prints `KEYCHAIN_SELFTEST step=... result=PASS|FAIL detail=status=<OSStatus>`, the last line is `KEYCHAIN_SELFTEST_RESULT PASS|FAIL`, and the process exits non-zero on failure. `ios/scripts/keychain-selftest-simulator.sh` drives it and CI fails if it does not report PASS.

The simulator needs Keychain entitlements before any call succeeds: an unsigned build fails every call with `-34018` (`errSecMissingEntitlement`), which an earlier CI run recorded. The target therefore declares `CredentialProbe/CredentialProbe.entitlements` and CI builds it with `OHAND_SIMULATOR_ADHOC_SIGN=1` (Xcode ad-hoc signing). `codesign -d --entitlements -` printed an empty dictionary for that build, so the simulator evidently takes the entitlements from the binary rather than the signature; the self-test results below are the evidence that this works.

### Locked-State Harness

The app suspends when the device locks, so the probe cannot read the Keychain from the foreground while locked. Instead, "Arm Locked Retrieval" stores the items and sets a flag. When the scene next enters the background (the user locks the device or leaves the app), `sceneDidEnterBackground` calls `beginBackgroundTask` and schedules retrievals of all five classes at +1, 3, 6, 10, 15, 20 and 25 seconds. Each attempt appends one line to `Documents/locked-retrieval.log`, written with `.noFileProtection` so it can be written while the device is locked:

```
<ISO-8601 time> offset=<n>s protectedData=<bool> appState=<active|inactive|background> class=<name> status=<OSStatus (name)> valueMatches=<bool>
```

`protectedData` is `UIApplication.isProtectedDataAvailable`. The log also records `ARMED`, `BACKGROUND`, `EXPIRED` (if the system ends the background task early) and `DONE` lines. After unlocking, "Show Locked-Retrieval Log" displays it. This harness has not been run: the simulator cannot lock, so it is exercised only on a physical device by P09.

Limits that P09 must record rather than assume: the background-task window is bounded by the system (roughly 30 seconds), so attempts after an `EXPIRED` line are missing; a probe that is not backgrounded at lock time (for example the screen locks while another app is in front) logs nothing; and the probe cannot run before the first unlock after a reboot, so **the pre-first-unlock state is not observable with this probe**. P09 must list that cell of the matrix as "not observed" unless it adds another mechanism, and may cite Apple's documented semantics only as documentation, not as evidence.

## Lock/Relaunch Protocol for P09

Run on a physical device with a passcode set. Record device model, iOS version and build, and the date.

1. **Locked retrieval.** Tap "Clear Locked-Retrieval Log", then "Arm Locked Retrieval". Lock the device within a few seconds and keep it locked for at least 30 seconds. Unlock, open the probe and tap "Show Locked-Retrieval Log". Expected per Apple's documentation (to be confirmed, not assumed): while locked, `WhenUnlocked`, `WhenUnlockedThisDeviceOnly` and `WhenPasscodeSetThisDeviceOnly` return `-25308`/`protectedData=false`, and the two `AfterFirstUnlock` classes return success. Record the actual status per class and per attempt.
2. **Unlocked retrieval.** With the device unlocked, tap "Retrieve & Check (no store)"; record each status.
3. **Relaunch.** After step 1 force-quit the probe, relaunch it and tap "Retrieve & Check (no store)" without storing. Launching does not store or delete, so this shows which items survive relaunch.
4. **Reboot.** Tap "Store All Credentials", reboot, unlock once, open the probe and tap "Retrieve & Check (no store)". Record which classes are readable after the first unlock. Retrieval before the first unlock is not observable (see above).
5. **Passcode.** Optionally remove the passcode, return to the probe and tap "Retrieve & Check (no store)"; Apple documents that `WhenPasscodeSetThisDeviceOnly` items are deleted when the passcode is removed. Record what is observed.

## Accessibility Semantics (Apple documentation, not probe evidence)

- `WhenUnlocked`, `WhenUnlockedThisDeviceOnly`: readable only while the device is unlocked at the moment of access; unreadable (not deleted) while locked.
- `AfterFirstUnlock`, `AfterFirstUnlockThisDeviceOnly`: unreadable after a restart until the first unlock; after that readable until the next restart, including while the device is locked.
- `WhenPasscodeSetThisDeviceOnly`: like `WhenUnlockedThisDeviceOnly`, and only exists while a passcode is set; removing the passcode deletes the item.
- `...ThisDeviceOnly` classes are not migrated by backup or restore.

## Considerations for Production (P09)

### Recommended Classes for M1

| Use Case | Recommended Class | Rationale |
| --- | --- | --- |
| Provider API keys | `AfterFirstUnlockThisDeviceOnly` | Secure, persistent across app relaunch, device-bound. |
| Session tokens (temporary) | `WhenUnlockedThisDeviceOnly` | Unreadable (not deleted) while locked, readable again when unlocked. |
| Cached local credentials | `AfterFirstUnlockThisDeviceOnly` | Balance of security and accessibility. |

### Constraints

- **Simulator**: Store and retrieve succeed for all classes (no enforced locking).
- **Device**: Lock state, reboot, and passcode requirements are enforced and vary by class.
- **Backup/Restore**: `...ThisDeviceOnly` classes do not transfer; others may be restored (unless excluded via backup API).
- **Passcode**: Removal of a passcode deletes `WhenPasscodeSetThisDeviceOnly` credentials permanently; ordinary passcode changes do not affect accessibility.

## Probe Build and Execution

```bash
# Simulator build with ad-hoc signing and the target's Keychain entitlements (macOS with Xcode)
OHAND_SIMULATOR_ADHOC_SIGN=1 ios/scripts/build-simulator.sh CredentialProbe
make ios-credential-probe   # same signed build
# An unsigned build (no OHAND_SIMULATOR_ADHOC_SIGN) compiles but every Keychain call fails with -34018

# Self-test on a booted-or-bootable simulator
ios/scripts/keychain-selftest-simulator.sh <simulator-udid> ios/.derived/Build/Products/Debug-iphonesimulator/CredentialProbe.app <evidence-dir>
```

`.github/workflows/ios.yml` runs the signed build, the self-test and a launch smoke test (`smoke-simulator.sh`, launch only, writes `credentialprobe-launch.png`) on every pull request, and uploads their logs in the `ios-evidence` artifact.

## Observed simulator results

Source: GitHub Actions run https://github.com/boldfield/ohand/actions/runs/37718723834 on commit `7ad001e` (artifact `ios-evidence`, files `keychain-selftest-console.log`, `credentialprobe-entitlements.txt`, `CredentialProbe-build.log`). Runner macOS 15.7.9, Xcode 16.4 (16F6), iPhone 16 simulator, iOS 18.5, UDID `F0E646EF-4792-4F36-B48B-EC89B3A6B73B`, `protectedDataAvailable=true`.

`KEYCHAIN_SELFTEST_RESULT PASS passed=29 failed=0`:

| Class | store | retrieve (value matches) | repeated store | retrieve after repeated store | absent after delete |
| --- | --- | --- | --- | --- | --- |
| `WhenUnlocked` | 0 | 0 | 0 | 0 | -25300 |
| `AfterFirstUnlock` | 0 | 0 | 0 | 0 | -25300 |
| `AfterFirstUnlockThisDeviceOnly` | 0 | 0 | 0 | 0 | -25300 |
| `WhenUnlockedThisDeviceOnly` | 0 | 0 | 0 | 0 | -25300 |
| `WhenPasscodeSetThisDeviceOnly` | 0 | 0 | 0 | 0 | -25300 |

Also observed: a decoy item under a different service was untouched by the probe's delete and was then removed. These results show only that the synthetic store, retrieve, replace and scoped-delete operations work on the simulator. They say nothing about lock behavior: the simulator was never locked, and the locked-state harness was not run.

An earlier CI run of the same self-test with the unsigned build recorded `-34018 (errSecMissingEntitlement)` for every operation, which is why the entitlements and ad-hoc signing are required.

## Results Recording for P09

P09 must record actual device behavior using the lock/relaunch procedures in this document:

1. **Device/OS metadata**: iPhone model, iOS version, build number.
2. **Lock behavior**: For each accessibility class, the locked-retrieval log lines (status, `protectedData`, offset) versus the unlocked "Retrieve & Check" result, including OSStatus codes, and any `EXPIRED` line.
3. **Relaunch behavior**: Accessibility after app relaunch and after reboot plus first unlock; the pre-first-unlock cell recorded as "not observed".
4. **Timestamp of test run**: When the device testing occurred.
5. **Any deviations** from documented behavior (e.g., unexpected access or denial).

**No synthetic test credentials or Keychain contents are published in P09 results.** Only the accessibility class behavior matrix is recorded. OSStatus codes (`-25308` for lock errors, etc.) are included in the matrix for diagnostic clarity.

## Privacy and Hygiene

- **No real credentials in public evidence**: All test data is synthetic and easily removable.
- **Probe credential cleanup**: The self-test deletes everything it stores. Items stored with the UI persist until "Clear Test Credentials" is tapped. Credentials never enter a webview, and no Keychain contents are written to public evidence; the logs hold only statuses and booleans.
- **No default credentials on fresh install**: The probe creates test data only when "Store All Credentials" or "Arm Locked Retrieval" is tapped.
- **Device-local testing**: All tests execute on the local device; no credentials leave the device.

## See Also

- [docs/features/m1-plan.md](../../docs/features/m1-plan.md) — P09 task specification for actual-device feasibility evidence.

## Related Tasks

- **P09** — Collect actual-device feasibility evidence (uses this probe's lock/relaunch procedure)
