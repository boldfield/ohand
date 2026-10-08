#!/usr/bin/env bash
# CaptureProbe simulator smoke test. Unlike a launch-only check, it reads what the app persisted after each phase:
#   1. plain cold launch                 -> idle screen, no record (a launch alone is not a capture entry)
#   2. terminate + shortcut URL (cold)   -> exactly one new record (shortcutURL, cold), shown as "Saved"
#   3. terminate + shortcut URL (cold)   -> one more record; the first record file is byte-identical
#   4. background + shortcut URL (warm)  -> one more record (warm)
#   5. background + plain launch (warm)  -> idle screen, no new record
#   6. shortcut URL while foreground     -> one more record
# The shortcut URL is delivered with `simctl openurl`, through the same scene URL callbacks a Shortcuts "Open URL"
# action uses. It cannot press a Control Center control, lock the device or test before-first-unlock; those need a
# physical device.
# Usage: smoke-capture-simulator.sh <udid> <path/to/CaptureProbe.app> <bundle-id> [evidence-dir]
set -euo pipefail

if [ "$#" -lt 3 ]; then
  echo "usage: $0 <udid> <app-path> <bundle-id> [evidence-dir]" >&2
  exit 2
fi
udid="$1"
app_path="$2"
bundle_id="$3"
evidence_dir="${4:-}"
script_dir="$(cd "$(dirname "$0")" && pwd)"
backgrounding_bundle_id="com.apple.Preferences"
shortcut_url="ohand-captureprobe://capture"
capture_root=""

if [ ! -d "${app_path}" ]; then
  echo "ERROR: ${app_path} does not exist; build it before the smoke test" >&2
  exit 1
fi

known_ids() {
  { ls "${capture_root}/records" 2>/dev/null || true; } | sed -n 's/\.json$//p' | sort
}

# Polls the verifier until the persisted files reach the expected state, then runs it once more with output.
verify_phase() {
  for _ in $(seq 1 30); do
    if python3 "${script_dir}/verify_capture_records.py" "${capture_root}" --quiet "$@"; then
      break
    fi
    sleep 1
  done
  python3 "${script_dir}/verify_capture_records.py" "${capture_root}" "$@"
}

screenshot() {
  if [ -n "${evidence_dir}" ]; then
    xcrun simctl io "${udid}" screenshot "${evidence_dir}/capture-$1.png"
  fi
}

background_app() {
  xcrun simctl launch "${udid}" "${backgrounding_bundle_id}"
  sleep 3
}

echo "=== Simulator under test ==="
xcrun simctl list devices | grep -F "${udid}"
[ -z "${evidence_dir}" ] || mkdir -p "${evidence_dir}"

echo "=== Booting (waits until fully booted) ==="
xcrun simctl bootstatus "${udid}" -b

echo "=== Installing ${app_path} ==="
xcrun simctl uninstall "${udid}" "${bundle_id}" 2>/dev/null || true
xcrun simctl install "${udid}" "${app_path}"

echo "=== Phase 1: plain cold launch creates no entry ==="
xcrun simctl launch "${udid}" "${bundle_id}"
data_container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data)"
capture_root="${data_container}/Library/Application Support/CaptureProbe"
echo "Capture root: ${capture_root}"
verify_phase --expect-presented idle
screenshot "1-plain-cold"

echo "=== Phase 2: terminate, then cold launch through the shortcut URL ==="
xcrun simctl terminate "${udid}" "${bundle_id}"
sleep 2
xcrun simctl openurl "${udid}" "${shortcut_url}"
verify_phase --expect-source shortcutURL --expect-kinds cold
screenshot "2-url-cold"
first_record="$(find "${capture_root}/records" -name '*.json' | head -n 1)"
first_checksum="$(shasum -a 256 "${first_record}" | cut -d ' ' -f 1)"

echo "=== Phase 3: terminate, then cold launch through the shortcut URL again ==="
previous_ids="$(known_ids)"
xcrun simctl terminate "${udid}" "${bundle_id}"
sleep 2
xcrun simctl openurl "${udid}" "${shortcut_url}"
# shellcheck disable=SC2086
verify_phase --expect-source shortcutURL --expect-kinds cold cold --known-ids ${previous_ids}
if [ "$(shasum -a 256 "${first_record}" | cut -d ' ' -f 1)" != "${first_checksum}" ]; then
  echo "ERROR: the first record changed across the process restart" >&2
  exit 1
fi
screenshot "3-url-cold-after-restart"

echo "=== Phase 4: background the app, then warm launch through the shortcut URL ==="
previous_ids="$(known_ids)"
background_app
xcrun simctl openurl "${udid}" "${shortcut_url}"
# shellcheck disable=SC2086
verify_phase --expect-source shortcutURL --expect-kinds cold cold warm --known-ids ${previous_ids}
screenshot "4-url-warm"

echo "=== Phase 5: background the app, then plain warm launch creates no entry ==="
previous_ids="$(known_ids)"
background_app
xcrun simctl launch "${udid}" "${bundle_id}"
# shellcheck disable=SC2086
verify_phase --expect-presented idle --expect-source shortcutURL --expect-kinds cold cold warm --known-ids ${previous_ids}
screenshot "5-plain-warm"

echo "=== Phase 6: shortcut URL while the app is already foreground ==="
previous_ids="$(known_ids)"
xcrun simctl openurl "${udid}" "${shortcut_url}"
# shellcheck disable=SC2086
verify_phase --expect-source shortcutURL --expect-kinds cold cold warm warm --known-ids ${previous_ids}
screenshot "6-url-foreground"

if [ -n "${evidence_dir}" ]; then
  rm -rf "${evidence_dir}/capture-records"
  mkdir -p "${evidence_dir}/capture-records"
  cp -R "${capture_root}/." "${evidence_dir}/capture-records/"
fi

xcrun simctl shutdown "${udid}"
echo "=== Capture smoke test passed: ${bundle_id} persisted one record per shortcut handoff (cold, restarted-cold, warm, foreground) and none for plain launches ==="
