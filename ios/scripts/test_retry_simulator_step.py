#!/usr/bin/env python3
"""Tests for retry-simulator-step.sh with stub commands and a stub prepare-simulator.sh."""
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
LAUNCH_FAILURE = (
    "/work/UITests.swift:22: error: -[ProbeUITests.HandoffUITests testHandoff] : Failed to launch "
    "<XCUIApplicationImpl: 0x1000 com.example.probe at /work/Probe.app> via Xcode: Timed out while launching application via Xcode.\n"
    "Test Case '-[ProbeUITests.HandoffUITests testHandoff]' failed (1.0 seconds).\n"
)
ASSERTION_FAILURE = (
    "/work/UITests.swift:30: error: -[ProbeUITests.HandoffUITests testHandoff] : XCTAssertEqual failed: (\"1\") is not equal to (\"2\")\n"
    "Test Case '-[ProbeUITests.HandoffUITests testHandoff]' failed (1.0 seconds).\n"
)


class RetrySimulatorStepTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.directory)
        self.scripts = os.path.join(self.directory, "scripts")
        os.mkdir(self.scripts)
        for name in ("retry-simulator-step.sh", "classify_simulator_fault.py"):
            shutil.copy(os.path.join(SCRIPTS, name), self.scripts)
        self.prepare_calls = os.path.join(self.directory, "prepare-calls.txt")
        self.write_executable("prepare-simulator.sh", f'#!/usr/bin/env bash\necho "$@" >> "{self.prepare_calls}"\nexit "${{STUB_PREPARE_STATUS:-0}}"\n')
        self.runs = os.path.join(self.directory, "runs.txt")
        self.log = os.path.join(self.directory, "evidence", "step.log")

    def write_executable(self, name, text):
        path = os.path.join(self.scripts, name)
        with open(path, "w", encoding="utf-8") as script:
            script.write(text)
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
        return path

    def stub_command(self, outputs_and_statuses):
        """A command whose nth run prints the nth output and exits with the nth status; records each run's suffix."""
        cases = "".join(
            f"  {index}) printf '%s' {self.quote(output)}; exit {status} ;;\n"
            for index, (output, status) in enumerate(outputs_and_statuses, start=1)
        )
        return self.write_executable(
            "stub-command.sh",
            f'#!/usr/bin/env bash\ntouch "{self.runs}"; count=$(( $(wc -l < "{self.runs}") + 1 ))\n'
            f'echo "run ${{count}} suffix=[${{OHAND_ATTEMPT_SUFFIX}}]" >> "{self.runs}"\n'
            f'case "${{count}}" in\n{cases}esac\n',
        )

    @staticmethod
    def quote(text):
        return "'" + text.replace("'", "'\\''") + "'"

    def retry(self, command, **env):
        environment = dict(os.environ, OHAND_PYTHON=sys.executable, **env)
        environment.pop("OHAND_HOST_PRESSURE_FILE", None)
        return subprocess.run(
            [os.path.join(self.scripts, "retry-simulator-step.sh"), "UDID-1", self.log, "--", command],
            capture_output=True, text=True, env=environment,
        )

    def read(self, path):
        with open(path, encoding="utf-8") as file:
            return file.read()

    def run_count(self):
        return len(self.read(self.runs).splitlines()) if os.path.exists(self.runs) else 0

    def recoveries(self):
        path = os.path.join(os.path.dirname(self.log), "simulator-recoveries.txt")
        return self.read(path).splitlines() if os.path.exists(path) else []

    def test_success_runs_once(self):
        result = self.retry(self.stub_command([("ok\n", 0)]))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.run_count(), 1)
        self.assertFalse(os.path.exists(self.prepare_calls))
        self.assertEqual(self.recoveries(), [])

    def test_infrastructure_failure_then_success_exits_zero_after_two_runs(self):
        result = self.retry(self.stub_command([(LAUNCH_FAILURE, 65), ("ok\n", 0)]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.read(self.runs).splitlines(), ["run 1 suffix=[]", "run 2 suffix=[-attempt-2]"])
        self.assertIn("::warning::", result.stdout)
        self.assertEqual(self.read(self.log + ".attempt-1"), LAUNCH_FAILURE)
        self.assertEqual(self.read(self.log), "ok\n")
        self.assertEqual(self.read(self.prepare_calls).split()[0], "UDID-1")
        recoveries = self.recoveries()
        self.assertEqual(len(recoveries), 1)
        fields = recoveries[0].split()
        self.assertEqual((fields[0], fields[1], fields[4]), ("step.log", "test-launch", "recovered"))

    def test_infrastructure_failure_twice_exits_non_zero_after_two_runs(self):
        result = self.retry(self.stub_command([(LAUNCH_FAILURE, 65), (LAUNCH_FAILURE, 66)]))
        self.assertEqual(result.returncode, 66)
        self.assertEqual(self.run_count(), 2)
        self.assertEqual(self.recoveries()[0].split()[4], "failed-twice")

    def test_attempt_suffix_token_is_replaced_per_attempt(self):
        command = self.stub_command([(LAUNCH_FAILURE, 65), ("ok\n", 0)])
        environment = dict(os.environ, OHAND_PYTHON=sys.executable)
        environment.pop("OHAND_HOST_PRESSURE_FILE", None)
        result = subprocess.run(
            [os.path.join(self.scripts, "retry-simulator-step.sh"), "UDID-1", self.log, "--", "bash", "-c",
             'echo "bundle=x%ATTEMPT_SUFFIX%.xcresult" >&2; exec "$0"', command],
            capture_output=True, text=True, env=environment,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("bundle=x.xcresult", result.stdout)
        self.assertIn("bundle=x-attempt-2.xcresult", result.stdout)

    def test_after_reprepare_hook_runs_between_the_attempts(self):
        marker = os.path.join(self.directory, "hook.txt")
        result = self.retry(
            self.stub_command([(LAUNCH_FAILURE, 65), ("ok\n", 0)]),
            OHAND_AFTER_REPREPARE=f'echo hook-ran > "{marker}"',
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.read(marker), "hook-ran\n")

    def test_test_failure_exits_with_the_original_status_after_one_run(self):
        result = self.retry(self.stub_command([(ASSERTION_FAILURE, 65), ("ok\n", 0)]))
        self.assertEqual(result.returncode, 65)
        self.assertEqual(self.run_count(), 1)
        self.assertFalse(os.path.exists(self.prepare_calls))
        self.assertFalse(os.path.exists(self.log + ".attempt-1"))
        self.assertEqual(self.recoveries(), [])

    def test_failed_preparation_is_final_and_does_not_rerun(self):
        result = self.retry(self.stub_command([(LAUNCH_FAILURE, 65), ("ok\n", 0)]), STUB_PREPARE_STATUS="1")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.run_count(), 1)


if __name__ == "__main__":
    unittest.main()
