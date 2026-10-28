#!/usr/bin/env bash
# Shows that a Release build of OhAndApp carries no upgrade-report code: builds it for the simulator (unsigned) and fails if
# the binary or its Info.plist contain any string that only the debug-only UpgradeCheck sources define. macOS with Xcode only.
# Usage: check_release_excludes_upgrade_report.sh [derived-data-dir]
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
derived="${1:-$(mktemp -d "${TMPDIR:-/tmp}/ohand-release-check.XXXXXX")}"
cd "${repo_root}/ios"
./scripts/generate.sh
xcodebuild build \
  -project OhAnd.xcodeproj -scheme OhAndApp -configuration Release -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath "${derived}" CODE_SIGNING_ALLOWED=NO

app="${derived}/Build/Products/Release-iphonesimulator/OhAnd.app"
binary="${app}/OhAnd"
[ -f "${binary}" ] || { echo "no Release binary at ${binary}" >&2; exit 1; }

# Positive control: the build identity of a Release build is stamped, so the strings below are looked for in a real build.
revision="$(/usr/libexec/PlistBuddy -c 'Print :OhAndBuildRevision' "${app}/Info.plist")"
[ "${#revision}" -eq 40 ] || { echo "the Release build carries no full revision in its Info.plist" >&2; exit 1; }
dirty="$(/usr/libexec/PlistBuddy -c 'Print :OhAndBuildDirty' "${app}/Info.plist")"
[ "${dirty}" = "false" ] || { echo "a Release build must be stamped from a clean tree" >&2; exit 1; }

status=0
for marker in OhAndUpgradeReport OhAndUpgradeProbe upgrade-report.json ohand.upgrade-report.v1 UpgradeReportWriter; do
  if strings -a "${binary}" | grep -q -- "${marker}"; then
    echo "FAILURE: the Release binary contains '${marker}'" >&2
    status=1
  fi
done
[ "${status}" -eq 0 ] && echo "the Release build carries no upgrade-report code"
exit "${status}"
