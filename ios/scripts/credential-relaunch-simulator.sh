#!/usr/bin/env bash
# Cross-process Keychain relaunch check for CredentialService (V04).
# Runs the hosted CredentialRelaunchTests twice, each as a fresh host app process: "write" stores a synthetic
# secret in the real simulator Keychain, "verify" (a different process) must still resolve it, then deletes it.
# Requires OhAndTests to have been built and run already (same derived data path, ad-hoc signed).
# Usage: credential-relaunch-simulator.sh <udid> <evidence-dir>   (run from the ios/ directory)
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <udid> <evidence-dir>" >&2
  exit 2
fi
udid="$1"
evidence_dir="$2"
mkdir -p "${evidence_dir}"

for phase in write verify; do
  log="${evidence_dir}/credential-relaunch-${phase}.log"
  echo "=== Credential relaunch check: phase ${phase} ==="
  TEST_RUNNER_OHAND_RELAUNCH_PHASE="${phase}" xcodebuild test-without-building \
    -project OhAnd.xcodeproj \
    -scheme OhAndTests \
    -configuration Debug \
    -sdk iphonesimulator \
    -destination "platform=iOS Simulator,id=${udid}" \
    -derivedDataPath .derived \
    -only-testing:OhAndTests/CredentialRelaunchTests 2>&1 | tee "${log}"

  if ! grep -Eq "Test [Cc]ase .*CredentialRelaunchTests.*testRelaunchPhase.* passed" "${log}"; then
    echo "ERROR: relaunch phase ${phase} did not report a passing testRelaunchPhase (skipped or missing)" >&2
    exit 1
  fi
  if grep -Eq "Test [Cc]ase .*CredentialRelaunchTests.*testRelaunchPhase.* (skipped|failed)" "${log}"; then
    echo "ERROR: relaunch phase ${phase} was skipped or failed" >&2
    exit 1
  fi
  if ! grep -Eq '\*\* TEST (EXECUTE )?SUCCEEDED \*\*' "${log}"; then
    echo "ERROR: relaunch phase ${phase} did not succeed" >&2
    exit 1
  fi
done
echo "=== Credential relaunch check passed ==="
