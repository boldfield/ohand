#!/usr/bin/env bash
# Build the Rust core library for iOS simulator and device architectures
# and generate the C FFI header. Invoked by Xcode build phase.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
RUST_PROJECT_DIR="${REPO_ROOT}/core"
BUILD_DIR="${BUILT_PRODUCTS_DIR}/rust-core"

# Ensure build directory exists
mkdir -p "${BUILD_DIR}"

# Target architectures for the current SDK
case "${SDKNAME}" in
  iphonesimulator*)
    TARGETS=("aarch64-apple-ios-sim" "x86_64-apple-ios")
    ;;
  iphoneos*)
    TARGETS=("aarch64-apple-ios")
    ;;
  *)
    echo "Unsupported SDK: ${SDKNAME}" >&2
    exit 1
    ;;
esac

# Get the workspace target directory
WORKSPACE_TARGET_DIR=$(cd "${REPO_ROOT}" && cargo metadata --format-version 1 | grep -o '"target_directory":"[^"]*"' | cut -d'"' -f4)

# Cross-compile for each target and collect libraries
LIBS=()
for target in "${TARGETS[@]}"; do
  output="${BUILD_DIR}/libohand_core-${target}.a"
  echo "Building ohand_core for ${target}..."

  # Ensure target is installed
  if ! rustup target list | grep -q "^${target} (installed)"; then
    echo "Installing Rust target ${target}..."
    rustup target add "${target}"
  fi

  # Build the library with locked dependencies
  cd "${REPO_ROOT}"
  cargo build \
    --release \
    --target "${target}" \
    --lib \
    --locked

  # Copy the built artifact from the workspace target directory
  cp "${WORKSPACE_TARGET_DIR}/${target}/release/libohand_core.a" "${output}"
  LIBS+=("${output}")
done

# Create a universal library for multi-architecture support
if [ ${#LIBS[@]} -gt 1 ]; then
  universal_lib="${BUILD_DIR}/libohand_core.a"
  echo "Creating universal library with lipo..."
  lipo -create "${LIBS[@]}" -output "${universal_lib}"
  # Clean up individual architecture libraries
  for lib in "${LIBS[@]}"; do
    rm "${lib}"
  done
else
  # Single target case (unlikely for iOS but handled)
  cp "${LIBS[0]}" "${BUILD_DIR}/libohand_core.a"
  rm "${LIBS[0]}"
fi

echo "Rust core library built: ${BUILD_DIR}/libohand_core.a"
