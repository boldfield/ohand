#!/usr/bin/env bash
# Puts a simulator into a known state before a UI-test suite: shut down (which ends every leftover app), optionally
# erase (which clears installed apps, permissions and recorded notification decisions), boot, wait until the boot has
# finished, then wait for the host and the simulator to go quiet. UI tests that start while the device is still
# booting, or while the host is busy with the previous suite, see accessibility reads and app launches that are many
# times slower. It records how long each step took and the host load so a later slow test has evidence to point at.
# macOS with Xcode only.
# Usage: prepare-simulator.sh <simulator-udid> [--erase] [diagnostics-log]
set -euo pipefail

udid="${1:?simulator udid required}"
shift
erase=0
if [ "${1:-}" = "--erase" ]; then
  erase=1
  shift
fi
diagnostics_log="${1:-}"

log() {
  local line
  line="$(date -u +%H:%M:%S) $*"
  echo "${line}"
  if [ -n "${diagnostics_log}" ]; then
    echo "${line}" >> "${diagnostics_log}"
  fi
}

host_snapshot() {
  log "host load: $(uptime | sed 's/.*load averages*: //')"
  log "host cpu: $(top -l 1 -n 0 | grep -m1 'CPU usage' || true)"
  log "host memory: $(top -l 1 -n 0 | grep -m1 'PhysMem' || true)"
}

step() {
  local label="$1"
  shift
  local started=${SECONDS}
  "$@"
  log "${label}: $((SECONDS - started))s"
}

if [ -n "${diagnostics_log}" ]; then
  mkdir -p "$(dirname "${diagnostics_log}")"
fi

log "preparing simulator ${udid} (erase=${erase})"
host_snapshot

step shutdown xcrun simctl shutdown "${udid}" 2>/dev/null || true
if [ "${erase}" -eq 1 ]; then
  step erase xcrun simctl erase "${udid}"
fi
step boot xcrun simctl boot "${udid}" 2>/dev/null || true
step bootstatus xcrun simctl bootstatus "${udid}" -b >/dev/null

# bootstatus returns when the boot completes, but the services started by a first boot (after an erase especially) keep
# the host busy for a while. Wait for the host to go idle, for at most 180 seconds, and record whether it did.
settle_limit=180
settle_started=${SECONDS}
idle_percent=0
while :; do
  idle_percent="$(top -l 2 -n 0 -s 2 | grep 'CPU usage' | tail -1 | sed -E 's/.* ([0-9.]+)% idle.*/\1/' | cut -d. -f1)"
  idle_percent="${idle_percent:-0}"
  if [ "${idle_percent}" -ge 60 ]; then
    break
  fi
  if [ $((SECONDS - settle_started)) -ge "${settle_limit}" ]; then
    break
  fi
done
if [ "${idle_percent}" -ge 60 ]; then
  log "settle: host ${idle_percent}% idle after $((SECONDS - settle_started))s"
else
  log "settle: host still only ${idle_percent}% idle after ${settle_limit}s; continuing"
fi
host_snapshot
log "simulator ${udid} ready"
