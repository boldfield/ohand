#!/usr/bin/env python3
"""Tests for classify_simulator_fault.py with synthetic log excerpts shaped like the cited runs' logs."""
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "classify_simulator_fault.py")

FAILED_TO_LAUNCH = (
    "Test Case '-[ProbeUITests.HandoffUITests testHandoff]' started.\n"
    "/work/ios/Probe/UITests/HandoffUITests.swift:22: error: -[ProbeUITests.HandoffUITests testHandoff] : "
    "Failed to launch <XCUIApplicationImpl: 0x1000 com.example.probe at /work/Probe.app> via Xcode: "
    "Timed out while launching application via Xcode.\n"
    "Test Case '-[ProbeUITests.HandoffUITests testHandoff]' failed (127.7 seconds).\n"
    "** TEST EXECUTE FAILED **\n"
)
NO_DEVICE = (
    "xcodebuild: error: Unable to find a device matching the provided destination specifier:\n"
    "\t\t{ platform:iOS Simulator, id:00000000-0000-0000-0000-000000000000 }\n"
    "\n"
    "\tThe requested device could not be found because no available devices matched the request.\n"
    "\n"
    "\tDestinations compatible with the \"ProbeUITests\" scheme:\n"
    "\t\t{ platform:iOS Simulator, id:dvtdevice-DVTiOSDeviceSimulatorPlaceholder-iphonesimulator:placeholder, name:Any iOS Simulator Device }\n"
)
NO_DISPLAY_PORT = (
    "Detected file type from extension: PNG\n"
    "Error: Device does not have a 'default' display port\n"
    "An error was encountered processing the command (domain=NSPOSIXErrorDomain, code=22):\n"
    "Device does not have a 'default' display port\n"
)
FOREGROUND = (
    "/work/ios/Probe/UITests/CaptureUITests.swift:40: error: -[ProbeUITests.CaptureUITests testCapture] : "
    "XCTAssertTrue failed - app never left the foreground\n"
    "Test Case '-[ProbeUITests.CaptureUITests testCapture]' failed (12.0 seconds).\n"
)
ASSERT_EQUAL = (
    "/work/ios/Tests/FooTests.swift:10: error: -[Tests.FooTests testFoo] : XCTAssertEqual failed: (\"1\") is not equal to (\"2\")\n"
    "Test Case '-[Tests.FooTests testFoo]' failed (0.1 seconds).\n"
)
HOME_POPULATED = (
    "/work/ios/Tests/HomeUITests.swift:55: error: -[Tests.HomeUITests testHomePopulated] : "
    "excused under or on glass\n"
    "Test Case '-[Tests.HomeUITests testHomePopulated]' failed (30.0 seconds).\n"
)
COMPILE_ERROR = (
    "/work/ios/App/Thing.swift:12:5: error: cannot find 'missing' in scope\n"
    "** BUILD FAILED **\n"
)
AUDIT_FINDING = (
    "ISSUE home Contrast failed\n"
    "/work/ios/Tests/AuditUITests.swift:90: error: -[Tests.AuditUITests testAudit] : audit did not run (code -56)\n"
    "Test Case '-[Tests.AuditUITests testAudit]' failed (3.0 seconds).\n"
)


def classify(log_text, udid=None):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "step.log")
        with open(path, "w", encoding="utf-8") as log_file:
            log_file.write(log_text)
        return run(path, udid)


def run(path, udid=None):
    result = subprocess.run([sys.executable, SCRIPT, path] + ([udid] if udid else []), capture_output=True, text=True, check=True)
    return result.stdout.splitlines()


class ClassifySimulatorFaultTests(unittest.TestCase):
    def test_each_listed_signature_alone_is_infrastructure(self):
        for name, log_text in [("launch", FAILED_TO_LAUNCH), ("device", NO_DEVICE), ("display port", NO_DISPLAY_PORT)]:
            with self.subTest(name):
                lines = classify(log_text)
                self.assertEqual(lines[0], "infrastructure")
                self.assertTrue(any(line.startswith("matched: ") for line in lines[1:]))

    def test_unlisted_failures_are_test_failures(self):
        for name, log_text in [
            ("app never left the foreground", FOREGROUND),
            ("XCTAssertEqual", ASSERT_EQUAL),
            ("testHomePopulated", HOME_POPULATED),
            ("compile error", COMPILE_ERROR),
            ("accessibility audit", AUDIT_FINDING),
        ]:
            with self.subTest(name):
                self.assertEqual(classify(log_text)[0], "test-failure")

    def test_listed_signature_beside_an_unlisted_failure_is_a_test_failure(self):
        for unlisted in (ASSERT_EQUAL, HOME_POPULATED, COMPILE_ERROR):
            with self.subTest(unlisted.splitlines()[0][:40]):
                self.assertEqual(classify(FAILED_TO_LAUNCH + unlisted)[0], "test-failure")
                self.assertEqual(classify(NO_DEVICE + unlisted)[0], "test-failure")

    def test_no_device_header_for_a_named_destination_is_a_test_failure(self):
        header_only = (
            "xcodebuild: error: Unable to find a device matching the provided destination specifier:\n"
            "\t\t{ platform:iOS Simulator, name:No Such Phone }\n"
        )
        self.assertEqual(classify(header_only)[0], "test-failure")
        named = header_only + "\n\tThe requested device could not be found because no available devices matched the request.\n"
        self.assertEqual(classify(named)[0], "test-failure")

    def test_no_device_by_id_is_bound_to_the_prepared_udid(self):
        self.assertEqual(classify(NO_DEVICE, "00000000-0000-0000-0000-000000000000")[0], "infrastructure")
        self.assertEqual(classify(NO_DEVICE, "11111111-1111-1111-1111-111111111111")[0], "test-failure")

    def test_listed_signature_beside_a_non_swift_compile_error_is_a_test_failure(self):
        for compile_error in (
            "foo.c:7:2: error: use of undeclared identifier 'bar'\n",
            "fatal error: module map file not found\n",
        ):
            with self.subTest(compile_error):
                self.assertEqual(classify(NO_DEVICE + compile_error)[0], "test-failure")
                self.assertEqual(classify(FAILED_TO_LAUNCH + compile_error)[0], "test-failure")

    def test_failed_test_case_without_a_listed_message_is_a_test_failure(self):
        log_text = NO_DEVICE + "Test Case '-[Tests.FooTests testFoo]' failed (0.1 seconds).\n"
        self.assertEqual(classify(log_text)[0], "test-failure")

    def test_empty_or_missing_log_is_a_test_failure(self):
        self.assertEqual(classify("")[0], "test-failure")
        self.assertEqual(run("/nonexistent/step.log")[0], "test-failure")

    def test_log_without_a_signature_is_a_test_failure(self):
        self.assertEqual(classify("something went wrong\n")[0], "test-failure")


if __name__ == "__main__":
    unittest.main()
