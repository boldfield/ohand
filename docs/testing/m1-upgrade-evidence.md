# M1 upgrade persistence and trial candidate build evidence

This document specifies the upgrade procedure for T10, the acceptance evidence for upgrade persistence, and the process for producing the signed trial candidate for T05 and T06.

## Overview

T10 produces reproducible version/build identifiers and an upgrade procedure that preserves application state. Acceptance requires:

1. Upgrade a prior seeded install without uninstalling; sources, corrections, tombstones, profile references and scheduled reminder identity survive.
2. Build metadata and exported content-free metrics identify the tested revision; signing material and personal records never enter artifacts or repository.
3. Produce the actual signed candidate artifact for T05, recording revision, version/build, checksum and signing route/validity metadata.
4. Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference.

**External evidence is required.** Device upgrade testing can only be performed by the account holder on a real device. Missing device access is a concrete execution block, not a reason to fabricate completion.

## Version and build identifiers

Build identifiers are deterministic: `<revision-prefix>-<run-id>`, where:
- `revision-prefix`: first 8 characters of the git commit SHA at build time
- `run-id`: 8-character identifier from the build/collection timestamp

A rebuild of the same revision at a different time produces a different build identifier (different run-id). Rebuilding at the same timestamp with identical source produces the same build identifier. This allows:
- Unique identification of each build artifact and evidence collection
- Traceability to the source revision and build time
- Reproducible builds (same input → same revision prefix)
- Prevention of silent rebuilds (rebuilt binaries require repeated device checks per T10 acceptance criterion 3)

### Generating build identifiers

Use `tools/release/build_info.py` to generate reproducible identifiers:

```bash
python3 tools/release/build_info.py --json
```

Output format:
```json
{
  "build_identifier": "4911be09-195208Z",
  "build_revision": "4911be0994f48a5c79e5e9ce04a0c2f24d731176",
  "build_revision_short": "4911be09",
  "run_id": "195208Z",
  "collected_at": "2026-10-12T19:52:44Z",
  "working_tree_clean": true
}
```

The build identifier `4911be09-195208Z` identifies this specific build attempt.

## Trial candidate signed build procedure

The signed trial candidate is produced by the maintainer on a Mac with Xcode, an Apple Development identity and the trial device configured per `docs/validation/signing.md`.

### Steps

1. Ensure P08b prerequisites are met (see `docs/validation/signing.md` Prerequisites section):
   - Xcode pinned version (26.6 as of F08) selected with `xcode-select`
   - One valid Apple Development identity
   - Manually managed Development provisioning profile covering both `com.boldfield.ohand.app` and `com.boldfield.ohand.app.capture-control`
   - Trial iPhone with Developer Mode enabled and attached

2. Generate build identifiers:
   ```bash
   python3 tools/release/build_info.py --json > /tmp/build_info.json
   ```

3. Sign the OhAndApp scheme with the trial candidate procedure:
   ```bash
   export OHAND_SIGNING_PROFILE_PATH=/path/to/profile.mobileprovision
   export OHAND_DEVICE_ID=<device-identifier-from-devicectl>
   export OHAND_DEVICE_LABEL=trial-iphone

   python3 tools/apple-build/sign_probe.py --scheme OhAndApp --install
   ```

4. The tool writes a sanitized evidence record to `docs/validation/evidence/apple-signing/` with format:
   ```
   apple-signing-<UTC-timestamp>-<run-id>.json
   ```

5. Record the signed artifact metadata:
   - Build identifier (from step 2)
   - Evidence record file name (from step 4)
   - Profile expiry date (from the evidence record)
   - Device label (from step 3)

6. Extract the signed .ipa from the archive and record its SHA-256 checksum:
   ```bash
   # The tool prints the evidence record path; extract the .app from the archive
   # and derive the .ipa checksum for T05 and T06
   sha256sum /path/to/extracted.app > docs/releases/m1-trial/candidate-checksum.txt
   ```

### Output artifacts

After successful signing:
- **Evidence record**: `docs/validation/evidence/apple-signing/apple-signing-<timestamp>-<run-id>.json`
  - Status: "succeeded"
  - Install: succeeded = true
  - Launch: running_confirmed = true
  - Profile expiry date (must cover two-week trial window)
- **Build metadata**: `docs/releases/m1-trial/candidate-metadata.json` (created by step 5 recording)
  - Build identifier
  - Build revision
  - Evidence record reference
  - Profile expiry
  - Device label

## Upgrade persistence and testing

The upgrade procedure verifies that data survives across versions:
- **Sources**: Original user-entered text and capture context
- **Corrections**: User-applied corrections to interpreted/generated fields
- **Tombstones**: Deleted or archived item markers
- **Profile references**: Active provider profiles and configuration
- **Scheduled reminders**: Reminder IDs, scheduling metadata and notification history

### Simulator upgrade testing

Simulator testing uses seeded fixture data without external services. The test procedure:

1. Build and install a known prior version (from a tagged release or fixture)
2. Populate with synthetic capture data using deterministic fixtures
3. Upgrade to the trial candidate
4. Verify all data categories survive the upgrade
5. Record evidence (logs, database state dumps)

### Device upgrade testing

Device testing must be performed by the account holder with actual user data or authorized synthetic data.

The test procedure:

1. Install a known prior version on the trial device
2. Perform captures and reminders (synthetic or user-authorized)
3. Upgrade to the trial candidate (in-place installation via devicectl or TestFlight)
4. Verify all data categories survive
5. Document results and limitations

Observed constraints:
- TestFlight may have different upgrade semantics than direct device-install; document the route used
- If the signing profile is renewed, the signing route/renewal must be recorded so T06 can evaluate the two-week trial window
- If upgrade is interrupted or fails, record the failure mode (filesystem error, background timeout, etc.)

## Evidence record format

The upgrade evidence record (`docs/validation/evidence/upgrade/`) contains:

```json
{
  "schema": "ohand.upgrade-evidence.v1",
  "status": "succeeded",
  "collected_at": "2026-10-12T20:30:00Z",
  "test_environment": "simulator",
  "prior_version": {
    "build_identifier": "6ac3d1f8-c2cb6485",
    "build_revision": "6ac3d1f8d20dde731434d609120ad9cf77ac5956"
  },
  "trial_version": {
    "build_identifier": "4911be09-195208Z",
    "build_revision": "4911be0994f48a5c79e5e9ce04a0c2f24d731176"
  },
  "upgrade_method": "in-place-reinstall",
  "data_persistence": {
    "sources": {
      "count_before": 5,
      "count_after": 5,
      "verified": true
    },
    "corrections": {
      "count_before": 2,
      "count_after": 2,
      "verified": true
    },
    "tombstones": {
      "count_before": 1,
      "count_after": 1,
      "verified": true
    },
    "profile_references": {
      "active_before": ["anthropic-v1"],
      "active_after": ["anthropic-v1"],
      "verified": true
    },
    "scheduled_reminders": {
      "count_before": 3,
      "count_after": 3,
      "verified": true
    }
  },
  "device": {
    "label": "trial-iphone",
    "environment": "real"
  },
  "evidence_artifacts": [
    "upgrade-database-before.json",
    "upgrade-database-after.json",
    "install-log.txt"
  ],
  "working_tree_clean": true,
  "private_evidence_reference": "195208Z"
}
```

## Build metadata and signing information

For T05 and T06, record the signed candidate metadata:

File: `docs/releases/m1-trial/candidate-metadata.json`

```json
{
  "schema": "ohand.trial-candidate.v1",
  "build_identifier": "4911be09-195208Z",
  "build_revision": "4911be0994f48a5c79e5e9ce04a0c2f24d731176",
  "collected_at": "2026-10-12T19:52:44Z",
  "signing_evidence_record": "apple-signing-20261012T195208Z-441b7e77.json",
  "bundle_identifier": "com.boldfield.ohand.app",
  "embedded_bundles": [
    {
      "bundle_identifier": "com.boldfield.ohand.app",
      "profile_expires_at": "2027-10-09T00:08:01Z"
    },
    {
      "bundle_identifier": "com.boldfield.ohand.app.capture-control",
      "profile_expires_at": "2027-10-09T00:08:01Z"
    }
  ],
  "profile_validity": {
    "earliest_expiry": "2027-10-09T00:08:01Z",
    "two_week_trial_deadline": "2027-09-25T00:08:01Z",
    "renewal_status": "not_planned"
  },
  "route": "development",
  "identity_class": "Apple Development",
  "checksum": {
    "algorithm": "sha256",
    "value": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  },
  "artifact_location": "docs/releases/m1-trial/OhAndApp.ipa"
}
```

## No external endpoint or vendor configuration

The trial candidate must not include:
- Signing material, certificate paths or team identifiers
- Provider API endpoints, credentials or model configuration
- Private device identifiers or hardware serialization
- User personal information or test captures

All configuration (providers, endpoints, credentials) is injected at runtime through the standard configuration mechanisms and does not enter artifacts or this repository.

## Review and acceptance

T10 is complete when the signed candidate is produced and both simulator and device upgrade evidence are collected. This requires external inputs:

1. Version/build infrastructure in `tools/release/` — provides deterministic, collision-free build identifiers
2. Upgrade procedure documentation in `docs/testing/m1-upgrade-evidence.md` — this document
3. Signed candidate metadata in `docs/releases/m1-trial/candidate-metadata.json` — produced by the maintainer with device access (P08)
4. Upgrade evidence records in `docs/validation/evidence/upgrade/` — requires simulator tests and real-device upgrade attempts
   - Simulator evidence (automated test)
   - Device evidence (maintainer-collected with authorized test device)
5. Every evidence record cites sanitized artifacts, build/revision and collection time — enforced by schema and evidence collection process

All private raw evidence (logs, profiles, device identifiers) is kept local outside the repository with an auditable reference. Missing external evidence (device access, signing credentials) blocks task completion and must be recorded honestly.
