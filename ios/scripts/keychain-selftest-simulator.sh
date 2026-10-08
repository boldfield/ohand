#!/usr/bin/env bash
# Boots a simulator, installs CredentialProbe, launches it with -runKeychainSelfTest and
# requires the probe to report KEYCHAIN_SELFTEST_RESULT PASS on its console.
# Usage: keychain-selftest-simulator.sh <udid> <path/to/CredentialProbe.app> <evidence-dir>
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <udid> <app-path> <evidence-dir>" >&2
  exit 2
fi
udid="$1"
app_path="$2"
evidence_dir="$3"
bundle_id="com.boldfield.ohand.probes.credential"
console_log="${evidence_dir}/keychain-selftest-console.log"

if [ ! -d "${app_path}" ]; then
  echo "ERROR: ${app_path} does not exist; build it before the self-test" >&2
  exit 1
fi
mkdir -p "${evidence_dir}"

xcrun simctl bootstatus "${udid}" -b
xcrun simctl install "${udid}" "${app_path}"

echo "=== Launching ${bundle_id} -runKeychainSelfTest ==="
xcrun simctl launch --console --terminate-running-process "${udid}" "${bundle_id}" -runKeychainSelfTest \
  > "${console_log}" 2>&1 &
launch_pid=$!

for _ in $(seq 1 60); do
  if grep -q '^KEYCHAIN_SELFTEST_RESULT' "${console_log}"; then
    break
  fi
  sleep 1
done
kill "${launch_pid}" 2>/dev/null || true
xcrun simctl terminate "${udid}" "${bundle_id}" 2>/dev/null || true

echo "=== Console output ==="
cat "${console_log}"

container="$(xcrun simctl get_app_container "${udid}" "${bundle_id}" data)"
if [ -f "${container}/Documents/keychain-selftest.log" ]; then
  cp "${container}/Documents/keychain-selftest.log" "${evidence_dir}/keychain-selftest-documents.log"
fi

if ! grep -q '^KEYCHAIN_SELFTEST_RESULT PASS' "${console_log}"; then
  echo "ERROR: Keychain self-test did not report PASS" >&2
  exit 1
fi
if grep -q 'result=FAIL' "${console_log}"; then
  echo "ERROR: at least one Keychain self-test step failed" >&2
  exit 1
fi
echo "=== Keychain self-test passed ==="
