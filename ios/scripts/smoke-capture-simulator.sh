#!/usr/bin/env bash
# CaptureProbe simulator smoke test. Unlike a launch-only check, it reads what the app persisted:
#   1. cold launch      -> one durable record (launchKind cold) and a rendered "Saved" outcome
#   2. terminate + cold -> a second record; the first record file is byte-identical (survived the process exit)
#   3. background + warm-> a third record (launchKind warm), distinct ID
# It cannot press a system control, lock the device or test before-first-unlock; those need a physical device.
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

if [ ! -d "${app_path}" ]; then
  echo "ERROR: ${app_path} does not exist; build it before the smoke test" >&2
  exit 1
fi

wait_for_record_count() {
  local expected="$1"
  local capture_root="$2"
  local count=0
  for _ in $(seq 1 30); do
    count="$(find "${capture_root}/records" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')"
    if [ "${count}" -ge "${expected}" ] && [ -f "${capture_root}/last-presented.json" ]; then
      sleep 1
      return 0
    fi
    sleep 1
  done
  echo "ERROR: expected ${expected} persisted record(s) but found ${count} after 30 seconds" >&2
  return 1
}

screenshot() {
  if [ -n "${evidence_dir}" ]; then
    xcrun simctl io "${udid}" screenshot "${evidence_dir}/capture-$1.png"
  fi
}

echo "=== Simulator under test ==="
xcrun simctl list devices | grep -F "${udid}"
[ -z "${evidence_dir}" ] || mkdir -p "${evidence_dir}"

echo "=== Booting (waits until fully booted) ==="
xcrun simctl bootstatus "${udid}" -b

echo "=== Installing ${app_path} ==="
xcrun simctl uninstall "${udid}" "${bundle_id}" 2>/dev/null || true
xcrun simctl install "${udid}" "${app_path}"

echo "=== Phase 1: cold launch ==="
xcrun simctl launch "${udid}" "${bundle_id}"
data_container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data)"
capture_root="${data_container}/Library/Application Support/CaptureProbe"
echo "Capture root: ${capture_root}"
wait_for_record_count 1 "${capture_root}"
python3 "${script_dir}/verify_capture_records.py" "${capture_root}" --expect-kinds cold
screenshot "1-cold"
first_record="$(find "${capture_root}/records" -name '*.json' | head -n 1)"
first_checksum="$(shasum -a 256 "${first_record}" | cut -d ' ' -f 1)"

echo "=== Phase 2: terminate, then cold launch again ==="
xcrun simctl terminate "${udid}" "${bundle_id}"
sleep 2
xcrun simctl launch "${udid}" "${bundle_id}"
wait_for_record_count 2 "${capture_root}"
python3 "${script_dir}/verify_capture_records.py" "${capture_root}" --expect-kinds cold cold
if [ "$(shasum -a 256 "${first_record}" | cut -d ' ' -f 1)" != "${first_checksum}" ]; then
  echo "ERROR: the first record changed across the process restart" >&2
  exit 1
fi
screenshot "2-cold-after-restart"

echo "=== Phase 3: background the app, then warm launch ==="
xcrun simctl launch "${udid}" "${backgrounding_bundle_id}"
sleep 3
xcrun simctl launch "${udid}" "${bundle_id}"
wait_for_record_count 3 "${capture_root}"
python3 "${script_dir}/verify_capture_records.py" "${capture_root}" --expect-kinds cold cold warm
screenshot "3-warm"

if [ -n "${evidence_dir}" ]; then
  rm -rf "${evidence_dir}/capture-records"
  mkdir -p "${evidence_dir}/capture-records"
  cp -R "${capture_root}/." "${evidence_dir}/capture-records/"
fi

xcrun simctl shutdown "${udid}"
echo "=== Capture smoke test passed: ${bundle_id} persisted cold, restarted-cold and warm entries ==="
