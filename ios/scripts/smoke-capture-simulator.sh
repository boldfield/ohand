#!/usr/bin/env bash
# CaptureProbe simulator smoke test. It runs the CaptureProbeUITests sequence on a freshly installed app, which
# checks what the capture screen renders in each phase:
#   1. plain cold launch                 -> idle screen (a launch alone is not a capture entry)
#   2. terminate + shortcut URL (cold)   -> a new saved entry (shortcutURL, cold)
#   3. terminate + shortcut URL (cold)   -> another new saved entry
#   4. background + shortcut URL (warm)  -> another new saved entry (warm)
#   5. background + plain launch (warm)  -> idle screen
#   6. shortcut URL while foreground     -> another new saved entry
# The URL is opened through the system (XCUIDevice.system.open), which delivers it through the scene's URL callbacks
# as a Shortcuts "Open URL" action would. Then it reads the files the app persisted in its data container and requires
# exactly one record per rendered entry, with the rendered ID and launch kind, none for the plain launches, and no
# pending entry left. It cannot press a Control Center control, lock the device or test before-first-unlock; those
# need a physical device.
# Run from ios/ after the project has been generated. macOS with Xcode only.
# Usage: smoke-capture-simulator.sh <udid> <bundle-id> [evidence-dir]
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <udid> <bundle-id> [evidence-dir]" >&2
  exit 2
fi
udid="$1"
bundle_id="$2"
evidence_dir="${3:-.evidence}"
script_dir="$(cd "$(dirname "$0")" && pwd)"
expected_phases=(plain-cold url-cold url-cold-after-restart url-warm plain-warm url-foreground)

mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
ui_test_log="${evidence_dir}/capture-uitests.log"

echo "=== Simulator under test ==="
xcrun simctl list devices | grep -F "${udid}"

echo "=== Booting (waits until fully booted) ==="
xcrun simctl bootstatus "${udid}" -b

echo "=== Removing any earlier install so the data container starts empty ==="
xcrun simctl uninstall "${udid}" "${bundle_id}" 2>/dev/null || true

echo "=== Running CaptureProbeUITests ==="
rm -rf "${evidence_dir}/CaptureProbeUITests.xcresult"
xcodebuild test \
  -project OhAnd.xcodeproj \
  -scheme CaptureProbeUITests \
  -configuration Debug \
  -sdk iphonesimulator \
  -destination "platform=iOS Simulator,id=${udid}" \
  -derivedDataPath .derived \
  -resultBundlePath "${evidence_dir}/CaptureProbeUITests.xcresult" \
  CODE_SIGNING_ALLOWED=NO 2>&1 | tee "${ui_test_log}"

echo "=== Rendered phases ==="
phase_lines="$(grep -o 'CAPTURE-PHASE .*' "${ui_test_log}" | awk '!seen[$2]++')"
echo "${phase_lines}"
phase_names=()
while read -r _ name _ _; do phase_names+=("${name}"); done <<< "${phase_lines}"
if [ "${phase_names[*]}" != "${expected_phases[*]}" ]; then
  echo "ERROR: rendered phases '${phase_names[*]}' != expected '${expected_phases[*]}'" >&2
  exit 1
fi

record_args=()
rendered_ids=()
kinds=()
while read -r _ name capture_id kind; do
  if [ "${capture_id}" = "idle" ]; then
    continue
  fi
  record_args+=(--expect-record "${capture_id}=${kind}")
  rendered_ids+=("${capture_id}")
  kinds+=("${kind}")
done <<< "${phase_lines}"
last_index=$((${#rendered_ids[@]} - 1))

echo "=== Verifying the persisted files ==="
data_container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data)"
capture_root="${data_container}/Library/Application Support/CaptureProbe"
echo "Capture root: ${capture_root}"
python3 "${script_dir}/verify_capture_records.py" "${capture_root}" \
  --expect-source shortcutURL \
  --expect-kinds "${kinds[@]}" \
  --known-ids "${rendered_ids[@]:0:${last_index}}" \
  "${record_args[@]}"

rm -rf "${evidence_dir}/capture-records"
mkdir -p "${evidence_dir}/capture-records"
cp -R "${capture_root}/." "${evidence_dir}/capture-records/"

xcrun simctl shutdown "${udid}"
echo "=== Capture smoke test passed: ${bundle_id} persisted exactly the ${#rendered_ids[@]} rendered handoff entries and none for plain launches ==="
