PYTHON ?= python3
export PYTHONDONTWRITEBYTECODE := 1

.PHONY: check test contract-check contract-test ios-check hygiene-check

# check: Validate contract correctness, compile, format, and lint.
# F01 establishes contract-check and contract-test. F02 adds cargo targets and lint.
# F05 adds native targets and documentation. F06 adds hygiene checks.
check: contract-check cargo-check cargo-fmt-check cargo-clippy ios-check hygiene-check

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

# ios-check: Validate iOS project configuration (any host with PyYAML).
# Runs static checks on project.yml structure, bundle identifiers, signing config, and plists.
ios-check:
	$(PYTHON) ios/scripts/check_project_config.py
	$(PYTHON) -m unittest discover --start-directory ios/scripts --pattern 'test_*.py' --verbose

# hygiene-check: Check for secret leaks and private-capture policy violations.
# Ensures no credentials, keys, or private recordings are committed to the public repository.
hygiene-check:
	$(PYTHON) tools/hygiene/check_hygiene.py --scan-mode tracked
	$(PYTHON) -m unittest discover --start-directory tools/hygiene --pattern 'test_*.py' --verbose
