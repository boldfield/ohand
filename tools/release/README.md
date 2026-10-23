# M1 Release and upgrade tools

This directory contains tooling for T10: generating reproducible version/build identifiers, signing trial candidates, and documenting upgrade persistence.

## Tools

- `build_info.py`: Generate reproducible build identifiers from the current git revision and timestamp
- `sign_trial_candidate.sh`: Sign the OhAndApp trial candidate for T05 and T06 testing
- `test_build_info.py`: Unit tests for build identifier generation

## Workflow

1. **Generate build identifiers**: `python3 build_info.py --json`
   - Produces deterministic `<revision-short>-<run-id>` identifiers
   - Same revision + timestamp = same identifier
   - Different timestamp = different identifier (prevents silent rebuilds)

2. **Sign trial candidate** (requires Mac with Xcode):
   - `OHAND_SIGNING_PROFILE_PATH=... python3 ../apple-build/sign_probe.py --scheme OhAndApp --install`
   - Produces signed .ipa and evidence record
   - Records in `docs/validation/evidence/apple-signing/`

3. **Test upgrade persistence**:
   - Simulator: automated, runs with fake providers
   - Device: performed by maintainer with real device and authorized data

4. **Record evidence**:
   - Upgrade evidence: `docs/validation/evidence/upgrade/`
   - Build metadata: `docs/releases/m1-trial/candidate-metadata.json`

See `docs/testing/m1-upgrade-evidence.md` for the full procedure.

## Build identifier format

Format: `<revision-prefix>-<run-id>`

Example: `4911be09-195208Z`

- `revision-prefix`: First 8 characters of git HEAD commit SHA
- `run-id`: 8-character hex identifier derived from collection timestamp

This identifier uniquely identifies the build artifact and its metadata (revision, timestamp, conditions).

## Testing

Run tests with:
```bash
python3 -m unittest discover --start-directory tools/release --pattern 'test_*.py' --verbose
```

Or add to the main Makefile test suite:
```bash
make test  # runs all tests including tools/release/test_build_info.py
```
