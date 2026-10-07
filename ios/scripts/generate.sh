#!/usr/bin/env bash
# Generates ios/OhAnd.xcodeproj with the pinned XcodeGen version (macOS only).
set -euo pipefail

PINNED_XCODEGEN_VERSION="2.40.0"
cd "$(dirname "$0")/.."

if command -v mint >/dev/null 2>&1; then
  generator=(mint run "yonaskolb/XcodeGen@${PINNED_XCODEGEN_VERSION}")
elif command -v xcodegen >/dev/null 2>&1; then
  installed_version="$(xcodegen --version | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"
  if [ "${installed_version}" != "${PINNED_XCODEGEN_VERSION}" ]; then
    echo "xcodegen ${installed_version} found; ${PINNED_XCODEGEN_VERSION} is required (see Mintfile)" >&2
    exit 1
  fi
  generator=(xcodegen)
else
  echo "Install mint (brew install mint) or XcodeGen ${PINNED_XCODEGEN_VERSION}" >&2
  exit 1
fi

"${generator[@]}" generate --spec project.yml --project .
