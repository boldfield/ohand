#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished, then wait (bounded; each probe is bounded too) until the simulator answers service calls promptly, failing the preparation if it never does. A freshly erased simulator keeps the
# hosted runner at 0% idle for many minutes after bootstatus reports success (first-boot daemons, Spotlight
# indexing), so CPU idle never becomes a usable signal; what UI tests actually see is launches and accessibility reads
# that stall for seconds to a minute. The wait therefore polls the simulator itself and logs how long it took; the host
# snapshots are diagnostics only. Output goes to stdout only. macOS with Xcode only.
#
# A first boot of an erased simulator is what starves the hosted runner, so a job pays for at most one per device type:
# `--erase` is a cold boot; `--shutdown-after` settles the device and leaves it shut down so it can be cloned; `--clone`
# gives a suite a `simctl clone` of such a settled device (first-boot state kept, no app data), which the caller deletes.
# Every preparation appends one line (label, kind, time to responsive, highest 1-minute load, lowest free memory) to the
# file named by OHAND_HOST_PRESSURE_FILE, if set. That file is diagnostic only.
# If the device does not become responsive, or the host does not settle, the preparation makes exactly one recovery attempt
# (host snapshot, shut down every simulator, restart the CoreSimulator service, wait up to 120 s for the host to quieten) and
# then repeats the same preparation with the same gates and timeouts; a second failure exits 1. Each recovery prints a
# `::warning::` line, records kind `<kind>-recovered` or `<kind>-failed-twice` in the pressure file and appends one line
# (label, kind, first-attempt seconds, second-attempt seconds, outcome) to simulator-recoveries.txt beside that file.
# Usage: prepare-simulator.sh <simulator-udid> [--erase] [--shutdown-after] [--label <name>]
#        prepare-simulator.sh --clone <settled-simulator-udid> --udid-file <path> [--label <name>]
set -euo pipefail

responsive_probe_limit_seconds=3
responsive_probes_required=10
# The OHAND_TEST_* variables are read only by test_prepare_simulator.py so the suite runs in seconds; the defaults are the gates.
responsive_timeout_seconds="${OHAND_TEST_RESPONSIVE_TIMEOUT_SECONDS:-480}"
probe_interval_seconds="${OHAND_TEST_PROBE_INTERVAL_SECONDS:-1}"
recovery_quiet_wait_seconds="${OHAND_TEST_RECOVERY_QUIET_WAIT_SECONDS:-120}"
recovery_quiet_idle_percent=50
probe_hard_limit_seconds=10
# After the readiness gate passes, a first or cloned boot keeps the runner busy with first-boot work. CPU idle cannot be the
# signal on a hosted runner: run 40165860852 on macos-26-intel stayed at 0% idle for 8 minutes with a fully responsive
# warm-booted simulator (one simulator process held ~200% CPU), so an idle threshold was never met by a healthy device.
# The settle step therefore asks the question UI tests care about, stricter than the readiness gate: the simulator must
# answer 30 probes in a row, each within 3 s (the gate needs 10). If that does not happen within 480 s the preparation
# fails with a host snapshot, so no UI test starts on a starved host. CPU idle is still logged for diagnosis.
settle_probes_required="${OHAND_TEST_SETTLE_PROBES:-30}"
settle_timeout_seconds="${OHAND_TEST_SETTLE_TIMEOUT_SECONDS:-480}"
# Test-only: a test can time the first attempt out quickly while the recovery attempt keeps a generous window, so a healthy
# fake device never races a short deadline. Unset, both attempts use the same timeouts.
first_attempt_responsive_timeout_seconds="${OHAND_TEST_FIRST_ATTEMPT_RESPONSIVE_TIMEOUT_SECONDS:-${responsive_timeout_seconds}}"
first_attempt_settle_timeout_seconds="${OHAND_TEST_FIRST_ATTEMPT_SETTLE_TIMEOUT_SECONDS:-${settle_timeout_seconds}}"

udid=""
erase=0
shutdown_after=0
clone_of=""
udid_file=""
label="simulator"
if [ "${1:-}" != "--clone" ]; then
  udid="${1:?simulator udid required}"
  shift
fi
while [ $# -gt 0 ]; do
  case "$1" in
    --erase) erase=1 ;;
    --shutdown-after) shutdown_after=1 ;;
    --clone) clone_of="${2:?settled simulator udid required}"; shift ;;
    --udid-file) udid_file="${2:?path required}"; shift ;;
    --label) label="${2:?label required}"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
if [ -n "${clone_of}" ] && [ -z "${udid_file}" ]; then
  echo "--clone needs --udid-file" >&2
  exit 2
fi

log() {
  echo "$(date -u +%H:%M:%S) $*"
}

host_snapshot() {
  local label="$1"
  log "--- host snapshot: ${label} ---"
  log "load: $(uptime | sed 's/.*load averages*: //')"
  top -l 2 -n 0 -s 2 | grep -E 'CPU usage|PhysMem|VM:' | tail -3 || true
  log "busiest processes by CPU:"
  ps -Ao pid,pcpu,pmem,rss,etime,args -r | head -16 | cut -c1-200 || true
  log "largest processes by resident memory:"
  ps -Ao pid,pcpu,pmem,rss,etime,args -m | head -9 | cut -c1-200 || true
  log "swap and compressor: $(vm_stat 2>/dev/null | awk '/Swapins/ {i=$2} /Swapouts/ {o=$2} /occupied by compressor/ {c=$5} END {printf "swapins %s swapouts %s compressor pages %s", i, o, c}')"
  log "booted simulators: $(xcrun simctl list devices booted | grep -c Booted || true)"
  log "simulator-related processes: $(pgrep -fl 'CoreSimulator|launchd_sim|Simulator' | wc -l | tr -d ' ')"
}

peak_load="0"
lowest_free_mb=""

# Remembers the highest 1-minute load average and the lowest free memory (free + speculative pages) seen so far.
sample_pressure() {
  local load free_mb
  load="$(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}')" || load=""
  free_mb="$(vm_stat 2>/dev/null | awk '/page size of/ {size=$8} /Pages free/ {free=$3} /Pages speculative/ {spec=$3} END {if (size) printf "%d", (free + spec) * size / 1048576}')" || free_mb=""
  if [ -n "${load}" ] && awk -v l="${load}" -v p="${peak_load}" 'BEGIN { exit !(l > p) }'; then
    peak_load="${load}"
  fi
  if [ -n "${free_mb}" ] && { [ -z "${lowest_free_mb}" ] || [ "${free_mb}" -lt "${lowest_free_mb}" ]; }; then
    lowest_free_mb="${free_mb}"
  fi
}

now_seconds() {
  perl -MTime::HiRes=time -e 'printf "%.2f", time'
}

# One round trip into the simulator's launchd. Prints the elapsed seconds; fails if the call itself fails or does not
# finish within the hard limit (the alarm survives exec, so a hung simctl is killed rather than blocking the readiness loop).
probe_simulator() {
  local started finished
  started="$(now_seconds)"
  perl -e 'alarm shift; exec @ARGV' "${probe_hard_limit_seconds}" xcrun simctl spawn "${udid}" launchctl list >/dev/null 2>&1 || return 1
  finished="$(now_seconds)"
  echo "${finished} ${started}" | awk '{printf "%.2f", $1 - $2}'
}

# Waits for <required> probes in a row within the probe limit, for at most <timeout> seconds.
wait_for_simulator_to_respond() {
  local required="${1:-${responsive_probes_required}}"
  local timeout="${2:-${responsive_timeout_seconds}}"
  local started=${SECONDS}
  local consecutive=0
  local elapsed
  local probe_count=0
  while [ $((SECONDS - started)) -lt "${timeout}" ]; do
    probe_count=$((probe_count + 1))
    sample_pressure
    if elapsed="$(probe_simulator)" && awk -v e="${elapsed}" -v l="${responsive_probe_limit_seconds}" 'BEGIN { exit !(e <= l) }'; then
      consecutive=$((consecutive + 1))
    else
      consecutive=0
    fi
    log "probe ${probe_count}: ${elapsed:-failed}s (need ${required} in a row within ${responsive_probe_limit_seconds}s, ${consecutive} so far)"
    if [ "${consecutive}" -ge "${required}" ]; then
      log "simulator responsive after $((SECONDS - started))s"
      return 0
    fi
    sleep "${probe_interval_seconds}"
  done
  log "ERROR: simulator did not answer ${required} probes in a row within ${responsive_probe_limit_seconds}s inside ${timeout}s"
  return 1
}

wait_for_host_to_settle() {
  local timeout="$1"
  local idle
  idle="$(top -l 2 -n 0 -s 1 | grep 'CPU usage' | tail -1 | sed -E 's/.* ([0-9.]+)% idle.*/\1/')" || idle=""
  log "settle: waiting for ${settle_probes_required} probes in a row (CPU ${idle:-unknown}% idle, diagnostic only)"
  wait_for_simulator_to_respond "${settle_probes_required}" "${timeout}"
}

step() {
  local label="$1"
  shift
  local started=${SECONDS}
  "$@"
  log "${label}: $((SECONDS - started))s"
}

record_pressure() {
  local kind="$1" seconds="$2"
  local swapins swapouts compressor_pages
  read -r swapins swapouts compressor_pages < <(vm_stat 2>/dev/null | awk '/Swapins/ {i=$2} /Swapouts/ {o=$2} /occupied by compressor/ {c=$5} END {print i+0, o+0, c+0}') || true
  log "preparation ${label}: ${kind}, responsive after ${seconds}s, peak load ${peak_load}, lowest free memory ${lowest_free_mb:-unknown} MB, swapins ${swapins:-unknown}, swapouts ${swapouts:-unknown}, compressor pages ${compressor_pages:-unknown}"
  if [ -n "${OHAND_HOST_PRESSURE_FILE:-}" ]; then
    mkdir -p "$(dirname "${OHAND_HOST_PRESSURE_FILE}")"
    if [ ! -s "${OHAND_HOST_PRESSURE_FILE}" ]; then
      echo "label kind seconds_to_responsive highest_load_1min lowest_free_memory_mb swapins swapouts compressor_pages" > "${OHAND_HOST_PRESSURE_FILE}"
    fi
    echo "${label} ${kind} ${seconds} ${peak_load} ${lowest_free_mb:-unknown} ${swapins:-unknown} ${swapouts:-unknown} ${compressor_pages:-unknown}" >> "${OHAND_HOST_PRESSURE_FILE}"
  fi
}

if [ -n "${clone_of}" ]; then
  kind="clone"
else
  kind="warm-boot"
  if [ "${erase}" -eq 1 ]; then
    kind="cold-boot"
  fi
fi

# Sets failed_gate to "responsive" or "settle" and returns 1 when a gate fails. Other failures exit the script. Called in an
# `if`, where `set -e` does not apply, so the steps that must not fail exit explicitly.
failed_gate=""
attempt_seconds=0
attempt_preparation() {
  local first_call="$1"
  local attempt_responsive_timeout="${responsive_timeout_seconds}" attempt_settle_timeout="${settle_timeout_seconds}"
  if [ "${first_call}" -eq 1 ]; then
    attempt_responsive_timeout="${first_attempt_responsive_timeout_seconds}"
    attempt_settle_timeout="${first_attempt_settle_timeout_seconds}"
  fi
  if [ -n "${clone_of}" ]; then
    # The settled template is shut down, and no other simulator may be running while the clone boots: shut down all of them.
    log "shutting down every booted simulator before cloning"
    xcrun simctl shutdown all 2>/dev/null || true
    host_snapshot "before"
    sample_pressure
    prepare_started=${SECONDS}
    udid="$(xcrun simctl clone "${clone_of}" "ohand-${label}-$$-${first_call}")" || exit 1
    log "cloned settled simulator ${clone_of} to ${udid}"
    # errexit is off inside a function called from an `if`, so a failed write must exit explicitly or the caller keeps a stale UDID.
    echo "${udid}" > "${udid_file}" || { echo "cannot write ${udid_file}" >&2; exit 1; }
  else
    log "preparing simulator ${udid} (kind=${kind})"
    host_snapshot "before"
    sample_pressure
    prepare_started=${SECONDS}
    step shutdown xcrun simctl shutdown "${udid}" 2>/dev/null || true
    if [ "${erase}" -eq 1 ]; then
      step erase xcrun simctl erase "${udid}" || exit 1
    fi
  fi
  step boot xcrun simctl boot "${udid}" 2>/dev/null || true
  step bootstatus xcrun simctl bootstatus "${udid}" -b >/dev/null || exit 1

  if ! wait_for_simulator_to_respond "${responsive_probes_required}" "${attempt_responsive_timeout}"; then
    host_snapshot "simulator never became responsive"
    failed_gate="responsive"
    attempt_seconds="$((SECONDS - prepare_started))"
    return 1
  fi
  sample_pressure
  host_snapshot "after boot completed and simulator responsive"
  responsive_seconds="$((SECONDS - prepare_started))"
  if ! wait_for_host_to_settle "${attempt_settle_timeout}"; then
    host_snapshot "host never settled"
    failed_gate="settle"
    attempt_seconds="$((SECONDS - prepare_started))"
    return 1
  fi
  # The whole attempt, both gates included, so the recoveries file compares like with like; record_pressure keeps the
  # time to responsive for its own column.
  attempt_seconds="$((SECONDS - prepare_started))"
  return 0
}

# Shuts down every simulator, restarts the CoreSimulator service and waits (bounded) for the host to quieten. The failed
# clone is deleted so a new one can be made from the template.
recover_host() {
  log "recovery: shutting down every simulator and restarting the CoreSimulator service"
  xcrun simctl shutdown all 2>/dev/null || true
  if [ -n "${clone_of}" ]; then
    xcrun simctl delete "${udid}" 2>/dev/null || true
  fi
  killall com.apple.CoreSimulator.CoreSimulatorService 2>/dev/null || true
  # The deadline is wall-clock and checked before every sample; a sample is cut off at the deadline (the alarm survives
  # exec), so neither `top` nor a sleep can carry the wait past it.
  local started deadline remaining waited idle=""
  started="$(now_seconds)"
  deadline="$(awk -v s="${started}" -v w="${recovery_quiet_wait_seconds}" 'BEGIN {printf "%.2f", s + w}')"
  while :; do
    remaining="$(awk -v d="${deadline}" -v n="$(now_seconds)" 'BEGIN {r = d - n; printf "%.2f", (r > 0 ? r : 0)}')"
    if awk -v r="${remaining}" 'BEGIN { exit !(r <= 0) }'; then
      log "recovery: stopped waiting for the host to quieten at the ${recovery_quiet_wait_seconds}s deadline"
      break
    fi
    idle="$(perl -MTime::HiRes=alarm -e 'alarm shift; exec @ARGV' "${remaining}" top -l 2 -n 0 -s 1 2>/dev/null | grep 'CPU usage' | tail -1 | sed -E 's/.* ([0-9.]+)% idle.*/\1/')" || idle=""
    # A sample cut off or unreadable is not a number; it never ends the wait early.
    [[ "${idle}" =~ ^[0-9]+(\.[0-9]+)?$ ]] || idle=""
    waited="$(awk -v s="${started}" -v n="$(now_seconds)" 'BEGIN {printf "%.2f", n - s}')"
    log "recovery: waiting for the host to quieten (CPU ${idle:-unknown}% idle, ${waited}s of ${recovery_quiet_wait_seconds}s)"
    if [ -n "${idle}" ] && awk -v i="${idle}" -v t="${recovery_quiet_idle_percent}" 'BEGIN { exit !(i >= t) }'; then
      break
    fi
    remaining="$(awk -v d="${deadline}" -v n="$(now_seconds)" 'BEGIN {r = d - n; printf "%.2f", (r > 5 ? 5 : (r > 0 ? r : 0))}')"
    sleep "${remaining}"
  done
}

record_recovery() {
  local outcome="$1" first_seconds="$2" second_seconds="$3"
  if [ -n "${OHAND_HOST_PRESSURE_FILE:-}" ]; then
    local recoveries_file
    recoveries_file="$(dirname "${OHAND_HOST_PRESSURE_FILE}")/simulator-recoveries.txt"
    echo "${label} ${kind} ${first_seconds} ${second_seconds} ${outcome}" >> "${recoveries_file}"
  fi
}

if attempt_preparation 1; then
  record_pressure "${kind}" "${responsive_seconds}"
else
  first_gate="${failed_gate}"
  first_seconds="${attempt_seconds}"
  echo "::warning::simulator preparation ${label} (${kind}): the ${first_gate} gate failed; recovering once"
  host_snapshot "recovery"
  recover_host
  if attempt_preparation 2; then
    echo "::warning::simulator preparation ${label} (${kind}): recovered after the ${first_gate} gate failed on the first attempt"
    record_pressure "${kind}-recovered" "${responsive_seconds}"
    record_recovery "recovered" "${first_seconds}" "${attempt_seconds}"
  else
    echo "::warning::simulator preparation ${label} (${kind}): failed twice (first the ${first_gate} gate, then the ${failed_gate} gate)"
    record_pressure "${kind}-failed-twice" "${attempt_seconds}"
    record_recovery "failed-twice" "${first_seconds}" "${attempt_seconds}"
    exit 1
  fi
fi
if [ "${shutdown_after}" -eq 1 ]; then
  step "shutdown after settling" xcrun simctl shutdown "${udid}"
fi
log "simulator ${udid} ready"
