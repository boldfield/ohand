import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import select_simulator as selector

PREFIX = selector.RUNTIME_PREFIX


def device(name, udid, available=True):
    return {"name": name, "udid": udid, "isAvailable": available, "state": "Shutdown"}


def simctl(**runtimes):
    return {"devices": {PREFIX + key.replace("_", "-"): value for key, value in runtimes.items()}}


class SelectSimulatorTests(unittest.TestCase):
    def test_wildcard_free_lookup_picks_concrete_runtime_key(self):
        data = simctl(**{"18_5": [device("iPhone 16", "AAAA")]})
        selected = selector.select_simulator(data)
        self.assertEqual(selected["udid"], "AAAA")
        self.assertEqual(selected["name"], "iPhone 16")
        self.assertEqual(selected["runtime"], "18.5")
        self.assertEqual(selected["runtime_key"], PREFIX + "18-5")

    def test_version_ordering_is_numeric_not_lexicographic(self):
        data = simctl(**{
            "9_3": [device("iPhone 6", "OLD")],
            "18_5": [device("iPhone 16", "NEW")],
            "18_10": [device("iPhone 17", "NEWEST")],
        })
        self.assertEqual(selector.select_simulator(data)["udid"], "NEWEST")

    def test_max_runtime_ignores_newer_runtimes(self):
        data = simctl(**{
            "18_5": [device("iPhone 16", "SDK")],
            "26_0": [device("iPhone 17", "TOONEW")],
        })
        selected = selector.select_simulator(data, "18.5")
        self.assertEqual(selected["udid"], "SDK")
        self.assertEqual(selected["runtime"], "18.5")

    def test_unavailable_and_non_iphone_devices_are_skipped(self):
        data = simctl(**{"18_5": [
            device("iPhone 15", "DEAD", available=False),
            device("iPad Pro", "IPAD"),
            device("iPhone 16e", "OK"),
        ]})
        self.assertEqual(selector.select_simulator(data)["udid"], "OK")

    def test_non_ios_runtimes_are_ignored(self):
        data = {"devices": {
            "com.apple.CoreSimulator.SimRuntime.watchOS-11-5": [device("iPhone Watch", "W")],
            PREFIX + "18-5": [device("iPhone 16", "I")],
        }}
        self.assertEqual(selector.select_simulator(data)["udid"], "I")

    def test_selection_is_deterministic_within_runtime(self):
        data = simctl(**{"18_5": [device("iPhone 16e", "B"), device("iPhone 16", "A")]})
        self.assertEqual(selector.select_simulator(data)["name"], "iPhone 16")

    def test_no_candidates_is_an_error(self):
        with self.assertRaises(selector.SelectionError):
            selector.select_simulator({"devices": {}})
        with self.assertRaises(selector.SelectionError):
            selector.select_simulator(simctl(**{"26_0": [device("iPhone 17", "X")]}), "18.5")

    def test_command_line_reads_stdin_and_prints_output_lines(self):
        data = simctl(**{"18_5": [device("iPhone 16", "AAAA")]})
        script = Path(selector.__file__)
        result = subprocess.run(
            [sys.executable, str(script), "--max-runtime", "18.5"],
            input=json.dumps(data), capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = dict(line.split("=", 1) for line in result.stdout.splitlines())
        self.assertEqual(lines["udid"], "AAAA")
        self.assertEqual(lines["name"], "iPhone 16")
        self.assertEqual(lines["runtime"], "18.5")

    def test_main_reports_failure_with_nonzero_status(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump({"devices": {}}, handle)
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = selector.main(["--input", handle.name])
        Path(handle.name).unlink()
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("No available iPhone simulator", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
