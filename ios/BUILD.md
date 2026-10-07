# iOS Project Build Documentation

## Reproducible Build Overview

The Oh And iOS project uses XcodeGen to generate a reproducible Xcode project from `project.yml`. This ensures that clean checkouts produce identical project structures and build settings.

## Requirements

- **macOS**: 14.5+
- **Xcode**: 15.4 (pinned; use `xcode-select` to verify)
- **iOS Deployment Target**: 16.0
- **Swift**: 5.9
- **XcodeGen**: 2.40.0 (pinned; verify with `xcodegen --version`)

## Building the Project

### 1. Generate Xcode Project

```bash
cd ios
xcodegen generate
```

This reads `project.yml` and generates `OhAnd.xcodeproj` with all target, scheme, and build setting definitions. The generated project must be regenerated after editing `project.yml`.

### 2. Build for Simulator

Simulator builds use ad-hoc signing by default. The project.yml settings (`CODE_SIGNING_REQUIRED=NO`, `AD_HOC_CODE_SIGNING_ALLOWED=YES`, `CODE_SIGN_IDENTITY: iPhone Developer`) enable ad-hoc code signing that does not require signing credentials or profiles:

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -configuration Debug \
  -sdk iphonesimulator
```

### 3. Build Options

- **Scheme**: `OhAndApp` (main app), `OhAndControl` (control target), `OhAndTests` (unit tests), `BridgeProbe`, `NotificationProbe`, `AudioProbe`, `TranscriptionProbe`, `CredentialProbe`, `CaptureProbe` (probes)
- **SDK**: `iphonesimulator` (simulator) or `iphoneos` (device)
- **Configuration**: `Debug` or `Release`
- **Derived Data**: Use `-derivedDataPath` to set build artifact location; default is `~/Library/Developer/Xcode/DerivedData/`

## Signing Configuration

### Simulator Builds (Ad-Hoc Signing)

Simulator builds use ad-hoc code signing by default; no external credentials or provisioning profiles are required. The project settings are:
- `CODE_SIGNING_REQUIRED=NO`: Signing is not required
- `AD_HOC_CODE_SIGNING_ALLOWED=YES`: Ad-hoc signing is permitted
- `CODE_SIGN_IDENTITY: iPhone Developer`: Default signing identity

```bash
xcodebuild -project OhAnd.xcodeproj -scheme OhAndApp -sdk iphonesimulator
```

### Device Builds

Device builds require signing credentials supplied at build time. Provide `DEVELOPMENT_TEAM`:

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -configuration Release \
  -sdk iphoneos \
  DEVELOPMENT_TEAM="ABCD1234567"
```

The project-level `CODE_SIGN_IDENTITY: iPhone Developer` is a placeholder default that must be overridden by build settings or Xcode provisioning.

**Important**: No certificates, provisioning profiles, team IDs, or signing credentials are committed to the repository. Signed builds require externally supplied credentials and a valid provisioning profile for the target device.

## Project Structure

The project follows the F01 ownership map for deterministic, concurrent development:

- `project.yml` - XcodeGen configuration file (reproducible project definition)
- `AppAssembly/` - Main application target source (OhAndApp)
- `OhAndCoreBridge/` - Core framework and Rust bindings bridge
- `Services/` - Service modules (ProtectedStorage, Authentication, Credentials, ProviderTransport, Notifications, Ingress, Transcription, ShadowReview, NotificationPermission, Permissions, Health, Metrics, JobRunner, Deletion, AudioRetention, Export, Reset, NotificationActions, BackgroundCompletion, TimeChangeCoordinator, Assembly, OptionalCapabilities)
- `Capture/` - UI capture modules (Text, Voice, Entry, Acknowledgment)
- `Tests/` - Unit and integration tests (organized by service/module)
- `<X>Probe/` - Probe applications (BridgeProbe, NotificationProbe, AudioProbe, TranscriptionProbe, CredentialProbe, CaptureProbe)

## Module Ownership

Ownership is enforced through directory-based source inclusion, so concurrent native tasks can add files to their owned directories without editing `project.yml`:

- **F03**: Creates `ios/project.yml` and framework/target skeletons
- **F05**: May edit `project.yml` to add CI targets after F03
- **P01**: Adds generated bindings to `core/bindings/` for use by B01
- **Service tasks**: Add implementations directly to their owned service directories

## Incremental Generation

After the first `xcodegen generate`, subsequent project changes can be applied by:

1. Editing `project.yml` to add targets or modify build settings
2. Running `xcodegen generate` again
3. No manual Xcode configuration is required

## Reproducibility and CI Validation

F03 establishes the build system, project structure, and proof-of-generation artifacts. F05 adds native CI integration and provides evidence that clean checkouts generate identical project structures and pass simulator build checks on macOS runners.

This documentation defines the reproducible build interface, command structure, and directory ownership; reproducibility is verified by F05's CI integration.

## SDK and Deployment Baseline

- **Xcode Version**: 15.4 (pinned; use `xcode-select` to verify)
- **iOS SDK**: 17.5 (bundled with Xcode 15.4; selected via `-sdk iphonesimulator` or `-sdk iphoneos`)
- **iOS Deployment Target**: 16.0 (minimum OS version; set in `project.yml` `options.deploymentTarget` and `IPHONEOS_DEPLOYMENT_TARGET`)
- **Swift Version**: 5.9

The iOS SDK version (17.5) is determined by the installed Xcode version, not by a setting in `project.yml`. The deployment target (16.0) is the minimum OS version the application supports. These are independent values: deployment target is configured in `project.yml`, while the SDK is selected by Xcode based on the `-sdk` flag and Xcode version.

These baselines are configurable by editing `project.yml`, but changes require regeneration and CI verification. Do not change them without updating F05 CI baselines.

## Probe and Target Modules

F03 reserves the following targets. Each probe is a minimal application with a foreground native surface for testing the specified capability:

### Main Application Targets

- `OhAndApp`: Production application with capture and management UI
- `OhAndControl`: Control target for testing capture control flow (distinct from OhAndApp)
- `OhAndTests`: Unit test bundle for integration tests
- `OhAndServices`: Services framework used by both OhAndApp and OhAndControl

### Probe Targets

- `BridgeProbe`: Rust-Swift boundary validation (P01)
- `NotificationProbe`: Native scheduling/list/cancel primitives (P05)
- `AudioProbe`: Recording interruption and partial-audio recovery (P03)
- `TranscriptionProbe`: On-device transcription availability (P04)
- `CredentialProbe`: Keychain accessibility and lock behavior (P11)
- `CaptureProbe`: System-control handoff and entry validation (P02)

Each probe target includes its own foreground UI screen for manual testing and demonstration.
