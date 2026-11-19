#!/usr/bin/env bash
# Runs a simulator test step and retries it once when it failed before any assertion ran.
# The command's output goes to stdout and to <log>. On a non-zero exit the log is classified by classify_simulator_fault.py:
# `test-failure` returns the original status at once. `infrastructure` (the runner could not launch the app or find a booted
# device; a closed list of signatures) prints a `::warning::` line, re-prepares the device with prepare-simulator.sh (a warm
# boot, with its gates), keeps the first log as <log>.attempt-1, appends one line (label, kind, first-attempt seconds,
# second-attempt seconds, outcome) to simulator-recoveries.txt beside OHAND_HOST_PRESSURE_FILE (or beside <log> when unset),
# and runs the command once more. The second exit status is final. If the re-preparation fails, its status is returned.
# So that a result bundle path is unique per attempt, the token %ATTEMPT_SUFFIX% in any argument is replaced by an empty string
# on the first run and by `-attempt-2` on the second: -resultBundlePath "${bundle}%ATTEMPT_SUFFIX%.xcresult". The same value is
# exported as OHAND_ATTEMPT_SUFFIX for commands that read it (an exported function run by bash -c, for shell syntax).
# A boot loses device state the caller set up (enrolled Face ID, for one), so OHAND_AFTER_REPREPARE, if set, is run with
# `bash -c` after the re-preparation and before the second run; a failure there is only reported.
# OHAND_RETRY_LABEL names the step in the recoveries line (default: the log's base name).
# Usage: retry-simulator-step.sh <udid> <log> -- <command...>
set -uo pipefail

if [ "$#" -lt 4 ] || [ "$3" != "--" ]; then
  echo "usage: $0 <udid> <log> -- <command...>" >&2
  exit 2
fi
udid="$1"
log="$2"
shift 3
script_dir="$(cd "$(dirname "$0")" && pwd)"
label="${OHAND_RETRY_LABEL:-$(basename "${log}")}"

run_attempt() {
  local suffix="$1"
  shift
  local attempt_args=()
  local argument
  for argument in "$@"; do
    attempt_args+=("${argument//%ATTEMPT_SUFFIX%/${suffix}}")
  done
  mkdir -p "$(dirname "${log}")"
  OHAND_ATTEMPT_SUFFIX="${suffix}" "${attempt_args[@]}" 2>&1 | tee "${log}"
  return "${PIPESTATUS[0]}"
}

attempt_started=${SECONDS}
run_attempt "" "$@"
first_status=$?
first_seconds=$((SECONDS - attempt_started))
if [ "${first_status}" -eq 0 ]; then
  exit 0
fi

classification="$("${OHAND_PYTHON:-python3}" "${script_dir}/classify_simulator_fault.py" "${log}" "${udid}")"
if [ "$(echo "${classification}" | head -1)" != "infrastructure" ]; then
  exit "${first_status}"
fi

echo "::warning::${label}: the step failed before any assertion ran (exit ${first_status}); preparing ${udid} again and retrying once"
echo "${classification}" | tail -n +2
mv -f "${log}" "${log}.attempt-1"
"${script_dir}/prepare-simulator.sh" "${udid}" --label "${label}-retry"
prepare_status=$?
if [ "${prepare_status}" -ne 0 ]; then
  echo "::warning::${label}: re-preparing the simulator failed (exit ${prepare_status}); the step is not retried"
  exit "${prepare_status}"
fi

if [ -n "${OHAND_AFTER_REPREPARE:-}" ] && ! bash -c "${OHAND_AFTER_REPREPARE}"; then
  echo "::warning::${label}: the after-re-preparation hook failed; retrying anyway"
fi

attempt_started=${SECONDS}
run_attempt "-attempt-2" "$@"
second_status=$?
second_seconds=$((SECONDS - attempt_started))
outcome="recovered"
if [ "${second_status}" -ne 0 ]; then
  outcome="failed-twice"
fi
if [ -n "${OHAND_HOST_PRESSURE_FILE:-}" ]; then
  recoveries_file="$(dirname "${OHAND_HOST_PRESSURE_FILE}")/simulator-recoveries.txt"
else
  recoveries_file="$(dirname "${log}")/simulator-recoveries.txt"
fi
echo "${label} test-launch ${first_seconds} ${second_seconds} ${outcome}" >> "${recoveries_file}"
exit "${second_status}"
