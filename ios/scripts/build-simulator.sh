#!/usr/bin/env bash
# Unsigned simulator build of one scheme (default OhAndApp). macOS with Xcode only.
set -euo pipefail

scheme="${1:-OhAndApp}"
cd "$(dirname "$0")/.."

./scripts/generate.sh
xcodebuild build \
  -project OhAnd.xcodeproj \
  -scheme "${scheme}" \
  -configuration Debug \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath .derived \
  CODE_SIGNING_ALLOWED=NO
