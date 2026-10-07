#!/usr/bin/env bash
# Boots a simulator by UDID, installs an app bundle, launches it and verifies it stays running.
# Usage: smoke-simulator.sh <udid> <path/to/App.app> <bundle-id> [evidence-dir]
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

if [ -n "${evidence_dir}" ]; then
  mkdir -p "${evidence_dir}"
  xcrun simctl io "${udid}" screenshot "${evidence_dir}/bridgeprobe-launch.png"
fi

xcrun simctl shutdown "${udid}"
echo "=== Smoke test passed: ${bundle_id} launched and stayed running ==="
