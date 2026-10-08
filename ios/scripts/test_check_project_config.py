import copy
import tempfile
import unittest
from pathlib import Path

import check_project_config as checks


class ProjectConfigChecks(unittest.TestCase):
    def test_committed_configuration_passes(self):
        self.assertEqual(checks.run_all_checks(), [])

    def test_removing_owned_root_from_target_is_reported(self):
        project = checks.load_project()
        project["targets"]["OhAndServices"]["sources"] = []
        self.assertTrue(any("Services/" in e for e in checks.check_project(project)))

    def test_services_compiled_twice_is_reported(self):
        project = checks.load_project()
        project["targets"]["OhAndApp"]["sources"].append({"path": "Services"})
        self.assertTrue(any("exactly one module" in e for e in checks.check_project(project)))

    def test_signing_identity_is_rejected(self):
        project = copy.deepcopy(checks.load_project())
        project["settings"]["CODE_SIGN_IDENTITY"] = "iPhone Developer"
        self.assertTrue(any("CODE_SIGN_IDENTITY" in e for e in checks.check_project(project)))

    def test_control_as_second_application_is_rejected(self):
        project = checks.load_project()
        project["targets"]["OhAndControl"] = {"type": "application"}
        self.assertTrue(any("second application" in e for e in checks.check_project(project)))

    def test_control_extension_must_be_embedded_in_host(self):
        project = checks.load_project()
        project["targets"]["CaptureProbe"]["dependencies"] = []
        self.assertTrue(any("must depend on" in e for e in checks.check_project(project)))

    def test_control_extension_must_not_be_compiled_into_host(self):
        project = checks.load_project()
        project["targets"]["OhAndApp"]["sources"][1]["excludes"] = ["**/.placeholder"]
        self.assertTrue(any("must exclude" in e for e in checks.check_project(project)))

    def test_control_extension_bundle_id_must_be_prefixed_by_host(self):
        project = checks.load_project()
        project["targets"]["CaptureProbeControl"]["settings"]["PRODUCT_BUNDLE_IDENTIFIER"] = "com.boldfield.ohand.probes.other"
        self.assertTrue(any("prefixed by" in e for e in checks.check_project(project)))

    def test_extension_must_include_shared_intent_directory(self):
        project = checks.load_project()
        project["targets"]["CaptureProbeControl"]["sources"] = [project["targets"]["CaptureProbeControl"]["sources"][0]]
        self.assertTrue(any("shared app-opening intent" in e for e in checks.check_project(project)))

    def test_host_must_not_exclude_shared_intent_directory(self):
        project = checks.load_project()
        project["targets"]["OhAndApp"]["sources"][1]["excludes"].append("Entry/Shared/**")
        self.assertTrue(any("must not exclude" in e for e in checks.check_project(project)))

    def test_host_must_compile_shared_intent_directory(self):
        project = checks.load_project()
        project["targets"]["OhAndApp"]["sources"] = [project["targets"]["OhAndApp"]["sources"][0]]
        self.assertTrue(any("must compile Capture" in e for e in checks.check_project(project)))

    def test_shared_intent_must_be_open_intent(self):
        original = checks.SHARED_INTENT_ROOTS["CaptureProbeControl"]
        checks.SHARED_INTENT_ROOTS["CaptureProbeControl"] = "CaptureProbe/Sources"
        try:
            self.assertTrue(any("must contain an OpenIntent" in e for e in checks.check_control_extensions(checks.load_project())))
        finally:
            checks.SHARED_INTENT_ROOTS["CaptureProbeControl"] = original

    def write_shared_intent(self, root, intent_source, control_source):
        (root / "Probe" / "Shared").mkdir(parents=True)
        (root / "Probe" / "Control").mkdir()
        (root / "Probe" / "Shared" / "Intent.swift").write_text(intent_source)
        (root / "Probe" / "Control" / "Control.swift").write_text(control_source)

    def shared_intent_errors(self, intent_source, control_source):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_shared_intent(root, intent_source, control_source)
            host = {"sources": [{"path": "Probe"}]}
            extension = {"sources": ["Probe/Control", "Probe/Shared"]}
            originals = (checks.SHARED_INTENT_ROOTS.copy(), checks.CONTROL_ROOTS.copy())
            checks.SHARED_INTENT_ROOTS["Fixture"] = "Probe/Shared"
            checks.CONTROL_ROOTS["Fixture"] = "Probe/Control"
            try:
                return checks.check_shared_intent("Host", host, "Fixture", extension, root)
            finally:
                checks.SHARED_INTENT_ROOTS.clear()
                checks.SHARED_INTENT_ROOTS.update(originals[0])
                checks.CONTROL_ROOTS.clear()
                checks.CONTROL_ROOTS.update(originals[1])

    OPEN_INTENT = "struct FixtureIntent: OpenIntent {\n    @Parameter(title: \"T\")\n    var target: FixtureScreen\n}\n"
    CONTROL = "ControlWidgetButton(action: FixtureIntent(target: .capture)) {}\n"

    def test_open_intent_with_target_run_by_control_passes(self):
        self.assertEqual(self.shared_intent_errors(self.OPEN_INTENT, self.CONTROL), [])

    def test_legacy_open_app_when_run_is_rejected(self):
        legacy = "struct FixtureIntent: AppIntent {\n    static let openAppWhenRun = true\n}\n"
        errors = self.shared_intent_errors(legacy, self.CONTROL)
        self.assertTrue(any("legacy openAppWhenRun" in e for e in errors))
        self.assertTrue(any("must contain an OpenIntent" in e for e in errors))

    def test_open_intent_without_target_parameter_is_rejected(self):
        errors = self.shared_intent_errors("struct FixtureIntent: OpenIntent {\n}\n", self.CONTROL)
        self.assertTrue(any("@Parameter target" in e for e in errors))

    def test_control_not_running_shared_intent_is_rejected(self):
        errors = self.shared_intent_errors(self.OPEN_INTENT, "ControlWidgetButton(action: OtherIntent()) {}\n")
        self.assertTrue(any("must run a shared OpenIntent" in e for e in errors))

    def test_control_redefining_shared_intent_is_rejected(self):
        errors = self.shared_intent_errors(self.OPEN_INTENT, self.CONTROL + self.OPEN_INTENT)
        self.assertTrue(any("must not redefine" in e for e in errors))

    def test_non_unit_test_bundle_is_rejected(self):
        project = checks.load_project()
        project["targets"]["OhAndTests"]["type"] = "bundle"
        self.assertTrue(any("bundle.unit-test" in e for e in checks.check_project(project)))

    def test_duplicate_bundle_identifier_is_rejected(self):
        project = checks.load_project()
        project["targets"]["AudioProbe"]["settings"]["PRODUCT_BUNDLE_IDENTIFIER"] = "com.boldfield.ohand.probes.bridge"
        self.assertTrue(any("not unique" in e for e in checks.check_project(project)))

    def write_workflow(self, directory, pins):
        steps = "".join(
            f"      - uses: maxim-lobanov/setup-xcode@v1\n        with:\n          xcode-version: '{pin}'\n" for pin in pins
        ) or "      - run: echo no xcode step\n"
        path = Path(directory) / "ios.yml"
        path.write_text(f"name: x\njobs:\n  build:\n    runs-on: macos-26\n    steps:\n{steps}")
        return path

    def test_committed_workflow_pin_matches_project(self):
        project = checks.load_project()
        self.assertEqual(checks.check_workflow_xcode_pin(project), [])
        self.assertEqual(str(project["options"]["xcodeVersion"]), "26.6")

    def test_workflow_pin_disagreeing_with_project_is_reported(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_workflow(directory, ["16.4"])
            errors = checks.check_workflow_xcode_pin(project, path)
        self.assertTrue(any("'16.4'" in e and "'26.6'" in e for e in errors), errors)

    def test_project_pin_change_without_workflow_change_is_reported(self):
        project = copy.deepcopy(checks.load_project())
        project["options"]["xcodeVersion"] = "27.0"
        errors = checks.check_workflow_xcode_pin(project)
        self.assertEqual(len([e for e in errors if "'27.0'" in e]), 2, errors)

    def test_job_without_exactly_one_xcode_step_is_reported(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            missing = self.write_workflow(directory, [])
            self.assertTrue(any("exactly one" in e for e in checks.check_workflow_xcode_pin(project, missing)))
            doubled = self.write_workflow(directory, ["26.6", "26.6"])
            self.assertTrue(any("exactly one" in e for e in checks.check_workflow_xcode_pin(project, doubled)))


if __name__ == "__main__":
    unittest.main()
