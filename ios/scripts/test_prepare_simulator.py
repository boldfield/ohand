import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

SCRIPTS = Path(__file__).resolve().parent
WORKFLOW = SCRIPTS.parents[1] / ".github" / "workflows" / "ios.yml"


class PrepareSimulatorTests(unittest.TestCase):
    def setUp(self):
        self.script = (SCRIPTS / "prepare-simulator.sh").read_text()
        self.workflow = WORKFLOW.read_text()

    def test_scripts_parse(self):
        for name in ("prepare-simulator.sh", "walkthrough-simulator.sh", "quiet-host.sh"):
            subprocess.run(["bash", "-n", str(SCRIPTS / name)], check=True)

    def test_readiness_thresholds_unchanged_and_timeout_bounded(self):
        self.assertIn("responsive_probe_limit_seconds=3", self.script)
        self.assertIn("responsive_probes_required=10", self.script)
        self.assertIn('responsive_timeout_seconds="${OHAND_TEST_RESPONSIVE_TIMEOUT_SECONDS:-480}"', self.script)
        self.assertIn('settle_timeout_seconds="${OHAND_TEST_SETTLE_TIMEOUT_SECONDS:-480}"', self.script)
        self.assertIn("settle_probes_required=\"${OHAND_TEST_SETTLE_PROBES:-30}\"", self.script)
        self.assertIn('probe_interval_seconds="${OHAND_TEST_PROBE_INTERVAL_SECONDS:-1}"', self.script)
        self.assertIn('recovery_quiet_wait_seconds="${OHAND_TEST_RECOVERY_QUIET_WAIT_SECONDS:-120}"', self.script)

    def test_pressure_file_records_swap_and_compressor(self):
        self.assertIn("swapins swapouts compressor_pages", self.script)

    def test_quiet_host_runs_first_in_both_simulator_jobs_and_spares_the_essentials(self):
        self.assertEqual(self.workflow.count("ios/scripts/quiet-host.sh"), 3)
        quiet = (SCRIPTS / "quiet-host.sh").read_text()
        for essential in ("CoreSimulator", "Xcode", "Runner.Listener", "provjobd"):
            self.assertNotIn(f"pkill -x {essential}", quiet)
        self.assertIn("mdutil -a -i off", quiet)

    def test_unknown_argument_is_rejected(self):
        result = subprocess.run(["bash", str(SCRIPTS / "prepare-simulator.sh"), "udid", "--bogus"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)

    def test_clone_requires_udid_file(self):
        result = subprocess.run(["bash", str(SCRIPTS / "prepare-simulator.sh"), "--clone", "template"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)

    def test_workflow_does_not_weaken_tests(self):
        # Only artifact uploads and their failure-diagnostics export (CI03) may be non-fatal; a test or build step must never be.
        # The export must run only after the job has failed and do nothing but call the exporter on an output
        # directory and the bundles that one fixed find invocation lists; no other command may hide in its arguments.
        plain_path = r"[\w./-]+"
        export_run = re.compile(
            r"(\.\./)*ios/scripts/export-failure-diagnostics\.sh"
            rf" {plain_path} \$\(find {plain_path} -name '\*\.xcresult' -prune\)"
        )

        def may_be_non_fatal(step):
            if str(step.get("uses", "")).startswith("actions/upload-artifact@"):
                return True
            condition = str(step.get("if", "")).replace("${{", "").replace("}}", "").strip()
            return condition == "failure()" and bool(export_run.fullmatch(str(step.get("run", "")).strip()))

        workflow = yaml.safe_load(self.workflow)
        for job in workflow["jobs"].values():
            for step in job["steps"]:
                if "continue-on-error" in step:
                    self.assertTrue(may_be_non_fatal(step), step.get("name"))
        exporter = "ios/scripts/export-failure-diagnostics.sh out $(find ios/.evidence -name '*.xcresult' -prune)"
        self.assertTrue(may_be_non_fatal({"if": "failure()", "continue-on-error": True, "run": exporter}))
        for weakened in (
            {"if": "always()", "run": exporter},
            {"run": exporter},
            {"if": "failure() || always()", "run": exporter},
            {"if": "failure()", "run": "xcodebuild test || true; " + exporter},
            {"if": "failure()", "run": exporter + " && false"},
            {"if": "failure()", "run": exporter + "\nxcodebuild test"},
            {"if": "failure()", "run": "ios/scripts/export-failure-diagnostics.sh out $(xcodebuild test)"},
            {"if": "failure()", "run": exporter + " $(xcodebuild test)"},
            {"if": "failure()", "run": "ios/scripts/export-failure-diagnostics.sh $(xcodebuild test) $(find ios/.evidence -name '*.xcresult' -prune)"},
            {"if": "failure()", "run": "ios/scripts/export-failure-diagnostics.sh out $(find ios/.evidence -name '*.xcresult' -prune -exec xcodebuild test \\;)"},
        ):
            self.assertFalse(may_be_non_fatal({"continue-on-error": True, **weakened}), weakened)
        self.assertNotIn("retry-tests-on-failure", self.workflow)
        for days in re.findall(r"retention-days:\s*(\d+)", self.workflow):
            self.assertLessEqual(int(days), 7)

    def test_notification_suites_use_clones_not_erases(self):
        self.assertIn('prepare-simulator.sh --clone "${TEMPLATE_UDID}"', self.workflow)
        self.assertNotIn("prepare-simulator.sh \"${SIMULATOR_UDID}\" --erase\n", self.workflow)


FAKE_XCRUN = r"""#!/usr/bin/env bash
# Fake xcrun: FAKE_DIR holds the call log; the first FAKE_UNRESPONSIVE_BOOTS boots never become responsive.
echo "$*" >> "${FAKE_DIR}/calls.log"
[ "$1" = "simctl" ] || exit 0
case "$2" in
  boot) echo boot >> "${FAKE_DIR}/boots.log" ;;
  clone)
    count=$(( $(cat "${FAKE_DIR}/clones.count" 2>/dev/null || echo 0) + 1 ))
    echo "${count}" > "${FAKE_DIR}/clones.count"
    echo "clone-${count}"
    ;;
  spawn)
    boots=$(wc -l < "${FAKE_DIR}/boots.log" 2>/dev/null | tr -d ' ')
    [ "${boots:-0}" -gt "${FAKE_UNRESPONSIVE_BOOTS:-0}" ] || exit 1
    # Boots up to FAKE_UNSETTLED_BOOTS answer the 10 responsive probes, then stop answering.
    spawns=$(( $(cat "${FAKE_DIR}/spawns.${boots}" 2>/dev/null || echo 0) + 1 ))
    echo "${spawns}" > "${FAKE_DIR}/spawns.${boots}"
    if [ "${boots:-0}" -le "${FAKE_UNSETTLED_BOOTS:-0}" ] && [ "${spawns}" -gt 10 ]; then exit 1; fi
    ;;
esac
exit 0
"""


class PrepareSimulatorRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        xcrun = bin_dir / "xcrun"
        xcrun.write_text(FAKE_XCRUN)
        xcrun.chmod(xcrun.stat().st_mode | stat.S_IEXEC)
        self.bin_dir = bin_dir
        self.evidence = self.dir / "evidence"
        self.fake_dir = self.dir / "fake"
        self.fake_dir.mkdir()

    def run_script(self, args, unresponsive_boots, unsettled_boots=0, extra_env=None):
        env = {
            **os.environ,
            "PATH": f"{self.bin_dir}:{os.environ['PATH']}",
            "FAKE_DIR": str(self.fake_dir),
            "FAKE_UNRESPONSIVE_BOOTS": str(unresponsive_boots),
            "FAKE_UNSETTLED_BOOTS": str(unsettled_boots),
            "OHAND_HOST_PRESSURE_FILE": str(self.evidence / "host-pressure.txt"),
            "OHAND_TEST_RESPONSIVE_TIMEOUT_SECONDS": "2",
            "OHAND_TEST_PROBE_INTERVAL_SECONDS": "0",
            "OHAND_TEST_SETTLE_PROBES": "3",
            "OHAND_TEST_RECOVERY_QUIET_WAIT_SECONDS": "0",
            **(extra_env or {}),
        }
        return subprocess.run(["bash", str(SCRIPTS / "prepare-simulator.sh"), *args], capture_output=True, text=True, env=env, timeout=120)

    def boots(self):
        return len((self.fake_dir / "boots.log").read_text().split())

    def recoveries(self):
        path = self.evidence / "simulator-recoveries.txt"
        return path.read_text().splitlines() if path.exists() else []

    def pressure(self):
        return (self.evidence / "host-pressure.txt").read_text()

    def test_responsive_first_attempt_has_no_recovery(self):
        result = self.run_script(["udid", "--erase", "--label", "first"], 0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("::warning::", result.stdout)
        self.assertEqual(self.boots(), 1)
        self.assertEqual(self.recoveries(), [])
        self.assertFalse((self.evidence / "simulator-recoveries.txt").exists())
        self.assertIn("first cold-boot ", self.pressure())

    def test_recovers_once_when_second_attempt_answers(self):
        result = self.run_script(["udid", "--erase", "--label", "second"], 1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        warnings = [line for line in result.stdout.splitlines() if line.startswith("::warning::")]
        self.assertTrue(any("second" in line and "cold-boot" in line and "responsive gate" in line for line in warnings), warnings)
        self.assertEqual(self.boots(), 2)
        self.assertIn("second cold-boot-recovered ", self.pressure())
        lines = self.recoveries()
        self.assertEqual(len(lines), 1)
        label, kind, first_seconds, second_seconds, outcome = lines[0].split()
        self.assertEqual((label, kind, outcome), ("second", "cold-boot", "recovered"))
        self.assertTrue(first_seconds.isdigit() and second_seconds.isdigit())
        calls = (self.fake_dir / "calls.log").read_text()
        self.assertEqual(calls.count("simctl erase udid"), 2)

    def test_fails_after_exactly_two_attempts(self):
        result = self.run_script(["udid", "--label", "twice"], 99)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(self.boots(), 2)
        self.assertIn("twice warm-boot-failed-twice ", self.pressure())
        lines = self.recoveries()
        self.assertEqual(len(lines), 1)
        self.assertTrue(lines[0].endswith("failed-twice"))
        self.assertIn("::warning::", result.stdout)

    def test_clone_recovery_deletes_first_clone_and_records_second_udid(self):
        udid_file = self.dir / "suite-udid.txt"
        result = self.run_script(["--clone", "template", "--udid-file", str(udid_file), "--label", "suite"], 1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(udid_file.read_text().strip(), "clone-2")
        calls = (self.fake_dir / "calls.log").read_text().splitlines()
        self.assertIn("simctl delete clone-1", calls)
        self.assertNotIn("simctl delete clone-2", calls)
        self.assertIn("suite clone-recovered ", self.pressure())
        self.assertEqual(len(self.recoveries()), 1)

    def test_clone_fails_closed_when_udid_file_is_unwritable(self):
        udid_file = self.dir / "missing-dir" / "suite-udid.txt"
        result = self.run_script(["--clone", "template", "--udid-file", str(udid_file), "--label", "suite"], 0)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("ready", result.stdout)

    def test_settle_failure_records_whole_attempt_duration(self):
        result = self.run_script(["udid", "--erase", "--label", "settle"], 0, unsettled_boots=1, extra_env={"OHAND_TEST_RESPONSIVE_TIMEOUT_SECONDS": "60", "OHAND_TEST_SETTLE_TIMEOUT_SECONDS": "2"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("settle gate", result.stdout)
        label, kind, first_seconds, second_seconds, outcome = self.recoveries()[0].split()
        self.assertEqual(outcome, "recovered")
        # The settle gate alone ran for the 2 s timeout, so the first attempt lasted at least that long.
        self.assertGreaterEqual(int(first_seconds), 2)

    def test_recovery_wait_is_bounded_by_wall_clock_and_unreadable_cpu_waits(self):
        # A `top` that takes 1 s per call and reports a busy host: the wait must stop at the 3 s deadline, even though sleeps alone would stretch it.
        slow_top = self.bin_dir / "top"
        slow_top.write_text("#!/usr/bin/env bash\nsleep 1\necho 'CPU usage: 90.0% user, 5.0% sys, 5.0% idle'\n")
        slow_top.chmod(slow_top.stat().st_mode | stat.S_IEXEC)
        result = self.run_script(["udid", "--label", "bounded"], 1, extra_env={"OHAND_TEST_RECOVERY_QUIET_WAIT_SECONDS": "3"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        waited = [int(m) for m in re.findall(r"quieten \(CPU [^,]*, (\d+)s of 3s", result.stdout)]
        self.assertGreaterEqual(max(waited), 3)
        self.assertLessEqual(max(waited), 5)
        # An unreadable CPU sample keeps waiting until the deadline instead of ending the wait at once.
        slow_top.write_text("#!/usr/bin/env bash\nsleep 1\necho garbage\n")
        result = self.run_script(["udid", "--label", "unreadable"], 3, extra_env={"OHAND_TEST_RECOVERY_QUIET_WAIT_SECONDS": "3"})
        waits = [line for line in result.stdout.splitlines() if "waiting for the host to quieten" in line]
        self.assertGreater(len(waits), 1, result.stdout)


if __name__ == "__main__":
    unittest.main()
