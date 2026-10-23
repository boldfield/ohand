#!/bin/bash
# Generate reproducible build identifiers and inject into app metadata.
# This script is run as a pre-build phase for OhAndApp.

set -e

# Get the source root and plist path
PLIST_FILE="${SRCROOT}/AppAssembly/Info.plist"
BUILD_INFO_SCRIPT="${SRCROOT}/../tools/release/build_info.py"
TEMP_BUILD_INFO="${SRCROOT}/../.build-info.json"

# Generate build info as JSON
python3 "${BUILD_INFO_SCRIPT}" --json > "${TEMP_BUILD_INFO}" 2>/dev/null || {
    # If build_info.py fails, use a default identifier
    echo "Warning: Failed to generate build info, using default"
    echo '{"build_identifier":"unknown","build_revision":"unknown"}' > "${TEMP_BUILD_INFO}"
}

# Extract build_identifier from JSON and inject into plist
BUILD_IDENTIFIER=$(python3 -c "import json; print(json.load(open('${TEMP_BUILD_INFO}'))['build_identifier'])" 2>/dev/null || echo "unknown")

# Update CFBundleVersion in the plist
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${BUILD_IDENTIFIER}" "${PLIST_FILE}" 2>/dev/null || {
    # If plist update fails, log a warning but don't fail the build
    echo "Warning: Failed to inject build identifier into plist"
}

# Clean up temp file
rm -f "${TEMP_BUILD_INFO}"
