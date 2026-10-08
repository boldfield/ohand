#!/usr/bin/env bash
# Generates the C header for the Rust core ABI into <output_dir>/ohand_bindings.h.
# The header is build output and is never committed. Usage: tools/bindings/generate.sh <output_dir>
set -euo pipefail

output_dir="${1:?usage: tools/bindings/generate.sh <output_dir>}"
mkdir -p "${output_dir}"
output_dir="$(cd "${output_dir}" && pwd)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"
export CARGO_TARGET_DIR="${BINDINGS_TOOL_TARGET_DIR:-${repo_root}/target/bindings-tool}"

cd "${repo_root}"
cargo run --quiet --locked --manifest-path tools/bindings/Cargo.toml -- \
  generate core/bindings tools/bindings/cbindgen.toml "${output_dir}/ohand_bindings.h"
