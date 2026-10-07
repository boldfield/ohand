import copy
import unittest

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

    def test_non_unit_test_bundle_is_rejected(self):
        project = checks.load_project()
        project["targets"]["OhAndTests"]["type"] = "bundle"
        self.assertTrue(any("bundle.unit-test" in e for e in checks.check_project(project)))

    def test_duplicate_bundle_identifier_is_rejected(self):
        project = checks.load_project()
        project["targets"]["AudioProbe"]["settings"]["PRODUCT_BUNDLE_IDENTIFIER"] = "com.boldfield.ohand.probes.bridge"
        self.assertTrue(any("not unique" in e for e in checks.check_project(project)))


if __name__ == "__main__":
    unittest.main()
