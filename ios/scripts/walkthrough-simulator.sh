#!/usr/bin/env bash
# Runs the screenshot walkthrough (OhAndWalkthroughUITests) on one simulator and checks that every expected screen
# produced a PNG. Face ID is enrolled on the simulator and matching faces are sent while the test runs; the app's own
# authentication is untouched. A screen the test cannot reach fails the test, and a missing PNG fails this script.
# Usage: walkthrough-simulator.sh <simulator-udid> <evidence-dir> <label>
# Output: <evidence-dir>/walkthrough/<label>/*.png, <label>.log, <label>-<appearance>.xcresult, screenshots-<label>.txt. macOS only.
set -euo pipefail

udid="${1:?simulator udid required}"
evidence_dir="${2:?evidence directory required}"
label="${3:?label required}"

script_dir="$(cd "$(dirname "$0")" && pwd)"
ios_dir="$(cd "${script_dir}/.." && pwd)"
evidence_dir="$(mkdir -p "${evidence_dir}" && cd "${evidence_dir}" && pwd)"
output_dir="${evidence_dir}/walkthrough/${label}"
rm -rf "${output_dir}" "${evidence_dir}/${label}"-*.xcresult
mkdir -p "${output_dir}"

# One first boot, left to settle and shut down; the boot the test runs on is then a warm one that no longer carries first-boot work.
"${script_dir}/prepare-simulator.sh" "${udid}" --erase --shutdown-after --label "walkthrough-${label}-first-boot"
"${script_dir}/prepare-simulator.sh" "${udid}" --label "walkthrough-${label}"

# Mark the keyboard's slide-to-type introduction as seen, as a person who has used the keyboard before would have.
xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences DidShowGestureKeyboardIntroduction -bool true || true
xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences KeyboardDidShowContinuousPathIntroduction -bool true || true

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
# Shut the simulator down at the end: a booted simulator that is no longer needed loads the hosted runner for whatever follows.
trap 'kill "${matcher_pid}" 2>/dev/null || true; xcrun simctl shutdown "${udid}" 2>/dev/null || true' EXIT

# One test run per appearance: the simulator's appearance is set from outside the app, as a person would in Settings.
test_status=0
: > "${evidence_dir}/${label}.log"
for appearance in light dark; do
  xcrun simctl ui "${udid}" appearance "${appearance}"
  run_status=0
  (
    cd "${ios_dir}"
    TEST_RUNNER_WALKTHROUGH_OUTPUT_DIR="${output_dir}" TEST_RUNNER_WALKTHROUGH_APPEARANCE="${appearance}" xcodebuild test \
      -project OhAnd.xcodeproj \
      -scheme OhAndWalkthroughUITests \
      -configuration Debug \
      -sdk iphonesimulator \
      -destination "platform=iOS Simulator,id=${udid}" \
      -derivedDataPath .derived/Walkthrough \
      -resultBundlePath "${evidence_dir}/${label}-${appearance}.xcresult" \
      CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
  ) 2>&1 | tee -a "${evidence_dir}/${label}.log" || run_status=$?
  if [ "${run_status}" -ne 0 ]; then
    test_status="${run_status}"
    break
  fi
done
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
  for screen in home search settings daily-prompt unfinished-recordings processing audio export; do
    if ! ls "${output_dir}/${width}-${appearance}-"??"-${screen}.png" >/dev/null 2>&1; then
      echo "FAILURE: no screenshot for ${width} ${appearance} ${screen}" >&2
      missing=1
    fi
  done
done
(cd "${output_dir}" && ls -1 *.png) | tee "${evidence_dir}/screenshots-${label}.txt"
[ "${missing}" -eq 0 ]
