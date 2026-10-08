#!/usr/bin/env bash
# Runs the NotificationProbe UI tests on a simulator, once per permission decision, from a freshly
# uninstalled app so the system permission prompt appears. macOS with Xcode only.
# Usage: run-evidence.sh <simulator-udid> <evidence-directory>
set -euo pipefail

udid="${1:?simulator udid required}"
evidence_dir="${2:?evidence directory required}"
bundle_id="com.boldfield.ohand.probes.notification"
suites=(NotificationProbeAuthorizedUITests NotificationProbeDeniedUITests)

cd "$(dirname "$0")/../.."
mkdir -p "${evidence_dir}"
overall_status=0

for suite in "${suites[@]}"; do
  xcrun simctl uninstall "${udid}" "${bundle_id}" 2>/dev/null || true
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

  container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data 2>/dev/null || true)"
  if [ -n "${container}" ] && [ -f "${container}/Documents/notification-probe-report.jsonl" ]; then
    cp "${container}/Documents/notification-probe-report.jsonl" "${evidence_dir}/${suite}-report.jsonl"
  else
    echo "no report file found for ${suite}" | tee -a "${evidence_dir}/${suite}.log"
  fi
  if [ "${suite_status}" -ne 0 ]; then
    overall_status="${suite_status}"
  fi
done

exit "${overall_status}"
