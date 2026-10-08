# V04: Credential service validation

`ios/Services/Credentials/` stores provider secrets in the native Keychain behind opaque references. Evidence comes from the `ios.yml` run for the exact submitted commit (linked in the pull request); `OhAndTests.log` in the `ios-evidence` artifact lists every credential test below.

## Behavior

- A credential reference is a UUID string. Provider profiles hold only that reference; the callers' API returns only references, `CredentialStatus` (`present`, `absent`, `invalidated`) and `CredentialError`.
- Items are generic passwords with `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, not synchronized, in the app's own default access group. No shared access group is requested (`Config/OhAndApp.entitlements` lists only the app's own group).
- `updateCredential` overwrites the secret behind the same reference and re-applies the accessibility class. If the item is missing (for example after a restore) it re-binds the same reference. It never recurses; a racing insert gets one bounded replace retry.
- Empty secrets are rejected on add and update.
- `credentialStatus` is a health query that never reads secret bytes: it asks the Keychain for item attributes only (`KeychainBoundary.inspect` returns just an `OSStatus`) and maps `errSecSuccess` to `present`, `errSecItemNotFound` to `absent`, and `errSecDecode` or `errSecAuthFailed` to `invalidated`. Only `resolveSecret` requests the value.
- `invalidated` means an item exists but cannot yield a usable secret: `errSecDecode`, `errSecAuthFailed`, or (at dispatch, in `resolveSecret`) an empty stored value. Add and update reject empty secrets, so an empty value can only be written from outside the service; the health query reports it as `present` because detecting it would require reading the secret, and `resolveSecret` fails it explicitly with `invalidated`. A locked device (`errSecInteractionNotAllowed`) is a separate transient `deviceLocked` error and is never reported as invalidated. Other statuses become `storage(operation, status)`.
- `resolveSecret` is the only path to secret bytes. It is module-internal so the native provider transport can attach a secret at dispatch, while the app target, control extensions, webview and core bridge cannot call it. Dispatch against an absent or invalidated credential fails with `notFound` or `invalidated`, which the transport maps to the `unauthorized` effect class.
- Errors carry only the operation and numeric status; they never include a reference, service name or secret.

## What the tests show

| Acceptance condition | Test |
| --- | --- |
| Add, update, delete; idempotent delete; bounded recovery | `CredentialServiceTests` (fake Keychain boundary) |
| Invalidated, locked and storage errors map to distinct outcomes | `CredentialServiceTests` with injected `OSStatus` values |
| Real Keychain round trip, accessibility class read back from the item, not synchronized, re-created service sees the item, legacy-class item normalized on update, empty stored value fails `resolveSecret` as invalidated and recovers on update | `CredentialKeychainIntegrationTests` (real simulator Keychain, ad-hoc signed host) |
| Health query never reads secret bytes | `testStatusPathNeverReadsSecretBytes` (the fake sees zero `read` calls) and `testHealthQueryOfTheRealKeychainRequestsNoSecretData` (the real `inspect` requests attributes, not data) |
| Stored secret survives a true process relaunch | `CredentialRelaunchTests`, driven by `ios/scripts/credential-relaunch-simulator.sh`: the hosted test bundle runs twice as separate host app processes (`write`, then `verify`); the secret written by one process is resolved by the other, whose process id must differ, with the accessibility class read back from the real item. CI step "Run credential cross-process relaunch check"; logs `credential-relaunch-write.log` and `credential-relaunch-verify.log` in `ios-evidence` |
| A canary secret never appears in references, statuses, JSON or any rendering of any error under every injected fault | `CredentialBoundaryTests` |
| No logging APIs in the credential sources; `resolveSecret` not public; only the device-only after-first-unlock class and no access group appear in the sources; the host entitlement grants only the app's own group | `CredentialBoundaryTests` source checks |

## Not verified here

- **Physical lock behavior.** The simulator never locks. Locked-device mapping is tested only through an injected `errSecInteractionNotAllowed` (a transient `deviceLocked`, never `invalidated`). Actual lock, reboot and pre-first-unlock behavior requires a real device and is owned by the P09 device matrix using the P11 procedure (`docs/validation/credential-probe.md`); V04's lock acceptance is met here by the injected mapping plus the `AfterFirstUnlockThisDeviceOnly` class read back from the real item, and must not be claimed as device-verified until P09 reports.
- **Webview, control extensions, exports and synced profile data** are not built yet. Their isolation rests on the API surface above (only references and statuses cross it) and the canary and source checks, and must be re-checked by the tasks that add those surfaces.

## Running the tests

The hosted test bundle needs the app's Keychain entitlement, which the simulator honors only for an ad-hoc signed build. CI passes `CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=` to `xcodebuild test`; with `CODE_SIGNING_ALLOWED=NO` the real-Keychain tests fail with `-34018`. See `ios/BUILD.md`.
