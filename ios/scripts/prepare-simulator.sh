#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished, then wait (bounded) until the simulator answers service calls promptly. A freshly erased simulator keeps the
# 3-core hosted runner at 0% idle for many minutes after bootstatus reports success (first-boot daemons, Spotlight
# indexing), so CPU idle never becomes a usable signal; what UI tests actually see is launches and accessibility reads
# that stall for seconds to a minute. The wait therefore polls the simulator itself and logs how long it took; the host
# snapshots are diagnostics only. Output goes to stdout only. macOS with Xcode only.
# Usage: prepare-simulator.sh <simulator-udid> [--erase]
set -euo pipefail

responsive_probe_limit_seconds=3
responsive_probes_required=10
responsive_timeout_seconds=240

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

now_seconds() {
  perl -MTime::HiRes=time -e 'printf "%.2f", time'
}

# One round trip into the simulator's launchd. Prints the elapsed seconds; fails if the call itself fails.
probe_simulator() {
  local started finished
  started="$(now_seconds)"
  xcrun simctl spawn "${udid}" launchctl list >/dev/null 2>&1 || return 1
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
  log "WARNING: simulator did not answer ${responsive_probes_required} probes in a row within ${responsive_probe_limit_seconds}s inside ${responsive_timeout_seconds}s; continuing anyway"
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

wait_for_simulator_to_respond
host_snapshot "after boot completed and simulator responsive"
log "simulator ${udid} ready"
