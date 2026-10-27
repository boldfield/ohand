# M1 trial candidate records

A candidate record describes one signed build of `OhAndApp` (with its embedded capture extension) made for the trial phone.
`tools/release/sign_trial_candidate.sh` writes it; this directory holds the records that are kept.

## How a candidate is made

Run on a clean, full-history checkout of `main` on a Mac with Xcode, with the signing inputs described in
`tools/apple-build/sign_probe.py` in the environment (`OHAND_SIGNING_PROFILE_MAP_PATH` or `OHAND_SIGNING_PROFILE_PATH`,
`OHAND_SIGNING_CERT_PATH`, `OHAND_SIGNING_CERT_PASSWORD[_FILE]`). All of them are files outside the repository.

```
tools/release/sign_trial_candidate.sh --output-dir <directory outside the repository>
```

The wrapper

1. refuses a dirty tree or a shallow clone before any Apple tool runs (`tools/release/build_info.py --require-clean`);
2. calls `tools/apple-build/sign_probe.py` to archive, sign and export `OhAndApp`; that tool refuses the run when either bundle
   identifier (`com.boldfield.ohand.app`, `com.boldfield.ohand.app.capture-control`) has no usable profile;
3. keeps the exported `.ipa` in `--output-dir` (never inside the repository: it embeds a provisioning profile) and hashes it;
4. writes the record to `docs/releases/m1-trial/candidate-<build number>-<first 8 of the revision>.json`.

`tools/release/sign_trial_candidate.sh --verify <record> --archive <ipa>` recomputes the archive's SHA-256 and fails on a
mismatch.

## Build identity

Every build of the app carries four values in its `Info.plist`, stamped by `ios/scripts/generate_build_info.sh` from
`tools/release/build_info.py`:

| Key | Value |
| --- | --- |
| `CFBundleShortVersionString` | the marketing version in `ios/AppAssembly/Info.plist` |
| `CFBundleVersion` | the build number: the first-parent commit count of the revision |
| `OhAndBuildRevision` | the full 40-digit git SHA |
| `OhAndBuildDirty` | `true` when the tree had uncommitted changes |

The first-parent count grows with every commit on `main`, so a later build always installs over an earlier one. A build that
cannot name its revision fails; nothing is written as a placeholder. A Release build refuses a dirty or shallow tree.

## Record format: `ohand.trial-candidate.v1`

```json
{
  "schema": "ohand.trial-candidate.v1",
  "revision": "<full 40-digit SHA of main>",
  "version": "0.1.0",
  "build_number": "<first-parent count>",
  "archive_file": "OhAnd-<version>-<build>-<revision prefix>.ipa",
  "archive_sha256": "<64 lowercase hex digits of the exported .ipa>",
  "signing_route": "development",
  "profile_expires_at": "<UTC time the earliest-expiring profile stops working, e.g. 2026-12-31T00:00:00Z>",
  "created_at": "<UTC time the record was written>"
}
```

The record never holds a team identifier, a profile name or UUID, a certificate name or hash, a device identifier, or any hash of
those. The wrapper compares the text with every sensitive value the signing run registered and refuses to write the record
if one appears. `profile_expires_at` is the date to plan the next candidate by: the build stops launching on the phone after it.

The archive itself is not committed.
