"""Static checks on the simulator upgrade check's wiring that run on any host (the check itself needs macOS).

They keep the debug-only upgrade report out of Release sources, keep the workflow secret-free and short-lived, and keep the
check's script from ever uninstalling or erasing the app between the two builds.
"""

import re
import subprocess
import unittest
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parents[2]
UPGRADE_CHECK_DIRECTORY = REPO_ROOT / "ios" / "AppAssembly" / "UpgradeCheck"
SCRIPT = REPO_ROOT / "tools" / "release" / "simulator_upgrade_check.sh"
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "upgrade.yml"
PROJECT = REPO_ROOT / "ios" / "project.yml"
MARKERS = ("UpgradeReport", "OhAndUpgrade")


def debug_only_lines(source: str):
    """For each line, whether it sits inside `#if DEBUG` (and not in an `#else` / `#elseif` branch of it)."""
    stack = []
    flags = []
    for line in source.splitlines():
        stripped = line.strip()
        if stripped.startswith("#if "):
            stack.append(stripped == "#if DEBUG")
        elif stripped.startswith(("#else", "#elseif")):
            stack[-1] = False
        elif stripped == "#endif":
            stack.pop()
        flags.append(bool(stack) and all(stack))
    return flags


class ReleaseExclusionTests(unittest.TestCase):
    def test_every_upgrade_check_source_is_wrapped_in_debug_only_conditionals(self):
        sources = sorted(UPGRADE_CHECK_DIRECTORY.glob("*.swift"))
        self.assertTrue(sources)
        for source in sources:
            lines = source.read_text().splitlines()
            self.assertEqual(lines[0], "#if DEBUG", source.name)
            self.assertEqual(lines[-1], "#endif", source.name)
            flags = debug_only_lines(source.read_text())
            self.assertTrue(all(flags[1:-1]), f"{source.name} has code outside #if DEBUG")

    def test_no_other_app_source_mentions_the_report_outside_debug_conditionals(self):
        app_sources = [path for root in ("ios/AppAssembly", "ios/Capture", "ios/Services", "ios/OhAndCoreBridge", "app")
                       for path in (REPO_ROOT / root).rglob("*.swift") if UPGRADE_CHECK_DIRECTORY not in path.parents]
        mentioning = []
        for path in app_sources:
            source = path.read_text()
            flags = debug_only_lines(source)
            for number, line in enumerate(source.splitlines()):
                if any(marker in line for marker in MARKERS) and not flags[number]:
                    mentioning.append(f"{path.relative_to(REPO_ROOT)}:{number + 1}")
        self.assertEqual(mentioning, [])

    def test_the_call_site_is_in_the_app_delegate_under_debug(self):
        source = (REPO_ROOT / "ios" / "AppAssembly" / "AppDelegate.swift").read_text()
        self.assertIn("UpgradeReportRequest.requested()", source)

    def test_the_workflow_builds_a_release_app_and_looks_for_the_report(self):
        script = (REPO_ROOT / "tools" / "release" / "check_release_excludes_upgrade_report.sh").read_text()
        self.assertIn("-configuration Release", script)
        for marker in ("OhAndUpgradeReport", "upgrade-report.json", "ohand.upgrade-report.v1"):
            self.assertIn(marker, script)
        self.assertIn("check_release_excludes_upgrade_report.sh", WORKFLOW.read_text())


class ScriptTests(unittest.TestCase):
    def test_the_scripts_parse(self):
        for name in ("simulator_upgrade_check.sh", "check_release_excludes_upgrade_report.sh", "sign_trial_candidate.sh"):
            result = subprocess.run(["bash", "-n", str(REPO_ROOT / "tools" / "release" / name)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_check_never_uninstalls_or_erases_the_app_between_builds(self):
        commands = [line for line in SCRIPT.read_text().splitlines() if not line.strip().startswith("#")]
        text = "\n".join(commands)
        self.assertNotIn("simctl uninstall", text)
        self.assertNotIn("simctl erase", text)
        # The one erase is the first cold boot, before anything is installed.
        self.assertEqual(len(re.findall(r"--erase", text)), 1)
        self.assertLess(text.index("--erase"), text.index("simctl install"))
        self.assertEqual(len(re.findall(r"simctl install", text)), 2)

    def test_the_check_builds_two_different_revisions_and_installs_the_prior_first(self):
        text = SCRIPT.read_text()
        self.assertIn("worktree add --detach", text)
        self.assertIn("is not an upgrade", text)
        self.assertLess(text.index('simctl install "${udid}" "${prior_app}"'), text.index('simctl install "${udid}" "${candidate_app}"'))

    def test_seeding_goes_through_the_ui_test_target_not_through_a_database(self):
        text = SCRIPT.read_text()
        self.assertIn("UpgradeSeedUITests", text)
        self.assertNotIn("sqlite", text.lower())


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = yaml.safe_load(WORKFLOW.read_text())
        self.triggers = self.workflow.get("on", self.workflow.get(True))

    def test_read_only_and_secret_free(self):
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        self.assertNotIn("secrets.", WORKFLOW.read_text())

    def test_it_runs_for_the_paths_that_can_break_an_upgrade_and_on_demand(self):
        paths = self.triggers["pull_request"]["paths"]
        for required in ("tools/release/**", "ios/UITests/Upgrade/**", "ios/AppAssembly/UpgradeCheck/**"):
            self.assertIn(required, paths)
        self.assertTrue(any(path.startswith("core/src/store/schema") for path in paths))
        self.assertIn("workflow_dispatch", self.triggers)

    def test_artifacts_are_kept_for_seven_days(self):
        uploads = [step for job in self.workflow["jobs"].values() for step in job["steps"]
                   if str(step.get("uses", "")).startswith("actions/upload-artifact")]
        self.assertTrue(uploads)
        for step in uploads:
            self.assertEqual(step["with"]["retention-days"], 7)

    def test_it_builds_the_head_of_the_pull_request_with_full_history(self):
        checkouts = [step for job in self.workflow["jobs"].values() for step in job["steps"]
                     if str(step.get("uses", "")).startswith("actions/checkout")]
        for step in checkouts:
            self.assertEqual(step["with"]["fetch-depth"], 0)
            self.assertIn("pull_request.head.sha", step["with"]["ref"])

    def test_it_uses_the_pinned_xcode(self):
        project = yaml.safe_load(PROJECT.read_text())
        pinned = project["options"]["xcodeVersion"]
        versions = [step["with"]["xcode-version"] for job in self.workflow["jobs"].values() for step in job["steps"]
                    if str(step.get("uses", "")).startswith("maxim-lobanov/setup-xcode")]
        self.assertTrue(versions)
        self.assertEqual(set(versions), {pinned})


class ProjectTests(unittest.TestCase):
    def setUp(self):
        self.project = yaml.safe_load(PROJECT.read_text())

    def test_the_upgrade_ui_tests_have_no_target_application(self):
        target = self.project["targets"]["OhAndUpgradeUITests"]
        self.assertEqual(target["type"], "bundle.ui-testing")
        self.assertNotIn("TEST_TARGET_NAME", target["settings"])
        self.assertNotIn("dependencies", target)
        self.assertIn("OhAndUpgradeUITests", self.project["schemes"])

    def test_the_app_and_its_extension_are_stamped_with_the_build_identity(self):
        for name in ("OhAndApp", "OhAndCaptureControl"):
            target = self.project["targets"][name]
            scripts = [step["script"] for step in target["postBuildScripts"]]
            self.assertTrue(any("generate_build_info.sh" in script for script in scripts), name)
            self.assertIn(str(target["settings"]["ENABLE_USER_SCRIPT_SANDBOXING"]).lower(), ("no", "false"))


if __name__ == "__main__":
    unittest.main()
