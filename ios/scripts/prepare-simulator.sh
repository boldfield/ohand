#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished. It prints how long each step took and what is using the host (busiest processes, memory, booted
# simulators), because UI tests that run on a saturated hosted runner see launches and accessibility reads that are many
# times slower. Output goes to stdout only. macOS with Xcode only.
# Usage: prepare-simulator.sh <simulator-udid> [--erase]
set -euo pipefail

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

host_snapshot "after boot completed"
log "simulator ${udid} ready"
