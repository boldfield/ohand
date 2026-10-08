#!/bin/bash
# Generate reproducible C bindings from Rust FFI source.
# Outputs to build artifacts; generated files are not committed.
# Usage: tools/bindings/generate.sh <output_dir>

set -euo pipefail

output_dir="${1:-.}"
caller_cwd="$(pwd)"

# Build and run the binding generator from repo root
repo_root="$(git rev-parse --show-toplevel)"

# Resolve output_dir to absolute path before changing directories
if [[ ! "$output_dir" = /* ]]; then
    output_dir="${caller_cwd}/${output_dir}"
fi

cd "${repo_root}"

config_file="tools/bindings/cbindgen.toml"
if [ ! -f "$config_file" ]; then
    echo "ERROR: cbindgen config not found: $config_file" >&2
    exit 1
fi

cargo run -q --manifest-path tools/bindings/Cargo.toml -- "$config_file" "$output_dir"
