"""Behaviour tests for sign_probe.py and the credential-free simulator build.

Everything runs the real scripts as subprocesses against synthetic security, xcodebuild, xcrun, devicectl
and mint stand-ins placed first on PATH. No real Apple tool, credential, profile or device is involved, and
nothing here is evidence of a real signed build.
"""

import datetime
import json
import os
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import sign_probe

TOOL_DIRECTORY = Path(__file__).resolve().parent
SIGN_PROBE_SCRIPT = TOOL_DIRECTORY / "sign_probe.py"
STUB_SCRIPT = TOOL_DIRECTORY / "stub_apple_tools.py"
REPO_ROOT = sign_probe.REPO_ROOT
BUILD_SIMULATOR_SCRIPT = REPO_ROOT / "ios" / "scripts" / "build-simulator.sh"

STUBBED_TOOLS = ("security", "xcodebuild", "xcrun", "devicectl", "mint")
BUNDLE_IDENTIFIER = "com.boldfield.ohand.probes.bridge"
SYNTHETIC_TEAM = "SYNTHTEAM9"
SYNTHETIC_PROFILE_UUID = "11111111-2222-3333-4444-555555555555"
SYNTHETIC_PROFILE_NAME = "Synthetic Development Profile Name"
SYNTHETIC_DEVICE_ID = "SYNTHETIC-DEVICE-ID-0001"
SYNTHETIC_HARDWARE_UDIDS = ["synthetic-udid-a", "synthetic-udid-b"]
USER_DATA_PROFILE_DIRECTORY = "Library/Developer/Xcode/UserData/Provisioning Profiles"
SYNTHETIC_P12_PASSWORD = "synthetic-p12-password"
PROFILE_CONTENT_SENTINEL = "PROFILE-CONTENT-SENTINEL-5150"
PROFILE_FILE_STEM = "downloaded-profile-file-name"
PROFILE_EXPIRY = datetime.datetime(2099, 1, 2, 3, 4, 5)
ORIGINAL_SEARCH_LIST = ["/synthetic/Library/Keychains/login.keychain-db"]
EVIDENCE_KEYS = {
    "schema", "status", "failed_stage", "collected_at", "build_revision", "working_tree_clean", "build_identifier",
    "scheme", "bundle_identifier", "route", "identity_class", "signing_style", "export_method", "toolchain",
    "profile", "device", "install", "private_evidence_reference",
}


def profile_plist(**overrides):
    profile = {
        "Name": SYNTHETIC_PROFILE_NAME,
        "UUID": SYNTHETIC_PROFILE_UUID,
        "TeamIdentifier": [SYNTHETIC_TEAM],
        "ExpirationDate": PROFILE_EXPIRY,
        "ProvisionedDevices": list(SYNTHETIC_HARDWARE_UDIDS),
        "Entitlements": {"application-identifier": f"{SYNTHETIC_TEAM}.{BUNDLE_IDENTIFIER}", "get-task-allow": True},
        "Sentinel": PROFILE_CONTENT_SENTINEL,
    }
    profile.update(overrides)
    return profile


class SigningToolTestCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.home = root / "home"
        self.state = root / "state"
        self.binaries = root / "bin"
        self.inputs = root / "inputs"
        self.evidence = root / "evidence"
        self.private = root / "private"
        self.elsewhere = root / "elsewhere"
        for directory in (self.home, self.state, self.binaries, self.inputs, self.elsewhere):
            directory.mkdir()
        for tool in STUBBED_TOOLS:
            wrapper = self.binaries / tool
            wrapper.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{STUB_SCRIPT}" {tool} "$@"\n')
            wrapper.chmod(0o755)
        self.p12_path = self.inputs / "identity.p12"
        self.p12_path.write_bytes(b"synthetic-p12")
        self.password_file = self.inputs / "password.txt"
        self.password_file.write_text(SYNTHETIC_P12_PASSWORD + "\n")
        self.write_profile()

    def write_profile(self, **overrides):
        self.profile_path = self.inputs / f"{PROFILE_FILE_STEM}.mobileprovision"
        self.profile_path.write_bytes(plistlib.dumps(profile_plist(**overrides)))

    def environment(self, **extra):
        environment = {
            "PATH": f"{self.binaries}:/usr/bin:/bin",
            "HOME": str(self.home),
            "STUB_STATE_DIR": str(self.state),
            "OHAND_PRIVATE_EVIDENCE_DIR": str(self.private),
            "OHAND_SIGNING_PROFILE_PATH": str(self.profile_path),
            "OHAND_SIGNING_CERT_PATH": str(self.p12_path),
            "OHAND_SIGNING_CERT_PASSWORD": SYNTHETIC_P12_PASSWORD,
            "OHAND_DEVICE_ID": SYNTHETIC_DEVICE_ID,
        }
        environment.update(extra)
        return {key: value for key, value in environment.items() if value is not None}

    def arguments(self, *extra):
        return [sys.executable, str(SIGN_PROBE_SCRIPT), "--evidence-dir", str(self.evidence), *extra]

    def sign(self, *extra, **environment_overrides):
        return subprocess.run(self.arguments(*extra), capture_output=True, text=True, cwd=self.elsewhere,
                              env=self.environment(**environment_overrides), timeout=60)

    def calls(self):
        path = self.state / "calls.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text().splitlines()]

    def call_names(self):
        return [(call["tool"], call["argv"][0]) for call in self.calls()]

    def calls_of(self, tool, first_argument):
        return [call for call in self.calls() if call["tool"] == tool and call["argv"][0] == first_argument]

    def evidence_files(self):
        return sorted(self.evidence.glob("*.json")) if self.evidence.is_dir() else []

    def only_evidence(self):
        files = self.evidence_files()
        self.assertEqual(len(files), 1)
        return json.loads(files[0].read_text())

    def all_text_surfaces(self, result):
        surfaces = [result.stdout, result.stderr]
        surfaces += [path.read_text() for path in self.evidence_files()]
        surfaces += [path.read_text() for path in self.private.rglob("*") if path.is_file()]
        return surfaces

    def state_reset(self):
        for path in self.state.iterdir():
            path.unlink()
        for path in self.evidence.glob("*.json") if self.evidence.is_dir() else []:
            path.unlink()

    def created_keychain_paths(self):
        return [Path(call["argv"][-1]) for call in self.calls_of("security", "create-keychain")]

    def assert_keychain_cleaned_up(self):
        created = self.created_keychain_paths()
        self.assertEqual(len(created), 1)
        self.assertFalse(created[0].exists())
        self.assertEqual(len(self.calls_of("security", "delete-keychain")), 1)
        restored = [call["argv"] for call in self.calls_of("security", "list-keychains") if "-s" in call["argv"]][-1]
        self.assertEqual(restored[restored.index("-s") + 1:], ORIGINAL_SEARCH_LIST)
        self.assertEqual(json.loads((self.state / "search-list.json").read_text()), ORIGINAL_SEARCH_LIST)

    def installed_profile_files(self):
        library = self.home / "Library"
        return sorted(str(path) for path in library.rglob("*.mobileprovision")) if library.exists() else []


class InputValidationTests(SigningToolTestCase):
    def assert_rejected_before_any_apple_tool(self, result, message):
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.evidence_files(), [])

    def test_profile_path_is_required(self):
        result = self.sign(OHAND_SIGNING_PROFILE_PATH=None)
        self.assert_rejected_before_any_apple_tool(result, "OHAND_SIGNING_PROFILE_PATH is required")

    def test_certificate_requires_a_password(self):
        result = self.sign(OHAND_SIGNING_CERT_PASSWORD=None)
        self.assert_rejected_before_any_apple_tool(
            result, "OHAND_SIGNING_CERT_PASSWORD or OHAND_SIGNING_CERT_PASSWORD_FILE is required")

    def test_install_requires_a_device_identifier(self):
        result = self.sign("--install", OHAND_DEVICE_ID=None)
        self.assert_rejected_before_any_apple_tool(result, "--install requires OHAND_DEVICE_ID")

    def test_inputs_inside_the_repository_are_refused(self):
        result = self.sign(OHAND_SIGNING_PROFILE_PATH=str(REPO_ROOT / "Makefile"))
        self.assert_rejected_before_any_apple_tool(result, "OHAND_SIGNING_PROFILE_PATH must point outside the repository")

    def test_missing_certificate_file_is_refused(self):
        result = self.sign(OHAND_SIGNING_CERT_PATH=str(self.inputs / "absent.p12"))
        self.assert_rejected_before_any_apple_tool(result, "OHAND_SIGNING_CERT_PATH does not point to a readable file")

    def test_private_evidence_inside_the_repository_is_refused(self):
        result = self.sign(OHAND_PRIVATE_EVIDENCE_DIR=str(REPO_ROOT / "private-evidence"))
        self.assertEqual(result.returncode, 2)
        self.assertIn("OHAND_PRIVATE_EVIDENCE_DIR must be outside the repository", result.stderr)
        self.assertFalse((REPO_ROOT / "private-evidence").exists())
        self.assertEqual(self.calls(), [])

    def test_device_label_must_be_content_free_shape(self):
        result = self.sign(OHAND_DEVICE_LABEL="Alice's iPhone")
        self.assert_rejected_before_any_apple_tool(result, "OHAND_DEVICE_LABEL must be 1-32 characters")

    def test_unsupported_scheme_is_refused(self):
        result = self.sign("--scheme", "CaptureProbe")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(self.calls(), [])

    def test_profiles_that_do_not_fit_the_development_route_are_refused(self):
        wrong_application_identifier = {"application-identifier": f"{SYNTHETIC_TEAM}.com.example.other", "get-task-allow": True}
        cases = {
            "profile has expired": {"ExpirationDate": datetime.datetime(2001, 1, 1)},
            "lists no devices": {"ProvisionedDevices": []},
            "enterprise profile": {"ProvisionsAllDevices": True},
            "Xcode managed": {"IsXcodeManaged": True, "Name": "iOS Team Provisioning Profile: *"},
            "not a Development profile": {"Entitlements": {
                "application-identifier": f"{SYNTHETIC_TEAM}.{BUNDLE_IDENTIFIER}", "get-task-allow": False}},
            "does not cover the probe's bundle identifier": {"Entitlements": wrong_application_identifier},
            "missing required field(s): UUID": {"UUID": ""},
        }
        for expected_message, overrides in cases.items():
            with self.subTest(expected_message):
                self.state_reset()
                self.write_profile(**overrides)
                result = self.sign()
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(expected_message, result.stderr)
                self.assertEqual(self.calls_of("security", "create-keychain"), [])
                self.assertEqual(self.calls_of("xcodebuild", "archive"), [])
                self.assertEqual(self.evidence_files(), [])

    def test_malformed_profile_uuids_are_refused_without_touching_the_filesystem(self):
        profiles_parent = self.home / USER_DATA_PROFILE_DIRECTORY / ".."
        outside_target = Path(self.temporary.name) / "outside"
        traversal_victim = (profiles_parent / "escaped.mobileprovision").resolve()
        traversal_victim.parent.mkdir(parents=True)
        absolute_victim = Path(f"{outside_target}.mobileprovision")
        for victim in (traversal_victim, absolute_victim):
            victim.write_bytes(b"maintainer-owned")
        cases = {
            "traversal": "../escaped",
            "absolute path": str(outside_target),
            "embedded separator": f"{SYNTHETIC_PROFILE_UUID}/../../escaped",
            "not a UUID": "not-a-uuid",
            "UUID with a suffix": SYNTHETIC_PROFILE_UUID + "x",
            "UUID without hyphens": SYNTHETIC_PROFILE_UUID.replace("-", ""),
            "non-string": 12345,
            "list": [SYNTHETIC_PROFILE_UUID],
        }
        for case, malformed_uuid in cases.items():
            with self.subTest(case):
                self.state_reset()
                self.write_profile(UUID=malformed_uuid)
                result = self.sign("--install")
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("provisioning profile UUID is not a canonical UUID", result.stderr)
                self.assertEqual(self.calls_of("security", "create-keychain"), [])
                self.assertEqual(self.calls_of("xcodebuild", "archive"), [])
                self.assertEqual(self.evidence_files(), [])
                self.assertFalse((self.home / USER_DATA_PROFILE_DIRECTORY).exists())
                for victim in (traversal_victim, absolute_victim):
                    self.assertEqual(victim.read_bytes(), b"maintainer-owned")
                self.assertEqual(sorted(path.name for path in traversal_victim.parent.iterdir()), [traversal_victim.name])

    def test_lowercase_canonical_uuid_is_accepted(self):
        self.write_profile(UUID=SYNTHETIC_PROFILE_UUID.replace("1", "a"))
        self.assertEqual(self.sign().returncode, 0)

    def test_non_string_profile_name_is_refused(self):
        self.write_profile(Name=["not", "a", "string"])
        result = self.sign()
        self.assertEqual(result.returncode, 2)
        self.assertIn("provisioning profile Name is not a string", result.stderr)
        self.assertEqual(self.calls_of("security", "create-keychain"), [])

    def test_wildcard_profile_covering_the_prefix_is_accepted(self):
        self.write_profile(Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.com.boldfield.ohand.*", "get-task-allow": True})
        self.assertEqual(self.sign().returncode, 0)

    def test_profile_covering_function_matches_exact_wildcard_and_prefix_only(self):
        covers = sign_probe.profile_covers_bundle
        self.assertTrue(covers("T.*", "T", BUNDLE_IDENTIFIER))
        self.assertTrue(covers(f"T.{BUNDLE_IDENTIFIER}", "T", BUNDLE_IDENTIFIER))
        self.assertTrue(covers("T.com.boldfield.*", "T", BUNDLE_IDENTIFIER))
        self.assertFalse(covers("T.com.boldfield.ohand.probes.audio", "T", BUNDLE_IDENTIFIER))
        self.assertFalse(covers("OTHER.*", "T", BUNDLE_IDENTIFIER))
        self.assertFalse(covers("T.com.other.*", "T", BUNDLE_IDENTIFIER))

    def test_evidence_directory_default_is_anchored_to_the_repository_root(self):
        expected = REPO_ROOT / "docs" / "validation" / "evidence" / "apple-signing"
        original = os.getcwd()
        try:
            os.chdir(self.elsewhere)
            default = sign_probe.parse_arguments([]).evidence_dir
        finally:
            os.chdir(original)
        self.assertEqual(Path(default), expected)
        self.assertTrue(Path(default).is_absolute())
        self.assertEqual(sign_probe.DEFAULT_EVIDENCE_DIR, expected)


class SigningRouteTests(SigningToolTestCase):
    def test_install_with_temporary_keychain_succeeds_and_cleans_up(self):
        result = self.sign("--install", OHAND_DEVICE_LABEL="phone-a")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("signed build complete; installed on device", result.stdout)

        names = self.call_names()
        for earlier, later in [
            (("security", "create-keychain"), ("security", "import")),
            (("security", "import"), ("security", "set-key-partition-list")),
            (("security", "set-key-partition-list"), ("xcodebuild", "archive")),
            (("xcodebuild", "archive"), ("xcodebuild", "-exportArchive")),
            (("xcodebuild", "-exportArchive"), ("devicectl", "device")),
            (("devicectl", "device"), ("security", "delete-keychain")),
        ]:
            self.assertLess(names.index(earlier), names.index(later), f"{earlier} must precede {later}")
        self.assert_keychain_cleaned_up()
        self.assertEqual(self.installed_profile_files(), [])

    def test_unlock_does_not_use_a_placeholder_password(self):
        self.assertEqual(self.sign().returncode, 0)
        create_password = self.calls_of("security", "create-keychain")[0]["argv"]
        unlock_password = self.calls_of("security", "unlock-keychain")[0]["argv"]
        password = create_password[create_password.index("-p") + 1]
        self.assertGreaterEqual(len(password), 16)
        self.assertEqual(unlock_password[unlock_password.index("-p") + 1], password)
        self.assertNotIn("dummy", password)

    def test_archive_is_unsigned_and_export_signs_with_explicit_options_by_profile_uuid(self):
        self.assertEqual(self.sign("--install").returncode, 0)
        archive_arguments = self.calls_of("xcodebuild", "archive")[0]["argv"]
        self.assertIn("CODE_SIGNING_ALLOWED=NO", archive_arguments)
        self.assertEqual(archive_arguments[archive_arguments.index("-project") + 1], str(REPO_ROOT / "ios" / "OhAnd.xcodeproj"))
        self.assertEqual(archive_arguments[archive_arguments.index("-scheme") + 1], "BridgeProbe")
        self.assertEqual(archive_arguments[archive_arguments.index("-destination") + 1], "generic/platform=iOS")
        for argument in archive_arguments:
            self.assertNotIn("PROVISIONING_PROFILE", argument)
            self.assertNotIn("CODE_SIGN_IDENTITY", argument)

        options = json.loads((self.state / "export-options.json").read_text())
        self.assertEqual(options, {
            "method": "debugging",
            "signingStyle": "manual",
            "signingCertificate": "Apple Development",
            "teamID": SYNTHETIC_TEAM,
            "provisioningProfiles": {BUNDLE_IDENTIFIER: SYNTHETIC_PROFILE_UUID},
        })
        self.assertNotIn(PROFILE_FILE_STEM, json.dumps(options))

    def test_profile_is_installed_under_its_uuid_during_export_only(self):
        self.assertEqual(self.sign().returncode, 0)
        export_call = self.calls_of("xcodebuild", "-exportArchive")[0]
        self.assertEqual(export_call["installed_profiles_at_call"],
                         [f"{USER_DATA_PROFILE_DIRECTORY}/{SYNTHETIC_PROFILE_UUID}.mobileprovision"])
        self.assertEqual(self.installed_profile_files(), [])

    def test_profile_goes_to_the_pinned_xcode_directory_even_when_the_legacy_directory_exists(self):
        legacy = self.home / "Library" / "MobileDevice" / "Provisioning Profiles"
        legacy.mkdir(parents=True)
        self.assertEqual(self.sign().returncode, 0)
        export_call = self.calls_of("xcodebuild", "-exportArchive")[0]
        self.assertEqual(export_call["installed_profiles_at_call"],
                         [f"{USER_DATA_PROFILE_DIRECTORY}/{SYNTHETIC_PROFILE_UUID}.mobileprovision"])
        self.assertEqual(list(legacy.iterdir()), [])

    def test_device_install_targets_the_device_and_a_signed_app_bundle(self):
        self.assertEqual(self.sign("--install").returncode, 0)
        install_arguments = self.calls_of("devicectl", "device")[0]["argv"]
        self.assertEqual(install_arguments[:4], ["device", "install", "app", "--device"])
        self.assertEqual(install_arguments[4], SYNTHETIC_DEVICE_ID)
        self.assertTrue(install_arguments[5].endswith("/Payload/Probe.app"))

    def test_without_install_flag_no_device_is_touched(self):
        result = self.sign(OHAND_DEVICE_ID=None)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("install not requested", result.stdout)
        self.assertEqual(self.calls_of("devicectl", "device"), [])
        self.assertEqual(self.only_evidence()["install"], {"requested": False, "performed": False, "succeeded": False})

    def test_existing_development_identity_is_used_without_creating_a_keychain(self):
        result = self.sign("--install", OHAND_SIGNING_CERT_PATH=None, OHAND_SIGNING_CERT_PASSWORD=None,
                           STUB_LOGIN_IDENTITIES="Apple Development: Synthetic Person (SYNTHETIC1)")
        self.assertEqual(result.returncode, 0, result.stderr)
        for command in ("create-keychain", "import", "delete-keychain"):
            self.assertEqual(self.calls_of("security", command), [])
        self.assertFalse([call for call in self.calls_of("security", "list-keychains") if "-s" in call["argv"]])
        self.assertEqual(len(self.calls_of("devicectl", "device")), 1)

    def test_no_usable_identity_in_user_keychains_is_refused(self):
        for identities, expected in [("", "found 0"), ("Apple Distribution: Synthetic Person (SYNTHETIC1)", "no Apple Development identity")]:
            with self.subTest(identities=identities):
                self.state_reset()
                result = self.sign(OHAND_SIGNING_CERT_PATH=None, OHAND_SIGNING_CERT_PASSWORD=None, STUB_LOGIN_IDENTITIES=identities)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(expected, result.stderr)
                self.assertEqual(self.calls_of("xcodebuild", "archive"), [])

    def test_distribution_identity_in_the_supplied_certificate_is_refused_and_cleaned_up(self):
        result = self.sign(STUB_IDENTITIES="Apple Distribution: Synthetic Person (SYNTHETIC1)")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("no Apple Development identity found", result.stderr)
        self.assertEqual(self.calls_of("xcodebuild", "archive"), [])
        self.assert_keychain_cleaned_up()

    def test_ambiguous_certificate_with_two_development_identities_is_refused(self):
        result = self.sign(STUB_IDENTITIES="Apple Development: One (A1),Apple Development: Two (B2)")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("exactly one valid Apple Development identity, found 2", result.stderr)
        self.assert_keychain_cleaned_up()

    def test_wrong_certificate_password_fails_and_cleans_up(self):
        result = self.sign(STUB_P12_PASSWORD="a-different-password")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("import-identity failed (exit status 1)", result.stderr)
        self.assert_keychain_cleaned_up()
        self.assertEqual(self.only_evidence()["failed_stage"], "import-identity")

    def test_password_may_be_injected_through_a_file(self):
        result = self.sign(OHAND_SIGNING_CERT_PASSWORD=None, OHAND_SIGNING_CERT_PASSWORD_FILE=str(self.password_file))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_injected_variables_are_not_passed_to_child_tools(self):
        child_environments = self.inputs / "environments.txt"
        wrapper = self.binaries / "xcodebuild"
        wrapper.write_text(f'#!/bin/sh\nenv >> "{child_environments}"\nexec "{sys.executable}" "{STUB_SCRIPT}" xcodebuild "$@"\n')
        wrapper.chmod(0o755)
        self.assertEqual(self.sign("--install").returncode, 0)
        captured = child_environments.read_text()
        self.assertIn("PATH=", captured)
        for forbidden in ("OHAND_SIGNING", "OHAND_DEVICE", SYNTHETIC_P12_PASSWORD, SYNTHETIC_DEVICE_ID):
            self.assertNotIn(forbidden, captured)


class FailureAndCleanupTests(SigningToolTestCase):
    def test_failure_at_each_stage_exits_nonzero_and_cleans_up_every_resource(self):
        stages = [
            ("security:create-keychain", "create-keychain"),
            ("security:import", "import-identity"),
            ("security:set-key-partition-list", "key-partition-list"),
            ("xcodebuild:archive", "archive"),
            ("xcodebuild:-exportArchive", "export"),
            ("devicectl:device", "install"),
        ]
        for failing_call, stage in stages:
            with self.subTest(failing_call):
                self.state_reset()
                result = self.sign("--install", STUB_FAIL=failing_call)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"{stage} failed (exit status 1)", result.stderr)
                self.assertNotIn("signed build complete", result.stdout)
                self.assert_keychain_cleaned_up()
                self.assertEqual(self.installed_profile_files(), [])
                evidence = self.only_evidence()
                self.assertEqual(evidence["status"], "failed")
                self.assertEqual(evidence["failed_stage"], stage)
                self.assertFalse(evidence["install"]["succeeded"])
                self.assertEqual(evidence["install"]["performed"], stage == "install")

    def test_unreadable_keychain_search_list_is_refused_before_it_is_changed(self):
        result = self.sign("--install", STUB_EMPTY_KEYCHAIN_LIST="1")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("could not read the user keychain search list", result.stderr)
        self.assertEqual(self.calls_of("security", "create-keychain"), [])
        self.assertEqual([call for call in self.calls_of("security", "list-keychains") if "-s" in call["argv"]], [])
        self.assertEqual(self.calls_of("xcodebuild", "archive"), [])
        self.assertFalse((self.state / "search-list.json").exists())
        self.assertEqual(self.installed_profile_files(), [])

    def test_failure_to_restore_the_search_list_fails_the_run_at_the_cleanup_stage(self):
        result = self.sign("--install", STUB_FAIL_RESTORE_SEARCH_LIST="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("could not restore the keychain search list", result.stderr)
        self.assertNotIn("signed build complete", result.stdout)
        created = self.created_keychain_paths()
        self.assertEqual(len(created), 1)
        self.assertFalse(created[0].exists())
        self.assertEqual(self.installed_profile_files(), [])
        evidence = self.only_evidence()
        self.assertEqual(set(evidence), EVIDENCE_KEYS)
        self.assertEqual((evidence["status"], evidence["failed_stage"]), ("failed", "cleanup"))
        self.assertTrue(evidence["install"]["succeeded"])

    def test_failed_stage_is_kept_when_cleanup_also_fails(self):
        result = self.sign("--install", STUB_FAIL="xcodebuild:archive", STUB_FAIL_RESTORE_SEARCH_LIST="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("archive failed", result.stderr)
        self.assertIn("could not restore the keychain search list", result.stderr)
        self.assertEqual(self.only_evidence()["failed_stage"], "archive")

    def test_failed_delete_keychain_still_removes_the_temporary_keychain_file(self):
        result = self.sign("--install", STUB_FAIL="security:delete-keychain")
        self.assertEqual(result.returncode, 0, result.stderr)
        created = self.created_keychain_paths()
        self.assertEqual(len(created), 1)
        self.assertFalse(created[0].exists())
        self.assertEqual(self.only_evidence()["status"], "succeeded")

    def run_in_process(self, **environment_overrides):
        run = sign_probe.SigningRun("BridgeProbe", True, self.evidence, self.environment(**environment_overrides))
        saved_handlers = {number: signal.getsignal(number) for number in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
        self.addCleanup(lambda: [signal.signal(number, handler) for number, handler in saved_handlers.items()])
        with mock.patch.dict(os.environ, {"HOME": str(self.home)}), mock.patch("sys.stderr") as stderr, mock.patch("sys.stdout"):
            exit_code = sign_probe.execute(run)
        printed = "".join(call.args[0] for call in stderr.write.call_args_list)
        return exit_code, printed

    def assert_failed_at_cleanup(self, exit_code, printed, message):
        self.assertEqual(exit_code, 1)
        self.assertIn(message, printed)
        evidence = self.only_evidence()
        self.assertEqual(set(evidence), EVIDENCE_KEYS)
        self.assertEqual((evidence["status"], evidence["failed_stage"]), ("failed", "cleanup"))

    def test_unremovable_temporary_keychain_fails_the_run_at_the_cleanup_stage(self):
        real_unlink = Path.unlink

        def refuse_keychain_unlink(path, *arguments, **keywords):
            if path.name.endswith(".keychain-db"):
                raise PermissionError("synthetic refusal")
            return real_unlink(path, *arguments, **keywords)

        with mock.patch.object(Path, "unlink", refuse_keychain_unlink):
            exit_code, printed = self.run_in_process(STUB_FAIL="security:delete-keychain")
        self.assert_failed_at_cleanup(exit_code, printed, "temporary keychain could not be removed")

    def test_unremovable_installed_profile_fails_the_run_at_the_cleanup_stage(self):
        real_unlink = Path.unlink

        def refuse_profile_unlink(path, *arguments, **keywords):
            if path.suffix == ".mobileprovision" and path.parent.name == "Provisioning Profiles":
                raise PermissionError("synthetic refusal")
            return real_unlink(path, *arguments, **keywords)

        with mock.patch.object(Path, "unlink", refuse_profile_unlink):
            exit_code, printed = self.run_in_process()
        self.assert_failed_at_cleanup(exit_code, printed, "installed provisioning profile could not be removed")

    def test_unremovable_working_directory_fails_the_run_at_the_cleanup_stage(self):
        real_rmtree = sign_probe.shutil.rmtree
        leftovers = []

        def refuse_rmtree(path, *arguments, **keywords):
            leftovers.append(path)

        with mock.patch.object(sign_probe.shutil, "rmtree", refuse_rmtree):
            exit_code, printed = self.run_in_process()
        for leftover in leftovers:
            real_rmtree(leftover, ignore_errors=True)
        self.assert_failed_at_cleanup(exit_code, printed, "temporary working directory could not be removed")

    def profile_directory_entries(self):
        directory = self.home / USER_DATA_PROFILE_DIRECTORY
        return sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []

    def run_with_profile_copy_failing_after_a_partial_write(self, failure):
        def partial_copy(source, destination, *arguments, **keywords):
            Path(destination).write_bytes(Path(source).read_bytes()[:7])
            raise failure

        with mock.patch.object(sign_probe.shutil, "copyfile", partial_copy):
            return self.run_in_process()

    def test_failed_profile_copy_leaves_no_partial_profile_and_fails_at_the_install_stage(self):
        exit_code, printed = self.run_with_profile_copy_failing_after_a_partial_write(OSError("synthetic disk full"))
        self.assertEqual(exit_code, 1)
        self.assertIn("could not install the provisioning profile", printed)
        self.assertEqual(self.profile_directory_entries(), [])
        self.assert_keychain_cleaned_up()
        self.assertEqual((self.only_evidence()["status"], self.only_evidence()["failed_stage"]), ("failed", "profile-install"))
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])

    def test_interrupted_profile_copy_leaves_no_partial_profile(self):
        exit_code, printed = self.run_with_profile_copy_failing_after_a_partial_write(sign_probe.Interrupted())
        self.assertEqual(exit_code, 1)
        self.assertIn("interrupted", printed)
        self.assertEqual(self.profile_directory_entries(), [])
        self.assert_keychain_cleaned_up()
        self.assertEqual(self.only_evidence()["failed_stage"], "interrupted")

    def test_failed_rename_onto_the_uuid_name_removes_the_partial_file(self):
        real_replace = os.replace

        def refuse_profile_rename(source, destination, *arguments, **keywords):
            if Path(destination).suffix == ".mobileprovision":
                raise OSError("synthetic rename failure")
            return real_replace(source, destination, *arguments, **keywords)

        with mock.patch.object(sign_probe.os, "replace", refuse_profile_rename):
            exit_code, printed = self.run_in_process()
        self.assertEqual(exit_code, 1)
        self.assertIn("could not install the provisioning profile", printed)
        self.assertEqual(self.profile_directory_entries(), [])
        self.assertEqual(self.only_evidence()["failed_stage"], "profile-install")

    def test_a_rerun_after_an_interrupted_copy_installs_the_full_profile_and_removes_it(self):
        self.run_with_profile_copy_failing_after_a_partial_write(sign_probe.Interrupted())
        self.state_reset()
        result = self.sign("--install")
        self.assertEqual(result.returncode, 0, result.stderr)
        export_call = self.calls_of("xcodebuild", "-exportArchive")[0]
        self.assertEqual(export_call["installed_profiles_at_call"],
                         [f"{USER_DATA_PROFILE_DIRECTORY}/{SYNTHETIC_PROFILE_UUID}.mobileprovision"])
        self.assertEqual(self.profile_directory_entries(), [])

    def test_a_different_profile_already_installed_under_the_same_uuid_is_refused_and_kept(self):
        directory = self.home / USER_DATA_PROFILE_DIRECTORY
        directory.mkdir(parents=True)
        existing = directory / f"{SYNTHETIC_PROFILE_UUID}.mobileprovision"
        existing.write_bytes(b"truncated")
        result = self.sign("--install")
        self.assertEqual(result.returncode, 2)
        self.assertIn("a different provisioning profile with the same UUID is already installed", result.stderr)
        self.assertEqual(existing.read_bytes(), b"truncated")
        self.assertEqual(self.profile_directory_entries(), [existing.name])
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])
        self.assert_keychain_cleaned_up()

    def test_an_identical_profile_already_installed_is_used_and_kept(self):
        directory = self.home / USER_DATA_PROFILE_DIRECTORY
        directory.mkdir(parents=True)
        existing = directory / f"{SYNTHETIC_PROFILE_UUID}.mobileprovision"
        existing.write_bytes(self.profile_path.read_bytes())
        result = self.sign("--install")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(existing.read_bytes(), self.profile_path.read_bytes())
        self.assertEqual(self.profile_directory_entries(), [existing.name])

    def test_unsigned_export_is_a_failure(self):
        result = self.sign("--install", STUB_UNSIGNED_EXPORT="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("exported app is not code-signed", result.stderr)
        self.assertEqual(self.calls_of("devicectl", "device"), [])
        self.assert_keychain_cleaned_up()

    def test_termination_during_build_cleans_up_the_keychain_search_list_and_profile(self):
        process = subprocess.Popen(self.arguments("--install"), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                   cwd=self.elsewhere, env=self.environment(STUB_HANG="xcodebuild:archive"))
        self.addCleanup(process.kill)
        deadline = time.time() + 30
        while not (self.state / "hanging").exists():
            self.assertLess(time.time(), deadline, "archive stub never started")
            time.sleep(0.05)
        process.send_signal(signal.SIGTERM)
        stdout, stderr = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 1, stderr)
        self.assertIn("interrupted", stderr)
        self.assert_keychain_cleaned_up()
        self.assertEqual(self.installed_profile_files(), [])
        self.assertEqual(self.only_evidence()["failed_stage"], "interrupted")

    def test_work_directories_are_removed(self):
        before = set(Path(tempfile.gettempdir()).glob("ohand-signing-*"))
        self.assertEqual(self.sign("--install").returncode, 0)
        self.assertEqual(set(Path(tempfile.gettempdir()).glob("ohand-signing-*")), before)


class RedactionAndEvidenceTests(SigningToolTestCase):
    SENSITIVE_VALUES = (SYNTHETIC_TEAM, SYNTHETIC_PROFILE_UUID, SYNTHETIC_PROFILE_NAME, SYNTHETIC_DEVICE_ID,
                        SYNTHETIC_P12_PASSWORD, PROFILE_CONTENT_SENTINEL, *SYNTHETIC_HARDWARE_UDIDS)

    def test_sensitive_values_never_reach_stdout_stderr_logs_or_evidence(self):
        result = self.sign("--install", STUB_ECHO_ARGS="1", STUB_LEAK_TEXT="stub error naming " + " and ".join(SYNTHETIC_HARDWARE_UDIDS))
        self.assertEqual(result.returncode, 0, result.stderr)
        keychain_password = self.calls_of("security", "create-keychain")[0]["argv"][2]
        for surface in self.all_text_surfaces(result):
            for sensitive in self.SENSITIVE_VALUES + (keychain_password,):
                self.assertNotIn(sensitive, surface)

    def test_input_file_names_carrying_profile_metadata_never_reach_any_surface(self):
        sensitive_stem = f"{SYNTHETIC_TEAM}-{SYNTHETIC_PROFILE_UUID}"
        self.profile_path = self.inputs / f"{sensitive_stem}.mobileprovision"
        self.profile_path.write_bytes(plistlib.dumps(profile_plist()))
        sensitive_p12 = self.inputs / f"{SYNTHETIC_PROFILE_NAME.replace(' ', '-')}.p12"
        sensitive_p12.write_bytes(b"synthetic-p12")
        result = self.sign("--install", OHAND_SIGNING_CERT_PATH=str(sensitive_p12), STUB_ECHO_ARGS="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        log = next(self.private.glob("*/run.log")).read_text()
        self.assertIn("<ohand-signing-profile-path>", log)
        self.assertIn("<ohand-signing-cert-path>", log)
        for surface in self.all_text_surfaces(result):
            for sensitive in (SYNTHETIC_TEAM, SYNTHETIC_PROFILE_UUID, SYNTHETIC_PROFILE_NAME.replace(" ", "-")):
                self.assertNotIn(sensitive, surface)

    def test_private_log_keeps_labelled_placeholders_so_it_is_still_debuggable(self):
        result = self.sign("--install", STUB_ECHO_ARGS="1", STUB_LEAK_TEXT=SYNTHETIC_HARDWARE_UDIDS[0])
        self.assertEqual(result.returncode, 0, result.stderr)
        log_files = list(self.private.glob("*/run.log"))
        self.assertEqual(len(log_files), 1)
        log = log_files[0].read_text()
        for placeholder in ("<certificate-password>", "<keychain-password>", "<device-id>", "<device-udid>"):
            self.assertIn(placeholder, log)
        self.assertEqual(log_files[0].stat().st_mode & 0o777, 0o600)
        self.assertEqual(log_files[0].parent.stat().st_mode & 0o777, 0o700)

    def test_failure_messages_do_not_echo_tool_output(self):
        result = self.sign("--install", STUB_FAIL="xcodebuild:archive", STUB_ECHO_ARGS="1")
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("synthetic xcodebuild failure", result.stdout + result.stderr)
        self.assertNotIn("stub saw", result.stdout + result.stderr)
        self.assertIn("synthetic xcodebuild failure", next(self.private.glob("*/run.log")).read_text())

    def test_evidence_record_has_the_documented_schema(self):
        result = self.sign("--install", OHAND_DEVICE_LABEL="phone-a")
        self.assertEqual(result.returncode, 0, result.stderr)
        evidence = self.only_evidence()
        revision = subprocess.run(["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()

        self.assertEqual(set(evidence), EVIDENCE_KEYS)
        self.assertEqual(evidence["schema"], "ohand.apple-signing-evidence.v1")
        self.assertEqual(evidence["status"], "succeeded")
        self.assertIsNone(evidence["failed_stage"])
        self.assertEqual(evidence["build_revision"], revision)
        self.assertIsInstance(evidence["working_tree_clean"], bool)
        self.assertRegex(evidence["collected_at"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        self.assertRegex(evidence["build_identifier"], rf"^{revision[:8]}-[0-9a-f]{{8}}$")
        self.assertEqual((evidence["scheme"], evidence["bundle_identifier"]), ("BridgeProbe", BUNDLE_IDENTIFIER))
        self.assertEqual((evidence["route"], evidence["identity_class"], evidence["signing_style"], evidence["export_method"]),
                         ("development", "Apple Development", "manual", "debugging"))
        self.assertEqual(evidence["toolchain"], {"xcode_version": "Xcode 26.6", "xcode_build": "17A000"})
        self.assertEqual(evidence["profile"], {"expires_at": "2099-01-02T03:04:05Z", "device_scoped": True})
        self.assertEqual(evidence["device"], {"label": "phone-a"})
        self.assertEqual(evidence["install"], {"requested": True, "performed": True, "succeeded": True})
        self.assertEqual(evidence["private_evidence_reference"], evidence["build_identifier"].split("-")[1])
        self.assertTrue((self.private / evidence["private_evidence_reference"] / "run.log").is_file())

    def test_unlabelled_device_gets_a_random_content_free_label(self):
        self.assertEqual(self.sign("--install").returncode, 0)
        self.assertRegex(self.only_evidence()["device"]["label"], r"^device-[0-9a-f]{8}$")

    def test_evidence_file_name_carries_the_collection_date(self):
        self.assertEqual(self.sign().returncode, 0)
        self.assertRegex(self.evidence_files()[0].name, r"^apple-signing-\d{8}T\d{6}Z-[0-9a-f]{8}\.json$")

    def test_evidence_is_refused_if_it_would_contain_a_sensitive_value(self):
        self.write_profile(TeamIdentifier=["BridgeProbe"],
                           Entitlements={"application-identifier": f"BridgeProbe.{BUNDLE_IDENTIFIER}", "get-task-allow": True})
        result = self.sign()
        self.assertEqual(result.returncode, 1)
        self.assertIn("sanitized evidence would contain a sensitive value; not written", result.stderr)
        self.assertEqual(self.evidence_files(), [])
        self.assert_keychain_cleaned_up()


APP_BUNDLE_IDENTIFIER = "com.boldfield.ohand.app"
EXTENSION_BUNDLE_IDENTIFIER = "com.boldfield.ohand.app.capture-control"
SECOND_EXTENSION_IDENTIFIER = "com.boldfield.ohand.app.second-extension"
NESTED_APPLICATION_IDENTIFIER = "com.boldfield.ohand.app.companion"
FRAMEWORK_BUNDLE_IDENTIFIERS = ("com.boldfield.ohand.core-bridge", "com.boldfield.ohand.services")
EXTENSION_PROFILE_UUID = "66666666-7777-8888-9999-000000000000"
EXTENSION_PROFILE_NAME = "Synthetic Extension Profile Name"
SHORTER_EXPIRY = datetime.datetime(2098, 6, 7, 8, 9, 10)
EMBEDDED_EVIDENCE_KEYS = EVIDENCE_KEYS | {"embedded_bundles", "launch"}
PROBE_SCHEMES = {
    "BridgeProbe": "com.boldfield.ohand.probes.bridge",
    "AudioProbe": "com.boldfield.ohand.probes.audio",
    "TranscriptionProbe": "com.boldfield.ohand.probes.transcription",
    "NotificationProbe": "com.boldfield.ohand.probes.notification",
}


class EmbeddedExtensionTests(SigningToolTestCase):
    def wildcard_entitlements(self):
        return {"application-identifier": f"{SYNTHETIC_TEAM}.com.boldfield.ohand.*", "get-task-allow": True}

    def exact_entitlements(self, bundle_identifier):
        return {"application-identifier": f"{SYNTHETIC_TEAM}.{bundle_identifier}", "get-task-allow": True}

    def setUp(self):
        super().setUp()
        self.write_profile(Entitlements=self.wildcard_entitlements())

    def sign_app(self, *extra, **environment_overrides):
        return self.sign("--scheme", "OhAndApp", *extra, **environment_overrides)

    def write_mapped_profiles(self, mapping):
        entries = {}
        for index, (bundle_identifier, overrides) in enumerate(mapping.items()):
            path = self.inputs / f"mapped-{index}.mobileprovision"
            fields = {"Entitlements": self.exact_entitlements(bundle_identifier), "UUID": f"0000000{index}-2222-3333-4444-555555555555"}
            fields.update(overrides)
            path.write_bytes(plistlib.dumps(profile_plist(**fields)))
            entries[bundle_identifier] = str(path)
        map_path = self.inputs / "profile-map.json"
        map_path.write_text(json.dumps(entries))
        return map_path

    def mapped_environment(self, map_path, **extra):
        return dict(OHAND_SIGNING_PROFILE_PATH=None, OHAND_SIGNING_PROFILE_MAP_PATH=str(map_path), **extra)

    def assert_refused_before_keychain_and_export(self, result, exit_code, message):
        # Coverage is decided from the unsigned archive, so the archive is built once. Nothing touches the keychain,
        # installs a profile, signs, exports or installs on the device before the refusal.
        self.assertEqual(result.returncode, exit_code, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertEqual(len(self.calls_of("xcodebuild", "archive")), 1)
        self.assertEqual(self.calls_of("security", "create-keychain"), [])
        self.assertEqual(self.calls_of("security", "import"), [])
        self.assertEqual([call for call in self.calls_of("security", "list-keychains") if "-s" in call["argv"]], [])
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])
        self.assertLessEqual(set(self.devicectl_steps()), {("info", "details")})
        self.assertEqual(self.installed_profile_files(), [])
        for sensitive in (SYNTHETIC_TEAM, SYNTHETIC_PROFILE_UUID, EXTENSION_PROFILE_UUID, PROFILE_CONTENT_SENTINEL):
            self.assertNotIn(sensitive, result.stderr + result.stdout)

    def devicectl_steps(self):
        return [tuple(call["argv"][1:3]) for call in self.calls_of("devicectl", "device")]

    def test_extension_under_one_wildcard_profile_is_signed_installed_and_launched(self):
        result = self.sign_app("--install", OHAND_DEVICE_LABEL="phone-a")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("signed build complete; installed on device", result.stdout)

        options = json.loads((self.state / "export-options.json").read_text())
        self.assertEqual(options["provisioningProfiles"],
                         {APP_BUNDLE_IDENTIFIER: SYNTHETIC_PROFILE_UUID, EXTENSION_BUNDLE_IDENTIFIER: SYNTHETIC_PROFILE_UUID})
        self.assertEqual((options["method"], options["signingStyle"], options["teamID"]), ("debugging", "manual", SYNTHETIC_TEAM))
        for framework_identifier in FRAMEWORK_BUNDLE_IDENTIFIERS:
            self.assertNotIn(framework_identifier, json.dumps(options))
        archive_arguments = self.calls_of("xcodebuild", "archive")[0]["argv"]
        self.assertEqual(archive_arguments[archive_arguments.index("-scheme") + 1], "OhAndApp")
        self.assertIn("CODE_SIGNING_ALLOWED=NO", archive_arguments)

        export_call = self.calls_of("xcodebuild", "-exportArchive")[0]
        self.assertEqual(export_call["installed_profiles_at_call"],
                         [f"{USER_DATA_PROFILE_DIRECTORY}/{SYNTHETIC_PROFILE_UUID}.mobileprovision"])
        self.assertEqual(self.installed_profile_files(), [])
        self.assert_keychain_cleaned_up()

    def test_install_is_followed_by_launch_by_bundle_identifier_and_a_running_check(self):
        self.assertEqual(self.sign_app("--install").returncode, 0)
        self.assertEqual(self.devicectl_steps(), [("info", "details"), ("install", "app"), ("process", "launch"), ("info", "processes")])
        calls = {tuple(call["argv"][1:3]): call["argv"] for call in self.calls_of("devicectl", "device")}
        launch_arguments = calls[("process", "launch")]
        self.assertEqual(launch_arguments[-1], APP_BUNDLE_IDENTIFIER)
        for step in (("process", "launch"), ("info", "processes"), ("install", "app")):
            self.assertEqual(calls[step][calls[step].index("--device") + 1], SYNTHETIC_DEVICE_ID)
        names = self.call_names()
        self.assertLess(names.index(("xcodebuild", "archive")), names.index(("security", "create-keychain")))
        steps = [(call["tool"], call["argv"][0], *call["argv"][1:3]) for call in self.calls()]
        self.assertLess(names.index(("xcodebuild", "-exportArchive")), steps.index(("devicectl", "device", "install", "app")))

    def test_the_signed_bundles_follow_the_archive_rather_than_a_fixed_list(self):
        result = self.sign_app(STUB_ARCHIVE_EXTENSIONS=f"{EXTENSION_BUNDLE_IDENTIFIER},{SECOND_EXTENSION_IDENTIFIER}")
        self.assertEqual(result.returncode, 0, result.stderr)
        options = json.loads((self.state / "export-options.json").read_text())
        self.assertEqual(set(options["provisioningProfiles"]),
                         {APP_BUNDLE_IDENTIFIER, EXTENSION_BUNDLE_IDENTIFIER, SECOND_EXTENSION_IDENTIFIER})

        self.state_reset()
        result = self.sign_app(STUB_ARCHIVE_EXTENSIONS="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.state / "export-options.json").read_text())["provisioningProfiles"],
                         {APP_BUNDLE_IDENTIFIER: SYNTHETIC_PROFILE_UUID})

    def test_bundles_nested_at_any_depth_including_inside_frameworks_need_a_covering_profile(self):
        nested = (f"Frameworks/OhAndServices.framework/PlugIns/Hidden.appex={SECOND_EXTENSION_IDENTIFIER},"
                  f"Watch/Companion.app={NESTED_APPLICATION_IDENTIFIER}")
        result = self.sign_app("--install", STUB_ARCHIVE_NESTED=nested)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.state / "export-options.json").read_text())["provisioningProfiles"], {
            identifier: SYNTHETIC_PROFILE_UUID
            for identifier in (APP_BUNDLE_IDENTIFIER, EXTENSION_BUNDLE_IDENTIFIER, SECOND_EXTENSION_IDENTIFIER, NESTED_APPLICATION_IDENTIFIER)})
        self.assertEqual([(bundle["bundle_identifier"], bundle["kind"]) for bundle in self.only_evidence()["embedded_bundles"]], [
            (APP_BUNDLE_IDENTIFIER, "application"), (EXTENSION_BUNDLE_IDENTIFIER, "app-extension"),
            (NESTED_APPLICATION_IDENTIFIER, "nested-application"), (SECOND_EXTENSION_IDENTIFIER, "app-extension")])
        for framework_identifier in FRAMEWORK_BUNDLE_IDENTIFIERS:
            self.assertNotIn(framework_identifier, (self.state / "export-options.json").read_text())

        # An exact mapping that leaves the extension hidden inside a framework uncovered is refused from the archive.
        self.state_reset()
        map_path = self.write_mapped_profiles({APP_BUNDLE_IDENTIFIER: {}, EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID}})
        result = self.sign_app("--install", STUB_ARCHIVE_NESTED=f"Frameworks/OhAndServices.framework/PlugIns/Hidden.appex={SECOND_EXTENSION_IDENTIFIER}",
                               **self.mapped_environment(map_path))
        self.assert_refused_before_keychain_and_export(
            result, 3, f"no supplied provisioning profile is mapped to bundle identifier {SECOND_EXTENSION_IDENTIFIER}")
        self.assertEqual(self.only_evidence()["failed_stage"], "bundle-coverage")

        self.state_reset()
        self.write_profile(Entitlements=self.exact_entitlements(APP_BUNDLE_IDENTIFIER))
        result = self.sign_app(STUB_ARCHIVE_EXTENSIONS="", STUB_ARCHIVE_NESTED=f"Watch/Companion.app={NESTED_APPLICATION_IDENTIFIER}")
        self.assert_refused_before_keychain_and_export(
            result, 3, f"no supplied provisioning profile covers bundle identifier {NESTED_APPLICATION_IDENTIFIER}")

    def test_archive_enumeration_fails_closed_before_keychain_and_export_work(self):
        (self.inputs / "Elsewhere.appex").mkdir()
        cases = {
            "XPC service": (dict(STUB_ARCHIVE_NESTED=f"XPCServices/Helper.xpc={SECOND_EXTENSION_IDENTIFIER}"),
                            "archived app embeds XPC service XPCServices/Helper.xpc, which this tool does not sign"),
            "XPC service inside a framework": (
                dict(STUB_ARCHIVE_NESTED=f"Frameworks/OhAndServices.framework/XPCServices/Helper.xpc={SECOND_EXTENSION_IDENTIFIER}"),
                "archived app embeds XPC service Frameworks/OhAndServices.framework/XPCServices/Helper.xpc"),
            "missing Info.plist": (dict(STUB_ARCHIVE_NESTED="PlugIns/Bare.appex="),
                                   "archived bundle PlugIns/Bare.appex has no readable bundle identifier"),
            "unreadable Info.plist": (dict(STUB_ARCHIVE_NESTED="Frameworks/OhAndServices.framework/PlugIns/Broken.appex=unreadable"),
                                      "archived bundle Frameworks/OhAndServices.framework/PlugIns/Broken.appex has no readable bundle identifier"),
            "invalid identifier": (dict(STUB_ARCHIVE_EXTENSIONS="bad identifier!"), "has an invalid bundle identifier"),
            "duplicate identifier": (dict(STUB_ARCHIVE_NESTED=f"Frameworks/OhAndServices.framework/PlugIns/Copy.appex={EXTENSION_BUNDLE_IDENTIFIER}"),
                                     "archive embeds two bundles with the same bundle identifier"),
            "symbolic link to a bundle": (dict(STUB_ARCHIVE_SYMLINK=f"PlugIns/Linked.appex={self.inputs / 'Elsewhere.appex'}"),
                                          "archived bundle PlugIns/Linked.appex is not a plain bundle directory"),
        }
        for description, (environment, message) in cases.items():
            with self.subTest(description):
                self.state_reset()
                result = self.sign_app("--install", **environment)
                self.assert_refused_before_keychain_and_export(result, 4, message)
                self.assertEqual(self.devicectl_steps(), [])
                self.assertEqual(self.only_evidence()["failed_stage"], "archive-inspect")

    def test_symbolic_links_anywhere_in_the_archived_app_are_refused_before_keychain_and_export_work(self):
        # Each layout hides a bundle, or could, behind a link the walk does not follow. Before this rule, a linked
        # framework carrying PlugIns/Hidden.appex and a linked top-level app were both accepted and the hidden
        # extension never reached provisioningProfiles. Every case is now exit 4 with no keychain or export work.
        outside = self.inputs / "outside"
        hidden_framework = outside / "Carrier.framework"
        hidden_plugins = outside / "PlugIns"
        for bundle in (hidden_framework / "PlugIns" / "Hidden.appex", hidden_plugins / "Hidden.appex"):
            bundle.mkdir(parents=True)
            (bundle / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": SECOND_EXTENSION_IDENTIFIER}))
        (outside / "notes.txt").write_text("synthetic")
        cases = {
            "top-level application is a link": (dict(STUB_ARCHIVE_APP_SYMLINK="1"),
                                                "archived app OhAndApp.app is not a plain bundle directory"),
            "linked framework hiding an extension": (dict(STUB_ARCHIVE_SYMLINK=f"Frameworks/Carrier.framework={hidden_framework}"),
                                                     "archived app contains a symbolic link at Frameworks/Carrier.framework"),
            "linked PlugIns directory hiding an extension": (
                dict(STUB_ARCHIVE_EXTENSIONS="", STUB_ARCHIVE_SYMLINK=f"PlugIns={hidden_plugins}"),
                "archived app contains a symbolic link at PlugIns"),
            "link inside the app to another of its directories": (
                dict(STUB_ARCHIVE_SYMLINK="Frameworks/Alias.framework=OhAndServices.framework"),
                "archived app contains a symbolic link at Frameworks/Alias.framework"),
            "linked file inside an extension": (dict(STUB_ARCHIVE_SYMLINK=f"PlugIns/Extension0.appex/notes.txt={outside / 'notes.txt'}"),
                                                "archived app contains a symbolic link at PlugIns/Extension0.appex/notes.txt"),
            "dangling link": (dict(STUB_ARCHIVE_SYMLINK=f"Resources/Dangling={outside / 'missing'}"),
                              "archived app contains a symbolic link at Resources/Dangling"),
        }
        for description, (environment, message) in cases.items():
            with self.subTest(description):
                self.state_reset()
                result = self.sign_app("--install", **environment)
                self.assert_refused_before_keychain_and_export(result, 4, message)
                self.assertEqual(self.devicectl_steps(), [])
                self.assertEqual(self.only_evidence()["failed_stage"], "archive-inspect")
                self.assertNotIn(SECOND_EXTENSION_IDENTIFIER, result.stdout + result.stderr)
                self.assertFalse((self.state / "export-options.json").exists())

    def test_two_bundles_are_signed_under_a_per_bundle_profile_mapping(self):
        map_path = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {"Name": SYNTHETIC_PROFILE_NAME},
            EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID, "Name": EXTENSION_PROFILE_NAME, "ExpirationDate": SHORTER_EXPIRY},
        })
        result = self.sign_app("--install", **self.mapped_environment(map_path))
        self.assertEqual(result.returncode, 0, result.stderr)
        options = json.loads((self.state / "export-options.json").read_text())
        self.assertEqual(options["provisioningProfiles"],
                         {APP_BUNDLE_IDENTIFIER: "00000000-2222-3333-4444-555555555555", EXTENSION_BUNDLE_IDENTIFIER: EXTENSION_PROFILE_UUID})
        self.assertEqual(
            self.calls_of("xcodebuild", "-exportArchive")[0]["installed_profiles_at_call"],
            sorted(f"{USER_DATA_PROFILE_DIRECTORY}/{uuid}.mobileprovision" for uuid in ("00000000-2222-3333-4444-555555555555", EXTENSION_PROFILE_UUID)))
        self.assertEqual(self.installed_profile_files(), [])
        self.assert_keychain_cleaned_up()

        evidence = self.only_evidence()
        self.assertEqual(evidence["embedded_bundles"], [
            {"bundle_identifier": APP_BUNDLE_IDENTIFIER, "kind": "application", "profile_expires_at": "2099-01-02T03:04:05Z"},
            {"bundle_identifier": EXTENSION_BUNDLE_IDENTIFIER, "kind": "app-extension", "profile_expires_at": "2098-06-07T08:09:10Z"},
        ])
        self.assertEqual(evidence["profile"]["expires_at"], "2098-06-07T08:09:10Z")

    def test_extension_not_covered_by_the_supplied_profile_is_refused_before_keychain_work(self):
        self.write_profile(Entitlements=self.exact_entitlements(APP_BUNDLE_IDENTIFIER))
        result = self.sign_app("--install")
        self.assert_refused_before_keychain_and_export(
            result, 3, f"no supplied provisioning profile covers bundle identifier {EXTENSION_BUNDLE_IDENTIFIER}")
        evidence = self.only_evidence()
        self.assertEqual((evidence["status"], evidence["failed_stage"]), ("failed", "bundle-coverage"))
        self.assertFalse(evidence["install"]["performed"])

    def test_covering_profile_that_cannot_sign_a_bundle_is_refused_with_a_distinct_exit_status(self):
        cases = {
            "has expired": {"ExpirationDate": datetime.datetime(2001, 1, 1)},
            "is Xcode managed": {"IsXcodeManaged": True, "Name": "iOS Team Provisioning Profile: *"},
            "lists no devices": {"ProvisionedDevices": []},
            "is not a Development profile": {"Entitlements": {**self.wildcard_entitlements(), "get-task-allow": False}},
            "is an enterprise profile": {"ProvisionsAllDevices": True},
        }
        for fragment, overrides in cases.items():
            with self.subTest(fragment):
                self.state_reset()
                self.write_profile(**{"Entitlements": self.wildcard_entitlements(), **overrides})
                result = self.sign_app("--install")
                self.assert_refused_before_keychain_and_export(
                    result, 3, f"provisioning profile covering bundle identifier {APP_BUNDLE_IDENTIFIER} {fragment}")
                self.assertNotEqual(result.returncode, 1)
                self.assertNotEqual(result.returncode, 2)

    def test_covering_profile_that_does_not_list_the_target_device_is_refused(self):
        result = self.sign_app("--install", STUB_DEVICE_UDID="some-other-udid")
        self.assert_refused_before_keychain_and_export(
            result, 3, f"provisioning profile covering bundle identifier {APP_BUNDLE_IDENTIFIER} does not list the target device")
        self.assertEqual(self.devicectl_steps(), [("info", "details")])
        self.assertNotIn("some-other-udid", result.stderr + result.stdout)

    def test_target_device_check_accepts_an_injected_hardware_identifier_without_asking_devicectl(self):
        self.assertEqual(self.sign_app("--install", OHAND_DEVICE_UDID=SYNTHETIC_HARDWARE_UDIDS[1], STUB_DEVICE_UDID="ignored").returncode, 0)
        self.assertNotIn(("info", "details"), self.devicectl_steps())
        self.state_reset()
        result = self.sign_app("--install", OHAND_DEVICE_UDID="not-listed")
        self.assert_refused_before_keychain_and_export(result, 3, "does not list the target device")

    def test_unreadable_target_device_is_an_input_error_before_keychain_work(self):
        result = self.sign_app("--install", STUB_DEVICECTL_FAIL="details")
        self.assert_refused_before_keychain_and_export(result, 2, "set OHAND_DEVICE_UDID")

    def test_device_listing_is_not_checked_without_install(self):
        result = self.sign_app(OHAND_DEVICE_ID=None, STUB_DEVICE_UDID="some-other-udid")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls_of("devicectl", "device"), [])
        evidence = self.only_evidence()
        self.assertEqual(evidence["launch"], {"requested": False, "performed": False, "launched": False, "running_confirmed": False})

    def test_mapping_refusals_name_only_the_bundle_identifier(self):
        app_only = self.write_mapped_profiles({APP_BUNDLE_IDENTIFIER: {}})
        result = self.sign_app(**self.mapped_environment(app_only))
        self.assert_refused_before_keychain_and_export(
            result, 3, f"no supplied provisioning profile is mapped to bundle identifier {EXTENSION_BUNDLE_IDENTIFIER}")

        self.state_reset()
        wrong_profile = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {},
            EXTENSION_BUNDLE_IDENTIFIER: {"Entitlements": self.exact_entitlements(APP_BUNDLE_IDENTIFIER), "UUID": EXTENSION_PROFILE_UUID},
        })
        result = self.sign_app(**self.mapped_environment(wrong_profile))
        self.assert_refused_before_keychain_and_export(
            result, 3, f"no supplied provisioning profile covers bundle identifier {EXTENSION_BUNDLE_IDENTIFIER}")

        self.state_reset()
        expired = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {},
            EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID, "ExpirationDate": datetime.datetime(2001, 1, 1)},
        })
        result = self.sign_app(**self.mapped_environment(expired))
        self.assert_refused_before_keychain_and_export(
            result, 3, f"provisioning profile covering bundle identifier {EXTENSION_BUNDLE_IDENTIFIER} has expired")

    def test_mapping_inputs_are_validated(self):
        map_path = self.write_mapped_profiles({APP_BUNDLE_IDENTIFIER: {}, EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID}})
        both = self.sign_app(OHAND_SIGNING_PROFILE_MAP_PATH=str(map_path))
        self.assertEqual(both.returncode, 2)
        self.assertIn("set only one of OHAND_SIGNING_PROFILE_PATH and OHAND_SIGNING_PROFILE_MAP_PATH", both.stderr)

        probe = self.sign("--scheme", "BridgeProbe", **self.mapped_environment(map_path))
        self.assertEqual(probe.returncode, 2)
        self.assertIn("only supported for schemes that embed extensions", probe.stderr)

        stray = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {}, EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID}, SECOND_EXTENSION_IDENTIFIER: {},
        })
        result = self.sign_app(**self.mapped_environment(stray))
        self.assertEqual(result.returncode, 2)
        self.assertIn(f"names bundle identifier {SECOND_EXTENSION_IDENTIFIER}, which is not an app or extension bundle of OhAndApp", result.stderr)
        self.assertEqual(self.calls_of("security", "create-keychain"), [])
        self.assertEqual(self.installed_profile_files(), [])

        inside = self.inputs / "inside.json"
        inside.write_text(json.dumps({APP_BUNDLE_IDENTIFIER: str(REPO_ROOT / "Makefile")}))
        result = self.sign_app(**self.mapped_environment(inside))
        self.assertEqual(result.returncode, 2)
        self.assertIn("must point outside the repository", result.stderr)

        malformed = self.inputs / "malformed.json"
        malformed.write_text("[1, 2]")
        result = self.sign_app(**self.mapped_environment(malformed))
        self.assertEqual(result.returncode, 2)
        self.assertIn("must hold a JSON object", result.stderr)

        self.assertEqual(self.calls_of("security", "create-keychain"), [])
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])

    def test_profiles_from_different_teams_are_refused(self):
        map_path = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {},
            EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID, "TeamIdentifier": ["OTHERTEAM1"]},
        })
        result = self.sign_app(**self.mapped_environment(map_path))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("belong to different teams", result.stderr)
        self.assertNotIn("OTHERTEAM1", result.stderr)
        self.assertEqual(self.calls_of("xcodebuild", "archive"), [])

    def test_missing_profile_input_names_both_variables(self):
        result = self.sign_app(OHAND_SIGNING_PROFILE_PATH=None)
        self.assertEqual(result.returncode, 2)
        self.assertIn("OHAND_SIGNING_PROFILE_PATH is required", result.stderr)
        self.assertIn("OHAND_SIGNING_PROFILE_MAP_PATH", result.stderr)

    def test_cleanup_runs_after_a_failure_once_the_keychain_exists(self):
        stages = [("security:set-key-partition-list", "key-partition-list"), ("xcodebuild:-exportArchive", "export")]
        for failing_call, stage in stages:
            with self.subTest(failing_call):
                self.state_reset()
                result = self.sign_app("--install", STUB_FAIL=failing_call)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"{stage} failed (exit status 1)", result.stderr)
                self.assert_keychain_cleaned_up()
                self.assertEqual(self.installed_profile_files(), [])
                self.assertEqual(self.only_evidence()["failed_stage"], stage)

    def test_failed_install_does_not_launch(self):
        result = self.sign_app("--install", STUB_DEVICECTL_FAIL="install")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("install failed (exit status 1)", result.stderr)
        self.assertEqual(self.devicectl_steps(), [("info", "details"), ("install", "app")])
        evidence = self.only_evidence()
        self.assertEqual(evidence["install"], {"requested": True, "performed": True, "succeeded": False})
        self.assertEqual(evidence["launch"], {"requested": True, "performed": False, "launched": False, "running_confirmed": False})
        self.assert_keychain_cleaned_up()

    def test_failed_launch_exits_non_zero_and_is_recorded(self):
        result = self.sign_app("--install", STUB_DEVICECTL_FAIL="launch")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("launch failed (exit status 1)", result.stderr)
        self.assertNotIn("signed build complete", result.stdout)
        self.assertNotIn(("info", "processes"), self.devicectl_steps())
        evidence = self.only_evidence()
        self.assertEqual((evidence["status"], evidence["failed_stage"]), ("failed", "launch"))
        self.assertEqual(evidence["install"], {"requested": True, "performed": True, "succeeded": True})
        self.assertEqual(evidence["launch"], {"requested": True, "performed": True, "launched": False, "running_confirmed": False})
        self.assert_keychain_cleaned_up()
        self.assertEqual(self.installed_profile_files(), [])

    def test_app_that_is_not_running_after_launch_exits_non_zero(self):
        for variable, value, stage in [("STUB_NOT_RUNNING", "1", "running-check"), ("STUB_DEVICECTL_FAIL", "processes", "running-check")]:
            with self.subTest(variable):
                self.state_reset()
                result = self.sign_app("--install", **{variable: value})
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertNotIn("signed build complete", result.stdout)
                evidence = self.only_evidence()
                self.assertEqual((evidence["status"], evidence["failed_stage"]), ("failed", stage))
                self.assertEqual(evidence["launch"], {"requested": True, "performed": True, "launched": True, "running_confirmed": False})

    def test_unsigned_extension_in_the_export_is_a_failure(self):
        result = self.sign_app("--install", STUB_UNSIGNED_EXTENSION="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("an exported embedded bundle is not code-signed", result.stderr)
        self.assertEqual(self.devicectl_steps(), [("info", "details")])
        self.assert_keychain_cleaned_up()

    def test_evidence_record_is_schema_v2_with_bundles_expiry_and_launch_result(self):
        result = self.sign_app("--install", OHAND_DEVICE_LABEL="phone-a")
        self.assertEqual(result.returncode, 0, result.stderr)
        evidence = self.only_evidence()
        self.assertEqual(set(evidence), EMBEDDED_EVIDENCE_KEYS)
        self.assertEqual(evidence["schema"], "ohand.apple-signing-evidence.v2")
        self.assertEqual((evidence["scheme"], evidence["bundle_identifier"]), ("OhAndApp", APP_BUNDLE_IDENTIFIER))
        self.assertEqual(evidence["embedded_bundles"], [
            {"bundle_identifier": APP_BUNDLE_IDENTIFIER, "kind": "application", "profile_expires_at": "2099-01-02T03:04:05Z"},
            {"bundle_identifier": EXTENSION_BUNDLE_IDENTIFIER, "kind": "app-extension", "profile_expires_at": "2099-01-02T03:04:05Z"},
        ])
        self.assertEqual(evidence["profile"], {"expires_at": "2099-01-02T03:04:05Z", "device_scoped": True})
        self.assertEqual(evidence["install"], {"requested": True, "performed": True, "succeeded": True})
        self.assertEqual(evidence["launch"], {"requested": True, "performed": True, "launched": True, "running_confirmed": True})
        self.assertEqual(evidence["status"], "succeeded")
        self.assertIsNone(evidence["failed_stage"])

    def test_new_fields_and_inputs_never_leak_sensitive_values(self):
        map_path = self.write_mapped_profiles({
            APP_BUNDLE_IDENTIFIER: {"Name": SYNTHETIC_PROFILE_NAME},
            EXTENSION_BUNDLE_IDENTIFIER: {"UUID": EXTENSION_PROFILE_UUID, "Name": EXTENSION_PROFILE_NAME},
        })
        udid_text = " and ".join(SYNTHETIC_HARDWARE_UDIDS)
        result = self.sign_app("--install", OHAND_DEVICE_UDID=SYNTHETIC_HARDWARE_UDIDS[0], STUB_ECHO_ARGS="1",
                               STUB_LEAK_TEXT="stub error naming " + udid_text, **self.mapped_environment(map_path))
        self.assertEqual(result.returncode, 0, result.stderr)
        sensitive_values = RedactionAndEvidenceTests.SENSITIVE_VALUES + (
            EXTENSION_PROFILE_UUID, "00000000-2222-3333-4444-555555555555", EXTENSION_PROFILE_NAME, "mapped-0", "mapped-1")
        for surface in self.all_text_surfaces(result):
            for sensitive in sensitive_values:
                self.assertNotIn(sensitive, surface)
        log = next(self.private.glob("*/run.log")).read_text()
        self.assertIn("<ohand-signing-profile-map-entry>", log)

    def test_refusal_messages_for_a_failed_run_still_exclude_the_team_and_device_identifiers(self):
        self.write_profile(Entitlements=self.exact_entitlements(APP_BUNDLE_IDENTIFIER), ExpirationDate=datetime.datetime(2001, 1, 1))
        result = self.sign_app("--install", STUB_ECHO_ARGS="1")
        self.assert_refused_before_keychain_and_export(
            result, 3, f"provisioning profile covering bundle identifier {APP_BUNDLE_IDENTIFIER} has expired")
        for surface in self.all_text_surfaces(result):
            for sensitive in (SYNTHETIC_TEAM, SYNTHETIC_PROFILE_UUID, SYNTHETIC_DEVICE_ID, *SYNTHETIC_HARDWARE_UDIDS):
                self.assertNotIn(sensitive, surface)


class ProbeSchemesUnchangedTests(SigningToolTestCase):
    def test_every_probe_scheme_still_signs_one_bundle_installs_without_launching_and_writes_a_v1_record(self):
        for scheme, bundle_identifier in PROBE_SCHEMES.items():
            with self.subTest(scheme):
                self.state_reset()
                self.write_profile(Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.{bundle_identifier}", "get-task-allow": True})
                result = self.sign("--scheme", scheme, "--install", OHAND_DEVICE_LABEL="phone-a")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("signed build complete; installed on device", result.stdout)
                options = json.loads((self.state / "export-options.json").read_text())
                self.assertEqual(options, {
                    "method": "debugging", "signingStyle": "manual", "signingCertificate": "Apple Development",
                    "teamID": SYNTHETIC_TEAM, "provisioningProfiles": {bundle_identifier: SYNTHETIC_PROFILE_UUID}})
                device_steps = [tuple(call["argv"][1:3]) for call in self.calls_of("devicectl", "device")]
                self.assertEqual(device_steps, [("install", "app")])
                evidence = self.only_evidence()
                self.assertEqual(set(evidence), EVIDENCE_KEYS)
                self.assertEqual(evidence["schema"], "ohand.apple-signing-evidence.v1")
                self.assertEqual((evidence["scheme"], evidence["bundle_identifier"]), (scheme, bundle_identifier))
                self.assertEqual(evidence["install"], {"requested": True, "performed": True, "succeeded": True})
                self.assertEqual(evidence["profile"], {"expires_at": "2099-01-02T03:04:05Z", "device_scoped": True})
                names = self.call_names()
                self.assertLess(names.index(("security", "set-key-partition-list")), names.index(("xcodebuild", "archive")))
                self.assert_keychain_cleaned_up()

    def test_probe_refuses_a_profile_for_another_bundle_with_the_original_exit_status_and_message(self):
        self.write_profile(Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.com.boldfield.ohand.probes.audio", "get-task-allow": True})
        result = self.sign("--scheme", "BridgeProbe")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("provisioning profile does not cover the probe's bundle identifier", result.stderr)
        self.assertEqual(self.calls_of("xcodebuild", "archive"), [])


class ProfileParsingTests(unittest.TestCase):
    def test_covering_profile_problem_reports_each_unusable_state_and_none_for_a_usable_profile(self):
        now = datetime.datetime(2026, 10, 11, tzinfo=datetime.timezone.utc)

        def info(**overrides):
            fields = profile_plist(Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.*", "get-task-allow": True}, **overrides)
            parsed = sign_probe.read_profile_basics(fields)
            sign_probe.normalize_expiry(parsed)
            return parsed

        self.assertIsNone(sign_probe.covering_profile_problem(info(), now))
        self.assertIn("Xcode managed", sign_probe.covering_profile_problem(info(IsXcodeManaged=True), now))
        self.assertIn("has expired", sign_probe.covering_profile_problem(info(ExpirationDate=datetime.datetime(2020, 1, 1)), now))
        self.assertIn("lists no devices", sign_probe.covering_profile_problem(info(ProvisionedDevices=[]), now))
        self.assertTrue(info().covers(EXTENSION_BUNDLE_IDENTIFIER))
        self.assertFalse(info(TeamIdentifier=["OTHER"]).covers(EXTENSION_BUNDLE_IDENTIFIER))

    def test_non_date_expiry_is_an_input_error_for_embedding_profiles(self):
        parsed = sign_probe.read_profile_basics(profile_plist(ExpirationDate="tomorrow"))
        with self.assertRaises(sign_probe.SigningError) as context:
            sign_probe.normalize_expiry(parsed)
        self.assertEqual(context.exception.exit_code, sign_probe.EXIT_INPUT)


class ArchiveEnumerationTests(unittest.TestCase):
    """Direct checks of the archive walk, for conditions a stub archive cannot express portably."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.application = Path(self.temporary.name) / "OhAnd.app"
        self.plugins = self.application / "PlugIns"
        extension = self.plugins / "Extension.appex"
        extension.mkdir(parents=True)
        (self.application / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": APP_BUNDLE_IDENTIFIER}))
        (extension / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": EXTENSION_BUNDLE_IDENTIFIER}))

    def assert_refused(self, message):
        with self.assertRaises(sign_probe.SigningError) as context:
            sign_probe.SigningRun._signed_bundle_paths(self.application)
        self.assertEqual((context.exception.exit_code, context.exception.stage), (sign_probe.EXIT_TOOLING_INTEGRITY, "archive-inspect"))
        self.assertIn(message, context.exception.message)

    def test_a_readable_app_enumerates_the_application_and_its_extension(self):
        found = sign_probe.SigningRun._signed_bundle_paths(self.application)
        self.assertEqual(found, [(self.application, "application"), (self.plugins / "Extension.appex", "app-extension")])

    def test_a_walk_error_refuses_instead_of_skipping_the_directory(self):
        # os.walk ignores an unreadable directory unless onerror is given; the walk must turn that into a refusal.
        real_walk = os.walk

        def walk_reporting_an_unreadable_plugins_directory(top, **keywords):
            keywords["onerror"](PermissionError(13, "Permission denied", str(self.plugins)))
            return real_walk(top, **keywords)

        with mock.patch.object(sign_probe.os, "walk", walk_reporting_an_unreadable_plugins_directory):
            self.assert_refused("archived app directory PlugIns could not be read")

    @unittest.skipIf(os.geteuid() == 0, "a superuser can read a directory with no permission bits")
    def test_a_directory_without_read_permission_refuses(self):
        self.plugins.chmod(0o000)
        self.addCleanup(self.plugins.chmod, 0o700)
        self.assert_refused("archived app directory PlugIns could not be read")

    def test_an_application_that_is_a_link_or_a_file_is_refused(self):
        linked = Path(self.temporary.name) / "Linked.app"
        linked.symlink_to(self.application)
        with self.assertRaises(sign_probe.SigningError) as context:
            sign_probe.SigningRun._signed_bundle_paths(linked)
        self.assertEqual(context.exception.exit_code, sign_probe.EXIT_TOOLING_INTEGRITY)
        self.assertIn("archived app Linked.app is not a plain bundle directory", context.exception.message)
        plain_file = Path(self.temporary.name) / "File.app"
        plain_file.write_text("not a directory")
        with self.assertRaises(sign_probe.SigningError) as context:
            sign_probe.SigningRun._signed_bundle_paths(plain_file)
        self.assertIn("archived app File.app is not a plain bundle directory", context.exception.message)


class SimulatorPathTests(unittest.TestCase):
    def test_unsigned_simulator_build_ignores_signing_inputs_and_uses_no_signing_tools(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries, state = root / "bin", root / "state"
            binaries.mkdir()
            state.mkdir()
            for tool in STUBBED_TOOLS:
                wrapper = binaries / tool
                wrapper.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{STUB_SCRIPT}" {tool} "$@"\n')
                wrapper.chmod(0o755)
            hostile = {
                "OHAND_SIGNING_PROFILE_PATH": "/synthetic/profile.mobileprovision",
                "OHAND_SIGNING_CERT_PATH": "/synthetic/identity.p12",
                "OHAND_SIGNING_CERT_PASSWORD": SYNTHETIC_P12_PASSWORD,
                "OHAND_DEVICE_ID": SYNTHETIC_DEVICE_ID,
            }
            environment = {"PATH": f"{binaries}:/usr/bin:/bin", "HOME": str(root), "STUB_STATE_DIR": str(state), **hostile}
            result = subprocess.run(["bash", str(BUILD_SIMULATOR_SCRIPT), "BridgeProbe"], capture_output=True, text=True,
                                    env=environment, cwd=root, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = [json.loads(line) for line in (state / "calls.jsonl").read_text().splitlines()]

        self.assertEqual({call["tool"] for call in calls}, {"mint", "xcodebuild"})
        build_arguments = [call["argv"] for call in calls if call["tool"] == "xcodebuild"][0]
        self.assertEqual(build_arguments[0], "build")
        self.assertIn("CODE_SIGNING_ALLOWED=NO", build_arguments)
        self.assertEqual(build_arguments[build_arguments.index("-sdk") + 1], "iphonesimulator")
        joined = " ".join(build_arguments) + result.stdout + result.stderr
        for forbidden in list(hostile.values()) + ["PROVISIONING_PROFILE", "CODE_SIGN_IDENTITY", "DEVELOPMENT_TEAM"]:
            self.assertNotIn(forbidden, joined)


if __name__ == "__main__":
    unittest.main()
