#!/bin/bash
# Generate reproducible C bindings from Rust FFI source.
# Outputs to build artifacts; generated files are not committed.
# Usage: tools/bindings/generate.sh <output_dir>

set -euo pipefail

output_dir="${1:-.}"
config_file="tools/bindings/cbindgen.toml"

# Verify config exists
if [ ! -f "$config_file" ]; then
    echo "ERROR: cbindgen config not found: $config_file" >&2
    exit 1
fi

# Build and run the binding generator
cd "$(git rev-parse --show-toplevel)"
cargo run -q --manifest-path tools/bindings/Cargo.toml -- "$config_file" "$output_dir"
