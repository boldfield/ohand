"""Behaviour tests for build_info.py against real throwaway git repositories (synthetic content only)."""

import json
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import build_info

TOOL = Path(__file__).resolve().parent / "build_info.py"


def git(repo, *arguments):
    return subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=Synthetic", "-c", "user.email=synthetic@example.invalid",
         "-c", "commit.gpgsign=false", *arguments],
        capture_output=True, text=True, check=True).stdout.strip()


class BuildInfoTestCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name) / "repo"
        (self.repo / build_info.INFO_PLIST).parent.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", "-b", "main", str(self.repo)], check=True)
        self.write_plist("0.1.0")
        self.commit("first")

    def write_plist(self, version):
        (self.repo / build_info.INFO_PLIST).write_bytes(plistlib.dumps({"CFBundleShortVersionString": version}))

    def commit(self, message):
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def run_tool(self, *arguments):
        return subprocess.run([sys.executable, str(TOOL), "--repo", str(self.repo), *arguments],
                              capture_output=True, text=True)


class DerivationTests(BuildInfoTestCase):
    def test_identity_is_version_count_full_revision_and_clean_flag(self):
        info = build_info.derive(self.repo)
        self.assertEqual(info["version"], "0.1.0")
        self.assertEqual(info["build_number"], 1)
        self.assertEqual(info["revision"], git(self.repo, "rev-parse", "HEAD"))
        self.assertEqual(len(info["revision"]), 40)
        self.assertFalse(info["dirty"])

    def test_build_number_is_reproducible_and_grows_with_each_commit_on_main(self):
        first = build_info.derive(self.repo)
        self.assertEqual(build_info.derive(self.repo), first)
        (self.repo / "note.txt").write_text("synthetic")
        self.commit("second")
        second = build_info.derive(self.repo)
        self.assertEqual(second["build_number"], first["build_number"] + 1)

    def test_a_merged_branch_counts_only_the_first_parent_line(self):
        git(self.repo, "checkout", "-q", "-b", "topic")
        for index in range(3):
            (self.repo / f"topic-{index}.txt").write_text("synthetic")
            self.commit(f"topic {index}")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "-m", "merge topic", "topic")
        # first commit + merge commit; the three topic commits are not on the first-parent line
        self.assertEqual(build_info.derive(self.repo)["build_number"], 2)

    def test_a_later_main_commit_always_has_a_higher_number_than_its_ancestor(self):
        ancestor = build_info.derive(self.repo)
        git(self.repo, "checkout", "-q", "-b", "topic")
        (self.repo / "a.txt").write_text("synthetic")
        self.commit("topic")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "-m", "merge", "topic")
        self.assertGreater(build_info.derive(self.repo)["build_number"], ancestor["build_number"])

    def test_dirty_flag_covers_tracked_changes_and_untracked_files(self):
        (self.repo / "untracked.txt").write_text("synthetic")
        self.assertTrue(build_info.derive(self.repo)["dirty"])
        (self.repo / "untracked.txt").unlink()
        self.assertFalse(build_info.derive(self.repo)["dirty"])
        self.write_plist("0.2.0")
        info = build_info.derive(self.repo)
        self.assertTrue(info["dirty"])
        self.assertEqual(info["version"], "0.2.0")

    def test_plist_keys_carry_all_four_values(self):
        info = build_info.derive(self.repo)
        keys = build_info.plist_keys(info)
        self.assertEqual(keys, {
            "CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "1",
            "OhAndBuildRevision": info["revision"], "OhAndBuildDirty": False})


class RefusalTests(BuildInfoTestCase):
    def test_candidate_refuses_a_dirty_tree(self):
        (self.repo / "scratch.txt").write_text("synthetic")
        with self.assertRaisesRegex(build_info.BuildInfoError, "uncommitted changes"):
            build_info.derive(self.repo, require_clean=True)
        result = self.run_tool("--require-clean")
        self.assertEqual(result.returncode, 1)
        self.assertIn("clean tree", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_dirty_tree_is_reported_not_refused_without_the_candidate_flag(self):
        (self.repo / "scratch.txt").write_text("synthetic")
        result = self.run_tool()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)["dirty"])

    def test_a_missing_git_checkout_fails_and_writes_no_placeholder(self):
        elsewhere = Path(self.temporary.name) / "not-a-checkout"
        elsewhere.mkdir()
        (elsewhere / build_info.INFO_PLIST).parent.mkdir(parents=True)
        (elsewhere / build_info.INFO_PLIST).write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.0"}))
        result = subprocess.run([sys.executable, str(TOOL), "--repo", str(elsewhere)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("error:", result.stderr)

    def test_a_repository_without_commits_has_no_revision_and_fails(self):
        empty = Path(self.temporary.name) / "empty"
        subprocess.run(["git", "init", "-q", str(empty)], check=True)
        with self.assertRaises(build_info.BuildInfoError):
            build_info.derive(empty)

    def test_a_shallow_clone_is_refused_for_a_candidate_only(self):
        (self.repo / "b.txt").write_text("synthetic")
        self.commit("second")
        shallow = Path(self.temporary.name) / "shallow"
        subprocess.run(["git", "clone", "-q", "--depth", "1", f"file://{self.repo}", str(shallow)], check=True)
        info = build_info.derive(shallow)
        self.assertTrue(info["shallow"])
        with self.assertRaisesRegex(build_info.BuildInfoError, "shallow"):
            build_info.derive(shallow, require_clean=True)

    def test_a_missing_or_malformed_marketing_version_fails(self):
        (self.repo / build_info.INFO_PLIST).write_bytes(plistlib.dumps({"CFBundleName": "x"}))
        with self.assertRaises(build_info.BuildInfoError):
            build_info.derive(self.repo)
        self.write_plist("a note with spaces")
        with self.assertRaises(build_info.BuildInfoError):
            build_info.derive(self.repo)


if __name__ == "__main__":
    unittest.main()
