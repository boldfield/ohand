#!/usr/bin/env python3
"""Tests for the hygiene checker.

Seeded secrets are assembled at runtime from fragments so that this source file
never contains a scannable secret. Tests that need the real gitleaks binary
skip when it is absent, except when HYGIENE_REQUIRE_GITLEAKS=1 (set by the
hygiene workflow), where a missing or wrong-version binary is a failure.
"""

import hashlib
import json
import os
import random
import shutil
import string
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import check_hygiene
from check_hygiene import (
    Finding,
    GITLEAKS_VERSION,
    ScannerError,
    check_private_path,
    check_repository,
    check_signing_material,
    format_report,
    run_gitleaks,
)

SCRIPT_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check_hygiene.py")
REQUIRE_GITLEAKS = os.environ.get("HYGIENE_REQUIRE_GITLEAKS") == "1"
GITLEAKS_AVAILABLE = shutil.which("gitleaks") is not None

ALPHANUMERIC = string.ascii_letters + string.digits
BASE32_UPPER = string.ascii_uppercase + "234567"


def random_text(length, alphabet, seed):
    generator = random.Random(seed)
    return "".join(generator.choice(alphabet) for _ in range(length))


def seeded_aws_access_key():
    return "AKIA" + random_text(16, BASE32_UPPER, 1)


def seeded_github_token():
    return "ghp_" + random_text(36, ALPHANUMERIC, 2)


def seeded_anthropic_key():
    return "sk-ant-" + "api03-" + random_text(40, ALPHANUMERIC + "-_", 3)


def seeded_generic_password():
    return random_text(24, ALPHANUMERIC, 4)


def seeded_private_key_block():
    header = "-----BEGIN " + "RSA PRIVATE KEY-----"
    footer = "-----END " + "RSA PRIVATE KEY-----"
    return "\n".join([header, random_text(64, ALPHANUMERIC, 5), random_text(64, ALPHANUMERIC, 6), footer]) + "\n"


class TempRepoTestCase(unittest.TestCase):
    def setUp(self):
        self.repo_dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.repo_dir, ignore_errors=True)
        self.git("init", "-q")

    def git(self, *args):
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", *args],
            cwd=self.repo_dir, check=True, capture_output=True,
        )

    def write(self, relative_path, content):
        full_path = os.path.join(self.repo_dir, relative_path)
        os.makedirs(os.path.dirname(full_path), exist_ok=True)
        mode = "wb" if isinstance(content, bytes) else "w"
        with open(full_path, mode) as handle:
            handle.write(content)
        return full_path

    def commit_all(self):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "seed")

    def write_media_with_provenance(self, media_path, **overrides):
        media_bytes = b"synthetic tone " + media_path.encode()
        self.write(media_path, media_bytes)
        record = {
            "synthetic": True,
            "contains_personal_data": False,
            "generator": "sine-tone script",
            "description": "One second 440 Hz tone",
            "sha256": hashlib.sha256(media_bytes).hexdigest(),
        }
        record.update(overrides)
        self.write(media_path + ".provenance.json", json.dumps(record))

    def replace_with_dangling_symlink(self, relative_path):
        full_path = os.path.join(self.repo_dir, relative_path)
        os.remove(full_path)
        os.symlink("target-that-does-not-exist", full_path)

    def run_cli(self, *extra_args):
        return subprocess.run(
            [sys.executable, SCRIPT_PATH, "--repo", self.repo_dir, *extra_args],
            capture_output=True, text=True,
        )


class SigningMaterialPolicyTests(unittest.TestCase):
    def test_signing_material_and_env_files_are_rejected(self):
        for path in [
            "certs/apple.p12", "profile/app.mobileprovision", "keys/server.pem", "keys/server.key",
            "AuthKey_ABC123.p8", "android/release.keystore", "keys/signing.gpg", "keys/root.cer",
            "home/id_ed25519", ".env", ".env.production", "ios/.env.local", "KEYS/UPPER.P12",
        ]:
            with self.subTest(path=path):
                self.assertTrue(check_signing_material(path))

    def test_ordinary_files_and_env_templates_are_allowed(self):
        for path in ["core/src/lib.rs", "docs/contributing.md", ".env.example", "ios/.env.sample", "home/id_ed25519.pub"]:
            with self.subTest(path=path):
                self.assertEqual(check_signing_material(path), [])


class PrivatePathPolicyTests(unittest.TestCase):
    def test_private_directories_at_root_and_under_fixture_roots_are_rejected(self):
        for path in [
            "captures/personal-note.txt", "private/conversation.json", "recordings/transcript.txt",
            "Transcripts/day1.md", "voice-memos/a.txt", "conversations/x.json", "personal/diary.txt",
            "fixtures/recordings/notes.txt", "core/tests/fixtures/private/x.json", "ios/Tests/Fixtures/captures/y.txt",
        ]:
            with self.subTest(path=path):
                self.assertTrue(check_private_path(path))

    def test_ordinary_source_paths_and_root_files_are_allowed(self):
        for path in [
            "core/src/store/captures/mod.rs", "docs/privacy/policy.md", "fixtures/audio/tone.txt",
            "captures.md", "private", "ios/Sources/Capture/Entry.swift", "fixtures/captures.json",
        ]:
            with self.subTest(path=path):
                self.assertEqual(check_private_path(path), [])


class MediaPolicyTests(TempRepoTestCase):
    def policy_findings(self):
        return check_repository(self.repo_dir, run_scanner=False)

    def test_audio_outside_fixture_roots_fails_even_with_provenance(self):
        self.write_media_with_provenance("app/capture.m4a")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("outside documented synthetic fixture roots" in message for message in messages))

    def test_audio_in_fixture_root_without_provenance_fails(self):
        self.write("fixtures/audio/hello.wav", b"audio")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("missing tracked provenance record" in message for message in messages))

    def test_untracked_provenance_record_does_not_count(self):
        self.write("fixtures/audio/hello.wav", b"audio")
        self.commit_all()
        self.write("fixtures/audio/hello.wav.provenance.json", "{}")
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("missing tracked provenance record" in message for message in messages))

    def test_documented_synthetic_fixture_passes(self):
        for fixture_path in ["fixtures/audio/tone.m4a", "core/tests/fixtures/tone.wav", "ios/Tests/Fixtures/tone.caf"]:
            self.write_media_with_provenance(fixture_path)
        self.commit_all()
        self.assertEqual(self.policy_findings(), [])

    def test_provenance_must_declare_synthetic_and_no_personal_data(self):
        for override in [{"synthetic": False}, {"contains_personal_data": True}, {"generator": ""}, {"description": 3}]:
            with self.subTest(override=override):
                self.write_media_with_provenance("fixtures/audio/tone.m4a", **override)
                self.commit_all()
                messages = [finding.message for finding in self.policy_findings()]
                self.assertTrue(any("invalid provenance record" in message for message in messages))

    def test_provenance_digest_must_match_media(self):
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.write("fixtures/audio/tone.m4a", b"swapped in a private recording")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("sha256" in message for message in messages))

    def test_malformed_provenance_fails(self):
        self.write("fixtures/audio/tone.m4a", b"audio")
        self.write("fixtures/audio/tone.m4a.provenance.json", "not json")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("not readable JSON" in message for message in messages))

    def test_committed_signing_material_and_env_file_fail(self):
        self.write("ios/Signing/dist.p12", b"x")
        self.write(".env.production", "MODE=prod\n")
        self.commit_all()
        paths = {finding.path for finding in self.policy_findings()}
        self.assertEqual(paths, {"ios/Signing/dist.p12", ".env.production"})

    def test_non_media_files_in_private_roots_fail(self):
        for path in ["captures/personal-note.txt", "private/conversation.json", "recordings/transcript.txt"]:
            self.write(path, "synthetic placeholder\n")
        self.commit_all()
        paths = {finding.path for finding in self.policy_findings()}
        self.assertEqual(paths, {"captures/personal-note.txt", "private/conversation.json", "recordings/transcript.txt"})

    def test_deleted_recording_still_in_history_fails(self):
        self.write("recordings/call.m4a", b"private")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "recordings/call.m4a"))
        self.commit_all()
        findings = [finding for finding in self.policy_findings() if finding.path == "recordings/call.m4a"]
        self.assertTrue(findings)
        self.assertTrue(all("reachable history" in finding.message for finding in findings))

    def test_deleted_signing_material_and_private_file_still_in_history_fail(self):
        self.write("ios/dist.p12", b"x")
        self.write("captures/day.txt", "x\n")
        self.write(".env", "A=1\n")
        self.write("keep.txt", "keep\n")
        self.commit_all()
        for removed in ["ios/dist.p12", "captures/day.txt", ".env"]:
            os.remove(os.path.join(self.repo_dir, removed))
        self.commit_all()
        paths = {finding.path for finding in self.policy_findings()}
        self.assertEqual(paths, {"ios/dist.p12", "captures/day.txt", ".env"})

    def test_deleted_fixture_audio_without_provenance_still_in_history_fails(self):
        self.write("fixtures/audio/oops.wav", b"actually a private recording")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "fixtures/audio/oops.wav"))
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("no provenance record" in message and "reachable history" in message for message in messages))

    def test_deleted_fixture_audio_with_invalid_provenance_still_in_history_fails(self):
        self.write_media_with_provenance("fixtures/audio/tone.m4a", synthetic=False)
        self.commit_all()
        for removed in ["fixtures/audio/tone.m4a", "fixtures/audio/tone.m4a.provenance.json"]:
            os.remove(os.path.join(self.repo_dir, removed))
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("invalid provenance record" in message and "reachable history" in message for message in messages))

    def test_deleted_fixture_audio_with_valid_provenance_passes(self):
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        for removed in ["fixtures/audio/tone.m4a", "fixtures/audio/tone.m4a.provenance.json"]:
            os.remove(os.path.join(self.repo_dir, removed))
        self.commit_all()
        self.assertEqual(self.policy_findings(), [])

    def test_deleted_fixture_audio_with_incomplete_provenance_still_in_history_fails(self):
        for override in [{"generator": ""}, {"description": "  "}]:
            with self.subTest(override=override):
                self.setUp()
                self.write_media_with_provenance("fixtures/audio/tone.wav", **override)
                self.commit_all()
                for removed in ["fixtures/audio/tone.wav", "fixtures/audio/tone.wav.provenance.json"]:
                    os.remove(os.path.join(self.repo_dir, removed))
                self.commit_all()
                messages = [finding.message for finding in self.policy_findings()]
                self.assertTrue(
                    any("invalid provenance record" in message and "reachable history" in message for message in messages)
                )

    def test_deleted_file_re_added_in_approved_form_still_fails_for_earlier_version(self):
        self.write("fixtures/audio/tone.m4a", b"first")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "fixtures/audio/tone.m4a"))
        self.commit_all()
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("no provenance record" in message and "reachable history" in message for message in messages))

    def test_private_recording_overwritten_with_provenanced_fixture_still_fails(self):
        self.write("fixtures/audio/tone.m4a", b"real private voice")
        self.commit_all()
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        findings = self.policy_findings()
        self.assertEqual([finding.path for finding in findings], ["fixtures/audio/tone.m4a"])
        self.assertIn("reachable history", findings[0].message)

    def test_overwrite_then_delete_of_private_recording_still_fails(self):
        self.write("fixtures/audio/tone.m4a", b"real private voice")
        self.commit_all()
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        for removed in ["fixtures/audio/tone.m4a", "fixtures/audio/tone.m4a.provenance.json"]:
            os.remove(os.path.join(self.repo_dir, removed))
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("no provenance record" in message and "reachable history" in message for message in messages))

    def test_every_version_with_valid_provenance_passes(self):
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        self.write_media_with_provenance("fixtures/audio/tone.m4a", description="revised tone")
        self.write("fixtures/audio/tone.m4a", b"second synthetic tone")
        record_path = os.path.join(self.repo_dir, "fixtures/audio/tone.m4a.provenance.json")
        with open(record_path) as handle:
            record = json.load(handle)
        record["sha256"] = hashlib.sha256(b"second synthetic tone").hexdigest()
        self.write("fixtures/audio/tone.m4a.provenance.json", json.dumps(record))
        self.commit_all()
        self.assertEqual(self.policy_findings(), [])

    def test_recording_replaced_by_dangling_symlink_still_fails(self):
        self.write("recordings/call.m4a", b"sixteen byte rec")
        self.commit_all()
        self.replace_with_dangling_symlink("recordings/call.m4a")
        self.commit_all()
        findings = self.policy_findings()
        self.assertIn("recordings/call.m4a", [finding.path for finding in findings])
        self.assertTrue(any("private capture" in finding.message for finding in findings))

    def test_signing_material_replaced_by_dangling_symlink_still_fails(self):
        self.write("ios/Signing/dist.p12", b"x")
        self.commit_all()
        self.replace_with_dangling_symlink("ios/Signing/dist.p12")
        self.commit_all()
        self.assertEqual([finding.path for finding in self.policy_findings()], ["ios/Signing/dist.p12"])

    def test_tracked_dangling_symlink_in_private_directory_fails(self):
        os.makedirs(os.path.join(self.repo_dir, "captures"))
        os.symlink("target-that-does-not-exist", os.path.join(self.repo_dir, "captures/note.txt"))
        self.commit_all()
        self.assertEqual([finding.path for finding in self.policy_findings()], ["captures/note.txt"])

    def test_unprovenanced_fixture_replaced_by_dangling_symlink_still_fails(self):
        self.write("fixtures/audio/tone.m4a", b"real private voice")
        self.commit_all()
        self.replace_with_dangling_symlink("fixtures/audio/tone.m4a")
        self.commit_all()
        messages = [finding.message for finding in self.policy_findings()]
        self.assertTrue(any("no provenance record" in message and "reachable history" in message for message in messages))
        self.assertTrue(any("must be a regular file" in message for message in messages))

    def test_symlinked_fixture_with_matching_provenance_fails(self):
        link_target = "../../elsewhere/voice.m4a"
        os.makedirs(os.path.join(self.repo_dir, "fixtures/audio"))
        os.symlink(link_target, os.path.join(self.repo_dir, "fixtures/audio/tone.m4a"))
        record = {
            "synthetic": True,
            "contains_personal_data": False,
            "generator": "sine-tone script",
            "description": "One second 440 Hz tone",
            "sha256": hashlib.sha256(link_target.encode()).hexdigest(),
        }
        self.write("fixtures/audio/tone.m4a.provenance.json", json.dumps(record))
        self.commit_all()
        findings = self.policy_findings()
        self.assertTrue(findings)
        self.assertTrue(all("must be a regular file" in finding.message for finding in findings))

    def test_index_is_checked_even_when_worktree_file_is_missing(self):
        self.write("recordings/call.m4a", b"private")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "recordings/call.m4a"))
        self.assertIn("recordings/call.m4a", [finding.path for finding in self.policy_findings()])

    def test_shallow_clone_fails_closed(self):
        self.write("recordings/call.m4a", b"private")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "recordings/call.m4a"))
        self.commit_all()
        self.write("README.md", "later\n")
        self.commit_all()
        shallow_dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, shallow_dir, ignore_errors=True)
        subprocess.run(
            ["git", "clone", "-q", "--depth", "1", "file://" + self.repo_dir, shallow_dir + "/clone"],
            check=True, capture_output=True,
        )
        with self.assertRaises(ScannerError) as context:
            check_repository(shallow_dir + "/clone", run_scanner=False)
        self.assertIn("shallow", str(context.exception))
        completed = subprocess.run(
            [sys.executable, SCRIPT_PATH, "--repo", shallow_dir + "/clone", "--gitleaks", "gitleaks-that-is-not-installed"],
            capture_output=True, text=True,
        )
        self.assertEqual(completed.returncode, check_hygiene.EXIT_SCANNER_ERROR)
        self.assertIn("shallow", completed.stderr)
        self.assertNotIn("No hygiene issues", completed.stdout)

    def test_clean_repository_passes(self):
        self.write("README.md", "# hello\n")
        self.commit_all()
        self.assertEqual(self.policy_findings(), [])

    def test_policy_findings_reported_alongside_empty_scanner_result(self):
        self.write("ios/Signing/dist.p12", b"x")
        self.commit_all()
        with mock.patch.object(check_hygiene, "run_gitleaks", return_value=[]):
            findings = check_repository(self.repo_dir)
        self.assertEqual([finding.path for finding in findings], ["ios/Signing/dist.p12"])


class ScannerFailClosedTests(TempRepoTestCase):
    def fake_gitleaks(self, returncode, report_text=None, stderr=""):
        def runner(command, **kwargs):
            if report_text is not None:
                report_path = command[command.index("--report-path") + 1]
                with open(report_path, "w") as handle:
                    handle.write(report_text)
            return subprocess.CompletedProcess(command, returncode, stdout="", stderr=stderr)
        return mock.patch.object(check_hygiene.subprocess, "run", side_effect=runner)

    def test_missing_binary_fails_closed(self):
        with self.assertRaises(ScannerError) as context:
            run_gitleaks(self.repo_dir, binary="gitleaks-that-is-not-installed")
        self.assertIn("not installed", str(context.exception))

    def test_missing_binary_exits_with_scanner_error_code(self):
        self.write("README.md", "hi\n")
        self.commit_all()
        completed = self.run_cli("--gitleaks", "gitleaks-that-is-not-installed")
        self.assertEqual(completed.returncode, check_hygiene.EXIT_SCANNER_ERROR)
        self.assertIn("Hygiene scanner error", completed.stderr)
        self.assertNotIn("No hygiene issues", completed.stdout)

    def test_error_exit_code_fails_closed(self):
        with self.fake_gitleaks(1, report_text="[]", stderr="boom"):
            with self.assertRaises(ScannerError):
                run_gitleaks(self.repo_dir)

    def test_malformed_report_fails_closed(self):
        with self.fake_gitleaks(0, report_text="{not json"):
            with self.assertRaises(ScannerError):
                run_gitleaks(self.repo_dir)

    def test_unexpected_report_shape_fails_closed(self):
        with self.fake_gitleaks(0, report_text='{"Matches": []}'):
            with self.assertRaises(ScannerError):
                run_gitleaks(self.repo_dir)

    def test_findings_exit_with_empty_report_fails_closed(self):
        with self.fake_gitleaks(check_hygiene.GITLEAKS_FINDINGS_EXIT_CODE, report_text="[]"):
            with self.assertRaises(ScannerError):
                run_gitleaks(self.repo_dir)

    def test_timeout_fails_closed(self):
        with mock.patch.object(check_hygiene.subprocess, "run", side_effect=subprocess.TimeoutExpired("gitleaks", 1)):
            with self.assertRaises(ScannerError):
                run_gitleaks(self.repo_dir)

    def test_report_findings_are_mapped_without_secret_fields(self):
        report = [{"File": "a.txt", "RuleID": "aws-access-token", "Commit": "abcdef123456", "StartLine": 3,
                   "Secret": "REDACTED-VALUE", "Match": "REDACTED-VALUE"}]
        with self.fake_gitleaks(check_hygiene.GITLEAKS_FINDINGS_EXIT_CODE, report_text=json.dumps(report)):
            findings = run_gitleaks(self.repo_dir)
        self.assertEqual(len(findings), 1)
        self.assertEqual(findings[0].path, "a.txt")
        self.assertEqual(findings[0].line, 3)
        self.assertNotIn("REDACTED-VALUE", format_report(findings))

    def test_format_report_clean_and_failing(self):
        self.assertEqual(format_report([]), "No hygiene issues detected.")
        self.assertIn("x.p12: bad", format_report([Finding("x.p12", "bad")]))


class RealGitleaksTests(TempRepoTestCase):
    def setUp(self):
        if not GITLEAKS_AVAILABLE:
            if REQUIRE_GITLEAKS:
                self.fail("HYGIENE_REQUIRE_GITLEAKS=1 but gitleaks is not on PATH")
            self.skipTest("gitleaks is not installed; install it to run scanner tests")
        super().setUp()

    def test_installed_version_matches_pin(self):
        completed = subprocess.run(["gitleaks", "version"], capture_output=True, text=True)
        if REQUIRE_GITLEAKS:
            self.assertEqual(completed.stdout.strip().lstrip("v"), GITLEAKS_VERSION)

    def test_clean_repository_passes_through_cli(self):
        self.write("README.md", "# nothing secret\n")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_CLEAN, completed.stderr)
        self.assertIn("No hygiene issues detected.", completed.stdout)

    def test_committed_seeded_secrets_fail_and_are_never_printed(self):
        seeded_files = {
            "aws.txt": seeded_aws_access_key(),
            "config/settings.yml": "token: " + seeded_github_token(),
            ".github/workflow-env.yml": 'ANTHROPIC_API_KEY: "' + seeded_anthropic_key() + '"',
            "tools/hygiene/not_special.py": 'access_key = "' + seeded_aws_access_key() + '"',
            "service.py": 'api_key = "' + seeded_generic_password() + '"',
            "notes/key.txt": seeded_private_key_block(),
        }
        secret_values = [seeded_aws_access_key(), seeded_github_token(), seeded_anthropic_key(), seeded_generic_password()]
        for relative_path, content in seeded_files.items():
            self.write(relative_path, content + "\n" if not content.endswith("\n") else content)
        self.commit_all()

        completed = self.run_cli()

        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS, completed.stderr)
        combined_output = completed.stdout + completed.stderr
        for relative_path in seeded_files:
            self.assertIn(relative_path, combined_output)
        for secret_value in secret_values:
            self.assertNotIn(secret_value, combined_output)
        self.assertNotIn(random_text(64, ALPHANUMERIC, 5), combined_output)

    def test_secret_removed_in_later_commit_still_fails_from_history(self):
        self.write("leak.txt", seeded_github_token() + "\n")
        self.commit_all()
        self.write("leak.txt", "removed\n")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS, completed.stderr)
        self.assertNotIn(seeded_github_token(), completed.stdout + completed.stderr)

    def test_inline_allow_comment_cannot_suppress_a_finding(self):
        self.write("allowed.txt", "token = " + seeded_github_token() + "  # gitleaks:allow\n")
        self.commit_all()
        self.assertEqual(self.run_cli().returncode, check_hygiene.EXIT_FINDINGS)

    def test_committed_gitleaksignore_cannot_suppress_a_finding(self):
        self.write("leak.txt", seeded_github_token() + "\n")
        self.commit_all()
        fingerprint = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=self.repo_dir, capture_output=True, text=True, check=True
        ).stdout.strip()
        self.write(".gitleaksignore", f"{fingerprint}:leak.txt:github-pat:1\n")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn(".gitleaksignore: scanner suppression file must not be committed", completed.stderr)

    def test_policy_and_scanner_findings_combine(self):
        self.write("ios/Signing/dist.p12", b"x")
        self.write("leak.txt", seeded_github_token() + "\n")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn("dist.p12", completed.stderr)
        self.assertIn("leak.txt", completed.stderr)

    def test_documented_synthetic_fixture_passes_through_cli(self):
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_CLEAN, completed.stderr)

    def test_private_path_audio_fails_through_cli(self):
        self.write("recordings/call.m4a", b"private")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn("recordings/call.m4a", completed.stderr)

    def test_non_media_private_path_files_fail_through_cli(self):
        for path in ["captures/personal-note.txt", "private/conversation.json", "recordings/transcript.txt"]:
            self.write(path, "synthetic placeholder\n")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        for path in ["captures/personal-note.txt", "private/conversation.json", "recordings/transcript.txt"]:
            self.assertIn(path, completed.stderr)

    def test_deleted_private_recording_still_in_history_fails_through_cli(self):
        self.write("recordings/call.m4a", b"private")
        self.commit_all()
        os.remove(os.path.join(self.repo_dir, "recordings/call.m4a"))
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn("recordings/call.m4a", completed.stderr)
        self.assertIn("reachable history", completed.stderr)

    def test_overwritten_private_recording_fails_through_cli(self):
        self.write("fixtures/audio/tone.m4a", b"real private voice")
        self.commit_all()
        self.write_media_with_provenance("fixtures/audio/tone.m4a")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn("fixtures/audio/tone.m4a", completed.stderr)
        self.assertIn("reachable history", completed.stderr)

    def test_recording_replaced_by_dangling_symlink_fails_through_cli(self):
        self.write("recordings/call.m4a", b"sixteen byte rec")
        self.commit_all()
        self.replace_with_dangling_symlink("recordings/call.m4a")
        self.commit_all()
        completed = self.run_cli()
        self.assertEqual(completed.returncode, check_hygiene.EXIT_FINDINGS)
        self.assertIn("recordings/call.m4a", completed.stderr)
        self.assertNotIn("No hygiene issues", completed.stdout)

    def test_run_gitleaks_returns_redacted_findings(self):
        self.write("leak.txt", seeded_aws_access_key() + "\n")
        self.commit_all()
        findings = run_gitleaks(self.repo_dir)
        self.assertEqual([finding.path for finding in findings], ["leak.txt"])
        self.assertIn("aws-access-token", findings[0].message)


if __name__ == "__main__":
    unittest.main(verbosity=2)
