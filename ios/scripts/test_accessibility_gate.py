"""The accessibility job in ios.yml is a required gate: it must fail, never skip its audits, when no iPhone simulator is
selected, and its audit step must not be able to conclude green without audit evidence."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

SCRIPTS = Path(__file__).resolve().parent
WORKFLOW = SCRIPTS.parents[1] / ".github" / "workflows" / "ios.yml"
AUDIT_SCRIPT = "./scripts/accessibility-simulator.sh"
RUNTIME_PREFIX = "com.apple.CoreSimulator.SimRuntime.iOS-"


def accessibility_job():
    return yaml.safe_load(WORKFLOW.read_text())["jobs"]["accessibility"]


def step_named(job, name):
    matches = [step for step in job["steps"] if step.get("name") == name]
    if len(matches) != 1:
        raise AssertionError(f"expected one step named {name!r}, found {len(matches)}")
    return matches[0]


class AccessibilityGateTests(unittest.TestCase):
    def setUp(self):
        self.job = accessibility_job()

    def test_job_itself_cannot_be_skipped_or_made_non_fatal(self):
        self.assertNotIn("if", self.job)
        self.assertNotIn("continue-on-error", self.job)

    def test_every_step_up_to_the_audits_runs_unconditionally_and_fatally(self):
        names = [step.get("name") for step in self.job["steps"]]
        audits = names.index("Accessibility audits")
        for step in self.job["steps"][: audits + 1]:
            label = step.get("name") or step.get("uses")
            self.assertNotIn("if", step, f"{label} must not be conditional: a skipped step lets the gate pass unaudited")
            self.assertNotIn("continue-on-error", step, f"{label} must fail the job")

    def test_audit_step_runs_the_script_on_the_selected_udid(self):
        run = step_named(self.job, "Accessibility audits")["run"]
        self.assertIn("set -euo pipefail", run)
        self.assertIn(AUDIT_SCRIPT, run)
        self.assertIn("steps.simulator.outputs.udid", str(step_named(self.job, "Accessibility audits").get("env")))

    def test_selection_step_is_not_guarded(self):
        run = step_named(self.job, "Select simulator")["run"]
        self.assertIn("set -euo pipefail", run)
        for line in run.splitlines():
            if "select_simulator.py" in line:
                self.assertFalse(line.strip().startswith(("if ", "if!", "!")), line)
                self.assertNotIn("||", line)

    def run_selection(self, devices):
        """Runs the job's selection step against a fake `xcrun` that reports SDK 26.6 and the given devices."""
        run = step_named(self.job, "Select simulator")["run"]
        with tempfile.TemporaryDirectory() as workspace_dir:
            workspace = Path(workspace_dir)
            (workspace / "ios").mkdir()
            (workspace / "ios" / "scripts").symlink_to(SCRIPTS)
            bin_dir = workspace / "bin"
            bin_dir.mkdir()
            listing = workspace / "devices.json"
            listing.write_text(json.dumps({"devices": devices}))
            xcrun = bin_dir / "xcrun"
            xcrun.write_text(
                "#!/usr/bin/env bash\n"
                'if [ "$1" = "--sdk" ]; then echo 26.6; exit 0; fi\n'
                f'cat "{listing}"\n'
            )
            xcrun.chmod(0o755)
            output = workspace / "github-output"
            output.touch()
            environment = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", GITHUB_OUTPUT=str(output))
            result = subprocess.run(
                ["bash", "-c", run], cwd=workspace, env=environment, capture_output=True, text=True
            )
            return result.returncode, output.read_text()

    def test_no_iphone_fails_the_selection_step(self):
        status, output = self.run_selection({})
        self.assertNotEqual(status, 0)
        self.assertNotIn("udid=", output)
        ipad_only = {RUNTIME_PREFIX + "26-6": [{"name": "iPad Pro", "udid": "PAD", "isAvailable": True}]}
        status, _ = self.run_selection(ipad_only)
        self.assertNotEqual(status, 0)

    def test_an_iphone_is_selected_and_published(self):
        devices = {RUNTIME_PREFIX + "26-6": [{"name": "iPhone 17", "udid": "PHONE", "isAvailable": True}]}
        status, output = self.run_selection(devices)
        self.assertEqual(status, 0)
        self.assertIn("udid=PHONE", output.splitlines())

    def test_audit_script_refuses_an_empty_udid_and_requires_passing_audits(self):
        result = subprocess.run(["bash", str(SCRIPTS / "accessibility-simulator.sh"), "", "/nonexistent"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        script = (SCRIPTS / "accessibility-simulator.sh").read_text()
        self.assertIn('grep -q "^PASS "', script)
        self.assertIn("^(ISSUE|ERROR|FAIL|UNVERIFIED) ", script)
        self.assertTrue(script.rstrip().endswith('[ "${missing}" -eq 0 ]'))


if __name__ == "__main__":
    unittest.main()
