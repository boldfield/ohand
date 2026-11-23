"""accessibility-groups.txt partitions the test methods of the OhAndAccessibilityUITests scheme between the two accessibility
jobs: every test is in exactly one group and no group names a test that does not exist, so a test added without a group fails
`make check` instead of going unaudited."""

import re
import unittest
from collections import Counter
from pathlib import Path

import yaml

SCRIPTS = Path(__file__).resolve().parent
IOS = SCRIPTS.parent
GROUPS_FILE = SCRIPTS / "accessibility-groups.txt"
PROJECT = IOS / "project.yml"
TARGET = "OhAndAccessibilityUITests"
CLASS_PATTERN = re.compile(r"^\s*(?:final\s+)?class\s+(\w+)\s*:\s*XCTestCase")
METHOD_PATTERN = re.compile(r"^\s*func\s+(test\w*)\s*\(\s*\)")


def scheme_tests():
    """Every `<target>/<class>/<method>` in the Swift sources of the scheme's test target (the paths in project.yml)."""
    project = yaml.safe_load(PROJECT.read_text())
    source_paths = [source["path"] if isinstance(source, dict) else source for source in project["targets"][TARGET]["sources"]]
    tests = set()
    for source_path in source_paths:
        for swift_file in sorted((IOS / source_path).rglob("*.swift")):
            current_class = None
            for line in swift_file.read_text().splitlines():
                class_match = CLASS_PATTERN.match(line)
                if class_match:
                    current_class = class_match.group(1)
                    continue
                method_match = METHOD_PATTERN.match(line)
                if method_match and current_class:
                    tests.add(f"{TARGET}/{current_class}/{method_match.group(1)}")
    return tests


def grouped_tests():
    """(group, test) for every entry of the groups file."""
    entries = []
    for line in GROUPS_FILE.read_text().splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        group, test = line.split()
        entries.append((group, test))
    return entries


class AccessibilityGroupsTests(unittest.TestCase):
    def test_the_scheme_has_tests(self):
        self.assertGreater(len(scheme_tests()), 30)

    def test_every_test_is_in_exactly_one_group(self):
        counts = Counter(test for _, test in grouped_tests())
        missing = sorted(scheme_tests() - set(counts))
        repeated = sorted(test for test, count in counts.items() if count > 1)
        self.assertEqual(missing, [], "tests with no group in accessibility-groups.txt")
        self.assertEqual(repeated, [], "tests in more than one group")

    def test_no_group_names_a_test_that_does_not_exist(self):
        unknown = sorted(set(test for _, test in grouped_tests()) - scheme_tests())
        self.assertEqual(unknown, [])

    def test_there_are_two_groups_and_both_have_tests(self):
        self.assertEqual({group for group, _ in grouped_tests()}, {"a", "b"})

    def test_the_workflow_matrix_runs_exactly_the_groups_in_the_file(self):
        workflow = yaml.safe_load((IOS.parent / ".github" / "workflows" / "ios.yml").read_text())
        matrix_groups = workflow["jobs"]["accessibility"]["strategy"]["matrix"]["group"]
        self.assertEqual(sorted(matrix_groups), sorted({group for group, _ in grouped_tests()}))


if __name__ == "__main__":
    unittest.main()
