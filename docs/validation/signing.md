# Oh And: Apple Signing and Device Build Procedure

Status: P08 implementation. Initial signing infrastructure for reproducible signed builds.

## Overview

This document specifies the reproducible signed-build automation for Oh And probes on iOS. The procedure supports both unsigned simulator builds (credential-free) and signed device/adhoc builds (with externally supplied Apple identity and credentials). No signing material is committed to the repository.

## Design principles

1. **Simulator path is credential-free**: Simulator builds use `CODE_SIGNING_ALLOWED=NO` and require no certificates or provisioning profiles.
2. **Signed builds require explicit inputs**: Device and adhoc builds require externally supplied credentials injected at build time, never from repository files or environment defaults.
3. **Manual code signing for CI compatibility**: Uses `CODE_SIGN_STYLE=Manual` with explicit provisioning profiles and code signing identities for reproducible headless builds.
4. **Auditable evidence**: Every build records sanitized metadata (build time, bundle ID, code signing identity hash) with references to raw private evidence kept locally and to git revision.
5. **Proper keychain management**: Temporary keychains are created per build, configured for headless signing, and cleaned up to prevent credential leakage across builds.
6. **No per-build human approval**: The process is automated and scripted; enrollment and keychain setup are one-time operations.

## Prerequisites

- Xcode 16.4 or later (matches `ios/project.yml` `options.xcodeVersion`)
- iOS SDK 18.5 or later (bundled with Xcode 16.4)
- For simulator builds: None required
- For signed device/adhoc builds:
  - Apple Developer Account with active team membership
  - Signing certificate (Development or Distribution) exported as .p12 file
  - Provisioning profile(s) matching the bundle identifiers (.mobileprovision files)
  - Registered physical device UDID (for device installation)

## Terminology

| Term | Definition |
| --- | --- |
| **Team ID** | Apple Developer Team ID (e.g., `ABC123DEF4`); identifies the signing account |
| **Certificate** | `.p12` file containing the private key and signing certificate |
| **Provisioning Profile** | `.mobileprovision` file that maps the certificate to bundle IDs and devices |
| **Code Signing Identity** | The certificate selected for signing (identified by subject DN or hash) |
| **Keychain** | macOS system keystore where certificates and keys are stored |
| **Device UDID** | Unique Device IDentifier; a 40-character hex string identifying a physical device |

## Unsigned Simulator Builds

Simulator builds are unsigned and require no credentials. They are the default and safest build path, suitable for initial development and testing.

```bash
cd tools/apple-build
./sign-probe.sh BridgeProbe simulator
```

This produces:
- Unsigned `BridgeProbe.app` in `ios/.derived/Build/Products/Debug-iphonesimulator/`
- Build logs: `ios/.evidence/BridgeProbe-simulator-build.log`, `ios/.evidence/BridgeProbe-generate.log`
- Metadata: `ios/.evidence/BridgeProbe-build-metadata.txt` (build time, SDK, configuration, git revision)

The simulator build requires no credentials and can be run on any macOS machine with Xcode.

## Signed Device/Adhoc Builds

Signed builds require Apple identity and credentials. The build process is automated via the `sign-probe.sh` script; enrollment and keychain management are separate one-time operations (documented below).

**Important**: Signed device builds require external credentials that must be supplied at build time. This task cannot be completed without access to:
- Valid Apple Developer account credentials
- Development or Distribution signing certificate (.p12)
- Provisioning profile for the target device or ad-hoc distribution
- Physical device or simulator with the provisioning profile installed

If any of these prerequisites are unavailable, the task is **blocked**. Do not attempt to work around missing credentials by fabricating evidence or using placeholder values.

### Prerequisites: Account enrollment

Before the first signed build, verify your Apple Developer Account:

1. Log in to [developer.apple.com](https://developer.apple.com)
2. Find your **Team ID** under Account > Membership > Team ID
3. Create or download a **Development Certificate**:
   - Go to Certificates, Identifiers & Profiles > Certificates
   - Click "+" to create a new certificate (Development or Distribution)
   - Follow Apple's certificate request workflow
   - Download the `.cer` file and double-click to install in Keychain
   - Export the certificate as `.p12` (right-click, Export)
4. Register your **physical device**:
   - Connect device to macOS
   - Open Xcode > Window > Devices and Simulators
   - Select your device and copy the Identifier (UDID)
   - In developer.apple.com, go to Devices and register the UDID
5. Create a **Provisioning Profile**:
   - Go to Certificates, Identifiers & Profiles > Profiles
   - Create a new profile for your app (e.g., `com.boldfield.ohand.probes.bridge`)
   - Select your certificate and include your registered device
   - Download the `.mobileprovision` file

### Step 1: Export credentials to environment

Set the following environment variables before building. **Never commit these.**

```bash
export APPLE_TEAM_ID="ABC123DEF4"                      # Your team ID
export APPLE_CERT_PATH="$HOME/path/to/cert.p12"        # Path to certificate (not in repo)
export APPLE_CERT_PASSWORD="your-cert-password"        # Certificate password
export APPLE_PROFILE_PATH="$HOME/path/to/profile.mobileprovision"  # Path to profile (not in repo)
export APPLE_DEVICE_UDID="<40-character-device-id>"    # Device UDID for installation
```

For CI/server environments, store these securely (e.g., GitHub Secrets, CI provider vault) and inject at runtime. **Never log or echo these values.**

### Step 2: Build signed probe

Build the probe as adhoc or appstore:

```bash
cd tools/apple-build

# Adhoc build (for testing on registered devices with ad-hoc distribution)
./sign-probe.sh BridgeProbe adhoc

# Appstore build (for internal testing, requires TestFlight)
./sign-probe.sh BridgeProbe appstore
```

This:
- Generates the Xcode project
- Creates a temporary keychain with partition list configuration for headless signing
- Imports the certificate and configures it for automated code signing
- Installs the provisioning profile
- Builds and signs the probe for the specified distribution method
- If APPLE_DEVICE_UDID is set, installs the signed app to the device
- Records metadata with sanitized identifiers (build time, bundle ID, code signing identity hash, device install status)
- Cleans up the temporary keychain

### Step 3: Device installation and verification

If `APPLE_DEVICE_UDID` is provided in the environment, the script automatically installs the signed app to the device after building. You can also manually install a pre-built app:

```bash
DEVICE_UDID="<40-character-device-id>"
APP_BUNDLE="ios/.derived/Build/Products/Release-iphoneos/BridgeProbe.app"

# Install the app
xcrun devicectl device install app "$DEVICE_UDID" "$APP_BUNDLE"

# Verify installation
xcrun devicectl device info "$DEVICE_UDID"
```

### Device identifier recording

The build process records a sanitized hash of the device UDID in the evidence metadata. The raw device UDID is never recorded in version control or CI logs. The hash allows verification that the same device was used across builds without exposing the device identifier.

## Evidence collection and validation

Every build records sanitized evidence in the `ios/.evidence/` directory. These files are content-free and do not contain private data.

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
Git Revision: 31d44894823d460984870e98c7ad974a2b34965a
Git Branch: mr/7d4836ee
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
Device UDID Hash: a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6
Installation Status: Success
Installation Time: 2026-10-07T15:35:30Z
Git Revision: 31d44894823d460984870e98c7ad974a2b34965a
Git Branch: mr/7d4836ee
```

**Build logs**: Captured in `ios/.evidence/BridgeProbe-{build-type}-build.log` and `BridgeProbe-generate.log`.

### Private evidence

Raw private evidence (certificate passwords, full identity info, detailed profile data, device UDIDs) is kept locally on the build machine and is NOT submitted to CI or version control. These files should be:

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
   - `APPLE_DEVICE_UDID` (if available; optional)

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
          if [ -n "${{ secrets.APPLE_DEVICE_UDID }}" ]; then
            export APPLE_DEVICE_UDID="${{ secrets.APPLE_DEVICE_UDID }}"
          fi
      
      - name: Build signed probe
        run: |
          cd tools/apple-build
          ./sign-probe.sh BridgeProbe adhoc ios/.evidence
      
      - name: Clean up credentials
        if: always()
        run: |
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

### Ad-hoc builds

- **Distribution method**: Direct sideload via Xcode, Apple Configurator, or xcrun devicectl
- **Device limit**: Up to 100 devices per team per year
- **Validity duration**: The validity of the signing certificate (typically 1 year)
- **Use case**: Developer testing on personal devices or limited beta testing
- **Renewal**: Create a new certificate when the current one expires

### App Store builds

- **Distribution method**: Requires TestFlight or App Store submission; cannot be directly sideloaded
- **Device limit**: Unlimited for TestFlight or App Store
- **Validity duration**: The validity of the signing certificate (typically 1 year)
- **Use case**: Production distribution or wide internal testing
- **Renewal**: Create a new certificate when the current one expires

### Two-week trial coverage

**IMPORTANT**: M1 milestone two-week trial testing requires a distribution route whose actual install validity covers the full trial period.

**Recommended route**: Ad-hoc builds with a development certificate, sideloaded to a provisioned device.
- Ensures the app can be installed directly on the device
- Does not require App Store or TestFlight submission
- Signing certificate validity is the limiting factor (typically 1 year)

**NOT recommended for M1 trial**: App Store builds are not suitable for direct device testing because they require TestFlight or App Store distribution, adding process overhead outside the scope of this implementation.

To verify certificate validity:

```bash
security find-identity -v -p codesigning | grep "Apple Development"
```

To check provisioning profile validity:

```bash
ls -la ~/Library/MobileDevice/Provisioning\ Profiles/*.mobileprovision | head -5
```

## Troubleshooting

### Certificate not found in keychain

```
Error: code signing identity not found
```

**Cause**: Certificate not imported or keychain locked.

**Fix**:
1. Verify certificate path: `ls -la "$APPLE_CERT_PATH"`
2. Verify APPLE_CERT_PASSWORD is correct
3. Check keychain status: `security list-keychains`
4. Re-run the build; the script creates a fresh keychain each time

### Provisioning profile not found

```
Error: The provisioning profile is invalid
```

**Cause**: Profile not installed or mismatched bundle ID.

**Fix**:
1. Verify profile path: `ls -la "$APPLE_PROFILE_PATH"`
2. Check bundle IDs match in the profile and `ios/project.yml`
3. List installed profiles: `ls -la ~/Library/MobileDevice/Provisioning\ Profiles/`
4. Re-run the build; the script re-installs the profile each time

### Device not found for installation

```
Error: Device not available
```

**Cause**: Device UDID is not valid or device is not connected.

**Fix**:
1. Connect the device to macOS
2. Verify UDID: `xcrun device list | grep -i connected`
3. Ensure the device is provisioned for the profile
4. Re-run the build with a valid APPLE_DEVICE_UDID

### Automatic signing failures

If you encounter failures with `CODE_SIGN_STYLE=Automatic`, the issue is likely that Xcode does not have account access in a CI environment. The scripts use `CODE_SIGN_STYLE=Manual` with explicit provisioning profiles and code signing identities for reproducible headless builds.

## Validation and acceptance

### Simulator builds

✓ Verification:
- Build log shows `CODE_SIGNING_ALLOWED=NO`
- App bundle location matches expected `.derived/Build/Products/Debug-iphonesimulator/`
- No certificates or provisioning profiles in the repository
- Metadata file records build time, SDK, and git revision
- Build logs are in `ios/.evidence/` and are retained appropriately

### Signed device builds (with credentials)

✓ Verification (when credentials are available):
- Build log shows `CODE_SIGNING_ALLOWED=YES`, team ID, and manual signing
- App bundle location matches expected `.derived/Build/Products/Release-iphoneos/`
- Metadata file records sanitized identity hash, build time, git revision, and device UDID hash
- Code signature is valid: `codesign -v <app>.app` returns 0
- Provisioning profile is correctly installed in `~/Library/MobileDevice/Provisioning Profiles/`
- Keychain is created and cleaned up per build (no persistent keychain left behind)

✓ Verification (with real device):
- Signed probe installs on provisioned device via `xcrun devicectl device install app`
- App launches and runs without certificate errors
- Build metadata records installation success and device identifier hash
- Device remains available and the app remains installed throughout testing

### Two-week trial requirements

For M1 milestone completion:
- Signed build was successfully created and installed on a registered device
- Build metadata records device identifier hash, installation status, and time
- Certificate validity covers the full two-week trial period
- Device remains available and the app can be launched throughout the trial
- Build evidence (logs, metadata) with git revision is available for review

## Acceptance Criteria Status

**AC1 — Credential-free simulator path**: ✓ Implemented  
Build without credentials using `CODE_SIGNING_ALLOWED=NO` for simulator builds.

**AC2 — Signed build with device install and identifiers**: ⚠️ Conditional on external inputs  
With supplied credentials and device, builds are signed and installed to device; device UDID hash is recorded sanitized in metadata. Without credentials/device, task is blocked.

**AC3 — Keychain cleanup**: ✓ Implemented  
Temporary keychain is created per build, configured for headless signing, and deleted after build to prevent credential leakage.

**AC4 — Verified distribution validity**: ⚠️ Conditional on external verification  
Ad-hoc distribution via direct sideload is recommended; validity is the certificate lifespan (typically 1 year). No unverified expiry claims.

**AC5 — Auditable evidence**: ✓ Implemented  
Evidence includes build time, git revision, bundle ID, code signing identity hash (sanitized), and device UDID hash (sanitized). Raw private evidence is kept locally.

## Blocking Prerequisites

This task is **blocked** if any of the following prerequisites are unavailable:

- Valid Apple Developer account with active team membership
- Signing certificate (.p12 file) exported from the developer account
- Provisioning profile (.mobileprovision file) for the bundle ID and device
- Physical device UDID registered in the developer account and connected to the build machine

If any of these inputs are unavailable, work cannot proceed to real device installation. All other work (build automation, evidence handling, keychain management) remains valid when credentials are supplied.

## Future work (not in M1)

- Automated device provisioning and MDM integration
- WWDR certificate renewal automation
- Notarization and hardened runtime validation
- Multi-team/account support
- Simulator testing on different iOS versions
- TestFlight distribution automation
