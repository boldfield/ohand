PYTHON ?= python3
export PYTHONDONTWRITEBYTECODE := 1

.PHONY: check test contract-check contract-test

# F02 and F05 add their real build, lint and test commands as further prerequisites.
check: contract-check cargo-check

test: contract-test cargo-test

.PHONY: cargo-check cargo-test cargo-build

cargo-check:
	cargo check --all --all-targets

cargo-test:
	cargo test --all

cargo-build:
	cargo build --release

contract-check:
	$(PYTHON) tools/contracts/check_contracts.py

contract-test:
	$(PYTHON) -m unittest discover --start-directory tools/contracts --pattern 'test_*.py' --verbose
