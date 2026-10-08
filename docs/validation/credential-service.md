# V04: Credential service validation

`ios/Services/Credentials/` stores provider secrets in the native Keychain behind opaque references. Evidence comes from the `ios.yml` run for the exact submitted commit (linked in the pull request); `OhAndTests.log` in the `ios-evidence` artifact lists every credential test below.

## Behavior

- A credential reference is a UUID string. Provider profiles hold only that reference; the callers' API returns only references, `CredentialStatus` (`present`, `absent`, `invalidated`) and `CredentialError`.
- Items are generic passwords with `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, not synchronized, in the app's own default access group. No shared access group is requested (`Config/OhAndApp.entitlements` lists only the app's own group).
- `updateCredential` overwrites the secret behind the same reference and re-applies the accessibility class. If the item is missing (for example after a restore) it re-binds the same reference. It never recurses; a racing insert gets one bounded replace retry.
- Empty secrets are rejected on add and update.
- `invalidated` means an item exists but cannot yield a usable secret: an empty stored value, `errSecDecode` or `errSecAuthFailed`. A locked device (`errSecInteractionNotAllowed`) is a separate transient `deviceLocked` error and is never reported as invalidated. Other statuses become `storage(operation, status)`.
- `resolveSecret` is the only path to secret bytes. It is module-internal so the native provider transport can attach a secret at dispatch, while the app target, control extensions, webview and core bridge cannot call it. Dispatch against an absent or invalidated credential fails with `notFound` or `invalidated`, which the transport maps to the `unauthorized` effect class.
- Errors carry only the operation and numeric status; they never include a reference, service name or secret.

## What the tests show

| Acceptance condition | Test |
| --- | --- |
| Add, update, delete; idempotent delete; bounded recovery | `CredentialServiceTests` (fake Keychain boundary) |
| Invalidated, locked and storage errors map to distinct outcomes | `CredentialServiceTests` with injected `OSStatus` values |
| Real Keychain round trip, accessibility class read back from the item, not synchronized, re-created service sees the item (relaunch proxy), legacy-class item normalized on update, empty stored value reported invalidated | `CredentialKeychainIntegrationTests` (real simulator Keychain, ad-hoc signed host) |
| A canary secret never appears in references, statuses, JSON or any rendering of any error under every injected fault | `CredentialBoundaryTests` |
| No logging APIs in the credential sources; `resolveSecret` not public; only the device-only after-first-unlock class and no access group appear in the sources; the host entitlement grants only the app's own group | `CredentialBoundaryTests` source checks |

## Not verified here

- **Lock behavior.** The simulator never locks. Locked-device mapping is tested only through an injected `errSecInteractionNotAllowed`. Actual lock, reboot and pre-first-unlock behavior belongs to P09 using the P11 procedure (`docs/validation/credential-probe.md`).
- **True process relaunch.** Recreating the service object against the real Keychain is a proxy.
- **Webview, control extensions, exports and synced profile data** are not built yet. Their isolation rests on the API surface above (only references and statuses cross it) and the canary and source checks, and must be re-checked by the tasks that add those surfaces.

## Running the tests

The hosted test bundle needs the app's Keychain entitlement, which the simulator honors only for an ad-hoc signed build. CI passes `CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=` to `xcodebuild test`; with `CODE_SIGNING_ALLOWED=NO` the real-Keychain tests fail with `-34018`. See `ios/BUILD.md`.
