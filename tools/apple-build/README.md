# Apple Build and Signing Tools

Reproducible signed-build automation for Oh And probes on iOS.

## Quick start

### Unsigned simulator build (credential-free)

```bash
tools/apple-build/sign-probe.sh BridgeProbe simulator
```

Output: Unsigned app in `ios/.derived/Build/Products/Debug-iphonesimulator/BridgeProbe.app`

### Signed device build

Requires Apple Developer Account and credentials (Team ID, certificate, provisioning profile).

```bash
# 1. Export credentials to environment
export APPLE_TEAM_ID="ABC123DEF4"
export APPLE_CERT_PATH="$HOME/path/to/cert.p12"
export APPLE_CERT_PASSWORD="password"
export APPLE_PROFILE_PATH="$HOME/path/to/profile.mobileprovision"

# 2. Set up keychain (one-time)
tools/apple-build/manage-keychain.sh setup
tools/apple-build/manage-keychain.sh import
tools/apple-build/install-profile.sh install "$APPLE_PROFILE_PATH"

# 3. Build signed probe
tools/apple-build/sign-probe.sh BridgeProbe adhoc
# or
tools/apple-build/sign-probe.sh BridgeProbe appstore

# 4. Clean up keychain
tools/apple-build/manage-keychain.sh cleanup
```

Output: Signed app in `ios/.derived/Build/Products/Release-iphoneos/BridgeProbe.app`

## Scripts

| Script | Purpose |
| --- | --- |
| `sign-probe.sh` | Build and sign a probe app |
| `manage-keychain.sh` | Create, import, and clean up signing keychain |
| `install-profile.sh` | Install and manage provisioning profiles |

## Documentation

Full documentation: [`docs/validation/signing.md`](../../docs/validation/signing.md)

- Account enrollment and certificate setup
- CI/CD integration
- Evidence collection
- Troubleshooting

## Security

- **Never commit credentials** (certificates, profiles, team IDs)
- **Use environment variables** for credential injection
- **Temporary keychain** is local to the build machine and automatically cleaned up
- **Sanitized evidence** is recorded (build time, bundle ID, identity hash) without sensitive data
- **For CI**: Store credentials in CI provider secrets vault, inject at build time, clean up after build

## Probes

Supported probe names:
- `BridgeProbe`
- `NotificationProbe`
- `AudioProbe`
- `TranscriptionProbe`
- `CredentialProbe`
- `CaptureProbe`

## Notes

- Simulator builds (default) are unsigned and require no credentials
- Device/adhoc builds require explicit credential inputs
- All scripts are idempotent and safe to re-run
- Keychain timeout is 1 hour; adjust in `manage-keychain.sh` if builds take longer
