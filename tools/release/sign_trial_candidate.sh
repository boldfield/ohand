#!/usr/bin/env bash
# Produces the signed M1 trial candidate and its candidate record (docs/releases/m1-trial/README.md).
# Run it on a clean checkout of main (macOS with Xcode). Signing inputs come from the environment exactly as for
# tools/apple-build/sign_probe.py (OHAND_SIGNING_PROFILE_MAP_PATH or OHAND_SIGNING_PROFILE_PATH, OHAND_SIGNING_CERT_PATH,
# OHAND_SIGNING_CERT_PASSWORD[_FILE]); none of them is read from or written to the repository.
#
#   tools/release/sign_trial_candidate.sh --output-dir <directory outside the repository>
#   tools/release/sign_trial_candidate.sh --verify <record.json> --archive <candidate.ipa>
set -euo pipefail
exec python3 "$(cd "$(dirname "$0")" && pwd)/sign_trial_candidate.py" "$@"
