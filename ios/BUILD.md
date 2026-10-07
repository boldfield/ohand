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

- `project.yml` - XcodeGen configuration file (reproducible project definition)
- `Sources/App` - Main application target (OhAndApp)
- `Sources/Core` - Core framework (minimal Swift stubs, Rust bindings added by P01)
- `Sources/Services` - Services framework (extensible by concurrent M1 tasks)
- `Probes/*/Sources` - Probe applications for M1 validation

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

## Simulator Compatibility

Current probes build and run on:
- iPhone 16 Pro simulator
- iOS 16.0+ deployment target
- SwiftUI framework

## SDK and Deployment Baseline

- **SDK**: iOS 16.0 (configurable in project.yml and `IPHONEOS_DEPLOYMENT_TARGET`)
- **Swift Version**: 5.9
- **Xcode Version**: 15.4

These can be updated by editing `project.yml` and regenerating the project.
