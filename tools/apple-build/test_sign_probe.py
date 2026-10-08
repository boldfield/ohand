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
