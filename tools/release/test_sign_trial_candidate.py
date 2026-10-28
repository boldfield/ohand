"""Behaviour tests for sign_trial_candidate.py against the stubbed Apple tools of tools/apple-build (P08a).

Everything is synthetic. No real Apple tool, credential, profile or device is involved, and nothing here is evidence of
a real signed build.
"""

import hashlib
import json
import plistlib
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

APPLE_BUILD = Path(__file__).resolve().parents[1] / "apple-build"
sys.path.insert(0, str(APPLE_BUILD))

from test_sign_probe import (  # noqa: E402
    APP_BUNDLE_IDENTIFIER, EXTENSION_BUNDLE_IDENTIFIER, PROFILE_CONTENT_SENTINEL, SYNTHETIC_DEVICE_ID,
    SYNTHETIC_PROFILE_NAME, SYNTHETIC_PROFILE_UUID, SYNTHETIC_TEAM, SigningToolTestCase, profile_plist)

import sign_trial_candidate  # noqa: E402

CANDIDATE_SCRIPT = Path(__file__).resolve().parent / "sign_trial_candidate.sh"
REAL_GENERATOR = Path(__file__).resolve().parents[2] / "ios" / "scripts" / "generate.sh"
INFO_PLIST = Path("ios") / "AppAssembly" / "Info.plist"
SYNTHETIC_IDENTITY_FRAGMENT = "Synthetic Person"


def git(repo, *arguments):
    return subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=Synthetic", "-c", "user.email=synthetic@example.invalid",
         "-c", "commit.gpgsign=false", *arguments], capture_output=True, text=True, check=True).stdout.strip()


class CandidateTestCase(SigningToolTestCase):
    def setUp(self):
        super().setUp()
        self.write_profile(Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.com.boldfield.ohand.*",
                                         "get-task-allow": True})
        self.checkout = self.elsewhere / "checkout"
        (self.checkout / INFO_PLIST).parent.mkdir(parents=True)
        (self.checkout / INFO_PLIST).write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.0"}))
        # The archive step runs the certified checkout's own generator script, so the synthetic checkout carries a copy.
        generator = self.checkout / "ios" / "scripts" / "generate.sh"
        generator.parent.mkdir(parents=True)
        shutil.copy2(REAL_GENERATOR, generator)
        subprocess.run(["git", "init", "-q", "-b", "main", str(self.checkout)], check=True)
        git(self.checkout, "add", "-A")
        git(self.checkout, "commit", "-q", "-m", "synthetic")
        self.output = self.elsewhere / "candidate-output"
        self.records = self.elsewhere / "records"

    def candidate(self, *extra, **environment_overrides):
        arguments = [str(CANDIDATE_SCRIPT), "--repo", str(self.checkout), "--output-dir", str(self.output),
                     "--record-dir", str(self.records), "--evidence-dir", str(self.evidence), *extra]
        return subprocess.run(arguments, capture_output=True, text=True, cwd=self.elsewhere,
                              env=self.environment(**environment_overrides), timeout=120)

    def only_record(self):
        files = sorted(self.records.glob("*.json")) if self.records.is_dir() else []
        self.assertEqual(len(files), 1, files)
        return files[0], json.loads(files[0].read_text())

    def write_profile_map(self, mapping):
        entries = {}
        for index, bundle_identifier in enumerate(mapping):
            path = self.inputs / f"mapped-{index}.mobileprovision"
            path.write_bytes(plistlib.dumps(profile_plist(
                Entitlements={"application-identifier": f"{SYNTHETIC_TEAM}.{bundle_identifier}", "get-task-allow": True},
                UUID=f"0000000{index}-2222-3333-4444-555555555555")))
            entries[bundle_identifier] = str(path)
        map_path = self.inputs / "profile-map.json"
        map_path.write_text(json.dumps(entries))
        return map_path

    def mapped(self, mapping):
        return dict(OHAND_SIGNING_PROFILE_PATH=None, OHAND_SIGNING_PROFILE_MAP_PATH=str(self.write_profile_map(mapping)))


class CandidateRecordTests(CandidateTestCase):
    def test_signing_writes_the_archive_and_a_record_naming_revision_version_build_checksum_route_and_expiry(self):
        result = self.candidate()
        self.assertEqual(result.returncode, 0, result.stderr)
        path, record = self.only_record()
        revision = git(self.checkout, "rev-parse", "HEAD")
        archives = sorted(self.output.glob("*.ipa"))
        self.assertEqual(len(archives), 1)
        self.assertEqual(record["archive_file"], archives[0].name)
        self.assertEqual(record["archive_sha256"], hashlib.sha256(archives[0].read_bytes()).hexdigest())
        self.assertEqual(record["revision"], revision)
        self.assertEqual((record["version"], record["build_number"]), ("0.1.0", "1"))
        self.assertEqual(record["signing_route"], "development")
        self.assertRegex(record["profile_expires_at"], r"^2099-01-02T03:04:05Z$")
        self.assertEqual(set(record), sign_trial_candidate.RECORD_KEYS)
        self.assertEqual(path.name, f"candidate-1-{revision[:8]}.json")

    def test_the_candidate_is_built_as_oh_and_app_with_its_embedded_extension_covered(self):
        self.assertEqual(self.candidate().returncode, 0)
        archive_call = self.calls_of("xcodebuild", "archive")[0]["argv"]
        self.assertEqual(archive_call[archive_call.index("-scheme") + 1], "OhAndApp")
        options = json.loads((self.state / "export-options.json").read_text())
        self.assertEqual(set(options["provisioningProfiles"]), {APP_BUNDLE_IDENTIFIER, EXTENSION_BUNDLE_IDENTIFIER})
        self.assertEqual(self.calls_of("devicectl", "device"), [])

    def test_the_record_and_every_output_hold_no_signing_identifier_or_hash_of_one(self):
        result = self.candidate(OHAND_DEVICE_LABEL="phone-a")
        self.assertEqual(result.returncode, 0, result.stderr)
        path, _ = self.only_record()
        text = path.read_text()
        sensitive = [SYNTHETIC_TEAM, SYNTHETIC_PROFILE_UUID, SYNTHETIC_PROFILE_NAME, SYNTHETIC_DEVICE_ID,
                     PROFILE_CONTENT_SENTINEL, SYNTHETIC_IDENTITY_FRAGMENT, "phone-a", "SYNTHETIC1"]
        for value in sensitive:
            self.assertNotIn(value, text + result.stdout + result.stderr)
            for algorithm in (hashlib.md5, hashlib.sha1, hashlib.sha256):
                self.assertNotIn(algorithm(value.encode()).hexdigest(), text)

    def test_a_record_that_would_hold_a_sensitive_value_is_not_written(self):
        record = {"schema": "x", "profile_expires_at": SYNTHETIC_TEAM}
        with self.assertRaises(sign_trial_candidate.sign_probe.SigningError):
            sign_trial_candidate.write_record(record | {"build_number": "1", "revision": "a" * 40}, self.records, [SYNTHETIC_TEAM])
        self.assertEqual(list(self.records.glob("*")) if self.records.exists() else [], [])


class CandidateRootTests(CandidateTestCase):
    def test_the_archived_project_is_the_certified_checkout(self):
        result = self.candidate()
        self.assertEqual(result.returncode, 0, result.stderr)
        archive_call = self.calls_of("xcodebuild", "archive")[0]["argv"]
        archived_project = Path(archive_call[archive_call.index("-project") + 1])
        self.assertEqual(archived_project, self.checkout.resolve() / "ios" / "OhAnd.xcodeproj")


class CandidateRefusalTests(CandidateTestCase):
    def assert_nothing_signed(self, result, code):
        self.assertEqual(result.returncode, code, result.stderr)
        self.assertEqual(list(self.records.glob("*.json")) if self.records.exists() else [], [])
        self.assertEqual(list(self.output.glob("*")) if self.output.exists() else [], [])

    def test_a_dirty_tree_is_refused_before_any_apple_tool_runs(self):
        (self.checkout / "scratch.txt").write_text("synthetic")
        result = self.candidate()
        self.assert_nothing_signed(result, 1)
        self.assertIn("clean tree", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_tracked_modification_is_also_dirty(self):
        (self.checkout / INFO_PLIST).write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.2.0"}))
        result = self.candidate()
        self.assert_nothing_signed(result, 1)
        self.assertEqual(self.calls(), [])

    def test_a_missing_profile_for_the_extension_is_refused_and_nothing_is_recorded(self):
        result = self.candidate(**self.mapped([APP_BUNDLE_IDENTIFIER]))
        self.assert_nothing_signed(result, 3)
        self.assertIn(EXTENSION_BUNDLE_IDENTIFIER, result.stderr)
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])

    def test_a_missing_profile_for_the_app_is_refused_and_nothing_is_recorded(self):
        result = self.candidate(**self.mapped([EXTENSION_BUNDLE_IDENTIFIER]))
        self.assert_nothing_signed(result, 3)
        self.assertIn(APP_BUNDLE_IDENTIFIER, result.stderr)
        self.assertEqual(self.calls_of("xcodebuild", "-exportArchive"), [])

    def test_a_profile_map_covering_both_bundles_signs(self):
        result = self.candidate(**self.mapped([APP_BUNDLE_IDENTIFIER, EXTENSION_BUNDLE_IDENTIFIER]))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.only_record()

    def test_an_unsigned_export_is_a_failure_and_records_nothing(self):
        result = self.candidate(STUB_UNSIGNED_EXPORT="1")
        self.assert_nothing_signed(result, 1)

    def test_the_output_directory_must_be_outside_the_repository(self):
        inside = self.checkout / "out"
        arguments = [str(CANDIDATE_SCRIPT), "--repo", str(self.checkout), "--output-dir", str(inside),
                     "--record-dir", str(self.records), "--evidence-dir", str(self.evidence)]
        result = subprocess.run(arguments, capture_output=True, text=True, env=self.environment(), cwd=self.elsewhere)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.calls(), [])


class ChecksumTests(CandidateTestCase):
    def signed(self):
        self.assertEqual(self.candidate().returncode, 0)
        path, _ = self.only_record()
        return path, next(self.output.glob("*.ipa"))

    def verify(self, record, archive):
        return subprocess.run([str(CANDIDATE_SCRIPT), "--verify", str(record), "--archive", str(archive)],
                              capture_output=True, text=True, cwd=self.elsewhere)

    def test_the_kept_archive_matches_its_record(self):
        record, archive = self.signed()
        result = self.verify(record, archive)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_changed_archive_is_a_checksum_mismatch(self):
        record, archive = self.signed()
        archive.write_bytes(archive.read_bytes() + b"x")
        result = self.verify(record, archive)
        self.assertEqual(result.returncode, 1)
        self.assertIn("does not match", result.stderr)

    def test_a_record_without_a_valid_checksum_is_refused(self):
        record, archive = self.signed()
        document = json.loads(record.read_text())
        document["archive_sha256"] = "not-a-checksum"
        record.write_text(json.dumps(document))
        self.assertEqual(self.verify(record, archive).returncode, 2)


if __name__ == "__main__":
    unittest.main()
