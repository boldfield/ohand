"""Boundary tests for export-failure-diagnostics.sh using a fake xcresulttool (no Xcode needed)."""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "export-failure-diagnostics.sh"

FAKE_TOOL = r'''#!/usr/bin/env python3
import json, os, sys
ATTACHMENT_SIZE = int(os.environ.get('FAKE_SIZE', '1000'))
REFERENCES = int(os.environ.get('FAKE_REFERENCES', '1'))
from pathlib import Path
args = sys.argv[1:]
if args[:3] == ["get", "test-results", "summary"]:
    print("S" * 200)
elif args[:3] == ["get", "test-results", "tests"]:
    print(json.dumps({"testNodes": [{"nodeType": "Test Case", "name": "t", "nodeIdentifier": "Suite/t", "result": "Failed", "children": []}]}))
elif args[:2] == ["export", "attachments"]:
    out = Path(args[args.index("--output-path") + 1])
    (out / "a.png").write_bytes(b"x" * ATTACHMENT_SIZE)
    (out / "manifest.json").write_text(json.dumps([{"testIdentifier": "Suite/t", "attachments": [{"exportedFileName": "a.png"}] * REFERENCES}]))
'''


class ExportFailureDiagnosticsTests(unittest.TestCase):
    def run_export(self, cap_bytes, bundle_count=1, **fake):
        temp = Path(tempfile.mkdtemp())
        tool = temp / "fake-xcresulttool"
        tool.write_text(FAKE_TOOL)
        tool.chmod(0o755)
        bundles = []
        for number in range(bundle_count):
            bundle = temp / f"b{number}.xcresult"
            bundle.mkdir()
            bundles.append(str(bundle))
        output = temp / "out"
        environment = dict(os.environ, CAP_BYTES=str(cap_bytes), XCRESULTTOOL=str(tool), **fake)
        result = subprocess.run([str(SCRIPT), str(output), *bundles], env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        total = sum(path.stat().st_size for path in output.rglob("*") if path.is_file())
        return output, total

    def test_everything_kept_under_a_generous_cap(self):
        output, total = self.run_export(1_000_000)
        self.assertTrue(list((output / "00-b0-attachments").glob("*a.png")))
        self.assertIn("kept", (output / "MANIFEST.txt").read_text())
        self.assertLess(total, 1_000_000)

    def test_total_never_exceeds_the_cap_including_json_and_manifest(self):
        for cap_bytes in (0, 10, 100, 300, 600, 1000, 1500, 2500, 5000):
            for bundle_count in (1, 3):
                with self.subTest(cap_bytes=cap_bytes, bundles=bundle_count):
                    _, total = self.run_export(cap_bytes, bundle_count)
                    self.assertLessEqual(total, cap_bytes)

    def test_attachment_omitted_and_recorded_when_cap_is_small(self):
        output, _ = self.run_export(1500)
        self.assertFalse((output / "00-b0-attachments").exists())
        self.assertIn("omitted", (output / "MANIFEST.txt").read_text())

    def test_every_omitted_attachment_is_listed_without_truncation(self):
        output, total = self.run_export(1_000_000, FAKE_SIZE="600000", FAKE_REFERENCES="2000")
        self.assertLessEqual(total, 1_000_000)
        manifest = (output / "MANIFEST.txt").read_text()
        self.assertTrue(manifest.endswith("\n"))
        lines = manifest.splitlines()
        self.assertEqual(sum(line.startswith("kept") and "attachments/" in line for line in lines), 1)
        omitted = [line for line in lines if line.startswith("omitted")]
        self.assertEqual(len(omitted), 1999)
        self.assertTrue(all(line.endswith("cap reached") and "600000 bytes" in line for line in omitted))

    def test_manifest_stays_complete_when_one_attachment_nearly_fills_the_cap(self):
        output, total = self.run_export(1_000_000, FAKE_SIZE="900000", FAKE_REFERENCES="2000")
        self.assertLessEqual(total, 1_000_000)
        manifest = (output / "MANIFEST.txt").read_text()
        self.assertTrue(manifest.endswith("\n"))
        omitted = [line for line in manifest.splitlines() if line.startswith("omitted")]
        kept = [line for line in manifest.splitlines() if line.startswith("kept") and "attachments/" in line]
        self.assertEqual(len(omitted) + len(kept), 2000)
        self.assertTrue(all(line.endswith("cap reached") and "900000 bytes" in line for line in omitted))


if __name__ == "__main__":
    unittest.main()
