#!/usr/bin/env bash
# Runs the NotificationProbe UI tests once per permission decision, with each suite starting from a freshly erased and
# fully booted simulator. The erase is needed because uninstalling the app does not clear its recorded notification
# permission, and the system prompt only appears while the decision is notDetermined. macOS with Xcode only.
# Usage: run-notification-uitests.sh <simulator-udid> <evidence-directory>
set -euo pipefail

udid="${1:?simulator udid required}"
evidence_dir="${2:?evidence directory required}"
suites=(NotificationProbeAuthorizedUITests NotificationProbeDeniedUITests)

mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
script_dir="$(cd "$(dirname "$0")" && pwd)"
cd "${script_dir}/.."
overall_status=0

for suite in "${suites[@]}"; do
  "${script_dir}/prepare-simulator.sh" "${udid}" --erase "${evidence_dir}/${suite}.prepare.log"
  rm -rf "${evidence_dir}/${suite}.xcresult"
  suite_status=0
  xcodebuild test \
    -project OhAnd.xcodeproj \
    -scheme NotificationProbeUITests \
    -configuration Debug \
    -sdk iphonesimulator \
    -destination "platform=iOS Simulator,id=${udid}" \
    -derivedDataPath .derived \
    -resultBundlePath "${evidence_dir}/${suite}.xcresult" \
    -only-testing:"NotificationProbeUITests/${suite}" \
    CODE_SIGNING_ALLOWED=NO 2>&1 | tee "${evidence_dir}/${suite}.log" || suite_status=$?

  if [ "${suite_status}" -ne 0 ]; then
    overall_status="${suite_status}"
  fi
done

exit "${overall_status}"
