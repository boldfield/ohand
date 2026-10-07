# iOS Project Build

## Verification status

Nothing in this directory has been generated or built natively yet. Authoring happened on Linux, where neither XcodeGen nor Xcode exists. The only checks run so far are the static checks in `scripts/check_project_config.py` (see below). The native generate, simulator build and unit-test result for an exact revision is collected by macOS CI established in F05; until that runs, treat the project as unverified.

## Baseline (provisional)

| Item | Value | Where it is selected |
| --- | --- | --- |
| Xcode | 15.4 | Installed toolchain; `options.xcodeVersion` in `project.yml` |
| Simulator/device SDK | iOS 17.5 (bundled with Xcode 15.4) | `-sdk iphonesimulator` or `-sdk iphoneos` plus the installed Xcode |
| Deployment target | iOS 16.0 | `options.deploymentTarget` and `IPHONEOS_DEPLOYMENT_TARGET` |
| Swift | 5.9 | `SWIFT_VERSION` |
| XcodeGen | 2.40.0 | `Mintfile`; enforced by `scripts/generate.sh` |

The SDK is chosen by Xcode, not by `project.yml`; the deployment target is the minimum OS the app runs on.

## Reproducible unsigned simulator build

```bash
cd ios
./scripts/build-simulator.sh            # OhAndApp
./scripts/build-simulator.sh AudioProbe # any scheme
```

The script runs `scripts/generate.sh` (pinned XcodeGen, writes the git-ignored `OhAnd.xcodeproj`) and then `xcodebuild build -sdk iphonesimulator ... CODE_SIGNING_ALLOWED=NO`. `Config/Base.xcconfig` also disables signing for the simulator SDK, so no certificate, profile or team is needed. Project settings contain no `CODE_SIGN_IDENTITY` or `DEVELOPMENT_TEAM`.

Schemes: `OhAndApp`, `OhAndControl`, `OhAndTests` (build and test), and one per probe: `BridgeProbe`, `NotificationProbe`, `AudioProbe`, `TranscriptionProbe`, `CredentialProbe`, `CaptureProbe`. `OhAndCoreBridge` and `OhAndServices` are frameworks built as dependencies.

Run unit tests (macOS):

```bash
xcodebuild test -project OhAnd.xcodeproj -scheme OhAndTests -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 15' CODE_SIGNING_ALLOWED=NO
```

## Device signing

The team is configurable without touching tracked files: copy `Config/Local.xcconfig.example` to `Config/Local.xcconfig` (git-ignored) and set `DEVELOPMENT_TEAM`, or pass `DEVELOPMENT_TEAM=<id>` to `xcodebuild`. Bundle identifiers are under `com.boldfield.ohand`. No certificates, provisioning profiles or credentials are committed.

## Static checks (any host)

```bash
python3 ios/scripts/check_project_config.py
python3 -m unittest discover -s ios/scripts -p 'test_*.py'
```

These need PyYAML. They verify structure only: every F01-owned native root is a source root of exactly the expected target (and `Services/` of exactly one), the test bundle is a real unit-test bundle with a test action, bundle identifiers are unique and under the project prefix, no signing material or identity is configured, the simulator is unsigned in `Base.xcconfig`, plists parse, required usage strings and module-qualified scene delegates are present, and the control Swift condition is valueless. They do not prove the project generates or compiles.

## Source inclusion and ownership

Targets include whole directories, so owners add files under their F01 paths without editing `project.yml`. Directories hold `.placeholder` files so XcodeGen finds them; those are excluded from targets.

| Directory | Target |
| --- | --- |
| `OhAndCoreBridge/` | `OhAndCoreBridge` framework (B01) |
| `Services/` | `OhAndServices` framework, the only target that compiles it; depends on `OhAndCoreBridge` |
| `AppAssembly/`, `Capture/` | `OhAndApp` and `OhAndControl`; depend on `OhAndServices` |
| `Tests/` | `OhAndTests` unit-test bundle, hosted by `OhAndApp` |
| `<Name>Probe/` | the same-named probe application (whole directory except `Info.plist`) |

`Capture/` is app-side code and may import `OhAndServices`; `Services/` cannot import `Capture/`. B02's composition root in `Services/Assembly/` therefore wires service dependencies, and U01 in `AppAssembly/` composes `Capture/` views with it.

Every target's usage strings live in its own `Info.plist`: the app has microphone and speech recognition; `AudioProbe` has microphone; `TranscriptionProbe` has microphone and speech recognition.

## Targets

- `OhAndApp`: production app. Production capture uses a foreground native surface: `Capture/` views are presented from the foreground app scene, and no background-launched recording is assumed.
- `OhAndControl`: reserved control-entry build variant (system control/shortcut handoff, P02/C06). It compiles the same sources as `OhAndApp` with the Swift condition `OHAND_BUILD_TARGET_CONTROL`, so entry-specific code can be isolated with `#if OHAND_BUILD_TARGET_CONTROL` without a second source tree. It has its own bundle identifier.
- Probes: minimal apps each showing a foreground native screen for the capability named in the F01 ownership map.
