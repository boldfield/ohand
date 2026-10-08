#!/usr/bin/env bash
# Boots a simulator by UDID, installs CaptureProbe, launches it and verifies ingress record creation.
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

if [ ! -d "${app_path}" ]; then
  echo "ERROR: ${app_path} does not exist; build it before the smoke test" >&2
  exit 1
fi

echo "=== Simulator under test ==="
xcrun simctl list devices | grep -F "${udid}"

echo "=== Booting (waits until fully booted) ==="
xcrun simctl bootstatus "${udid}" -b

echo "=== Installing ${app_path} ==="
xcrun simctl install "${udid}" "${app_path}"

echo "=== Launching ${bundle_id} ==="
launch_output="$(xcrun simctl launch "${udid}" "${bundle_id}")"
echo "${launch_output}"
launched_pid="$(printf '%s\n' "${launch_output}" | sed -n 's/^.*: \([0-9][0-9]*\)$/\1/p' | tail -n 1)"
if [ -z "${launched_pid}" ]; then
  echo "ERROR: simctl launch did not report a process id" >&2
  exit 1
fi

sleep 3
echo "=== Verifying ${bundle_id} (pid ${launched_pid}) is still running ==="
if ! xcrun simctl spawn "${udid}" launchctl list | grep -F "UIKitApplication:${bundle_id}"; then
  echo "ERROR: ${bundle_id} is not running three seconds after launch" >&2
  exit 1
fi

echo "=== Verifying ingress record creation ==="
app_container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data)"
if [ -z "${app_container}" ]; then
  echo "ERROR: Could not determine app container path" >&2
  exit 1
fi
captured_id="$(defaults read "${app_container}/Library/Preferences/com.boldfield.ohand.probes.capture.plist" com.boldfield.ohand.probes.capture.id 2>/dev/null || echo "")"
if [ -z "${captured_id}" ]; then
  echo "ERROR: Ingress record ID not found in app preferences" >&2
  exit 1
fi
echo "Found ingress record ID: ${captured_id}"
echo "Smoke test verified: ingress record persisted with ID ${captured_id}"

if [ -n "${evidence_dir}" ]; then
  mkdir -p "${evidence_dir}"
  echo "=== Capturing evidence ==="
  xcrun simctl io "${udid}" screenshot "${evidence_dir}/captureprobe-launch.png"
  echo "Ingress record ID: ${captured_id}" >> "${evidence_dir}/capture-evidence.txt"
  echo "Smoke test passed: CaptureProbe launched, ingress record created and verified, app stayed running"
fi

xcrun simctl shutdown "${udid}"
echo "=== Smoke test passed: ${bundle_id} launched, ingress record persisted and verified ==="
