# Signed device build procedure

This document covers the maintainer procedure for P08b and, from P11a, for signing the production `OhAndApp` scheme with its embedded `OhAndCaptureControl` extension. The tooling in [`tools/apple-build/`](../../tools/apple-build/) was written and tested on Linux against synthetic `security`, `xcodebuild`, `xcrun`/`devicectl` and `mint` stand-ins (`make apple-build-test`, part of `make test`). **P08b has recorded a real signed build and device install on a maintainer Mac.** See the [Evidence and validity](#evidence-and-validity) section for the observed run; statements about Xcode behaviour that section verifies are confirmed by that run, and the subsection notes which assumptions remain untested.

## Route

One route is supported: **Development**. It uses an `Apple Development` identity and a Development provisioning profile that lists the target device, and installs with `devicectl`. Ad-hoc and App Store routes are intentionally not implemented: neither is needed for the two-week trial on the maintainer's own device, and an App Store build cannot be installed directly on a device.

The probe is archived unsigned (`CODE_SIGNING_ALLOWED=NO`), then `xcodebuild -exportArchive` signs it from an explicit export options file written for each run:

| Key | Value |
| --- | --- |
| `method` | `debugging` |
| `signingStyle` | `manual` |
| `signingCertificate` | `Apple Development` |
| `teamID` | read from the profile |
| `provisioningProfiles` | each signed bundle's identifier mapped to the profile **UUID read from the profile** (never its file name) |

Signing at export keeps each bundle identifier's profile separate. Passing a profile specifier on the `xcodebuild` command line would apply it to every target, including the frameworks, which do not take profiles. Two kinds of scheme are supported:

- **Application-only probes** (`BridgeProbe`, `AudioProbe`, `TranscriptionProbe`, `NotificationProbe`): one profile, one `provisioningProfiles` entry, schema v1 evidence record, install only.
- **`OhAndApp`** (P11a), which embeds the `OhAndCaptureControl` app extension (`com.boldfield.ohand.app.capture-control`) alongside the `OhAndCoreBridge` and `OhAndServices` frameworks. The signed bundles are read from the project spec and verified against the unsigned archive's application and app extensions (`PlugIns` and `Extensions`); there is no fixed list, and frameworks are never listed. `provisioningProfiles` gets one entry per application or extension. One supplied profile may cover every bundle (a wildcard such as `<team>.com.boldfield.ohand.*`, or an exact identifier); alternatively a per-bundle mapping assigns one profile to each bundle (see [Inputs](#inputs)). Install is followed by a launch and a running check, and the evidence record is schema v2.

Before project generation or the archive, the tool reads the signed bundles from `ios/project.yml` with PyYAML (the scheme's application and every app-extension target it depends on with embedding on; frameworks are never listed) and refuses with exit `3`, naming only the bundle identifier, when a bundle is uncovered or its covering profile is expired, Xcode managed or does not list the target device. Reading the spec fails closed. The reader models a strict subset of XcodeGen for the application and its dependency targets: an explicit `PRODUCT_BUNDLE_IDENTIFIER` in flat settings or `settings.base`, per-configuration `settings.configs` overrides only when they repeat that identifier, and a fixed set of target and dependency keys. The run stops before project generation with exit `4`, and nothing is skipped, when the spec cannot be read; when a dependency of the application has no resolvable type or bundle identifier or is of a kind the tool does not know how to sign; when a configuration overrides a bundle identifier with a different one; when an app extension embeds a signed bundle of its own; when the application or any target it reaches through its dependencies, directly or through a framework, has an executable build phase (`preBuildScripts`, `postCompileScripts`, `postBuildScripts` or `buildRules`) other than the one reviewed phase (the Rust core build of `OhAndCoreBridge`, accepted only on a framework target and only exactly as `ios/project.yml` declares it; it writes a static library and header outside any bundle), a source entry with a key outside the modeled set (such as `buildPhase` or `type`), or a source that is, or is a directory containing, an `.app`, `.appex`, `.xpc` or `.framework` bundle; when the `INFOPLIST_FILE` of the application or an extension cannot be read or sets a literal `CFBundleIdentifier` instead of `$(PRODUCT_BUNDLE_IDENTIFIER)`; when a declared scheme of the same name has actions or builds anything but its own target; or when the spec uses a construct outside the subset (a top-level `include`, `templates`, `settings.groups`, a platform list, `options.transitivelyLinkDependencies`, or any other target key such as `info` or `configFiles`, or dependency key such as `copy`). The message names the construct and the target. Targets reached through a framework get the same target, source and build-phase checks, and must themselves be frameworks or libraries; an application or extension reached that way refuses the run. After the archive, its application and extensions must be exactly the bundles the spec declared. A difference is a tooling-integrity failure (exit `4`, failed stage `archive-mismatch`, message naming the differing bundle identifiers), not a profile decision: the coverage already checked would not apply, so the tool signs nothing and the spec or the project's embedding must be reconciled first. Everything that touches the keychain, installs a profile or signs happens after both checks.

## Prerequisites

Only the maintainer can provide these (for `OhAndApp` the profile or profiles must cover **both** `com.boldfield.ohand.app` and `com.boldfield.ohand.app.capture-control`; the app requests only `keychain-access-groups` under its own prefix, so a wildcard Development profile such as the one used for P08b is expected to cover both, and a later capability may require explicit App IDs and the per-bundle mapping); see [the external prerequisites](../features/m1-external-prerequisites.md#signed-build-evidence-for-p08b) for the account, Xcode, device and profile steps. In short: the Xcode version pinned in `ios/project.yml` selected with `xcode-select`, an Apple Development identity, a **manually managed** Development profile that covers the bundle identifier and lists the iPhone, Developer Mode enabled on the iPhone, and the iPhone attached. XcodeGen (or `mint`) at the pinned version is needed because the tool runs `ios/scripts/generate.sh`. `OhAndApp` also needs PyYAML for the `python3` that runs the tool (`python3 -m pip install pyyaml`), because it reads `ios/project.yml` before the build; without it the run stops with exit `2` before any build.

The export signs manually, and `xcodebuild` is expected to reject an Xcode-managed profile (the "iOS Team Provisioning Profile" that Xcode's automatic signing creates) under manual signing. The tool therefore refuses a profile with `IsXcodeManaged` set, with exit status `2` for a probe and `3` for `OhAndApp` (see [Procedure](#procedure)). Create the profile in the Apple Developer portal instead: a Development profile for the probe's bundle identifier (an explicit App ID, or a wildcard that covers it), the Apple Development certificate, and the iPhone. Download it and point `OHAND_SIGNING_PROFILE_PATH` at it. This replaces the automatic-signing profile source in step 3 of the external prerequisites document for the profile file only; registering the device with the team is still needed.

Command Line Tools alone cannot sign or reach a device.

## Inputs

Inputs come from the environment or from files outside the repository. The tool refuses certificate, password and profile paths that resolve inside the repository, and refuses a private-evidence directory inside it. Nothing is read from tracked files.

| Variable | Required | Meaning |
| --- | --- | --- |
| `OHAND_SIGNING_PROFILE_PATH` | yes, unless the map is set | Development `.mobileprovision` file. For `OhAndApp` it must cover every signed bundle. |
| `OHAND_SIGNING_PROFILE_MAP_PATH` | `OhAndApp` only, instead of the above | JSON file outside the repository mapping each signed bundle identifier to its own `.mobileprovision` file, for example `{"com.boldfield.ohand.app": "/path/app.mobileprovision", "com.boldfield.ohand.app.capture-control": "/path/control.mobileprovision"}`. Every signed bundle needs an entry; an entry naming a bundle that is not a signed bundle of the scheme is refused (exit `2`) before the build. All profiles must belong to one team. Setting both variables is refused. |
| `OHAND_SIGNING_CERT_PATH` | no | `.p12` holding the Apple Development identity. If set, it is imported into a per-run temporary keychain. If unset, exactly one valid Apple Development identity must already be in the user's keychains (for example the one Xcode created). |
| `OHAND_SIGNING_CERT_PASSWORD` or `OHAND_SIGNING_CERT_PASSWORD_FILE` | with the `.p12` | Password, or a file holding it |
| `OHAND_SIGNING_PROJECT_SPEC_PATH` | no | `OhAndApp` only. XcodeGen spec to read the signed bundles from; defaults to `ios/project.yml`. It exists so the behaviour tests can drive the spec reading; leave it unset for a real run, because project generation always uses `ios/project.yml`. |
| `OHAND_DEVICE_ID` | with `--install` | The device identifier `xcrun devicectl list devices` shows |
| `OHAND_DEVICE_UDID` | no | `OhAndApp` with `--install` only. The device's hardware identifier, used to check that each covering profile lists the target device. If unset, the tool asks `xcrun devicectl device info details` for it; set this variable if that lookup fails. It is never printed or recorded. |
| `OHAND_DEVICE_LABEL` | no | Content-free label for the evidence record (`a-z`, `0-9`, `-`, at most 32 characters); a random `device-xxxxxxxx` label is used otherwise |
| `OHAND_PRIVATE_EVIDENCE_DIR` | no | Where the redacted raw run log is kept; defaults to `~/.ohand-private-evidence` |

Export these in the same shell step that runs the tool. Do not pass them on a command line. The tool does not forward `OHAND_SIGNING_*` or `OHAND_DEVICE_*` to the tools it launches.

## Procedure

1. Check that the signing-capable toolchain is selected: `xcodebuild -version` must report the Xcode version pinned in `ios/project.yml`, and `security find-identity -v -p codesigning` must list one valid `Apple Development` identity. Do not paste that output anywhere; it contains your name and team identifier.
2. Export the inputs above. Find the device identifier with `xcrun devicectl list devices`; do not paste that output either.
3. Run the tool from any directory:

   ```bash
   python3 tools/apple-build/sign_probe.py --scheme BridgeProbe --install
   ```

   Without `--install` it builds and exports only. With `--install` it requires `OHAND_DEVICE_ID` and exits non-zero without touching anything when that is missing.
4. Read the exit status: `0` success; `1` a build, signing, install, launch or running-check stage failed, or cleanup could not restore the keychain search list or remove the temporary keychain, installed profile or working directory; `2` an input was missing or unusable, with the variable named in the message; `3` (`OhAndApp` only) a bundle has no usable profile; `4` (`OhAndApp` only) the project spec could not be resolved to its signed bundles, or the archive embeds different bundles than the spec. A failed `devicectl install` is exit `1`.
5. The tool prints the name of the sanitized evidence record it wrote. (`OhAndApp` records are schema `ohand.apple-signing-evidence.v2`; probe records stay v1.) By default that is `docs/validation/evidence/apple-signing/apple-signing-<UTC timestamp>-<run id>.json`, anchored to the repository root regardless of the caller's working directory (override with `--evidence-dir`). A failed run also writes a record with `"status": "failed"` and `failed_stage` naming where it stopped (`cleanup` when only the cleanup after an otherwise successful run failed; the original stage is kept when cleanup fails after another failure); only a record with `"status": "succeeded"` and `install.succeeded` true counts as an install.
6. Commit the sanitized record and follow the P08b steps in the external prerequisites document. Do not commit anything from the private evidence directory.

### Signing and launching OhAndApp (steps P11b follows)

1. Prepare the Development profile or profiles as above, for both `com.boldfield.ohand.app` and `com.boldfield.ohand.app.capture-control`, with the iPhone listed. Export `OHAND_SIGNING_PROFILE_PATH` (one covering profile) or `OHAND_SIGNING_PROFILE_MAP_PATH` (one profile per bundle), the certificate and device variables, and optionally `OHAND_DEVICE_LABEL`. Unlock the iPhone and keep it attached.
2. Run `python3 tools/apple-build/sign_probe.py --scheme OhAndApp --install`.
3. Exit `3` means a bundle was refused before project generation, the archive or any keychain work. The message names only the bundle identifier and says which of these applies: no supplied profile covers it, or the covering profile is expired, Xcode managed, an enterprise profile, not a Development profile, lists no devices, or does not list the target device. Fix the profile in the developer portal and rerun. Exit `4` is not a profile problem: read the message (failed stage `project-spec` or `archive-mismatch`) and fix the project spec or its embedding. Exit `1` with failed stage `launch` or `running-check` means the app installed but did not start or was not in the device's process list (a locked device is a likely cause); the record then shows `install.succeeded` true and `launch.running_confirmed` false.
4. A successful run needs a record with `"status": "succeeded"`, `install.succeeded` true and `launch.running_confirmed` true. It also lists every signed bundle with its profile's expiry (`embedded_bundles`) and the earliest of those expiries (`profile.expires_at`). The record does not show whether the control appears in the system control gallery; check that by hand on the device and report it as observed or as not checked.
5. Commit the sanitized record to `main` under `docs/validation/evidence/apple-signing/` and unblock P11b with a note naming that file.

Unobserved assumptions for this procedure (P11a was built and tested only against synthetic stand-ins on Linux; none of these has been run against a real device): `xcrun devicectl device info details`, `device process launch` and `device info processes` accept `--json-output` and return `result.hardwareProperties.udid`, `result.process.processIdentifier` and `result.runningProcesses[].processIdentifier` respectively; `device process launch` accepts `--terminate-existing`; and `xcodebuild -exportArchive` accepts a `provisioningProfiles` entry for an app extension under manual signing. If one is wrong, the run fails at the named stage with a fixed message and the redacted private log has the details; the tool should then be corrected rather than the check removed.

## What the tool does

- Resolves the profile with `security cms -D -i`, parses the result in memory, and takes name, UUID, team and expiry from it. The profile contents are never logged. It refuses a profile whose UUID is not a canonical `8-4-4-4-12` hexadecimal UUID (the UUID names the installed file, so a value with path separators or `..` could otherwise point outside Xcode's profile directory) or that is expired, lists no devices, provisions all devices, is not a Development profile (`get-task-allow`), or does not cover the probe's bundle identifier.
- With a `.p12`: creates a keychain with a random per-run password under a private temporary directory, imports the identity, runs `set-key-partition-list` so `codesign` can use the key without a prompt, puts the keychain first in the user search list (refusing, before changing anything, if the current list cannot be read, because it could then not be restored), and requires exactly one `Apple Development` identity in it. A `.p12` that provides only another identity class (for example Apple Distribution) is refused.
- Installs each profile it signs with under its UUID in `~/Library/Developer/Xcode/UserData/Provisioning Profiles` (created if missing; the location Xcode 16 and later read) for the run, then removes it afterwards unless that UUID was already installed. It copies to a run-specific `.ohand-signing-<run id>.partial` file in that directory and renames it onto the UUID name, so an interrupted copy never leaves a truncated profile under the UUID name, and cleanup removes the partial file too. If a file with that UUID name already exists it is reused only when its bytes equal the supplied profile; a different file is refused with exit status `2`.
- Archives unsigned, exports with the options above, checks the exported `.ipa` holds one signed `.app`, and installs that `.app` with `xcrun devicectl device install app`. For `OhAndApp` it first checks that the archive embeds exactly the bundles the project spec declared, and after export checks that the exported app embeds the same bundle identifiers as the archive and that every extension is signed, then launches the app by bundle identifier with `xcrun devicectl device process launch` and confirms the launched process identifier appears in `xcrun devicectl device info processes`. A failed launch, a launch result without a process identifier, or a missing process exits `1`.
- For `OhAndApp` with `--install`, before project generation, checks that every covering profile lists the target device's hardware identifier (from `OHAND_DEVICE_UDID`, or `xcrun devicectl device info details`).
- Cleans up on every exit it can handle: success, a failing stage, bad input after setup, and `SIGTERM`, `SIGHUP` or `SIGINT`. It restores the original keychain search list, deletes the temporary keychain (and removes the file itself if `delete-keychain` fails), removes the installed profile and the working directory, and exits non-zero if the keychain could not be removed. `SIGKILL` or power loss cannot be handled; if that happens, look for `ohand-signing-*` directories under the temporary directory, delete the keychain inside with `security delete-keychain`, check `security list-keychains -d user`, and remove any `.ohand-signing-*.partial` file and the profile the run installed from `~/Library/Developer/Xcode/UserData/Provisioning Profiles`.

The `.p12` password and the temporary keychain password are passed to `security` as arguments, which are visible to other local processes for the length of each call; run it on a machine you control.

## Output handling

- Stdout and stderr carry fixed messages only. Output of `security`, `xcodebuild` and `devicectl` is written to `run.log` in the private evidence directory (mode `0600` in a `0700` directory), with the input file paths (profile, certificate, password file; file names can carry the profile UUID or team identifier), the passwords, device identifier, the hardware identifiers listed in the profile, team identifier and profile name/UUID replaced by labelled placeholders. The log may still include other text those tools print, such as the certificate's common name, so keep it local and out of Git.
- The sanitized record has fixed fields only: schema, status, failed stage, collection time, build revision and whether the tree was clean, a build identifier (`<revision prefix>-<run id>`), scheme and bundle identifier, route and identity class, Xcode version, the profile's expiry date, the device label, whether install was requested/performed/succeeded, and the run id naming the private log. It carries no team identifier, certificate detail, raw device identifier, profile content, or any hash of them. The tool refuses to write the record if a registered sensitive value would appear in it.
- The `OhAndApp` (v2) record adds `embedded_bundles` (bundle identifier, kind, and the expiry date of the profile that signs it) and `launch` (requested, performed, launched, running confirmed). Bundle identifiers are not sensitive and are recorded; the profile UUIDs, team identifier, device identifier and hardware identifiers are still never recorded, and the tool still refuses to write a record that would contain one.
- The device label is not derived from the device identifier. It identifies the device only to the maintainer, who chose it.

## Unsigned simulator builds

`ios/scripts/build-simulator.sh` stays credential-free: it passes `CODE_SIGNING_ALLOWED=NO` (or an ad-hoc identity when `OHAND_SIMULATOR_ADHOC_SIGN=1`), never reads the variables above, and uses no signing tool. A behaviour test runs it with the signing variables set and asserts none reaches the build command or its output.

## Assumptions P08b must confirm on a real Mac

These could not be tested on Linux. The October 2026 run confirmed the following:

- ✓ `xcodebuild -exportArchive` accepts an unsigned archive with `method` `debugging` and the manual options above under Xcode 26.6.
- ✓ A profile placed in `~/Library/Developer/Xcode/UserData/Provisioning Profiles` is found by name-independent UUID lookup during export on a Mac with stored profiles.
- ⚠️ UUID lookup on a Mac where Xcode has not yet stored any profile: not exercised. The October 2026 run occurred on a Mac with an Xcode-managed profile already stored (per [m1-external-prerequisites.md](../features/m1-external-prerequisites.md#signed-build-evidence-for-p08b)), so the fresh-Mac case was not observed.
- ✓ A manually created Development profile satisfies the manual export options.
- ⚠️ Xcode itself rejects an Xcode-managed profile under manual signing. The tool (sign_probe.py:154-156) refuses `IsXcodeManaged` profiles before calling xcodebuild, so Xcode's own rejection behaviour was not observed.
- ⚠️ `security list-keychains -d user` parsing: confirmed if a `.p12` was imported, untested if keychain search-list modification was bypassed. The October 2026 record does not indicate whether `OHAND_SIGNING_CERT_PATH` was set, so this remains unconfirmed for now.
- ✓ `devicectl device install app` accepts the extracted `.app` with the `OHAND_DEVICE_ID` format.

## Evidence and validity

On 2026-10-09, the maintainer successfully built and installed BridgeProbe on a trial device using Xcode 26.6 (build 17F113) with a clean working tree. The signed build evidence is recorded in [`apple-signing-20261009T000932Z-c2cb6485.json`](./evidence/apple-signing/apple-signing-20261009T000932Z-c2cb6485.json):

| Attribute | Value |
| --- | --- |
| Collection time | 2026-10-09T00:10:14Z |
| Build revision | `6ac3d1f8d20dde731434d609120ad9cf77ac5956` |
| Build identifier | `6ac3d1f8-c2cb6485` |
| Scheme | BridgeProbe |
| Device label | `trial-phone` |
| Profile expiry | 2027-10-09T00:08:01Z |

**Verified assumptions:** The run confirmed the assumptions marked ✓ above.

**Install validity:** The BridgeProbe install is signed with a Development provisioning profile valid until 2027-10-09T00:08:01Z. A two-week trial must begin before 2027-09-25T00:08:01Z to complete before profile expiry. The trial build (T10) is covered only if signed with this same profile; per [m1-external-prerequisites.md](../features/m1-external-prerequisites.md#signed-build-evidence-for-p08b), that profile is a wildcard App ID with the same certificate and device.

**Renewal:** The profile expires 2027-10-09T00:08:01Z. With an expiry approximately one year away at the time of collection, renewal is not planned within the trial window and remains untested.
