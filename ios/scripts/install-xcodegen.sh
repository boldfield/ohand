#!/usr/bin/env bash
# Puts the pinned XcodeGen on PATH from the project's published release archive, checked against a recorded SHA-256.
# The archive is kept in XCODEGEN_ARCHIVE_DIR (default ~/.cache/ohand-xcodegen), which ios.yml and upgrade.yml restore
# from the Actions cache under a key made of the version and the digest, so a warm job makes no network request.
# A failed download is tried three times with a pause; a digest mismatch is never retried.
# The version must equal PINNED_XCODEGEN_VERSION in generate.sh and the Mintfile (check_project_config.py).
set -euo pipefail

PINNED_XCODEGEN_VERSION="2.40.0"
PINNED_XCODEGEN_SHA256="7a57affbf81f57aaa3c76c2fa484d0dab940829e980951941aa7e5960dab64e6"
DOWNLOAD_ATTEMPTS=3
DOWNLOAD_PAUSE_SECONDS="${XCODEGEN_DOWNLOAD_PAUSE_SECONDS:-15}"

archive_directory="${XCODEGEN_ARCHIVE_DIR:-${HOME}/.cache/ohand-xcodegen}"
# The unpacked tool stays outside the cached directory, so the cache holds only the verified archive.
install_root="${XCODEGEN_INSTALL_DIR:-${RUNNER_TEMP:-${archive_directory}}}"
archive_url="https://github.com/yonaskolb/XcodeGen/releases/download/${PINNED_XCODEGEN_VERSION}/xcodegen.zip"
archive_path="${archive_directory}/xcodegen-${PINNED_XCODEGEN_VERSION}-${PINNED_XCODEGEN_SHA256}.zip"
install_directory="${install_root}/xcodegen-${PINNED_XCODEGEN_VERSION}"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

mkdir -p "${archive_directory}"

if [ -f "${archive_path}" ] && [ "$(sha256_of "${archive_path}")" = "${PINNED_XCODEGEN_SHA256}" ]; then
  echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: using the cached archive ${archive_path}"
else
  rm -f "${archive_path}"
  download_path="${archive_path}.download"
  attempt=1
  until curl --fail --silent --show-error --location --connect-timeout 20 --max-time 120 \
      --output "${download_path}" "${archive_url}"; do
    if [ "${attempt}" -ge "${DOWNLOAD_ATTEMPTS}" ]; then
      echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: download of ${archive_url} failed ${DOWNLOAD_ATTEMPTS} times" >&2
      rm -f "${download_path}"
      exit 1
    fi
    echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: download attempt ${attempt} failed; retrying in ${DOWNLOAD_PAUSE_SECONDS}s" >&2
    attempt=$((attempt + 1))
    sleep "${DOWNLOAD_PAUSE_SECONDS}"
  done
  downloaded_sha256="$(sha256_of "${download_path}")"
  if [ "${downloaded_sha256}" != "${PINNED_XCODEGEN_SHA256}" ]; then
    echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: SHA-256 of ${archive_url} is ${downloaded_sha256}, ${PINNED_XCODEGEN_SHA256} is recorded; not retrying" >&2
    rm -f "${download_path}"
    exit 1
  fi
  mv "${download_path}" "${archive_path}"
  echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: downloaded and verified ${archive_path} (attempt ${attempt})"
fi

rm -rf "${install_directory}"
mkdir -p "${install_directory}"
unzip -q "${archive_path}" -d "${install_directory}"
binary_directory="${install_directory}/xcodegen/bin"
if [ ! -x "${binary_directory}/xcodegen" ]; then
  echo "XcodeGen ${PINNED_XCODEGEN_VERSION}: the archive has no xcodegen/bin/xcodegen" >&2
  exit 1
fi

installed_version="$("${binary_directory}/xcodegen" --version | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"
if [ "${installed_version}" != "${PINNED_XCODEGEN_VERSION}" ]; then
  echo "the installed xcodegen reports ${installed_version}; ${PINNED_XCODEGEN_VERSION} is pinned" >&2
  exit 1
fi

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "${binary_directory}" >> "${GITHUB_PATH}"
  echo "XcodeGen ${installed_version} installed at ${binary_directory} and added to PATH for later steps"
else
  echo "XcodeGen ${installed_version} installed at ${binary_directory}; add it to PATH:"
  echo "  export PATH=\"${binary_directory}:\${PATH}\""
fi
