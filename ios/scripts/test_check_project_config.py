import copy
import plistlib
import tempfile
import unittest
from pathlib import Path

import yaml

import check_project_config as checks


class ProjectConfigChecks(unittest.TestCase):
    def test_committed_configuration_passes(self):
        self.assertEqual(checks.run_all_checks(), [])

    def test_transitive_framework_must_be_embedded_in_app(self):
        project = checks.load_project()
        app = project["targets"]["OhAndApp"]
        app["dependencies"] = [d for d in app["dependencies"] if d["target"] != "OhAndCoreBridge"]
        errors = checks.check_embedded_frameworks(project)
        self.assertEqual(len(errors), 1)
        self.assertIn("OhAndCoreBridge", errors[0])
        self.assertTrue(any("OhAndCoreBridge" in e for e in checks.check_project(project)))

    def test_transitive_framework_with_embed_disabled_is_reported(self):
        project = checks.load_project()
        for dependency in project["targets"]["OhAndApp"]["dependencies"]:
            if dependency["target"] == "OhAndCoreBridge":
                dependency["embed"] = False
        self.assertTrue(any("OhAndCoreBridge" in e for e in checks.check_embedded_frameworks(project)))

    def test_deeper_transitive_framework_is_reported(self):
        project = checks.load_project()
        project["targets"]["OhAndCoreBridge"]["dependencies"] = [{"target": "OhAndExtra"}]
        project["targets"]["OhAndExtra"] = {"type": "framework"}
        errors = [e for e in checks.check_embedded_frameworks(project) if e.startswith("OhAndApp ")]
        self.assertEqual(len(errors), 1)
        self.assertIn("OhAndExtra", errors[0])

    def test_directly_embedded_frameworks_pass(self):
        self.assertEqual(checks.check_embedded_frameworks(checks.load_project()), [])

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

    def write_workflow(self, directory, pins, runs_on="xcode-27"):
        steps = "".join(
            f"      - uses: maxim-lobanov/setup-xcode@v1\n        with:\n          xcode-version: '{pin}'\n" for pin in pins
        ) or "      - run: echo no xcode step\n"
        path = Path(directory) / "ios.yml"
        path.write_text(f"name: x\njobs:\n  build:\n    runs-on: {runs_on}\n    steps:\n{steps}")
        return path

    def test_committed_workflow_pin_matches_project(self):
        project = checks.load_project()
        self.assertEqual(checks.check_workflow_xcode_pin(project), [])
        self.assertEqual(str(project["options"]["xcodeVersion"]), "27.2")

    def write_upload_workflow(self, directory, continue_on_error):
        step = {"name": "Upload evidence", "uses": "actions/upload-artifact@v4", "with": {"name": "x", "path": "y"}}
        if continue_on_error is not None:
            step["continue-on-error"] = continue_on_error
        path = Path(directory) / "upload.yml"
        path.write_text(yaml.safe_dump({"jobs": {"build": {"steps": [{"run": "true"}, step]}}}))
        return path

    def test_committed_artifact_uploads_are_non_fatal(self):
        self.assertEqual(checks.check_artifact_uploads_non_fatal(), [])

    def test_artifact_upload_with_continue_on_error_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_upload_workflow(directory, True)
            self.assertEqual(checks.check_artifact_uploads_non_fatal([path]), [])

    def test_artifact_upload_without_continue_on_error_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            for value in (None, False):
                path = self.write_upload_workflow(directory, value)
                errors = checks.check_artifact_uploads_non_fatal([path])
                self.assertEqual(len(errors), 1)
                self.assertIn("continue-on-error", errors[0])

    EXCLUDED_OUT = "out\n!out/**/*.xcresult\n!out/**/*.xcresult/**"

    def write_budget_workflow(self, directory, path=EXCLUDED_OUT, retention=2, name="x", condition=None, export=False,
                              export_condition="failure()", export_continue=True):
        steps = []
        if export:
            steps.append({"name": "Export", "if": export_condition, "continue-on-error": export_continue,
                          "run": "ios/scripts/export-failure-diagnostics.sh out/diagnostics out/a.xcresult"})
        step = {"name": "Upload", "uses": "actions/upload-artifact@v4", "continue-on-error": True,
                "with": {"name": name, "path": path, "retention-days": retention}}
        if condition is not None:
            step["if"] = condition
        steps.append(step)
        workflow = Path(directory) / "budget.yml"
        workflow.write_text(yaml.safe_dump({"jobs": {"build": {"steps": steps}}}))
        return workflow

    def budget_errors(self, **options):
        with tempfile.TemporaryDirectory() as directory:
            return checks.check_artifact_budget([self.write_budget_workflow(directory, **options)])

    def failure_errors(self, **options):
        options = {"path": self.EXCLUDED_OUT, "name": "x-failure", "condition": "failure()", "retention": 1, "export": True, **options}
        return self.budget_errors(**options)

    def test_committed_artifacts_follow_the_budget(self):
        self.assertEqual(checks.check_artifact_budget(), [])

    def test_routine_upload_excluding_bundles_with_two_day_retention_passes(self):
        self.assertEqual(self.budget_errors(), [])

    def test_retention_above_two_days_is_reported(self):
        for retention in (3, 7, None):
            errors = self.budget_errors(retention=retention)
            self.assertEqual(len(errors), 1)
            self.assertIn("retention-days", errors[0])

    def test_routine_upload_without_bundle_exclusion_is_reported(self):
        errors = self.budget_errors(path="out")
        self.assertEqual(len(errors), 1)
        self.assertIn(".xcresult", errors[0])

    def test_exclusion_for_an_unrelated_root_is_reported(self):
        errors = self.budget_errors(path="out\n!other/**/*.xcresult\n!other/**/*.xcresult/**")
        self.assertEqual(len(errors), 1)
        self.assertIn(".xcresult", errors[0])

    def test_exclusion_without_bundle_contents_is_reported(self):
        self.assertEqual(len(self.budget_errors(path="out\n!out/**/*.xcresult")), 1)

    def test_routine_upload_of_bundle_path_is_reported(self):
        errors = self.budget_errors(path="out/*.xcresult\n!out/**/*.xcresult\n!out/**/*.xcresult/**")
        self.assertTrue(any("includes a .xcresult" in error for error in errors))

    def test_failure_upload_of_the_export_after_the_export_step_passes(self):
        self.assertEqual(self.failure_errors(), [])

    def test_failure_upload_of_bundles_is_reported_even_when_failed(self):
        errors = self.failure_errors(path="out/**/*.xcresult")
        self.assertTrue(any(".xcresult" in error for error in errors))

    def test_failure_upload_requires_failure_condition(self):
        errors = self.failure_errors(condition="always()")
        self.assertEqual(len(errors), 1)
        self.assertIn("failure()", errors[0])

    def test_failure_upload_must_keep_exactly_one_day(self):
        for retention in (2, 7, None):
            errors = self.failure_errors(retention=retention)
            self.assertEqual(len(errors), 1)
            self.assertIn("retention-days", errors[0])

    def test_failure_upload_without_the_export_step_is_reported(self):
        errors = self.failure_errors(export=False)
        self.assertEqual(len(errors), 1)
        self.assertIn("export-failure-diagnostics.sh", errors[0])

    def test_export_step_must_run_only_on_failure_and_not_fail_the_job(self):
        for options in ({"export_condition": "always()"}, {"export_continue": False}):
            errors = self.failure_errors(**options)
            self.assertEqual(len(errors), 1)
            self.assertIn("export-failure-diagnostics.sh", errors[0])

    def test_workflow_pin_disagreeing_with_project_is_reported(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_workflow(directory, ["16.4"])
            errors = checks.check_workflow_xcode_pin(project, path)
        self.assertTrue(any("'16.4'" in e and "'27.2'" in e for e in errors), errors)

    def test_project_pin_change_without_workflow_change_is_reported(self):
        project = copy.deepcopy(checks.load_project())
        project["options"]["xcodeVersion"] = "27.0.1"
        errors = checks.check_workflow_xcode_pin(project)
        self.assertEqual(len([e for e in errors if "'27.0.1'" in e]), 4, errors)

    def test_pin_27_2_on_macos_26_runner_is_reported(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_workflow(directory, ["27.2"], runs_on="macos-26")
            errors = checks.check_workflow_xcode_pin(project, path)
        self.assertTrue(any("'macos-26'" in e and "'xcode-27'" in e for e in errors), errors)

    def test_pin_27_2_on_xcode_27_runner_passes(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_workflow(directory, ["27.2"])
            self.assertEqual(checks.check_workflow_xcode_pin(project, path), [])

    def test_job_without_exactly_one_xcode_step_is_reported(self):
        project = checks.load_project()
        with tempfile.TemporaryDirectory() as directory:
            missing = self.write_workflow(directory, [])
            self.assertTrue(any("exactly one" in e for e in checks.check_workflow_xcode_pin(project, missing)))
            doubled = self.write_workflow(directory, ["27.2", "27.2"])
            self.assertTrue(any("exactly one" in e for e in checks.check_workflow_xcode_pin(project, doubled)))

    def copy_plists(self, directory):
        for plist_path in checks.IOS_ROOT.glob("*/Info.plist"):
            target = Path(directory) / plist_path.parent.name
            target.mkdir()
            (target / "Info.plist").write_bytes(plist_path.read_bytes())
        return Path(directory)

    def test_app_plist_with_launch_screen_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            errors = checks.check_plists(self.copy_plists(directory))
        self.assertEqual([e for e in errors if "UILaunchScreen" in e], [])

    def test_app_plist_without_launch_screen_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copy_plists(directory)
            plist_path = root / "AppAssembly/Info.plist"
            with plist_path.open("rb") as handle:
                plist = plistlib.load(handle)
            del plist["UILaunchScreen"]
            with plist_path.open("wb") as handle:
                plistlib.dump(plist, handle)
            errors = checks.check_plists(root)
        self.assertTrue(any("UILaunchScreen" in e for e in errors), errors)


class FaceIdUsageChecks(unittest.TestCase):
    def write_project(self, directory, plist_has_key, source_text="import LocalAuthentication\nlet context = LAContext()\n"):
        root = Path(directory)
        (root / "Auth").mkdir()
        (root / "Auth/Gate.swift").write_text(source_text)
        (root / "Host").mkdir()
        plist = {"NSFaceIDUsageDescription": "Unlock."} if plist_has_key else {}
        (root / "Host/Info.plist").write_bytes(plistlib.dumps(plist))
        project = {
            "targets": {
                "AuthFramework": {"type": "framework", "sources": [{"path": "Auth"}]},
                "HostApp": {
                    "type": "application",
                    "settings": {"INFOPLIST_FILE": "Host/Info.plist"},
                    "dependencies": [{"target": "AuthFramework"}],
                },
            }
        }
        return project, root

    def test_committed_app_plist_declares_face_id(self):
        with (checks.IOS_ROOT / "AppAssembly/Info.plist").open("rb") as handle:
            self.assertTrue(plistlib.load(handle).get("NSFaceIDUsageDescription"))
        self.assertIn("NSFaceIDUsageDescription", checks.APP_USAGE_STRINGS)

    def test_app_plist_without_face_id_key_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for plist_path in checks.IOS_ROOT.glob("*/Info.plist"):
                (root / plist_path.parent.name).mkdir()
                (root / plist_path.parent.name / "Info.plist").write_bytes(plist_path.read_bytes())
            app_path = root / "AppAssembly/Info.plist"
            plist = plistlib.loads(app_path.read_bytes())
            del plist["NSFaceIDUsageDescription"]
            app_path.write_bytes(plistlib.dumps(plist))
            errors = checks.check_plists(root)
        self.assertTrue(any("AppAssembly" in e and "NSFaceIDUsageDescription" in e for e in errors))

    def test_target_using_lacontext_without_key_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            project, root = self.write_project(directory, plist_has_key=False)
            errors = checks.check_face_id_usage(project, root)
        self.assertEqual(len(errors), 1)
        self.assertIn("AuthFramework", errors[0])
        self.assertIn("NSFaceIDUsageDescription", errors[0])

    def test_evaluate_policy_without_key_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            project, root = self.write_project(directory, False, "func go() { handle.evaluatePolicy(p) }\n")
            self.assertEqual(len(checks.check_face_id_usage(project, root)), 1)

    def test_target_using_lacontext_with_key_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            project, root = self.write_project(directory, plist_has_key=True)
            self.assertEqual(checks.check_face_id_usage(project, root), [])

    def test_target_not_using_lacontext_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            project, root = self.write_project(directory, False, "let value = 1\n")
            self.assertEqual(checks.check_face_id_usage(project, root), [])


if __name__ == "__main__":
    unittest.main()
