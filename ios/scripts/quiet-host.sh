#!/usr/bin/env bash
# Stops host work the simulator jobs do not need, so the hosted runner's few cores go to the simulator and the tests.
# Runs once at job start (macOS hosted runner, passwordless sudo). Every action is logged and best-effort: a service that
# is absent or already stopped is not an error. It never touches the runner, CoreSimulator, Xcode or the evidence.
# Usage: quiet-host.sh
set -uo pipefail

log() {
  echo "$(date -u +%H:%M:%S) quiet-host: $*"
}

try() {
  local description="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    log "${description}: done"
  else
    log "${description}: skipped (exit $?)"
  fi
}

log "load before: $(uptime | sed 's/.*load averages*: //')"

# Spotlight indexing walks every volume, including the simulator runtimes and DerivedData.
try "Spotlight indexing off on all volumes" sudo mdutil -a -i off

# spindump was seen running for 17 minutes at 78% CPU; the job does not read its reports.
for service in com.apple.spindump com.apple.diagnosticd_spindump; do
  try "disable ${service}" sudo launchctl disable "system/${service}"
  try "stop ${service}" sudo launchctl bootout "system/${service}"
done
try "kill spindump" sudo pkill -x spindump

# Widgets and Spotlight workers are not used by any step.
for process_name in WeatherWidget mds_stores mdworker_shared; do
  try "stop ${process_name}" sudo pkill -x "${process_name}"
done
try "stop widget agents" pkill -f 'WidgetKit|Widget Extension'

log "load after: $(uptime | sed 's/.*load averages*: //')"
