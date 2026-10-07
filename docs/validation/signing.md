# Oh And: Apple Signing and Device Build Procedure

Status: P08 implementation. Initial signing infrastructure for reproducible signed builds.

## Overview

This document specifies the reproducible signed-build automation for Oh And probes on iOS. The procedure supports both unsigned simulator builds (credential-free) and signed device/adhoc builds (with externally supplied Apple identity and credentials). No signing material is committed to the repository.

## Design principles

1. **Simulator path is credential-free**: Simulator builds use `CODE_SIGNING_ALLOWED=NO` and require no certificates or provisioning profiles.
2. **Signed builds require explicit inputs**: Device and adhoc builds require externally supplied credentials injected at build time, never from repository files or environment defaults.
3. **Automatic code signing**: Uses Xcode's Automatic code signing (`CODE_SIGN_STYLE=Automatic`) to simplify credential management and reduce manual certificate/profile handling.
4. **Auditable evidence**: Every build records sanitized metadata (build time, bundle ID, code signing identity hash) with references to raw private evidence kept locally.
5. **No per-build human approval**: The process is automated and scripted; enrollment and keychain setup are one-time operations.

## Prerequisites

- Xcode 16.4 or later (matches `ios/project.yml` `options.xcodeVersion`)
- iOS SDK 18.5 or later (bundled with Xcode 16.4)
- Apple Developer Account with:
  - Active team membership
  - Signing certificate (Development or Distribution)
  - Provisioning profile(s) matching the bundle identifiers

## Terminology

| Term | Definition |
| --- | --- |
| **Team ID** | Apple Developer Team ID (e.g., `ABC123DEF4`); identifies the signing account |
| **Certificate** | `.p12` file containing the private key and signing certificate |
| **Provisioning Profile** | `.mobileprovision` file that maps the certificate to bundle IDs and devices |
| **Code Signing Identity** | The certificate selected for signing (identified by subject DN or hash) |
| **Keychain** | macOS system keystore where certificates and keys are stored |

## Unsigned Simulator Builds

Simulator builds are unsigned and require no credentials. They are the default and safest build path.

```bash
cd tools/apple-build
./sign-probe.sh BridgeProbe simulator
```

This produces:
- Unsigned `BridgeProbe.app` in `.derived/Build/Products/Debug-iphonesimulator/`
- Build log: `ios/.evidence/BridgeProbe-simulator-build.log`
- Metadata: `ios/.evidence/BridgeProbe-build-metadata.txt` (build time, SDK, configuration)

## Signed Device/Adhoc Builds

Signed builds require Apple identity and credentials. The build process is automated via the `sign-probe.sh` script; enrollment and keychain management are separate one-time operations (documented below).

### Prerequisites: Account enrollment

Before the first signed build, verify your Apple Developer Account:

1. Log in to [developer.apple.com](https://developer.apple.com)
2. Find your **Team ID** under Account > Membership > Team ID
3. Create or download a **Development Certificate**:
   - Go to Certificates, Identifiers & Profiles > Certificates
   - Click "+" to create a new certificate (Development or Distribution)
   - Follow Apple's certificate request workflow
   - Download the `.cer` file and double-click to install in Keychain
4. Create a **Provisioning Profile**:
   - Go to Certificates, Identifiers & Profiles > Profiles
   - Create a new profile for your app (e.g., `com.boldfield.ohand.probes.bridge`)
   - Select your certificate
   - Download the `.mobileprovision` file

### Step 1: Export credentials to environment

Set the following environment variables before building. **Never commit these.**

```bash
export APPLE_TEAM_ID="ABC123DEF4"                      # Your team ID
export APPLE_CERT_PATH="$HOME/path/to/cert.p12"        # Path to certificate (not in repo)
export APPLE_CERT_PASSWORD="your-cert-password"        # Certificate password
export APPLE_PROFILE_PATH="$HOME/path/to/profile.mobileprovision"  # Path to profile (not in repo)
```

For CI/server environments, store these securely (e.g., GitHub Secrets, CI provider vault) and inject at runtime. **Never log or echo these values.**

### Step 2: Set up temporary keychain (one-time)

Prepare the keychain for signing:

```bash
tools/apple-build/manage-keychain.sh setup ohand-build
tools/apple-build/manage-keychain.sh import ohand-build
tools/apple-build/install-profile.sh install "$APPLE_PROFILE_PATH"
```

This:
- Creates a temporary keychain (`ohand-build`)
- Imports the certificate
- Installs the provisioning profile to `~/Library/MobileDevice/Provisioning\ Profiles/`

### Step 3: Build signed probe

Build the probe as adhoc or appstore:

```bash
cd tools/apple-build

# Adhoc build (for testing on devices, ad-hoc distribution)
./sign-probe.sh BridgeProbe adhoc

# Appstore build (for App Store submission)
./sign-probe.sh BridgeProbe appstore
```

This:
- Generates the Xcode project
- Builds and signs the probe for the specified distribution method
- Records metadata with sanitized identifiers (build time, bundle ID, code signing identity hash)
- Produces the signed `.app` bundle

### Step 4: Clean up keychain (after build)

Lock and remove the temporary keychain:

```bash
tools/apple-build/manage-keychain.sh lock ohand-build
tools/apple-build/manage-keychain.sh cleanup ohand-build
```

This prevents accidental reuse and ensures the keychain is not left unlocked.

## Evidence collection and validation

Every build records sanitized evidence in the `.evidence/` directory. These files are content-free and do not contain private data.

### Simulator build evidence

**File**: `ios/.evidence/BridgeProbe-build-metadata.txt`

```
Build Type: Simulator (Unsigned)
Probe: BridgeProbe
Build Time: 2026-10-07T15:30:45Z
Build Configuration: Debug
SDK: iphonesimulator
Product Location: .derived/Build/Products/Debug-iphonesimulator/BridgeProbe.app
Signing: None (simulator)
```

### Signed build evidence

**File**: `ios/.evidence/BridgeProbe-build-metadata.txt`

```
Build Type: adhoc
Probe: BridgeProbe
Bundle Identifier: com.boldfield.ohand.probes.bridge
Build Time: 2026-10-07T15:35:22Z
Build Configuration: Release
SDK: iphoneos
Product Location: .derived/Build/Products/Release-iphoneos/BridgeProbe.app
Code Signing Identity Hash: 5a3b2c1d9e8f7g6h5i4j3k2l1m0n9o8
Team ID: (sanitized)
Provisioning Profile: (sanitized)
Export Method: ad-hoc
```

**Build logs**: Captured in `ios/.evidence/BridgeProbe-{build-type}-build.log` and `BridgeProbe-generate.log`.

### Private evidence

Raw private evidence (certificate passwords, full identity info, detailed profile data) is kept locally on the build machine and is NOT submitted to CI or version control. These files should be:

- Stored in `.gitignore` locations (e.g., `~/.apple-build-secrets/`)
- Protected with file permissions (mode 600)
- Cleaned up after the build or trial period

References to private evidence in task results or CI logs should use placeholder text like `(sanitized)` or `<certificate-id>`.

## CI/CD integration

For GitHub Actions or other CI runners, the signing process can be integrated as follows:

1. **Store secrets securely** in the CI provider's secret vault:
   - `APPLE_TEAM_ID`
   - `APPLE_CERT_PATH` (upload certificate file as artifact)
   - `APPLE_CERT_PASSWORD`
   - `APPLE_PROFILE_PATH` (upload profile as artifact)

2. **Example GitHub Actions workflow**:

```yaml
jobs:
  signed-build:
    runs-on: macos-15
    env:
      APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
      APPLE_CERT_PASSWORD: ${{ secrets.APPLE_CERT_PASSWORD }}
    steps:
      - uses: actions/checkout@v4
      
      - name: Set up Apple credentials
        run: |
          # Write certificate and profile from secrets
          echo "${{ secrets.APPLE_CERT_BASE64 }}" | base64 -d > cert.p12
          echo "${{ secrets.APPLE_PROFILE_BASE64 }}" | base64 -d > profile.mobileprovision
          export APPLE_CERT_PATH="$(pwd)/cert.p12"
          export APPLE_PROFILE_PATH="$(pwd)/profile.mobileprovision"
      
      - name: Prepare keychain
        run: |
          tools/apple-build/manage-keychain.sh setup ohand-build
          tools/apple-build/manage-keychain.sh import ohand-build
          tools/apple-build/install-profile.sh install "$APPLE_PROFILE_PATH"
      
      - name: Build signed probe
        run: |
          cd tools/apple-build
          ./sign-probe.sh BridgeProbe adhoc
      
      - name: Clean up
        if: always()
        run: |
          tools/apple-build/manage-keychain.sh cleanup ohand-build
          rm -f cert.p12 profile.mobileprovision
      
      - name: Upload evidence
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: build-evidence
          path: ios/.evidence
          retention-days: 7
```

## Distribution and trial validity

### Adhoc builds

- **Validity**: 30 days from the signing date
- **Distribution**: Ad-hoc provisioning; limited to up to 100 devices per team per year
- **Use case**: Developer testing on personal devices or limited beta
- **Renewal**: Regenerate with a new build to extend validity

### Appstore builds

- **Validity**: Until the certificate expires (typically 1 year); provisioning profile expires after 1 year
- **Distribution**: Apple App Store; unlimited users
- **Use case**: Production distribution
- **Renewal**: Request a new certificate when the current one expires; update provisioning profile

### Two-week trial coverage

For M1 milestone two-week trial testing on a device:
- If using **adhoc distribution**: Ensure the build is signed within 30 days of the trial start
- If using **appstore distribution**: Ensure the certificate is valid throughout the trial period
- **Recommended**: Use appstore builds for trials lasting longer than 1 week to avoid renewal during the trial

To verify remaining validity:

```bash
codesign -dv ios/.derived/Build/Products/Release-iphoneos/BridgeProbe.app 2>&1 | grep Authority
```

To check provisioning profile expiry:

```bash
security cms -D -i ~/Library/MobileDevice/Provisioning\ Profiles/<uuid>.mobileprovision 2>&1 | grep -A 2 "ExpirationDate"
```

## Troubleshooting

### Certificate not found in keychain

```
Error: code signing identity not found
```

**Cause**: Certificate not imported or keychain locked.

**Fix**:
1. Verify certificate path: `ls -la "$APPLE_CERT_PATH"`
2. Re-import: `tools/apple-build/manage-keychain.sh import ohand-build`
3. Check status: `tools/apple-build/manage-keychain.sh status ohand-build`

### Provisioning profile not found

```
Error: The provisioning profile is invalid
```

**Cause**: Profile not installed or mismatched bundle ID.

**Fix**:
1. Verify profile path: `ls -la "$APPLE_PROFILE_PATH"`
2. Re-install: `tools/apple-build/install-profile.sh install "$APPLE_PROFILE_PATH"`
3. Check bundle IDs match in the profile and `ios/project.yml`
4. List installed: `tools/apple-build/install-profile.sh list`

### Keychain timeout

```
Error: Keychain locked
```

**Cause**: Keychain locked after timeout.

**Fix**:
1. Unlock: `tools/apple-build/manage-keychain.sh unlock ohand-build`
2. Increase timeout in `manage-keychain.sh` if builds take longer than 1 hour

### Build fails with "Code Signing Allowed=NO"

This occurs if the environment incorrectly sets `CODE_SIGNING_ALLOWED=NO` for device builds.

**Fix**: Ensure signed builds use `CODE_SIGNING_ALLOWED=YES` (set in `sign-probe.sh`).

## Validation and acceptance

### Simulator builds

✓ Verification:
- Build log shows `CODE_SIGNING_ALLOWED=NO`
- App bundle location matches expected `.derived/Build/Products/Debug-iphonesimulator/`
- No certificates or provisioning profiles in the repository
- Metadata file records build time and SDK

### Signed device builds

✓ Verification (without real device):
- Build log shows `CODE_SIGNING_ALLOWED=YES` and team ID
- App bundle location matches expected `.derived/Build/Products/Release-iphoneos/`
- Metadata file records sanitized identity hash and build time
- Code signature is valid: `codesign -v <app>.app` returns 0

✓ Verification (with real device):
- Signed probe installs on provisioned device via Xcode or iPhone Configurator
- App launches and runs without certificate errors
- App remains installed for at least 30 days (adhoc) or 1 year (appstore)
- Device identifiers recorded (sanitized: UDID hash, device model, OS version)

### Two-week trial requirements

- Signed probe remains installable and launchable throughout the trial period
- No certificate expiry during trial
- Device evidence collected: trial start/end dates, device identifier hash, OS version

## Notes

- The signing process does not require per-build human approval; it is fully automated once credentials are configured.
- All scripts are idempotent: running `manage-keychain.sh setup` multiple times is safe.
- Temporary keychains are local to the build machine and do not affect system keychain or other applications.
- CI/CD environments should use runner-local keychain setup and tear-down to avoid cross-build credential leakage.
- For unattended CI builds, consider using GitHub secrets or CI provider vaults for credential storage.

## Future work (not in M1)

- Automated device provisioning and MDM integration
- WWDR certificate renewal automation
- Notarization and hardened runtime validation
- Multi-team/account support
- Simulator testing on different iOS versions
