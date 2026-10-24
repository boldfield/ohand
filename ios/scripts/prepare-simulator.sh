#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished, then wait (bounded; each probe is bounded too) until the simulator answers service calls promptly, failing the preparation if it never does. A freshly erased simulator keeps the
# 3-core hosted runner at 0% idle for many minutes after bootstatus reports success (first-boot daemons, Spotlight
# indexing), so CPU idle never becomes a usable signal; what UI tests actually see is launches and accessibility reads
# that stall for seconds to a minute. The wait therefore polls the simulator itself and logs how long it took; the host
# snapshots are diagnostics only. Output goes to stdout only. macOS with Xcode only.
#
# A first boot of an erased simulator is what starves the hosted runner, so a job pays for at most one per device type:
# `--erase` is a cold boot; `--shutdown-after` settles the device and leaves it shut down so it can be cloned; `--clone`
# gives a suite a `simctl clone` of such a settled device (first-boot state kept, no app data), which the caller deletes.
# Every preparation appends one line (label, kind, time to responsive, highest 1-minute load, lowest free memory) to the
# file named by OHAND_HOST_PRESSURE_FILE, if set. That file is diagnostic only.
# Usage: prepare-simulator.sh <simulator-udid> [--erase] [--shutdown-after] [--label <name>]
#        prepare-simulator.sh --clone <settled-simulator-udid> --udid-file <path> [--label <name>]
set -euo pipefail

responsive_probe_limit_seconds=3
responsive_probes_required=10
responsive_timeout_seconds=240
probe_hard_limit_seconds=10

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

wait_for_simulator_to_respond() {
  local started=${SECONDS}
  local consecutive=0
  local elapsed
  local probe_count=0
  while [ $((SECONDS - started)) -lt "${responsive_timeout_seconds}" ]; do
    probe_count=$((probe_count + 1))
    sample_pressure
    if elapsed="$(probe_simulator)" && awk -v e="${elapsed}" -v l="${responsive_probe_limit_seconds}" 'BEGIN { exit !(e <= l) }'; then
      consecutive=$((consecutive + 1))
    else
      consecutive=0
    fi
    log "probe ${probe_count}: ${elapsed:-failed}s (need ${responsive_probes_required} in a row within ${responsive_probe_limit_seconds}s, ${consecutive} so far)"
    if [ "${consecutive}" -ge "${responsive_probes_required}" ]; then
      log "simulator responsive after $((SECONDS - started))s"
      return 0
    fi
    sleep 1
  done
  log "ERROR: simulator did not answer ${responsive_probes_required} probes in a row within ${responsive_probe_limit_seconds}s inside ${responsive_timeout_seconds}s"
  return 1
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
  log "preparation ${label}: ${kind}, responsive after ${seconds}s, peak load ${peak_load}, lowest free memory ${lowest_free_mb:-unknown} MB"
  if [ -n "${OHAND_HOST_PRESSURE_FILE:-}" ]; then
    mkdir -p "$(dirname "${OHAND_HOST_PRESSURE_FILE}")"
    if [ ! -s "${OHAND_HOST_PRESSURE_FILE}" ]; then
      echo "label kind seconds_to_responsive highest_load_1min lowest_free_memory_mb" > "${OHAND_HOST_PRESSURE_FILE}"
    fi
    echo "${label} ${kind} ${seconds} ${peak_load} ${lowest_free_mb:-unknown}" >> "${OHAND_HOST_PRESSURE_FILE}"
  fi
}

if [ -n "${clone_of}" ]; then
  kind="clone"
  # Only the settled template may be booted at a time with the clone: shut down everything else first.
  log "shutting down every booted simulator before cloning"
  xcrun simctl shutdown all 2>/dev/null || true
  host_snapshot "before"
  sample_pressure
  prepare_started=${SECONDS}
  udid="$(xcrun simctl clone "${clone_of}" "ohand-${label}-$$")"
  log "cloned settled simulator ${clone_of} to ${udid}"
  echo "${udid}" > "${udid_file}"
else
  kind="warm-boot"
  if [ "${erase}" -eq 1 ]; then
    kind="cold-boot"
  fi
  log "preparing simulator ${udid} (kind=${kind})"
  host_snapshot "before"
  sample_pressure
  prepare_started=${SECONDS}
  step shutdown xcrun simctl shutdown "${udid}" 2>/dev/null || true
  if [ "${erase}" -eq 1 ]; then
    step erase xcrun simctl erase "${udid}"
  fi
fi
step boot xcrun simctl boot "${udid}" 2>/dev/null || true
step bootstatus xcrun simctl bootstatus "${udid}" -b >/dev/null

if ! wait_for_simulator_to_respond; then
  host_snapshot "simulator never became responsive"
  record_pressure "${kind}-failed" "$((SECONDS - prepare_started))"
  exit 1
fi
sample_pressure
host_snapshot "after boot completed and simulator responsive"
record_pressure "${kind}" "$((SECONDS - prepare_started))"
if [ "${shutdown_after}" -eq 1 ]; then
  step "shutdown after settling" xcrun simctl shutdown "${udid}"
fi
log "simulator ${udid} ready"
