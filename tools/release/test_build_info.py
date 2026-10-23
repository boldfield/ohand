#!/usr/bin/env python3
"""Tests for build_info.py - build identifier generation."""

import unittest
import json
import subprocess
import tempfile
import os
import sys
from pathlib import Path


class TestBuildInfo(unittest.TestCase):
    """Test build identifier generation."""

    def setUp(self):
        """Set up test environment."""
        self.test_dir = Path(__file__).parent
        self.build_info_script = self.test_dir / "build_info.py"
        assert self.build_info_script.exists(), "build_info.py not found"

    def run_build_info(self, *args):
        """Run build_info.py with given arguments."""
        result = subprocess.run(
            [sys.executable, str(self.build_info_script)] + list(args),
            capture_output=True,
            text=True,
            cwd=self.test_dir.parent.parent,  # Run from repo root
        )
        return result

    def test_basic_execution(self):
        """Test that build_info.py executes without error."""
        result = self.run_build_info()
        self.assertEqual(result.returncode, 0, f"stderr: {result.stderr}")

    def test_output_format(self):
        """Test that output is in key=value format by default."""
        result = self.run_build_info()
        self.assertEqual(result.returncode, 0)
        lines = result.stdout.strip().split('\n')
        self.assertGreater(len(lines), 0)

        for line in lines:
            self.assertIn('=', line, f"Line should be key=value format: {line}")

    def test_json_output(self):
        """Test JSON output format."""
        result = self.run_build_info("--json")
        self.assertEqual(result.returncode, 0, f"stderr: {result.stderr}")

        data = json.loads(result.stdout)
        required_keys = [
            "build_identifier",
            "build_revision",
            "build_revision_short",
            "run_id",
            "collected_at",
            "working_tree_clean",
        ]
        for key in required_keys:
            self.assertIn(key, data, f"Missing key: {key}")

    def test_build_identifier_format(self):
        """Test that build identifier has correct format."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        build_id = data["build_identifier"]
        parts = build_id.split('-')
        self.assertEqual(len(parts), 2, f"Build ID should have 2 parts: {build_id}")

        # First part should be 8-char revision prefix
        self.assertEqual(len(parts[0]), 8, f"Revision prefix should be 8 chars: {parts[0]}")
        self.assertTrue(parts[0].isalnum(), f"Revision prefix should be alphanumeric: {parts[0]}")

        # Second part should be 8-char run ID
        self.assertEqual(len(parts[1]), 8, f"Run ID should be 8 chars: {parts[1]}")

    def test_revision_consistency(self):
        """Test that build_revision is 40 characters (SHA-1)."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        revision = data["build_revision"]
        self.assertEqual(len(revision), 40, f"Revision should be 40 chars: {revision}")
        self.assertTrue(revision.isalnum(), f"Revision should be alphanumeric: {revision}")

    def test_revision_prefix_matches(self):
        """Test that revision prefix is first 8 chars of full revision."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        full_revision = data["build_revision"]
        prefix = data["build_revision_short"]
        self.assertEqual(prefix, full_revision[:8])

    def test_custom_timestamp(self):
        """Test custom timestamp override."""
        custom_timestamp = "2026-10-12T19:52:44Z"
        result = self.run_build_info("--json", "--timestamp", custom_timestamp)
        self.assertEqual(result.returncode, 0)

        data = json.loads(result.stdout)
        self.assertEqual(data["collected_at"], custom_timestamp)

    def test_export_vars_file(self):
        """Test exporting to a variables file."""
        with tempfile.NamedTemporaryFile(mode='w', delete=False, suffix='.sh') as f:
            vars_file = f.name

        try:
            result = self.run_build_info("--export-vars", vars_file)
            self.assertEqual(result.returncode, 0, f"stderr: {result.stderr}")

            # Read the file and verify contents
            with open(vars_file) as f:
                content = f.read()

            self.assertIn("build_identifier=", content)
            self.assertIn("build_revision=", content)
            self.assertIn("working_tree_clean=", content)

            # Verify it's valid shell format
            lines = content.strip().split('\n')
            for line in lines:
                self.assertRegex(
                    line,
                    r'^[a-z_][a-z0-9_]*=',
                    f"Invalid shell variable line: {line}"
                )
        finally:
            if os.path.exists(vars_file):
                os.unlink(vars_file)

    def test_deterministic_run_id_same_timestamp(self):
        """Test that same timestamp produces same run ID."""
        timestamp = "2026-10-12T19:52:44Z"

        result1 = self.run_build_info("--json", "--timestamp", timestamp)
        result2 = self.run_build_info("--json", "--timestamp", timestamp)

        data1 = json.loads(result1.stdout)
        data2 = json.loads(result2.stdout)

        # Run IDs should be the same for the same timestamp
        self.assertEqual(data1["run_id"], data2["run_id"])

    def test_unique_run_id_different_timestamps(self):
        """Test that different timestamps produce different run IDs (collision resistance)."""
        timestamp1 = "2026-10-12T19:52:44Z"
        timestamp2 = "2026-11-22T19:52:44Z"

        result1 = self.run_build_info("--json", "--timestamp", timestamp1)
        result2 = self.run_build_info("--json", "--timestamp", timestamp2)

        data1 = json.loads(result1.stdout)
        data2 = json.loads(result2.stdout)

        # Run IDs should be different for different timestamps
        self.assertNotEqual(data1["run_id"], data2["run_id"],
                           f"Run IDs must be unique; both {timestamp1} and {timestamp2} produced {data1['run_id']}")

    def test_run_id_is_hex(self):
        """Test that run_id is valid hexadecimal."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        run_id = data["run_id"]
        # Should be a valid hex string
        try:
            int(run_id, 16)
        except ValueError:
            self.fail(f"Run ID is not valid hexadecimal: {run_id}")

    def test_timestamp_in_iso8601(self):
        """Test that collected_at is valid ISO 8601."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        timestamp = data["collected_at"]
        # Should match ISO 8601 format with Z suffix
        self.assertRegex(timestamp, r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$')

    def test_working_tree_clean_bool(self):
        """Test that working_tree_clean is a boolean."""
        result = self.run_build_info("--json")
        data = json.loads(result.stdout)

        self.assertIsInstance(data["working_tree_clean"], bool)


if __name__ == "__main__":
    unittest.main()
