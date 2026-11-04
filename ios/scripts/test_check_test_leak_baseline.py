import tempfile
import unittest
from pathlib import Path

import check_test_leak_baseline as checker


class CheckTestLeakBaselineTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_tree_using_the_helper_passes(self):
        self.write("Services/ExampleTests.swift", "coreLeakBaseline = CoreLeakBaseline.capture()\ncoreLeakBaseline.check()\n")
        self.assertEqual(checker.violations(self.root), [])
        self.assertEqual(checker.main(["check", str(self.root)]), 0)

    def test_property_assignment_is_a_violation(self):
        self.write("Services/ExampleTests.swift", "        baselineBuffers = CoreHandle.liveResultBufferCount\n")
        self.assertEqual(
            checker.violations(self.root),
            [("Services/ExampleTests.swift", 1, "assigns a live count to a baseline")])
        self.assertEqual(checker.main(["check", str(self.root)]), 1)

    def test_local_assignment_is_a_violation(self):
        self.write("Services/ExampleTests.swift", "let baselineHandles = CoreHandle.liveHandleCount\n")
        self.assertEqual(len(checker.violations(self.root)), 1)

    def test_assertion_is_a_violation(self):
        self.write(
            "Services/ExampleTests.swift",
            'XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")\n')
        self.assertEqual(
            checker.violations(self.root),
            [("Services/ExampleTests.swift", 1, "asserts on a live count directly")])

    def test_each_assertion_variant_is_a_violation(self):
        self.write(
            "Services/ExampleTests.swift",
            "XCTAssertTrue(CoreHandle.liveResultBufferCount == 0)\nXCTAssertNotEqual(CoreHandle.liveHandleCount, 3)\n")
        self.assertEqual([number for _, number, _ in checker.violations(self.root)], [1, 2])

    def test_comparison_in_a_wait_condition_is_not_a_violation(self):
        self.write(
            "Services/ExampleTests.swift",
            "while CoreHandle.liveResultBufferCount != 0, Date() < deadline {}\n"
            "_ = pump(until: { CoreHandle.liveResultBufferCount == baseline.buffers })\n")
        self.assertEqual(checker.violations(self.root), [])

    def test_support_directory_is_exempt(self):
        self.write(
            "Support/CoreLeakBaseline.swift",
            "let handles = CoreHandle.liveHandleCount\nXCTAssertEqual(CoreHandle.liveResultBufferCount, buffers)\n")
        self.assertEqual(checker.violations(self.root), [])

    def test_counter_tests_are_exempt(self):
        self.write(
            "CoreBridge/CoreHandleTests.swift",
            "XCTAssertEqual(CoreHandle.liveHandleCount, coreLeakBaseline.handles + 1)\n")
        self.assertEqual(checker.violations(self.root), [])

    def test_other_files_in_the_counter_directory_are_not_exempt(self):
        self.write("CoreBridge/OtherTests.swift", "XCTAssertEqual(CoreHandle.liveHandleCount, 0)\n")
        self.assertEqual(len(checker.violations(self.root)), 1)

    def test_commented_mention_is_not_a_violation(self):
        self.write("Services/ExampleTests.swift", "// x = CoreHandle.liveHandleCount\n")
        self.assertEqual(checker.violations(self.root), [])

    def test_missing_directory_is_an_error(self):
        self.assertEqual(checker.main(["check", str(self.root / "absent")]), 2)

    def test_repository_tests_pass(self):
        self.assertEqual(checker.violations(checker.DEFAULT_TESTS), [])


if __name__ == "__main__":
    unittest.main()
