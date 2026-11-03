import re
import subprocess
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
        timeout = int(re.search(r"responsive_timeout_seconds=(\d+)", self.script).group(1))
        self.assertLessEqual(timeout, 480)

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


if __name__ == "__main__":
    unittest.main()
