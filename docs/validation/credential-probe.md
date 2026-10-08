# P11: Credential Probe — Keychain Accessibility and Lock Behavior

## Overview

The Credential Probe establishes and tests credential accessibility using iOS Keychain with synthetic test data. This probe validates native credential isolation independently of the production schema/bridge dependencies before P09 device testing.

## Scope

- **Synthetic credentials only**: All test credentials are generated with predictable identifiers (e.g., `synthetic-credential-WhenUnlocked`).
- **Simulator-focused**: Tests run on the simulator to validate Keychain API accessibility and state transitions. Physical device-specific lock behavior (e.g., interrupted access after lock, passcode requirements) is deferred to P09.
- **No production dependencies**: The probe builds and runs without the core bridge, service composition, or production schema.

## Keychain Accessibility Classes Tested

The probe evaluates five standard iOS Keychain accessibility classes:

### 1. `kSecAttrAccessibleWhenUnlocked`

**Accessibility**: Data is accessible only when the device is unlocked.

- **Simulator behavior**: Accessible during normal app execution.
- **Lock/relaunch on device**: Data is accessible only while the device is unlocked; becomes inaccessible while locked. Requires the device to be unlocked at the moment of access.
- **Use case in M1**: Not suitable for credentials needed while device is locked.
- **Test outcome**: Not run in simulator CI; physical lock behavior validated on device by P09.

### 2. `kSecAttrAccessibleAfterFirstUnlock`

**Accessibility**: Data is accessible after the first device unlock each boot, then remains accessible until the device restarts.

- **Simulator behavior**: Always accessible (simulator does not enforce lock state).
- **Lock/relaunch on device**: Accessible after first unlock; remains accessible even after lock until the next device restart. After restart, requires unlock again.
- **Use case in M1**: Suitable for provider credentials that survive app relaunch and device lock but not device restart.
- **Test outcome**: Not run in simulator CI; physical behavior validated on device by P09.

### 3. `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`

**Accessibility**: Like `AfterFirstUnlock`, but bound to the device and not transferable via backup/restore.

- **Simulator behavior**: Identical to `AfterFirstUnlock` on simulator.
- **Lock/relaunch on device**: Accessible after first unlock; remains accessible even after lock until the next device restart. Not transferred during backup/restore.
- **Use case in M1**: Recommended for device-specific credentials (e.g., locally created provider tokens).
- **Test outcome**: Not run in simulator CI; backup/restore behavior tested separately on device by P09.

### 4. `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`

**Accessibility**: Data is accessible only while the device is unlocked, bound to this device.

- **Simulator behavior**: Accessible during normal execution.
- **Lock/relaunch on device**: Becomes unreadable (inaccessible) while locked but is not deleted from storage. Readable again after unlock. Not transferred via backup.
- **Use case in M1**: High security for temporary session data or recently entered credentials.
- **Test outcome**: Not run in simulator CI; lock-mediated access validated on device by P09.

### 5. `kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly`

**Accessibility**: Accessible only if the device has a passcode set, and only while unlocked.

- **Simulator behavior**: Accessible (no passcode requirement enforced).
- **Lock/relaunch on device**: Requires both a set passcode and device unlock. Items are deleted (not merely inaccessible) if the passcode is removed; changing the passcode does not affect accessibility.
- **Use case in M1**: Credentials that require strong device protection and are tied to the device's security configuration.
- **Test outcome**: Not run in simulator CI; passcode requirement tested on device by P09.

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

The probe provides:

1. **Store All Credentials**: Stores synthetic test credentials for each accessibility class. Credentials persist until explicitly cleared, allowing separate retrieval tests.
2. **Retrieve & Check**: Attempts to retrieve previously stored credentials without modifying them, displaying immediate access results with error status codes (e.g., `-25308` for `errSecInteractionNotAllowed` indicating a locked device).
3. **Clear Test Credentials**: Removes all probe test credentials from the Keychain, leaving production data untouched.
4. **Status Display**: Shows timestamp of each operation and accessibility class results. On device, lock-related access denials are shown with their OSStatus code for diagnostic purposes.

## Lock/Relaunch Tests for P09

The CredentialProbe provides a foreground UI for manual testing. Physical device testing must cover:

### Probe Limitations with Lock State

The probe is a foreground app, so it suspends when the device locks. Direct retrieval while the device is locked (with the app suspended) requires:
- A background processing mechanism, system framework extension, or
- A separate test harness that accesses the Keychain while the main app is suspended

For the lock-state testing requirements, P09 may:
1. Use the probe to store credentials, then manually verify retrieval access through alternative means (system frameworks in a debug tool, background task with deliberate timing constraints, etc.)
2. Or document only the accessible states that can be observed through app relaunch and foreground UI interactions
3. Or follow the procedures below using the probe to store/retrieve across lock/unlock cycles

### Lock Behavior

1. **Store a credential** with `WhenUnlocked` accessibility via the probe's "Store All Credentials" button.
2. **Lock the device** (press power button or use Control Center); the app suspends.
3. **Unlock the device**; the app may relaunch or remain suspended.
4. **Return to the probe** and tap "Retrieve & Check" to verify the credential is accessible (or not) after unlock.

Note: Testing actual retrieval *while locked* (before unlock) requires out-of-app verification.

### Relaunch After Lock

1. **Store credentials** with all five accessibility classes via "Store All Credentials".
2. **Lock the device**; the probe suspends in the background.
3. **Force quit the app** via Settings or Xcode; alternatively let it remain backgrounded.
4. **Unlock the device** and relaunch the probe app (or it may relaunch automatically).
5. **Tap "Retrieve & Check"** and observe results.
   - `AfterFirstUnlock` and `AfterFirstUnlockThisDeviceOnly` should remain accessible (persisting until next device restart).
   - `WhenUnlocked` and `WhenUnlockedThisDeviceOnly` are accessible when tapped after unlock.
   - `WhenPasscodeSetThisDeviceOnly` is accessible after unlock (if passcode remains set).

### Passcode Requirement (WhenPasscodeSetThisDeviceOnly)

1. **Ensure device has a passcode** set.
2. **Store a credential** with `WhenPasscodeSetThisDeviceOnly` accessibility.
3. **Unlock the device** and tap "Retrieve & Check" in the probe (expect success).
4. **Lock the device**; the probe suspends.
5. **Unlock again** and return to the probe; tap "Retrieve & Check" (expect success, since the device is now unlocked).

Note: Direct retrieval while the device is locked (before unlock) cannot be executed through the foreground probe. Observing locked-state denial would require a background mechanism or separate debug harness outside the scope of this probe.

### Device Reboot

1. **Store credentials** with all five accessibility classes.
2. **Reboot the device** (power off and on).
3. **After the device finishes booting**, unlock it (the probe is a foreground app and will launch normally once unlocked).
4. **Tap "Retrieve & Check"** to retrieve all credentials after the first unlock.
5. **Document which classes remain accessible** after the reboot and first unlock.

Note: Attempting retrieval before the first unlock cannot be executed through the foreground probe, as the app cannot run before unlock. Observing the pre-unlock state would require a background mechanism or system-level instrumentation.

## Accessibility Before/After First Unlock

### Before First Unlock (on device after reboot)

- `WhenUnlocked`: ✗ Not accessible (requires unlock).
- `AfterFirstUnlock`: ✗ Not accessible (requires first unlock).
- `AfterFirstUnlockThisDeviceOnly`: ✗ Not accessible (requires first unlock).
- `WhenUnlockedThisDeviceOnly`: ✗ Not accessible (requires unlock).
- `WhenPasscodeSetThisDeviceOnly`: ✗ Not accessible (requires unlock and passcode).

### After First Unlock (during normal use, including after lock)

- `WhenUnlocked`: ✓ Accessible (while device is unlocked).
- `AfterFirstUnlock`: ✓ Accessible (until next restart).
- `AfterFirstUnlockThisDeviceOnly`: ✓ Accessible (until next restart).
- `WhenUnlockedThisDeviceOnly`: ✓ Accessible (while device is unlocked).
- `WhenPasscodeSetThisDeviceOnly`: ✓ Accessible (if passcode set and device is unlocked).

## Considerations for Production (P09)

### Recommended Classes for M1

| Use Case | Recommended Class | Rationale |
| --- | --- | --- |
| Provider API keys | `AfterFirstUnlockThisDeviceOnly` | Secure, persistent across app relaunch, device-bound. |
| Session tokens (temporary) | `WhenUnlockedThisDeviceOnly` | High security; unreadable while locked, readable after unlock. |
| Cached local credentials | `AfterFirstUnlockThisDeviceOnly` | Balance of security and accessibility. |

### Constraints

- **Simulator**: All accessibility classes appear accessible (no enforced locking).
- **Device**: Lock state, reboot, and passcode requirements are enforced and vary by class.
- **Backup/Restore**: `...ThisDeviceOnly` classes do not transfer; others may be restored (unless excluded via backup API).
- **Passcode**: Removal of a passcode deletes `WhenPasscodeSetThisDeviceOnly` credentials permanently; ordinary passcode changes do not affect accessibility.

## Probe Build and Execution

### Build Command

```bash
ios/scripts/build-simulator.sh CredentialProbe
```

This command generates the project, builds for simulator with unsigned configuration, and places the `.app` bundle in the derived data directory.

Alternatively, to use xcodebuild directly:

```bash
cd ios
./scripts/generate.sh
xcodebuild build \
  -project OhAnd.xcodeproj \
  -scheme CredentialProbe \
  -configuration Debug \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO
```

### Clean Build

```bash
cd ios
xcodebuild clean \
  -project OhAnd.xcodeproj \
  -scheme CredentialProbe
```

## Simulator Build and Results

The CI workflow (`.github/workflows/ios.yml`) builds and launches CredentialProbe on the simulator to verify that the target compiles without production schema/bridge dependencies and that the app process starts and remains running. The CI smoke test is a launch-only validation — no Keychain operations or UI interactions are executed during the automated smoke test.

Full testing of Keychain accessibility (Store, Retrieve, Clear operations) and lock behavior requires manual execution on a simulator or physical device using the procedures in "Lock/Relaunch Tests for P09" section below. All five accessibility classes appear accessible in the simulator during manual use because the simulator does not enforce device lock state; lock behavior (where `WhenUnlocked`, `WhenUnlockedThisDeviceOnly`, and `WhenPasscodeSetThisDeviceOnly` become inaccessible or require device unlock) is observed only on physical devices and is deferred to P09 physical device testing.

## Results Recording for P09

P09 must record actual device behavior using the lock/relaunch procedures in this document:

1. **Device/OS metadata**: iPhone model, iOS version, build number.
2. **Lock behavior**: For each accessibility class, the result of locked vs. unlocked retrieval, including OSStatus codes.
3. **Relaunch behavior**: Accessibility after app relaunch and device reboot.
4. **Timestamp of test run**: When the device testing occurred.
5. **Any deviations** from documented behavior (e.g., unexpected access or denial).

**No synthetic test credentials or Keychain contents are published in P09 results.** Only the accessibility class behavior matrix is recorded. OSStatus codes (`-25308` for lock errors, etc.) are included in the matrix for diagnostic clarity.

## Privacy and Hygiene

- **No real credentials in public evidence**: All test data is synthetic and easily removable.
- **Probe credential cleanup**: Credentials persist in the Keychain until explicitly cleared via the "Clear Test Credentials" button. Manual deletion is required for cleanup.
- **No default credentials on fresh install**: The probe creates test data only when "Store All Credentials" is tapped.
- **Device-local testing**: All tests execute on the local device; no credentials leave the device.

## See Also

- [M1 Device and Trial Protocol](m1-protocol.md) — Overall testing methodology for device feasibility.
- [docs/features/m1-plan.md](../../docs/features/m1-plan.md) — P09 task specification for actual-device feasibility evidence.

## Related Tasks

- **P05** — Probe native notification scheduling
- **P09** — Collect actual-device feasibility evidence (uses this probe's lock/relaunch procedure)
