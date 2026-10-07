PYTHON ?= python3
export PYTHONDONTWRITEBYTECODE := 1

.PHONY: check test contract-check contract-test

# check: Validate contract correctness, compile, format, and lint.
# F01 establishes contract-check and contract-test. F02 adds cargo targets and lint.
# F05 later adds native targets and documentation.
check: contract-check cargo-check cargo-fmt-check cargo-clippy

# test: Run contract validation tests and cargo test suite.
test: contract-test cargo-test

.PHONY: cargo-check cargo-test cargo-build cargo-fmt-check cargo-clippy

# cargo-check: Verify compilation with pinned dependencies (Cargo.lock).
cargo-check:
	cargo check --all --all-targets --locked

# cargo-test: Run test suite with locked dependencies; fails on real test failures.
cargo-test:
	cargo test --all --locked

# cargo-build: Release build with LTO and optimization, reproducible against Cargo.lock.
cargo-build:
	cargo build --release --locked

# cargo-fmt-check: Enforce consistent code formatting (no in-place changes).
cargo-fmt-check:
	cargo fmt --all -- --check

# cargo-clippy: Run lint pass with all warnings-as-errors on all targets.
cargo-clippy:
	cargo clippy --all-targets --locked -- -D warnings

contract-check:
	$(PYTHON) tools/contracts/check_contracts.py

contract-test:
	$(PYTHON) -m unittest discover --start-directory tools/contracts --pattern 'test_*.py' --verbose
