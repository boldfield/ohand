# iOS Project Build Documentation

## Reproducible Build Overview

The Oh And iOS project uses XcodeGen to generate a reproducible Xcode project from `project.yml`. This ensures that clean checkouts produce identical project structures and build settings.

## Requirements

- **macOS**: 14.5+
- **Xcode**: 15.4+
- **iOS Deployment Target**: 16.0+
- **Swift**: 5.9+
- **XcodeGen**: 2.40.0 (or compatible version specified in build tools)

## Building the Project

### 1. Generate Xcode Project

```bash
cd ios
xcodegen generate
```

This reads `project.yml` and generates `OhAnd.xcodeproj`.

### 2. Build for Simulator

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -configuration Debug \
  -sdk iphonesimulator \
  -derivedDataPath .derived
```

### 3. Build Options

- **Scheme**: `OhAndApp`, `BridgeProbe`, `NotificationProbe`, `AudioProbe`, `TranscriptionProbe`, `CredentialProbe`, `CaptureProbe`
- **SDK**: `iphonesimulator` (simulator) or `iphoneos` (device)
- **Configuration**: `Debug` or `Release`

## Signing Configuration

Unsigned simulator builds require no signing configuration and are the default.

For device builds, signing credentials must be provided at build time via command line or Xcode build settings:

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -configuration Release \
  -sdk iphoneos \
  CODE_SIGN_IDENTITY="iPhone Developer" \
  DEVELOPMENT_TEAM="ABCD1234567" \
  PROVISIONING_PROFILE_SPECIFIER="OhAnd Distribution Profile"
```

**Important**: No certificates, provisioning profiles, or team IDs are committed to the repository. All signing material must be supplied at build time.

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

## Proof of Reproducibility

Simulator build validation and CI integration are established by F05. This documentation defines the build interface and directory structure; F05 will demonstrate that clean checkouts generate identical XcodeGen output and link real CI evidence.

## SDK and Deployment Baseline

- **SDK**: iOS 16.0 (configurable in project.yml and `IPHONEOS_DEPLOYMENT_TARGET`)
- **Swift Version**: 5.9
- **Xcode Version**: 15.4

These can be updated by editing `project.yml` and regenerating the project.

## Expected Target Probes

The following probe targets are reserved by F03 and supported by the structure above:
- `BridgeProbe`: Round-trip boundary validation
- `NotificationProbe`: Native scheduling/list/cancel primitives
- `AudioProbe`: Recording interruption and partial-audio recovery
- `TranscriptionProbe`: On-device transcription availability
- `CredentialProbe`: Keychain accessibility and lock behavior
- `CaptureProbe`: System-control handoff and entry validation
