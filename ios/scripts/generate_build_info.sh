#!/usr/bin/env bash
# Stamps the build identity (tools/release/build_info.py) into the Info.plist of the product being built: the marketing
# version, CFBundleVersion (the first-parent commit count), OhAndBuildRevision (the full git SHA) and OhAndBuildDirty.
# Runs as the last build phase of OhAndApp and its capture extension, on the built product's plist, so the source plist
# stays unchanged. A Release build (the signed candidate) refuses a dirty or shallow tree. Any failure to derive the
# identity fails the build; nothing is written as a placeholder.
set -euo pipefail

repo_root="$(cd "${SRCROOT}/.." && pwd)"
built_plist="${TARGET_BUILD_DIR}/${INFOPLIST_PATH}"
require_clean=()
if [ "${CONFIGURATION}" = "Release" ]; then
  require_clean=(--require-clean)
fi

identity="$(/usr/bin/python3 "${repo_root}/tools/release/build_info.py" --repo "${repo_root}" --format plist-keys ${require_clean[@]+"${require_clean[@]}"})"
read_key() {
  printf '%s' "${identity}" | /usr/bin/python3 -c 'import json, sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

set_key() {
  local key="$1" type="$2" value="$3"
  /usr/libexec/PlistBuddy -c "Delete :${key}" "${built_plist}" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c "Add :${key} ${type} ${value}" "${built_plist}"
}

set_key CFBundleShortVersionString string "$(read_key CFBundleShortVersionString)"
set_key CFBundleVersion string "$(read_key CFBundleVersion)"
set_key OhAndBuildRevision string "$(read_key OhAndBuildRevision)"
dirty_text="$(read_key OhAndBuildDirty)"
if [ "${dirty_text}" = "True" ]; then dirty_value=true; else dirty_value=false; fi
set_key OhAndBuildDirty bool "${dirty_value}"
echo "build identity: version $(read_key CFBundleShortVersionString) build $(read_key CFBundleVersion) revision $(read_key OhAndBuildRevision) dirty ${dirty_value}"
