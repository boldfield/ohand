#!/bin/bash
# Sign the M1 trial candidate build and record metadata.
#
# This script uses the existing signing infrastructure in tools/apple-build/sign_probe.py
# to produce a signed trial candidate for T05 (device testing) and T06 (distribution).
#
# Usage:
#   sign_trial_candidate.sh [--no-install] [--scheme SCHEME]
#
# Environment variables (from apple-build signing procedure):
#   OHAND_SIGNING_PROFILE_PATH (or OHAND_SIGNING_PROFILE_MAP_PATH for OhAndApp)
#   OHAND_SIGNING_CERT_PATH (optional)
#   OHAND_SIGNING_CERT_PASSWORD (or OHAND_SIGNING_CERT_PASSWORD_FILE)
#   OHAND_DEVICE_ID (with --install)
#   OHAND_DEVICE_LABEL (optional)
#   OHAND_PRIVATE_EVIDENCE_DIR (optional, defaults to ~/.ohand-private-evidence)
#
# Produces:
#   - Signed .ipa artifact
#   - Sanitized evidence record in docs/validation/evidence/apple-signing/
#   - Build metadata in docs/releases/m1-trial/

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCHEME="${SCHEME:-OhAndApp}"
INSTALL_FLAG=""
TIMESTAMP=$(date -u +'%Y-%m-%dT%H:%M:%SZ')

# Check for no-install flag
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-install)
            INSTALL_FLAG=""
            ;;
        --scheme)
            SCHEME="$2"
            shift
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
    shift
done

# Generate build info
echo "Generating build info for trial candidate..." >&2
python3 "$REPO_ROOT/tools/release/build_info.py" \
    --json \
    --timestamp "$TIMESTAMP" \
    --export-vars /tmp/build_info.sh

source /tmp/build_info.sh
rm /tmp/build_info.sh

echo "Build identifier: $build_identifier" >&2
echo "Revision: $build_revision_short" >&2
echo "Timestamp: $collected_at" >&2

# Ensure output directory exists
mkdir -p "$REPO_ROOT/docs/releases/m1-trial"

# Run the signing script (with or without install depending on flags)
echo "Signing $SCHEME for trial..." >&2
if [ -n "$INSTALL_FLAG" ]; then
    python3 "$REPO_ROOT/tools/apple-build/sign_probe.py" \
        --scheme "$SCHEME" \
        --install
else
    python3 "$REPO_ROOT/tools/apple-build/sign_probe.py" \
        --scheme "$SCHEME"
fi

echo "Trial candidate signed successfully" >&2
echo "Use the build identifier: $build_identifier" >&2
