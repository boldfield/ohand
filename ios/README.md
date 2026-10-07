# Oh And iOS

Native iOS application and probe components for Oh And M1.

## Architecture

The iOS project implements:
- **OhAndApp**: Main production application with SwiftUI capture and management UI
- **OhAndServices**: Services framework for concurrent native task implementations
- **OhAndControl**: Control target for testing and production capture control flow
- **OhAndCoreBridge**: Swift bridge to Rust core (maintained by B01)
- **Probe applications**: Isolated validation targets for M1 feasibility testing

## Reproducible Build System

This project uses **XcodeGen** to generate a fully reproducible Xcode project from `project.yml`. This ensures that clean checkouts generate identical project structures and build settings.

### Generate Project

```bash
xcodegen generate
```

This creates `OhAnd.xcodeproj` from `project.yml`. The generated project is not committed to the repository.

### Build for Simulator

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -configuration Debug \
  -sdk iphonesimulator \
  -derivedDataPath .derived
```

See [BUILD.md](BUILD.md) for complete build documentation.

## Project Structure

```
ios/
  AppAssembly/          # Main application composition (U01)
  OhAndCoreBridge/      # Rust core bridge (B01)
  Services/             # Service modules (extensible by M1 tasks)
    ProtectedStorage/   # File protection and device lock integration (C01)
    Authentication/     # Session-scoped read-auth boundary (C01)
    Credentials/        # Keychain storage (V04)
    ProviderTransport/  # Native HTTP and TLS validation (V05)
    Notifications/      # Schedule/cancel/list, event ingestion (N02)
    Ingress/            # Durable capture import (C02)
    Transcription/      # On-device transcript attachment (C05)
    ShadowReview/       # Optional shadow-review feature (E03)
    JobRunner/          # Foreground lifecycle job execution (J02)
    Deletion/           # Audio, ingress, cache cleanup (L02)
    AudioRetention/     # Expiry sweep and retention status (L03)
    Export/             # Consistent snapshot export (L04)
    Reset/              # Delete-all generation fencing (L05)
    Assembly/           # Service dependency injection (B02)
  Capture/              # UI capture modules
    Text/               # Text entry surface (C03)
    Voice/              # Recording control with recovery (C04)
    Entry/              # System control entry handoff (C06)
    Acknowledgment/     # Save acknowledgment UI (C06)
  Tests/                # Test modules (organized by service)
  BridgeProbe/          # Rust-Swift boundary validation (P01)
  NotificationProbe/    # Native notification scheduling (P05)
  AudioProbe/           # Audio capture and recovery (P03)
  TranscriptionProbe/   # On-device transcription (P04)
  CredentialProbe/      # Keychain credential protection (P11)
  CaptureProbe/         # System control handoff (P02)
  project.yml           # XcodeGen configuration (reproducible)
  BUILD.md              # Build documentation
  .gitignore            # Excludes generated projects and build artifacts
```

## Signing

**Simulator builds** use ad-hoc code signing and require no configuration. The project.yml sets:
- `CODE_SIGNING_REQUIRED=NO`: Signing not required
- `AD_HOC_CODE_SIGNING_ALLOWED=YES`: Ad-hoc signing permitted
- `CODE_SIGN_IDENTITY: iPhone Developer`: Default signing identity

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -sdk iphonesimulator \
  -configuration Debug
```

No external signing credentials, provisioning profiles, or team IDs are required for simulator builds.

**Device builds** require signing credentials supplied at build time. Override `DEVELOPMENT_TEAM` at the command line:

```bash
xcodebuild -project OhAnd.xcodeproj \
  -scheme OhAndApp \
  -sdk iphoneos \
  -configuration Release \
  DEVELOPMENT_TEAM="ABCD1234567"
```

The project-level `CODE_SIGN_IDENTITY: iPhone Developer` is a default placeholder. Actual signing is controlled by build-time overrides and device-provisioning settings, not repository files.

No certificates, provisioning profiles, team IDs, or signing material are committed to the repository.

## Module Ownership

Ownership is enforced by directory-based source inclusion in `project.yml`. Concurrent tasks add implementations to their owned directories without editing `project.yml`:

### Application Modules

| Module | Responsibility | Task |
| --- | --- | --- |
| `AppAssembly/` | Production app composition and UI assembly | U01 |
| `OhAndCoreBridge/` | Swift → Rust bridge, error conversion, threading | B01 |
| `Services/` | Native service implementations (see sub-modules) | Various |
| `Capture/` | User-facing capture UI modules | C03, C04, C06 |
| `Tests/` | Unit and integration tests | Various |

### Service Modules

| Module | Responsibility | Task |
| --- | --- | --- |
| `Services/ProtectedStorage/` | File protection, device lock integration | C01 |
| `Services/Authentication/` | Session-scoped read-auth boundary | C01 |
| `Services/Credentials/` | Keychain storage, opaque reference keys | V04 |
| `Services/ProviderTransport/` | Native HTTP, TLS validation | V05 |
| `Services/Notifications/` | Schedule/cancel/list, event ingestion | N02 |
| `Services/Ingress/` | Durable capture import | C02 |
| `Services/Transcription/` | On-device transcript attachment | C05 |
| `Services/ShadowReview/` | Optional shadow-review feature | E03 |
| `Services/JobRunner/` | Foreground lifecycle job execution | J02 |
| `Services/Deletion/` | Audio, ingress, cache cleanup | L02 |
| `Services/AudioRetention/` | Expiry sweep, retention status | L03 |
| `Services/Export/` | Consistent snapshot export | L04 |
| `Services/Reset/` | Delete-all generation fencing | L05 |
| `Services/Assembly/` | Production service dependency injection | B02 |

### Probe Targets

| Module | Responsibility | Task |
| --- | --- | --- |
| `BridgeProbe/` | Round-trip boundary validation | P01 |
| `NotificationProbe/` | Native scheduling/list/cancel primitives | P05 |
| `AudioProbe/` | Recording interruption and partial-audio recovery | P03 |
| `TranscriptionProbe/` | On-device transcription availability | P04 |
| `CredentialProbe/` | Keychain accessibility and lock behavior | P11 |
| `CaptureProbe/` | System-control handoff and entry validation | P02 |

## Deployment Target and SDK

- **Xcode**: 15.4
- **iOS SDK**: 17.5 (bundled with Xcode 15.4; not set in `project.yml`)
- **iOS Deployment Target**: 16.0 (minimum OS version; set in `project.yml` `options.deploymentTarget`)
- **Swift**: 5.9

The iOS SDK (17.5) is the system framework version bundled with Xcode 15.4. The deployment target (16.0) is the minimum iOS version the app supports, configurable in `project.yml` via `options.deploymentTarget`.

## Generated Project

The Xcode project (`OhAnd.xcodeproj`) is **generated** and not committed. Regenerate after editing `project.yml`:

```bash
xcodegen generate
```

Do not manually edit the generated Xcode project settings.

## Next Steps

1. **F01-F03** establish the project skeleton and build infrastructure
2. **P01-P11** validate individual capabilities with probe applications
3. **P10** decides on Tauri vs. SwiftUI management shell based on probe results
4. **U01** assembles the selected production shell using the validated infrastructure
