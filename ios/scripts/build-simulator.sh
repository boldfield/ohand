#!/usr/bin/env bash
# Simulator build of one scheme (default OhAndApp). macOS with Xcode only.
# Unsigned by default. OHAND_SIMULATOR_ADHOC_SIGN=1 ad-hoc signs with the target's entitlements, which the
# simulator needs before Keychain calls stop failing with errSecMissingEntitlement (-34018).
set -euo pipefail

scheme="${1:-OhAndApp}"
cd "$(dirname "$0")/.."

signing_settings=(CODE_SIGNING_ALLOWED=NO)
if [ "${OHAND_SIMULATOR_ADHOC_SIGN:-0}" = "1" ]; then
  signing_settings=(CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=)
fi

./scripts/generate.sh
xcodebuild build \
  -project OhAnd.xcodeproj \
  -scheme "${scheme}" \
  -configuration Debug \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath .derived \
  "${signing_settings[@]}"
