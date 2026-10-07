"""Unit tests for check_contracts using synthetic contracts and manifests."""

import json
import unittest

import check_contracts

SYNTHETIC_CONTRACT = """# Synthetic contract

| Module | Responsibility | Task(s) |
| --- | --- | --- |
| `core/` | Workspace root | A01 |
| `Makefile` | Commands | A01, B01 |
| `docs/reference.md` | Reference | (reference) |

## Shared File Ordering

- Makefile: A01, then B01.

## Next Steps

- None.
"""


def synthetic_manifest():
    return {
        "tasks": [
            {"key": "A01", "depends_on": [], "file_scope": ["core/", "Makefile"]},
            {"key": "B01", "depends_on": ["A01"], "file_scope": ["Makefile"]},
        ]
    }


class ValidContractTests(unittest.TestCase):
    def test_reconciled_contract_passes(self):
        self.assertEqual(check_contracts.validate(SYNTHETIC_CONTRACT, synthetic_manifest()), [])

    def test_reference_rows_are_not_ownership(self):
        ownership = check_contracts.parse_ownership_map(SYNTHETIC_CONTRACT)
        self.assertNotIn("docs/reference.md", ownership)
        self.assertEqual(ownership["Makefile"], {"A01", "B01"})


class DriftDetectionTests(unittest.TestCase):
    def test_missing_manifest_path_fails(self):
        manifest = synthetic_manifest()
        manifest["tasks"][1]["file_scope"].append("ios/")
        problems = check_contracts.validate(SYNTHETIC_CONTRACT, manifest)
        self.assertIn("ownership map is missing B01 for ios/", problems)

    def test_owner_not_in_manifest_fails(self):
        contract = SYNTHETIC_CONTRACT.replace("| `core/` | Workspace root | A01 |", "| `core/` | Workspace root | A01, B01 |")
        problems = check_contracts.validate(contract, synthetic_manifest())
        self.assertIn("ownership map assigns core/ to B01, which the manifest does not", problems)

    def test_f01_reservation_is_limited_to_declared_paths(self):
        contract = SYNTHETIC_CONTRACT.replace("| `core/` | Workspace root | A01 |", "| `core/` | Workspace root | A01, F01 |")
        problems = check_contracts.validate(contract, synthetic_manifest())
        self.assertIn("ownership map assigns core/ to F01, which the manifest does not", problems)

    def test_unserialized_shared_editors_fail(self):
        manifest = synthetic_manifest()
        manifest["tasks"][1]["depends_on"] = []
        problems = check_contracts.validate(SYNTHETIC_CONTRACT, manifest)
        self.assertTrue(any("not serialized" in problem for problem in problems), problems)

    def test_shared_path_without_ordering_entry_fails(self):
        contract = SYNTHETIC_CONTRACT.replace("- Makefile: A01, then B01.", "- Nothing shared.")
        problems = check_contracts.validate(contract, synthetic_manifest())
        self.assertTrue(any("no Shared File Ordering entry" in problem for problem in problems), problems)

    def test_dependency_cycle_fails(self):
        manifest = synthetic_manifest()
        manifest["tasks"][0]["depends_on"] = ["B01"]
        problems = check_contracts.validate(SYNTHETIC_CONTRACT, manifest)
        self.assertTrue(any(problem.startswith("dependency cycle") for problem in problems), problems)

    def test_unknown_dependency_fails(self):
        manifest = synthetic_manifest()
        manifest["tasks"][1]["depends_on"].append("Z99")
        problems = check_contracts.validate(SYNTHETIC_CONTRACT, manifest)
        self.assertIn("B01 depends on unknown task Z99", problems)

    def test_fenced_code_fails(self):
        contract = SYNTHETIC_CONTRACT + "\n```rust\ntrait Example {}\n```\n"
        problems = check_contracts.validate(contract, synthetic_manifest())
        self.assertTrue(any("fenced code block" in problem for problem in problems), problems)


class RepositoryContractTests(unittest.TestCase):
    def test_repository_contract_reconciles_with_manifest(self):
        manifest = json.loads(check_contracts.DEFAULT_MANIFEST_PATH.read_text(encoding="utf-8"))
        contract_text = check_contracts.DEFAULT_CONTRACT_PATH.read_text(encoding="utf-8")
        self.assertEqual(check_contracts.validate(contract_text, manifest), [])


if __name__ == "__main__":
    unittest.main()
