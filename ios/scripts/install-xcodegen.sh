#!/usr/bin/env bash
# Puts the pinned XcodeGen on PATH from the project's published release archive, checked against a recorded SHA-256.
# In GitHub Actions the directory holding the archive is restored from the Actions cache (key: version and digest),
# so a warm job makes no network request. A failed download is retried; a digest mismatch never is.
set -euo pipefail

PINNED_XCODEGEN_VERSION="2.40.0"
PINNED_XCODEGEN_SHA256="7a57affbf81f57aaa3c76c2fa484d0dab940829e980951941aa7e5960dab64e6"
DOWNLOAD_ATTEMPTS=3
DOWNLOAD_PAUSE_SECONDS="${XCODEGEN_DOWNLOAD_PAUSE_SECONDS:-10}"

cache_directory="${XCODEGEN_CACHE_DIR:-${HOME}/.cache/ohand-xcodegen}"
archive_url="https://github.com/yonaskolb/XcodeGen/releases/download/${PINNED_XCODEGEN_VERSION}/xcodegen.zip"
archive_path="${cache_directory}/xcodegen-${PINNED_XCODEGEN_VERSION}-${PINNED_XCODEGEN_SHA256}.zip"
install_directory="${cache_directory}/xcodegen-${PINNED_XCODEGEN_VERSION}"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

archive_matches_pin() {
  [ -f "${archive_path}" ] && [ "$(sha256_of "${archive_path}")" = "${PINNED_XCODEGEN_SHA256}" ]
}

mkdir -p "${cache_directory}"

if archive_matches_pin; then
  echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: using cached archive"
else
  rm -f "${archive_path}"
  download_path="${archive_path}.download"
  attempt=1
  while true; do
    if curl --fail --silent --show-error --location --max-time 120 --output "${download_path}" "${archive_url}"; then
      break
    fi
    if [ "${attempt}" -ge "${DOWNLOAD_ATTEMPTS}" ]; then
      echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: download failed after ${DOWNLOAD_ATTEMPTS} attempts" >&2
      rm -f "${download_path}"
      exit 1
    fi
    echo "XcodeGen download attempt ${attempt} failed; retrying in ${DOWNLOAD_PAUSE_SECONDS}s" >&2
    attempt=$((attempt + 1))
    sleep "${DOWNLOAD_PAUSE_SECONDS}"
  done
  downloaded_sha256="$(sha256_of "${download_path}")"
  if [ "${downloaded_sha256}" != "${PINNED_XCODEGEN_SHA256}" ]; then
    echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: SHA-256 mismatch (expected ${PINNED_XCODEGEN_SHA256}, got ${downloaded_sha256}); not retrying" >&2
    rm -f "${download_path}"
    exit 1
  fi
  mv "${download_path}" "${archive_path}"
fi

rm -rf "${install_directory}"
mkdir -p "${install_directory}"
unzip -q "${archive_path}" -d "${install_directory}"
binary_directory="${install_directory}/xcodegen/bin"
test -x "${binary_directory}/xcodegen"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "${binary_directory}" >> "${GITHUB_PATH}"
fi
export PATH="${binary_directory}:${PATH}"
installed_version="$(xcodegen --version | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"
if [ "${installed_version}" != "${PINNED_XCODEGEN_VERSION}" ]; then
  echo "installed xcodegen reports ${installed_version}; ${PINNED_XCODEGEN_VERSION} is pinned" >&2
  exit 1
fi
echo "XcodeGen ${installed_version} installed at ${binary_directory}"
