#!/usr/bin/env bash
# Unsigned simulator build of one scheme (default OhAndApp). macOS with Xcode only.
set -euo pipefail

scheme="${1:-OhAndApp}"
cd "$(dirname "$0")/.."
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

./scripts/generate.sh

# Generate Rust bindings header to a location accessible during the build.
# The Xcode build's HEADER_SEARCH_PATHS will find it here before preBuildScripts run.
output_dir="${repo_root}/build"
mkdir -p "${output_dir}"
"${repo_root}/tools/bindings/generate.sh" "${output_dir}"

xcodebuild build \
  -project OhAnd.xcodeproj \
  -scheme "${scheme}" \
  -configuration Debug \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath .derived \
  CODE_SIGNING_ALLOWED=NO
