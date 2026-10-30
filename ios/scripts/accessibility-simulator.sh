#!/usr/bin/env bash
# Runs the accessibility audits (OhAndAccessibilityUITests) on one simulator at the default content size and at the
# largest accessibility size, and checks that each run produced its audit log and screenshots. Any audit issue fails the
# test, and so this script. The UI tests must already be built (build-for-testing into .derived/Accessibility) so no
# compile runs beside a booted simulator.
# Usage: accessibility-simulator.sh <simulator-udid> <evidence-dir>
# Output: <evidence-dir>/accessibility/{audit-<size>.log, <size>-*.png}, accessibility-<size>.log, accessibility-<size>.xcresult. macOS only.
set -euo pipefail

udid="${1:?simulator udid required}"
evidence_dir="${2:?evidence directory required}"

script_dir="$(cd "$(dirname "$0")" && pwd)"
ios_dir="$(cd "${script_dir}/.." && pwd)"
evidence_dir="$(mkdir -p "${evidence_dir}" && cd "${evidence_dir}" && pwd)"
output_dir="${evidence_dir}/accessibility"
rm -rf "${output_dir}" "${evidence_dir}"/accessibility-*.xcresult
mkdir -p "${output_dir}"

# One first boot, left to settle and shut down; the boot the tests run on is then a warm one.
"${script_dir}/prepare-simulator.sh" "${udid}" --erase --shutdown-after --label "accessibility-first-boot"
"${script_dir}/prepare-simulator.sh" "${udid}" --label "accessibility"

# As a person who has used the keyboard before: no slide-to-type introduction.
xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences DidShowGestureKeyboardIntroduction -bool true || true
xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences KeyboardDidShowContinuousPathIntroduction -bool true || true
# The recording screens need the microphone; the app is installed by the test run, so this can fail harmlessly and the test answers the prompt.
xcrun simctl privacy "${udid}" grant microphone com.boldfield.ohand.app || true

# Face ID is enrolled and a matching face is presented while the tests run, as in the walkthrough, in case a screen asks the device owner.
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
trap 'kill "${matcher_pid}" 2>/dev/null || true; xcrun simctl shutdown "${udid}" 2>/dev/null || true' EXIT

# Light appearance at both sizes: the size is what changes between the runs.
xcrun simctl ui "${udid}" appearance light
test_status=0
for size in default xxxl; do
  run_status=0
  : > "${evidence_dir}/accessibility-${size}.log"
  (
    cd "${ios_dir}"
    TEST_RUNNER_ACCESSIBILITY_OUTPUT_DIR="${output_dir}" TEST_RUNNER_ACCESSIBILITY_CONTENT_SIZE="${size}" xcodebuild test-without-building \
      -project OhAnd.xcodeproj \
      -scheme OhAndAccessibilityUITests \
      -configuration Debug \
      -sdk iphonesimulator \
      -destination "platform=iOS Simulator,id=${udid}" \
      -derivedDataPath .derived/Accessibility \
      -resultBundlePath "${evidence_dir}/accessibility-${size}.xcresult" \
      CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
  ) 2>&1 | tee -a "${evidence_dir}/accessibility-${size}.log" || run_status=$?
  if [ "${run_status}" -ne 0 ]; then
    echo "FAILURE: accessibility audits at the ${size} content size exited with ${run_status}" >&2
    test_status="${run_status}"
  fi
done
kill "${matcher_pid}" 2>/dev/null || true

# What the audits found, in the log that goes into the artifact.
for size in default xxxl; do
  grep -E "^(ISSUE|EXEMPT|PASS|FAIL) " "${output_dir}/audit-${size}.log" 2>/dev/null || true
done | tee "${evidence_dir}/accessibility-summary.txt"

if [ "${test_status}" -ne 0 ]; then
  exit "${test_status}"
fi

# Both sizes ran every audit and left screenshots (the largest size's go into the artifact for review).
missing=0
for size in default xxxl; do
  if [ ! -s "${output_dir}/audit-${size}.log" ]; then
    echo "FAILURE: no audit log for the ${size} content size" >&2
    missing=1
  fi
  if ! ls "${output_dir}/${size}-"*.png >/dev/null 2>&1; then
    echo "FAILURE: no screenshots for the ${size} content size" >&2
    missing=1
  fi
  if grep -q "^ISSUE " "${output_dir}/audit-${size}.log" 2>/dev/null; then
    echo "FAILURE: audit issues at the ${size} content size" >&2
    missing=1
  fi
done
[ "${missing}" -eq 0 ]
