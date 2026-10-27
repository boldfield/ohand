"""Behaviour tests for verify_upgrade_report.py: a good run passes, and each way an upgrade can lose data fails."""

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import verify_upgrade_report as verify

TOOL = Path(__file__).resolve().parent / "verify_upgrade_report.py"
PRIOR = "1" * 40
CANDIDATE = "2" * 40
REMINDER = {"reminder_id": "rem-1", "schedule_generation": 2, "resolved_instant": "2026-10-27T11:00:00Z"}


def expectations():
    return {
        "kept": {"itemID": "item-kept", "textLength": 44},
        "corrected": {"itemID": "item-corrected", "originalTextLength": 36, "correctedTextLength": 56},
        "deleted": {"itemID": "item-deleted"},
        "reminder": {"itemID": "item-reminder", "textLength": 37, "storedTimeLabel": "synthetic label"},
        "provider": {"profileName": "upgrade-check-provider"},
    }


def report(build_number="40"):
    return {
        "schema": "ohand.upgrade-report.v1",
        "generated_at": "2026-10-27T10:00:00Z",
        "build": {"version": "0.1.0", "build_number": build_number, "revision": CANDIDATE, "dirty": False},
        "recent_item_count": 3,
        "items": [
            {"item_id": "item-kept", "revision": 1, "item_type": None, "lifecycle_state": "active", "text_basis": "original",
             "text_length": 44, "original_text_length": 44, "corrected_text_length": None, "reminder": None},
            {"item_id": "item-corrected", "revision": 2, "item_type": None, "lifecycle_state": "active", "text_basis": "corrected",
             "text_length": 56, "original_text_length": 36, "corrected_text_length": 56, "reminder": None},
            {"item_id": "item-reminder", "revision": 2, "item_type": "action", "lifecycle_state": "active", "text_basis": "original",
             "text_length": 37, "original_text_length": 37, "corrected_text_length": None,
             "reminder": {**REMINDER, "request_state": "resolved", "schedule_state": "scheduled"}},
        ],
        "probes": [{"item_id": "item-deleted", "state": "not_found"}],
        "providers": [{"profile_version": "v1", "protocol": "open_ai", "model": "synthetic-model",
                       "destination_classes": ["cloud"], "has_endpoint": False, "credential_bound": True,
                       "credential_status": "present", "revoked": False, "default_for": ["text_interpretation"],
                       "route_covered": True}],
        "pending_reminder_requests": [{"reminder_id": "rem-1", "schedule_generation": 2}],
        "other_pending_request_count": 0,
    }


def facts():
    return {
        "collected_at": "2026-10-27T10:05:00Z",
        "run": {"workflow_run": "https://example.invalid/run/1"},
        "prior": {"revision": PRIOR, "build_number": 7},
        "candidate": {"revision": CANDIDATE, "build_number": 40},
        "prior_is_ancestor": True,
        "uninstalled": False,
        "installed": {"before_seeding": {"build_number": "7"}, "after_upgrade": {"build_number": "40"},
                      "after_relaunch": {"build_number": "40"}},
        "data_container": {"before_upgrade": "/synthetic/container", "after_upgrade": "/synthetic/container",
                           "after_relaunch": "/synthetic/container", "marker_carried": True},
        "simulator": {"name": "iPhone Synthetic", "runtime": "iOS 26.0"},
        "ui_checks": {"seed_exit_status": 0, "verify_exit_status": 0},
    }


def run(facts_value=None, expectations_value=None, after=None, relaunch=None, before=None):
    return verify.evaluate(facts_value or facts(), expectations_value or expectations(), after or report(),
                           relaunch or report(), before)


def failed(checks):
    return [result["check"] for result in checks.results if not result["passed"]]


class GoodRunTests(unittest.TestCase):
    def test_a_complete_upgrade_passes_and_is_recorded(self):
        checks, record = run()
        self.assertEqual(failed(checks), [])
        self.assertEqual(record["result"], "passed")
        self.assertEqual(record["installed_build_numbers"], {"before_seeding": "7", "after_upgrade": "40", "after_relaunch": "40"})
        self.assertTrue(record["data_container_path_unchanged_by_install"])
        self.assertTrue(record["data_container_unchanged_after_relaunch"])
        self.assertEqual(record["prior_revision_has_upgrade_report"], "absent")

    def test_a_prior_report_with_the_same_reminder_is_accepted_and_recorded(self):
        checks, record = run(before=report("7"))
        self.assertEqual(failed(checks), [])
        self.assertEqual(record["prior_revision_has_upgrade_report"], "present")

    def test_the_record_holds_no_capture_text_container_path_or_device_identifier(self):
        _, record = run()
        text = json.dumps(record)
        self.assertNotIn("/synthetic/container", text)
        for forbidden in ("udid", "UDID", "Upgrade check"):
            self.assertNotIn(forbidden, text)


class LossTests(unittest.TestCase):
    def assert_fails(self, name_fragment, **kwargs):
        checks, record = run(**kwargs)
        self.assertEqual(record["result"], "failed")
        self.assertTrue(any(name_fragment in name for name in failed(checks)), failed(checks))

    def test_a_missing_source_fails(self):
        after = report()
        after["items"] = [item for item in after["items"] if item["item_id"] != "item-kept"]
        after["recent_item_count"] = 2
        self.assert_fails("kept capture", after=after)

    def test_a_correction_that_is_no_longer_authoritative_fails(self):
        after = report()
        after["items"][1]["text_basis"] = "original"
        self.assert_fails("correction present and authoritative", after=after)

    def test_a_deleted_item_that_returns_fails(self):
        after = report()
        after["probes"] = [{"item_id": "item-deleted", "state": "active"}]
        after["items"].append({**copy.deepcopy(after["items"][0]), "item_id": "item-deleted"})
        after["recent_item_count"] = 4
        self.assert_fails("deleted item", after=after)

    def test_a_lost_provider_credential_fails(self):
        after = report()
        after["providers"][0]["credential_status"] = "absent"
        self.assert_fails("credential", after=after)

    def test_a_provider_that_processing_no_longer_routes_to_fails(self):
        after = report()
        after["providers"][0]["route_covered"] = False
        self.assert_fails("routes to the provider", after=after)

    def test_a_missing_pending_notification_fails(self):
        after = report()
        after["pending_reminder_requests"] = []
        self.assert_fails("exactly one pending", after=after)

    def test_a_duplicate_or_orphan_pending_notification_fails(self):
        after = report()
        after["pending_reminder_requests"].append({"reminder_id": "orphan", "schedule_generation": 1})
        self.assert_fails("exactly one pending", after=after)

    def test_a_changed_reminder_fire_time_after_relaunch_fails(self):
        relaunch = report()
        relaunch["items"][2]["reminder"]["resolved_instant"] = "2026-10-27T12:00:00Z"
        self.assert_fails("fire time are unchanged", relaunch=relaunch)

    def test_a_changed_reminder_identifier_against_the_prior_report_fails(self):
        before = report("7")
        before["items"][2]["reminder"]["reminder_id"] = "other"
        self.assert_fails("prior report: reminder", before=before)

    def test_a_changed_provider_model_or_destination_across_the_upgrade_fails(self):
        for key, value in (("model", "other-model"), ("destination_classes", ["private_server"]), ("protocol", "anthropic")):
            before = report("7")
            before["providers"][0][key] = value
            self.assert_fails("provider profile version, type, model", before=before)

    def test_an_older_prior_report_without_model_fields_still_compares_what_it_has(self):
        before = report("7")
        del before["providers"][0]["model"], before["providers"][0]["destination_classes"]
        checks, _ = run(before=before)
        self.assertEqual(failed(checks), [])

    def test_the_provider_must_name_a_model_and_destinations(self):
        after = report()
        after["providers"][0]["destination_classes"] = []
        self.assert_fails("names its model", after=after)

    def test_the_same_build_number_is_not_an_upgrade(self):
        changed = facts()
        changed["installed"]["before_seeding"]["build_number"] = "40"
        changed["prior"]["build_number"] = 40
        self.assert_fails("raised the build number", facts_value=changed)

    def test_a_lost_marker_file_fails(self):
        changed = facts()
        changed["data_container"]["marker_carried"] = False
        self.assert_fails("contents carried over", facts_value=changed)

    def test_a_moved_container_directory_is_recorded_not_failed_when_the_contents_carried_over(self):
        changed = facts()
        changed["data_container"]["after_upgrade"] = "/synthetic/other"
        changed["data_container"]["after_relaunch"] = "/synthetic/other"
        checks, record = run(facts_value=changed)
        self.assertEqual(failed(checks), [])
        self.assertFalse(record["data_container_path_unchanged_by_install"])

    def test_a_container_that_changes_again_on_relaunch_fails(self):
        changed = facts()
        changed["data_container"]["after_relaunch"] = "/synthetic/other"
        self.assert_fails("after relaunch", facts_value=changed)

    def test_an_uninstall_fails(self):
        changed = facts()
        changed["uninstalled"] = True
        self.assert_fails("never uninstalled", facts_value=changed)

    def test_a_report_that_names_another_build_fails(self):
        after = report()
        after["build"]["revision"] = PRIOR
        self.assert_fails("names the candidate build", after=after)

    def test_a_failed_ui_run_fails(self):
        changed = facts()
        changed["ui_checks"]["verify_exit_status"] = 1
        self.assert_fails("UI run after the upgrade", facts_value=changed)

    def test_the_same_revision_for_both_builds_fails(self):
        changed = facts()
        changed["candidate"]["revision"] = PRIOR
        self.assert_fails("different full revisions", facts_value=changed)


class AppInfoTests(unittest.TestCase):
    def test_build_number_and_version_are_read_from_simctl_appinfo(self):
        text = '{\n    CFBundleIdentifier = "com.boldfield.ohand.app";\n    CFBundleShortVersionString = "0.1.0";\n    CFBundleVersion = 412;\n}'
        self.assertEqual(verify.parse_appinfo(text), {"build_number": "412", "version": "0.1.0"})
        self.assertEqual(verify.parse_appinfo('CFBundleVersion = "7";')["build_number"], "7")
        self.assertIsNone(verify.parse_appinfo("nothing")["build_number"])


class CommandLineTests(unittest.TestCase):
    def test_exit_status_follows_the_checks_and_the_record_is_written(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "facts.json").write_text(json.dumps(facts()))
            (root / "expectations.json").write_text(json.dumps(expectations()))
            (root / "after.json").write_text(json.dumps(report()))
            arguments = [sys.executable, str(TOOL), "--facts", str(root / "facts.json"), "--expectations", str(root / "expectations.json"),
                         "--report-after", str(root / "after.json"), "--report-relaunch", str(root / "after.json"),
                         "--output", str(root / "record.json")]
            good = subprocess.run(arguments, capture_output=True, text=True)
            self.assertEqual(good.returncode, 0, good.stdout + good.stderr)
            self.assertEqual(json.loads((root / "record.json").read_text())["result"], "passed")
            broken = report()
            broken["pending_reminder_requests"] = []
            (root / "after.json").write_text(json.dumps(broken))
            bad = subprocess.run(arguments, capture_output=True, text=True)
            self.assertEqual(bad.returncode, 1)
            self.assertIn("FAIL", bad.stdout)
            self.assertEqual(json.loads((root / "record.json").read_text())["result"], "failed")

    def test_unreadable_input_is_a_distinct_failure(self):
        result = subprocess.run([sys.executable, str(TOOL), "--facts", "/nonexistent", "--expectations", "/nonexistent",
                                 "--report-after", "/nonexistent", "--report-relaunch", "/nonexistent", "--output", "/tmp/x"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
