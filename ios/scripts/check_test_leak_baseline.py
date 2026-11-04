#!/usr/bin/env python3
"""Fail when a test file reads the core's live counts as a baseline or asserts on them directly.

`CoreHandle.liveHandleCount` and `CoreHandle.liveResultBufferCount` are process-wide. A test that snapshots them in
`setUp` and compares in `tearDown` races a core read that frees its buffer a moment after the test body ends, so every
test class captures and checks through `CoreLeakBaseline` (`ios/Tests/Support/CoreLeakBaseline.swift`), which waits,
bounded, on both sides. Outside `ios/Tests/Support/`, a line that names either count may not assign it (to a property
or a local) or pass it to an `XCTAssert`. `CoreBridge/CoreHandleTests.swift` tests the counters themselves and is exempt.
"""
import re
import sys
from pathlib import Path

DEFAULT_TESTS = Path(__file__).resolve().parents[1] / "Tests"
SUPPORT_DIRECTORY = "Support"
EXEMPT_FILES = ("CoreBridge/CoreHandleTests.swift",)

COUNT = r"CoreHandle\.live(?:ResultBuffer|Handle)Count"
NAMES_COUNT = re.compile(COUNT)
ASSIGNS_COUNT = re.compile(r"(?<![=!<>])=\s*(?:await\s+)?" + COUNT)
ASSERTS_COUNT = re.compile(r"\bXCTAssert\w*\s*\(.*" + COUNT)


def violations(tests_root):
    """Returns (relative path, line number, reason) for every offending line under `tests_root`."""
    found = []
    for path in sorted(Path(tests_root).rglob("*.swift")):
        relative = path.relative_to(tests_root).as_posix()
        if relative.split("/")[0] == SUPPORT_DIRECTORY or relative in EXEMPT_FILES:
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            code = line.split("//", 1)[0]
            if not NAMES_COUNT.search(code):
                continue
            if ASSERTS_COUNT.search(code):
                found.append((relative, number, "asserts on a live count directly"))
            elif ASSIGNS_COUNT.search(code):
                found.append((relative, number, "assigns a live count to a baseline"))
    return found


def main(argv):
    tests_root = Path(argv[1]) if len(argv) > 1 else DEFAULT_TESTS
    if not tests_root.is_dir():
        print(f"check_test_leak_baseline: {tests_root} is not a directory", file=sys.stderr)
        return 2
    found = violations(tests_root)
    for relative, number, reason in found:
        print(f"{relative}:{number}: {reason}; use CoreLeakBaseline (ios/Tests/Support/CoreLeakBaseline.swift)", file=sys.stderr)
    if found:
        return 1
    print("check_test_leak_baseline: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
