#!/usr/bin/env bash
# Fails unless the static library exports exactly the functions the header declares.
# Usage: tools/bindings/check-library.sh <ohand_bindings.h> <libohand_bindings.a>
set -euo pipefail

header_file="${1:?usage: tools/bindings/check-library.sh <header> <static-library>}"
library_file="${2:?usage: tools/bindings/check-library.sh <header> <static-library>}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"
export CARGO_TARGET_DIR="${BINDINGS_TOOL_TARGET_DIR:-${repo_root}/target/bindings-tool}"

cd "${repo_root}"
cargo run --quiet --locked --manifest-path tools/bindings/Cargo.toml -- \
  check-library "${header_file}" "${library_file}"
