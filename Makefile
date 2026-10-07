PYTHON ?= python3
export PYTHONDONTWRITEBYTECODE := 1

.PHONY: check test contract-check contract-test

# F02 and F05 add their real build, lint and test commands as further prerequisites.
check: contract-check

test: contract-test

contract-check:
	$(PYTHON) tools/contracts/check_contracts.py

contract-test:
	$(PYTHON) -m unittest discover --start-directory tools/contracts --pattern 'test_*.py' --verbose
