#!/usr/bin/env bash
# Proves on a simulator that installing a newer OhAndApp build over an older one keeps the person's data. macOS with Xcode only.
#
#   1. builds OhAndApp (Debug, simulator) at a prior revision, taken from a git worktree, and at the candidate (this checkout);
#   2. installs the prior build and records its build number;
#   3. seeds it by driving its UI (ios/UITests/Upgrade, a UI test bundle with no target app, so xcodebuild never installs the
#      app): four text captures, a correction, a deletion, a scheduled reminder, and a provider profile that processing
#      routes to. Every write goes through the app's own services;
#   4. installs the candidate over it with `simctl install` (never uninstalling or erasing) and records the build number
#      and the data container;
#   5. launches the candidate with -OhAndUpgradeReport, which makes the debug app write a content-free report after its own
#      Face ID authentication succeeds; the matching face is presented from outside, as the walkthrough does;
#   6. checks the upgraded app through its UI, relaunches it for a second report, and judges everything with
#      tools/release/verify_upgrade_report.py, which writes the simulator record.
#
# Usage: simulator_upgrade_check.sh --udid <simulator> --prior-revision <full sha> --evidence-dir <dir>
#            [--simulator-name <name>] [--simulator-runtime <iOS version>] [--run-url <url>]
# Output (in the evidence dir): seed-expectations.json, report-*.json, appinfo-*.txt, simulator-upgrade-record.json, logs.
set -euo pipefail

bundle_identifier="com.boldfield.ohand.app"
udid=""
prior_revision=""
evidence_dir=""
simulator_name="unknown"
simulator_runtime="unknown"
run_url=""
while [ $# -gt 0 ]; do
  case "$1" in
    --udid) udid="${2:?}"; shift ;;
    --prior-revision) prior_revision="${2:?}"; shift ;;
    --evidence-dir) evidence_dir="${2:?}"; shift ;;
    --simulator-name) simulator_name="${2:?}"; shift ;;
    --simulator-runtime) simulator_runtime="${2:?}"; shift ;;
    --run-url) run_url="${2:?}"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "${udid}" ] && [ -n "${prior_revision}" ] && [ -n "${evidence_dir}" ] || { echo "--udid, --prior-revision and --evidence-dir are required" >&2; exit 2; }

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
scripts="${repo_root}/ios/scripts"
mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ohand-upgrade.XXXXXX")"
prior_tree="${work_dir}/prior"
matcher_pid=""

cleanup() {
  [ -n "${matcher_pid}" ] && kill "${matcher_pid}" 2>/dev/null || true
  xcrun simctl shutdown "${udid}" 2>/dev/null || true
  git -C "${repo_root}" worktree remove --force "${prior_tree}" 2>/dev/null || true
  rm -rf "${work_dir}"
}
trap cleanup EXIT

log() { echo "$(date -u +%H:%M:%S) $*"; }

# ---- revisions -------------------------------------------------------------------------------------------------------

candidate_revision="$(git -C "${repo_root}" rev-parse HEAD)"
case "${prior_revision}" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*) ;;
  *) echo "--prior-revision must be a commit SHA" >&2; exit 2 ;;
esac
prior_revision="$(git -C "${repo_root}" rev-parse --verify "${prior_revision}^{commit}")"
if [ "${prior_revision}" = "${candidate_revision}" ]; then
  echo "the prior revision is the candidate revision: rebuilding one revision is not an upgrade" >&2
  exit 2
fi
prior_is_ancestor=false
if git -C "${repo_root}" merge-base --is-ancestor "${prior_revision}" "${candidate_revision}"; then prior_is_ancestor=true; fi
git -C "${repo_root}" worktree add --detach "${prior_tree}" "${prior_revision}" >/dev/null
candidate_build="$(python3 "${repo_root}/tools/release/build_info.py" --repo "${repo_root}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["build_number"])')"
log "prior ${prior_revision}; candidate ${candidate_revision} (first-parent count ${candidate_build})"

# ---- builds (before any simulator boots, so the compiler and a booted simulator never run together) --------------------

build_app() {
  local tree="$1" name="$2"
  log "building OhAndApp at ${name}"
  # The prior tree reuses the candidate's Rust build cache; cargo keys it by package path and revision, so only changed crates rebuild.
  if [ "${name}" = "prior" ] && [ -d "${repo_root}/target" ] && [ ! -e "${tree}/target" ]; then ln -s "${repo_root}/target" "${tree}/target"; fi
  (
    cd "${tree}/ios"
    ./scripts/generate.sh
    xcodebuild build \
      -project OhAnd.xcodeproj -scheme OhAndApp -configuration Debug -sdk iphonesimulator \
      -destination "platform=iOS Simulator,id=${udid}" \
      -derivedDataPath "${work_dir}/derived-${name}" \
      CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
  ) > "${evidence_dir}/build-${name}.log" 2>&1 || { tail -40 "${evidence_dir}/build-${name}.log" >&2; echo "build of ${name} failed" >&2; exit 1; }
  [ -d "${work_dir}/derived-${name}/Build/Products/Debug-iphonesimulator/OhAnd.app" ]
}
build_app "${repo_root}" candidate
build_app "${prior_tree}" prior
prior_app="${work_dir}/derived-prior/Build/Products/Debug-iphonesimulator/OhAnd.app"
candidate_app="${work_dir}/derived-candidate/Build/Products/Debug-iphonesimulator/OhAnd.app"
# The build numbers are the ones the built apps carry (a prior revision from before the build identity existed keeps the
# number in its source plist), and the candidate's must be the first-parent count build_info.py derives.
prior_build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "${prior_app}/Info.plist")"
built_candidate_build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "${candidate_app}/Info.plist")"
[ "${built_candidate_build}" = "${candidate_build}" ] || { echo "the candidate app carries build ${built_candidate_build}, not the derived ${candidate_build}" >&2; exit 1; }
log "prior app carries build ${prior_build}; candidate app carries build ${built_candidate_build}"

log "building the upgrade UI tests"
(
  cd "${repo_root}/ios"
  xcodebuild build-for-testing \
    -project OhAnd.xcodeproj -scheme OhAndUpgradeUITests -configuration Debug -sdk iphonesimulator \
    -destination "platform=iOS Simulator,id=${udid}" \
    -derivedDataPath "${work_dir}/derived-uitests" \
    CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
) > "${evidence_dir}/build-uitests.log" 2>&1 || { tail -40 "${evidence_dir}/build-uitests.log" >&2; echo "build of the UI tests failed" >&2; exit 1; }
pkill -x XCBBuildService 2>/dev/null || true

# ---- simulator ---------------------------------------------------------------------------------------------------------

"${scripts}/prepare-simulator.sh" "${udid}" --erase --shutdown-after --label upgrade-first-boot
"${scripts}/prepare-simulator.sh" "${udid}" --label upgrade

xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences DidShowGestureKeyboardIntroduction -bool true || true
xcrun simctl spawn "${udid}" defaults write com.apple.keyboard.preferences KeyboardDidShowContinuousPathIntroduction -bool true || true
# Enrol Face ID and keep presenting a matching face: the app's own authentication runs unmodified.
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

# ---- helpers -----------------------------------------------------------------------------------------------------------

appinfo() { xcrun simctl appinfo "${udid}" "${bundle_identifier}" 2>&1 | tee "${evidence_dir}/appinfo-$1.txt" >/dev/null; }
build_number_of() { python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); import verify_upgrade_report as v; print(v.parse_appinfo(open(sys.argv[2]).read())["build_number"] or "")' "${repo_root}/tools/release" "${evidence_dir}/appinfo-$1.txt"; }
container() { xcrun simctl get_app_container "${udid}" "${bundle_identifier}" data; }
report_path() { echo "$(container)/Library/Application Support/OhAnd/UpgradeCheck/upgrade-report.json"; }

# Launches the installed app asking for an upgrade report, waits for the app to write it (after its own authentication),
# copies it to the evidence directory and stops the app.
collect_report() {
  local name="$1" probes="$2" timeout_seconds="${3:-${report_timeout_seconds}}" path
  path="$(report_path)"
  xcrun simctl terminate "${udid}" "${bundle_identifier}" 2>/dev/null || true
  rm -f "${path}"
  xcrun simctl launch "${udid}" "${bundle_identifier}" -OhAndUpgradeReport -OhAndUpgradeProbe "${probes}" >/dev/null
  local waited=0
  while [ ! -s "${path}" ] && [ "${waited}" -lt "${timeout_seconds}" ]; do sleep 2; waited=$((waited + 2)); done
  xcrun simctl terminate "${udid}" "${bundle_identifier}" 2>/dev/null || true
  if [ -s "${path}" ]; then cp "${path}" "${evidence_dir}/report-${name}.json"; return 0; fi
  return 1
}
report_timeout_seconds=240

run_ui_tests() {
  local only_testing="$1" log_name="$2" status=0
  (
    cd "${repo_root}/ios"
    TEST_RUNNER_UPGRADE_OUTPUT_DIR="${evidence_dir}" xcodebuild test-without-building \
      -project OhAnd.xcodeproj -scheme OhAndUpgradeUITests -configuration Debug -sdk iphonesimulator \
      -destination "platform=iOS Simulator,id=${udid}" \
      -derivedDataPath "${work_dir}/derived-uitests" \
      -only-testing:"${only_testing}" \
      -resultBundlePath "${evidence_dir}/${log_name}.xcresult" \
      CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
  ) > "${evidence_dir}/${log_name}.log" 2>&1 || status=$?
  return "${status}"
}

# ---- 1. prior build, seeded through its UI ------------------------------------------------------------------------------

log "installing the prior build"
xcrun simctl install "${udid}" "${prior_app}"
appinfo before-seeding
log "prior build number: $(build_number_of before-seeding)"

seed_status=0
rm -f "${evidence_dir}/seed-expectations.json"
run_ui_tests OhAndUpgradeUITests/UpgradeSeedUITests seed || seed_status=$?
if [ "${seed_status}" -ne 0 ] || [ ! -s "${evidence_dir}/seed-expectations.json" ]; then
  tail -60 "${evidence_dir}/seed.log" >&2 || true
  echo "FAILURE: seeding the prior build through its UI failed (exit ${seed_status})" >&2
  exit 1
fi
deleted_item="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["deleted"]["itemID"])' "${evidence_dir}/seed-expectations.json")"
kept_item="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["kept"]["itemID"])' "${evidence_dir}/seed-expectations.json")"
xcrun simctl terminate "${udid}" "${bundle_identifier}" 2>/dev/null || true
container_before="$(container)"
# A marker file in the prior container: the simulator's installd may give the installed app a new container directory
# (the identifier in the path), so what proves the data carried over is that the marker, and the app's own data, arrive.
marker_token="$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
mkdir -p "${container_before}/Library"
printf '%s' "${marker_token}" > "${container_before}/Library/upgrade-check-marker"
log "data container before the upgrade recorded"

# The prior revision may already carry the upgrade report; when it does, its reminder identifiers are compared afterwards.
prior_report_present=false
if collect_report before-upgrade "${deleted_item},${kept_item}" 30; then prior_report_present=true; log "the prior revision wrote an upgrade report"; else log "the prior revision has no upgrade report"; fi

# ---- 2. the upgrade: the candidate over the prior build, never uninstalling ---------------------------------------------

xcrun simctl terminate "${udid}" "${bundle_identifier}" 2>/dev/null || true
log "installing the candidate over the prior build"
xcrun simctl install "${udid}" "${candidate_app}"
appinfo after-upgrade
container_after_upgrade="$(container)"
marker_carried=false
if [ "$(cat "${container_after_upgrade}/Library/upgrade-check-marker" 2>/dev/null || true)" = "${marker_token}" ]; then marker_carried=true; fi
log "candidate build number: $(build_number_of after-upgrade)"

collect_report after-upgrade "${deleted_item},${kept_item}" || { echo "FAILURE: the upgraded app wrote no upgrade report within ${report_timeout_seconds} s of launch" >&2; exit 1; }

verify_status=0
run_ui_tests OhAndUpgradeUITests/UpgradeVerifyUITests verify || verify_status=$?
xcrun simctl terminate "${udid}" "${bundle_identifier}" 2>/dev/null || true

collect_report after-relaunch "${deleted_item},${kept_item}" || { echo "FAILURE: the relaunched app wrote no upgrade report" >&2; exit 1; }
appinfo after-relaunch
container_after_relaunch="$(container)"

# ---- 3. judge ------------------------------------------------------------------------------------------------------------

BUILD_NUMBER_BEFORE="$(build_number_of before-seeding)" BUILD_NUMBER_AFTER="$(build_number_of after-upgrade)" \
BUILD_NUMBER_RELAUNCH="$(build_number_of after-relaunch)" \
PRIOR_REVISION="${prior_revision}" PRIOR_BUILD="${prior_build}" CANDIDATE_REVISION="${candidate_revision}" CANDIDATE_BUILD="${candidate_build}" \
PRIOR_IS_ANCESTOR="${prior_is_ancestor}" CONTAINER_BEFORE="${container_before}" CONTAINER_AFTER="${container_after_upgrade}" \
CONTAINER_RELAUNCH="${container_after_relaunch}" MARKER_CARRIED="${marker_carried}" SEED_STATUS="${seed_status}" VERIFY_STATUS="${verify_status}" \
SIMULATOR_NAME="${simulator_name}" SIMULATOR_RUNTIME="${simulator_runtime}" RUN_URL="${run_url}" \
python3 - "${evidence_dir}/facts.json" <<'PY'
import datetime, json, os, sys
env = os.environ
facts = {
    "collected_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "run": {"workflow_run": env["RUN_URL"] or None},
    "prior": {"revision": env["PRIOR_REVISION"], "build_number": int(env["PRIOR_BUILD"])},
    "candidate": {"revision": env["CANDIDATE_REVISION"], "build_number": int(env["CANDIDATE_BUILD"])},
    "prior_is_ancestor": env["PRIOR_IS_ANCESTOR"] == "true",
    "uninstalled": False,
    "installed": {
        "before_seeding": {"build_number": env["BUILD_NUMBER_BEFORE"]},
        "after_upgrade": {"build_number": env["BUILD_NUMBER_AFTER"]},
        "after_relaunch": {"build_number": env["BUILD_NUMBER_RELAUNCH"]},
    },
    "data_container": {"before_upgrade": env["CONTAINER_BEFORE"], "after_upgrade": env["CONTAINER_AFTER"],
                       "after_relaunch": env["CONTAINER_RELAUNCH"],
                       "marker_carried": env["MARKER_CARRIED"] == "true"},
    "simulator": {"name": env["SIMULATOR_NAME"], "runtime": env["SIMULATOR_RUNTIME"]},
    "ui_checks": {"seed_exit_status": int(env["SEED_STATUS"]), "verify_exit_status": int(env["VERIFY_STATUS"])},
}
json.dump(facts, open(sys.argv[1], "w"), indent=2)
PY

before_flag=()
if [ "${prior_report_present}" = "true" ]; then before_flag=(--report-before "${evidence_dir}/report-before-upgrade.json"); fi
python3 "${repo_root}/tools/release/verify_upgrade_report.py" \
  --facts "${evidence_dir}/facts.json" --expectations "${evidence_dir}/seed-expectations.json" \
  --report-after "${evidence_dir}/report-after-upgrade.json" --report-relaunch "${evidence_dir}/report-after-relaunch.json" \
  ${before_flag[@]+"${before_flag[@]}"} --output "${evidence_dir}/simulator-upgrade-record.json"
