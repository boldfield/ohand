#!/usr/bin/env python3
"""Classifies a failed simulator test step as `infrastructure` or `test-failure`.

Reads the step's log and prints the verdict on the first line, then one `matched: <line>` line per signature match.
`infrastructure` means no product code was judged: the test runner could not launch the app or could not find a booted
device. It is answered only when the log has at least one match from the closed list below and nothing else in the log
failed. Anything else, including an empty or missing log, is `test-failure`, so a step is never retried because of a failed
assertion, an accessibility audit finding or a compile error.

Usage: classify_simulator_fault.py <log>
"""
import re
import sys

# The closed list. Add an entry only with the run it was observed in; the pattern is taken from that run's log.
INFRASTRUCTURE_SIGNATURES = [
    # Run 44148283529 attempt 2 (CaptureProbe handoff UI test): the runner never got the app to launch.
    ("failed-to-launch", re.compile(r"Failed to launch <XCUIApplicationImpl: .*Timed out while launching application via Xcode")),
    # Run 44145594127 attempt 2 (Tauri handoff UI tests): the simulator was not booted when xcodebuild looked for it.
    ("no-matching-device", re.compile(r"xcodebuild: error: Unable to find a device matching the provided destination specifier")),
    # Run 44145594127 attempt 1 (Tauri round-trip step): simctl could not reach the device's display.
    ("no-default-display-port", re.compile(r"Device does not have a 'default' display port")),
]

# An XCTest failure line: `<file>:<line>: error: -[Class test] : <message>`. Its message must match a signature.
TEST_FAILURE_LINE = re.compile(r"error: -\[[^\]]*\] : (?P<message>.*)")
# Failures that are never infrastructure, wherever they appear.
UNLISTED_FAILURE_PATTERNS = [
    re.compile(r"\.swift:\d+(:\d+)?: error: (?!-\[)"),  # compile error
    re.compile(r"XCTAssert\w*"),
    re.compile(r"app never left the foreground"),
    re.compile(r"\*\* BUILD FAILED \*\*"),
    re.compile(r"^(xcodebuild: )?error: "),
]
FAILED_TEST_CASE = re.compile(r"^Test Case '.*' failed \(")


def matched_signature(line):
    return any(pattern.search(line) for _, pattern in INFRASTRUCTURE_SIGNATURES)


def classify(log_text):
    """Returns (verdict, matched lines)."""
    matched_lines = []
    failed_test_cases = 0
    signature_failed_tests = 0
    unlisted_failure = False
    for line in log_text.splitlines():
        stripped = line.strip()
        is_signature = matched_signature(stripped)
        if is_signature:
            matched_lines.append(stripped)
        test_failure = TEST_FAILURE_LINE.search(stripped)
        if test_failure:
            if matched_signature(test_failure.group("message")):
                signature_failed_tests += 1
            else:
                unlisted_failure = True
            continue
        if FAILED_TEST_CASE.match(stripped):
            failed_test_cases += 1
            continue
        if not is_signature and any(pattern.search(stripped) for pattern in UNLISTED_FAILURE_PATTERNS):
            unlisted_failure = True
    # A failed test case with no failure line of a listed kind failed for a reason the list does not cover.
    if failed_test_cases > signature_failed_tests:
        unlisted_failure = True
    if matched_lines and not unlisted_failure:
        return "infrastructure", matched_lines
    return "test-failure", matched_lines


def main(argv):
    if len(argv) != 2:
        print("usage: classify_simulator_fault.py <log>", file=sys.stderr)
        return 2
    try:
        with open(argv[1], encoding="utf-8", errors="replace") as log_file:
            log_text = log_file.read()
    except OSError:
        log_text = ""
    verdict, matched_lines = classify(log_text)
    print(verdict)
    for line in matched_lines:
        print(f"matched: {line[:300]}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
