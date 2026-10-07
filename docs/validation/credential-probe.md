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
- **Lock/relaunch on device**: Data is accessible immediately after unlock and remains inaccessible while locked. Requires biometric or passcode unlock each time.
- **Use case in M1**: Not suitable for credentials needed before user authentication (e.g., session tokens).
- **Test outcome**: ✓ Stored & Retrieved in simulator; physical lock behavior depends on device-level testing.

### 2. `kSecAttrAccessibleAfterFirstUnlock`

**Accessibility**: Data is accessible after the first device unlock each boot, then remains accessible until the device locks again.

- **Simulator behavior**: Always accessible (simulator does not enforce lock state).
- **Lock/relaunch on device**: Accessible until the next lock; requires unlock after reboot.
- **Use case in M1**: Suitable for provider credentials that survive reboot and app relaunch.
- **Test outcome**: ✓ Stored & Retrieved in simulator; physical behavior validated on device.

### 3. `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`

**Accessibility**: Like `AfterFirstUnlock`, but bound to the device and not transferable via backup/restore.

- **Simulator behavior**: Identical to `AfterFirstUnlock` on simulator.
- **Lock/relaunch on device**: Accessible after first unlock, not accessible after lock. Not transferred during backup/restore.
- **Use case in M1**: Recommended for device-specific credentials (e.g., locally created provider tokens).
- **Test outcome**: ✓ Stored & Retrieved in simulator; backup/restore behavior tested separately.

### 4. `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`

**Accessibility**: Data is accessible only while the device is unlocked, bound to this device.

- **Simulator behavior**: Accessible during normal execution.
- **Lock/relaunch on device**: Requires unlock on every access; not transferred via backup.
- **Use case in M1**: High security for temporary session data or recently entered credentials.
- **Test outcome**: ✓ Stored & Retrieved in simulator; lock-mediated access validated on device.

### 5. `kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly`

**Accessibility**: Accessible only if the device has a passcode set, and only while unlocked.

- **Simulator behavior**: Accessible (no passcode requirement enforced).
- **Lock/relaunch on device**: Requires both a set passcode and device unlock. Inaccessible if passcode is removed.
- **Use case in M1**: Credentials that require strong device protection.
- **Test outcome**: ✓ Stored & Retrieved in simulator; passcode requirement tested on device.

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

1. **Refresh Tests**: Attempts to store and retrieve each synthetic credential, displaying immediate access results.
2. **Clear Test Credentials**: Removes all probe test credentials from the Keychain, leaving production data untouched.
3. **Status Display**: Shows timestamp of last test run and accessibility class results.

The UI label indicates "Simulator" vs. device context; physical lock behavior cannot be tested on a simulator.

## Lock/Relaunch Tests for P09

Physical device testing must cover:

### Lock Behavior

1. **Store a credential** with `WhenUnlocked` accessibility.
2. **Lock the device** (press power button or use Control Center).
3. **Attempt to retrieve** the credential while locked (expect failure).
4. **Unlock the device**.
5. **Attempt to retrieve** the credential while unlocked (expect success).

### Relaunch After Lock

1. **Store credentials** with `AfterFirstUnlock` and `WhenUnlocked` classes.
2. **Lock the device**.
3. **Force quit the app** (or let it remain backgrounded).
4. **Relaunch the app**.
5. **Unlock the device**.
6. **Attempt retrieval** of both credentials (expect both accessible; `AfterFirstUnlock` should succeed without additional unlock).

### Passcode Requirement (WhenPasscodeSetThisDeviceOnly)

1. **Ensure device has a passcode** set.
2. **Store a credential** with `WhenPasscodeSetThisDeviceOnly` accessibility.
3. **Unlock and retrieve** (expect success).
4. **Lock the device**.
5. **Attempt retrieval** while locked (expect failure).

### Device Reboot

1. **Store credentials** with all five accessibility classes.
2. **Reboot the device** (power off and on).
3. **Before unlocking**, attempt retrieval (expect all to fail or timeout).
4. **After first unlock**, retrieve all credentials.
5. **Document which classes remain accessible** and which require re-unlock.

## Accessibility Before/After First Unlock

### Before First Unlock (on device after reboot)

- `WhenUnlocked`: ✗ Not accessible (requires unlock).
- `AfterFirstUnlock`: ✗ Not accessible (requires first unlock).
- `AfterFirstUnlockThisDeviceOnly`: ✗ Not accessible (requires first unlock).
- `WhenUnlockedThisDeviceOnly`: ✗ Not accessible (requires unlock).
- `WhenPasscodeSetThisDeviceOnly`: ✗ Not accessible (requires unlock and passcode).

### After First Unlock (during normal use)

- `WhenUnlocked`: ✓ Accessible.
- `AfterFirstUnlock`: ✓ Accessible (until next lock/reboot).
- `AfterFirstUnlockThisDeviceOnly`: ✓ Accessible (until next lock/reboot).
- `WhenUnlockedThisDeviceOnly`: ✓ Accessible.
- `WhenPasscodeSetThisDeviceOnly`: ✓ Accessible (if passcode set).

## Considerations for Production (P09)

### Recommended Classes for M1

| Use Case | Recommended Class | Rationale |
| --- | --- | --- |
| Provider API keys | `AfterFirstUnlockThisDeviceOnly` | Secure, persistent across app relaunch, device-bound. |
| Session tokens (temporary) | `WhenUnlockedThisDeviceOnly` | High security; cleared on lock. |
| Cached local credentials | `AfterFirstUnlockThisDeviceOnly` | Balance of security and accessibility. |

### Constraints

- **Simulator**: All accessibility classes appear accessible (no enforced locking).
- **Device**: Lock state, reboot, and passcode requirements are enforced and vary by class.
- **Backup/Restore**: `...ThisDeviceOnly` classes do not transfer; others may be restored (unless excluded via backup API).
- **Passcode**: Removal or change invalidates `WhenPasscodeSetThisDeviceOnly` credentials.

## Probe Build and Execution

### Build Command

```bash
ios/scripts/generate.sh
xcodebuild build -scheme CredentialProbe -configuration Debug
```

Or using the unified Makefile:

```bash
make ios-credential-probe
```

### Clean Build

```bash
xcodebuild clean -scheme CredentialProbe
```

## Results Recording for P09

This probe itself produces no production-ready evidence. P09 must record:

1. **Device/OS metadata**: iPhone model, iOS version, build number.
2. **Lock behavior**: For each accessibility class, the result of locked vs. unlocked retrieval.
3. **Relaunch behavior**: Accessibility after app relaunch and device reboot.
4. **Timestamp of test run**: When the device testing occurred.
5. **Any deviations** from documented behavior (e.g., unexpected access or denial).

**No synthetic test credentials or Keychain contents are published in P09 results.** Only the accessibility class behavior matrix is recorded.

## Privacy and Hygiene

- **No real credentials in public evidence**: All test data is synthetic and easily removable.
- **Probe credential cleanup**: Credentials are removed after each test session or via the "Clear" button.
- **No default credentials on fresh install**: The probe creates test data only when run.
- **Device-local testing**: All tests execute on the local device; no credentials leave the device.

## See Also

- [P05: Probe native notification scheduling](notification-probe.md) — Related probe for native integration.
- [P01: Prove the Rust-to-Swift boundary](core-binding.md) — Predecessor for boundary validation.
- [P09: Collect actual-device feasibility evidence](device-feasibility.md) — Physical testing using this probe's guidelines.
- [M1 Device and Trial Protocol](m1-protocol.md) — Overall testing methodology.
