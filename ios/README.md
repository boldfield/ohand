# Oh And iOS

Native iOS application and probe components for Oh And M1.

## Architecture

The iOS project implements:
- **OhAndApp**: Main production application with SwiftUI capture and management UI
- **OhAndServices**: Services framework for concurrent native task implementations
- **OhAndCaptureControl**: System-control (WidgetKit) app extension embedded in the app; sources in `Capture/Entry/Control/`
- **OhAndCoreBridge**: Swift bridge to Rust core (maintained by B01)
- **Probe applications**: Isolated validation targets for M1 feasibility testing

## Build

The Xcode project is generated from `project.yml` with a pinned XcodeGen and is not committed. See [BUILD.md](BUILD.md) for the unsigned simulator build, the SDK/deployment baseline, signing configuration and, importantly, the current verification status: no native build has been run yet; that evidence comes from macOS CI (F05).

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
      Control/          # OhAndCaptureControl extension sources
      Shared/           # intent compiled into both the app and the control extension
    Acknowledgment/     # Save acknowledgment UI (C06)
  Tests/                # Test modules (organized by service)
  BridgeProbe/          # Rust-Swift boundary validation (P01)
  NotificationProbe/    # Native notification scheduling (P05)
  AudioProbe/           # Audio capture and recovery (P03)
  TranscriptionProbe/   # On-device transcription (P04)
  CredentialProbe/      # Keychain credential protection (P11)
  CaptureProbe/         # System control handoff (P02)
    Control/            # CaptureProbeControl extension sources
    Shared/             # intent compiled into both the probe app and its control
  Config/               # Shared xcconfig (unsigned simulator; local signing include)
  scripts/              # generate/build scripts and static project checks
  Mintfile              # Pinned XcodeGen version
  project.yml           # XcodeGen configuration
  BUILD.md              # Build documentation
  .gitignore            # Excludes generated projects and build artifacts
```

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

## Baseline

Xcode 16.4 (iOS 18.5 SDK), deployment target iOS 16.0 (control code gated with `@available(iOS 18.0, *)`), Swift 5 language mode, XcodeGen 2.40.0. Details in [BUILD.md](BUILD.md).
