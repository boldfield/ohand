#!/usr/bin/env bash
# Builds the host static library and the header from the same sources, then checks that the
# library exports exactly the functions the header declares. Generated files stay under target/.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"
cd "${repo_root}"

product_target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
header_dir="${repo_root}/target/bindings-header"

cargo build --quiet --locked --package ohand-bindings
tools/bindings/generate.sh "${header_dir}"
tools/bindings/check-library.sh "${header_dir}/ohand_bindings.h" "${product_target_dir}/debug/libohand_bindings.a"
echo "bindings verified: library exports match ${header_dir}/ohand_bindings.h"
