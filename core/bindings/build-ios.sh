#!/usr/bin/env bash
# Xcode run-script phase: builds libohand_bindings.a for the SDK and architectures Xcode is
# building and places it in $OHAND_RUST_OUTPUT_DIR together with the generated C header
# (tools/bindings), which Xcode finds through HEADER_SEARCH_PATHS. Neither is committed.
# macOS with Xcode and rustup only.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
  echo "error: rustup and cargo are required to build the Rust core for iOS" >&2
  exit 1
fi

output_dir="${OHAND_RUST_OUTPUT_DIR:?OHAND_RUST_OUTPUT_DIR must be set by the Xcode target}"
platform_name="${PLATFORM_NAME:?PLATFORM_NAME must be set by Xcode}"
architectures="${ARCHS:?ARCHS must be set by Xcode}"
deployment_target="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

release_flag=""
profile_directory="debug"
if [ "${CONFIGURATION:-Debug}" = "Release" ]; then
  release_flag="--release"
  profile_directory="release"
fi

cd "${repo_root}"

# rust-toolchain.toml pins the toolchain; make sure it is installed (older rustup does not
# install it implicitly) and fail loudly if it cannot be.
if ! rustup show active-toolchain >/dev/null 2>&1; then
  pinned_channel="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
  rustup toolchain install "${pinned_channel}" --profile minimal
fi

rust_targets=""
for architecture in ${architectures}; do
  case "${platform_name}:${architecture}" in
    iphonesimulator:arm64) rust_target="aarch64-apple-ios-sim" ;;
    iphonesimulator:x86_64) rust_target="x86_64-apple-ios" ;;
    iphoneos:arm64) rust_target="aarch64-apple-ios" ;;
    *)
      echo "error: unsupported platform/architecture ${platform_name}/${architecture}" >&2
      exit 1
      ;;
  esac
  rustup target add "${rust_target}"
  rust_targets="${rust_targets} ${rust_target}"
done

# Xcode's compiler and SDK variables must not steer host builds of the generator or the
# C build of the bundled SQLite; cc-rs and rustc locate the SDK through xcrun instead.
run_with_clean_environment() {
  env -i \
    HOME="${HOME}" \
    PATH="${PATH}" \
    TMPDIR="${TMPDIR:-/tmp}" \
    DEVELOPER_DIR="${DEVELOPER_DIR:-$(xcode-select -p)}" \
    IPHONEOS_DEPLOYMENT_TARGET="${deployment_target}" \
    ${CARGO_HOME:+CARGO_HOME="${CARGO_HOME}"} \
    ${RUSTUP_HOME:+RUSTUP_HOME="${RUSTUP_HOME}"} \
    "$@"
}

mkdir -p "${output_dir}"
run_with_clean_environment "${repo_root}/tools/bindings/generate.sh" "${output_dir}"
built_libraries=""
for rust_target in ${rust_targets}; do
  run_with_clean_environment \
    cargo build --package ohand-bindings --locked --target "${rust_target}" ${release_flag}
  # Every slice must export exactly what the shared header declares. Release libraries are
  # LTO bitcode that nm cannot read, so only debug libraries are inspected; they are built
  # from the same sources and header.
  if [ -z "${release_flag}" ]; then
    run_with_clean_environment "${repo_root}/tools/bindings/check-library.sh" \
      "${output_dir}/ohand_bindings.h" \
      "${repo_root}/target/${rust_target}/${profile_directory}/libohand_bindings.a"
  fi
  built_libraries="${built_libraries} ${repo_root}/target/${rust_target}/${profile_directory}/libohand_bindings.a"
done

temporary_library="${output_dir}/libohand_bindings.a.tmp.$$"
# shellcheck disable=SC2086
lipo -create ${built_libraries} -output "${temporary_library}"
mv -f "${temporary_library}" "${output_dir}/libohand_bindings.a"
echo "Built ${output_dir}/libohand_bindings.a for ${platform_name} (${architectures})"
