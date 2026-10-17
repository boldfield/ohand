#!/usr/bin/env bash
# Runs the screenshot walkthrough (OhAndWalkthroughUITests) on one simulator and checks that every expected screen
# produced a PNG. Face ID is enrolled on the simulator and matching faces are sent while the test runs; the app's own
# authentication is untouched. A screen the test cannot reach fails the test, and a missing PNG fails this script.
# Usage: walkthrough-simulator.sh <simulator-udid> <evidence-dir> <label>
# Output: <evidence-dir>/walkthrough/<label>/*.png, <label>.log, <label>.xcresult, screenshots-<label>.txt. macOS only.
set -euo pipefail

udid="${1:?simulator udid required}"
evidence_dir="${2:?evidence directory required}"
label="${3:?label required}"

script_dir="$(cd "$(dirname "$0")" && pwd)"
ios_dir="$(cd "${script_dir}/.." && pwd)"
evidence_dir="$(mkdir -p "${evidence_dir}" && cd "${evidence_dir}" && pwd)"
output_dir="${evidence_dir}/walkthrough/${label}"
rm -rf "${output_dir}" "${evidence_dir}/${label}.xcresult"
mkdir -p "${output_dir}"

"${script_dir}/prepare-simulator.sh" "${udid}" --erase

# Enrol Face ID, then keep presenting a matching face until the test finishes. A match with no prompt on screen is ignored.
xcrun simctl spawn "${udid}" notifyutil -s com.apple.BiometricKit.enrollmentChanged 1
xcrun simctl spawn "${udid}" notifyutil -p com.apple.BiometricKit.enrollmentChanged
sleep 2
(
  while true; do
    xcrun simctl spawn "${udid}" notifyutil -p com.apple.BiometricKit_Sim.pearl.match >/dev/null 2>&1 || true
    sleep 1
  done
) &
matcher_pid=$!
trap 'kill "${matcher_pid}" 2>/dev/null || true' EXIT

test_status=0
(
  cd "${ios_dir}"
  TEST_RUNNER_WALKTHROUGH_OUTPUT_DIR="${output_dir}" xcodebuild test \
    -project OhAnd.xcodeproj \
    -scheme OhAndWalkthroughUITests \
    -configuration Debug \
    -sdk iphonesimulator \
    -destination "platform=iOS Simulator,id=${udid}" \
    -derivedDataPath .derived/Walkthrough \
    -resultBundlePath "${evidence_dir}/${label}.xcresult" \
    CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
) 2>&1 | tee "${evidence_dir}/${label}.log" || test_status=$?
kill "${matcher_pid}" 2>/dev/null || true

if [ "${test_status}" -ne 0 ]; then
  echo "FAILURE: walkthrough ${label} exited with ${test_status}" >&2
  exit "${test_status}"
fi

# Light and dark, one PNG each, for these screens.
width="compact"
case "${label}" in *regular*) width="regular" ;; esac
missing=0
for appearance in light dark; do
  for screen in home search settings unfinished-recordings audio-retention providers daily-prompt; do
    if ! ls "${output_dir}/${width}-${appearance}-"??"-${screen}.png >/dev/null 2>&1; then
      echo "FAILURE: no screenshot for ${width} ${appearance} ${screen}" >&2
      missing=1
    fi
  done
done
(cd "${output_dir}" && ls -1 *.png) | tee "${evidence_dir}/screenshots-${label}.txt"
[ "${missing}" -eq 0 ]
