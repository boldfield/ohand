#!/usr/bin/env bash
# Build the Rust core library for iOS simulator and device architectures
# Invoked by Xcode build phase
set -euo pipefail

RUST_PROJECT_DIR="$(cd "$(dirname "$0")/../../core" && pwd)"
BUILD_DIR="${BUILT_PRODUCTS_DIR}/rust-core"
CORE_BUILD="${BUILD_DIR}"

# Ensure build directory exists
mkdir -p "${CORE_BUILD}"

# Target architectures for the current SDK
case "${SDKNAME}" in
  iphonesimulator)
    TARGETS=("aarch64-apple-ios-sim" "x86_64-apple-ios-sim")
    ;;
  iphoneos)
    TARGETS=("aarch64-apple-ios")
    ;;
  *)
    echo "Unsupported SDK: ${SDKNAME}" >&2
    exit 1
    ;;
esac

# Cross-compile for each target
LIBS=()
for target in "${TARGETS[@]}"; do
  output="${CORE_BUILD}/libohand_core-${target}.a"
  echo "Building ohand_core for ${target}..."

  # Add the target if not already installed
  rustup target add "${target}" 2>/dev/null || true

  # Build the library
  cd "${RUST_PROJECT_DIR}"
  cargo build \
    --release \
    --target "${target}" \
    --lib \
    --locked

  # Copy the artifact
  cp "target/${target}/release/libohand_core.a" "${output}"
  LIBS+=("${output}")
done

# Create a universal library if multiple targets
if [ ${#LIBS[@]} -gt 1 ]; then
  universal_lib="${CORE_BUILD}/libohand_core.a"
  echo "Creating universal library..."
  lipo -create "${LIBS[@]}" -output "${universal_lib}"
  # Keep only the universal library
  for lib in "${LIBS[@]}"; do
    rm "${lib}"
  done
else
  # Single target case
  cp "${LIBS[0]}" "${CORE_BUILD}/libohand_core.a"
  rm "${LIBS[0]}"
fi

echo "Rust core library built: ${CORE_BUILD}/libohand_core.a"
