#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished, then wait (bounded) until the host CPU has settled. A freshly erased simulator keeps the host saturated for
# a while after bootstatus reports success (first-boot daemons), and UI tests that start in that window see launches and
# accessibility reads many times slower. The settle wait is an explicit poll that logs how long it took; the host
# snapshots are diagnostics only. Output goes to stdout only. macOS with Xcode only.
# Usage: prepare-simulator.sh <simulator-udid> [--erase]
set -euo pipefail

settle_idle_percent=50
settle_consecutive_samples=2
settle_timeout_seconds=300

udid="${1:?simulator udid required}"
erase=0
if [ "${2:-}" = "--erase" ]; then
  erase=1
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

host_idle_percent() {
  top -l 2 -s 1 -n 0 | grep 'CPU usage' | tail -1 | sed -E 's/.* ([0-9]+)(\.[0-9]+)?% idle.*/\1/'
}

wait_for_host_to_settle() {
  local started=${SECONDS}
  local consecutive=0
  local idle
  while [ $((SECONDS - started)) -lt "${settle_timeout_seconds}" ]; do
    idle="$(host_idle_percent || true)"
    case "${idle}" in
      ''|*[!0-9]*) idle=0 ;;
    esac
    if [ "${idle}" -ge "${settle_idle_percent}" ]; then
      consecutive=$((consecutive + 1))
    else
      consecutive=0
    fi
    log "settle poll: host ${idle}% idle (need ${settle_idle_percent}% for ${settle_consecutive_samples} samples in a row, ${consecutive} so far)"
    if [ "${consecutive}" -ge "${settle_consecutive_samples}" ]; then
      log "host settled after $((SECONDS - started))s"
      return 0
    fi
  done
  log "WARNING: host did not settle within ${settle_timeout_seconds}s; continuing anyway"
}

step() {
  local label="$1"
  shift
  local started=${SECONDS}
  "$@"
  log "${label}: $((SECONDS - started))s"
}

log "preparing simulator ${udid} (erase=${erase})"
host_snapshot "before"

step shutdown xcrun simctl shutdown "${udid}" 2>/dev/null || true
if [ "${erase}" -eq 1 ]; then
  step erase xcrun simctl erase "${udid}"
fi
step boot xcrun simctl boot "${udid}" 2>/dev/null || true
step bootstatus xcrun simctl bootstatus "${udid}" -b >/dev/null

wait_for_host_to_settle
host_snapshot "after boot completed and host settled"
log "simulator ${udid} ready"
