import json
import tempfile
import unittest
import uuid
from pathlib import Path

import verify_capture_records as verifier


def write_record(root, launch_kind, **overrides):
    capture_id = str(uuid.uuid4())
    record = {
        "captureId": capture_id,
        "source": "directLaunch",
        "launchKind": launch_kind,
        "protectedDataAvailable": True,
        "committedAt": "2026-03-01T09:30:00.000Z",
        "syntheticText": "Synthetic probe capture",
    }
    record.update(overrides)
    (root / "records").mkdir(exist_ok=True)
    (root / "records" / f"{capture_id}.json").write_text(json.dumps(record))
    return capture_id


def write_presented(root, capture_id, status="Saved", lines=None):
    (root / "last-presented.json").write_text(
        json.dumps({"captureId": capture_id, "statusText": status, "lines": lines or [f"Capture ID: {capture_id}"]})
    )


class VerifyCaptureRecordsTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def run_verifier(self, *kinds):
        return verifier.main([str(self.root), "--expect-kinds", *kinds])

    def test_accepts_cold_and_warm_records_with_matching_presentation(self):
        write_record(self.root, "cold")
        warm_id = write_record(self.root, "warm")
        write_presented(self.root, warm_id)
        self.assertEqual(self.run_verifier("cold", "warm"), 0)

    def test_rejects_missing_records(self):
        (self.root / "records").mkdir()
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_rejects_unexpected_launch_kinds(self):
        capture_id = write_record(self.root, "cold")
        write_presented(self.root, capture_id)
        self.assertEqual(self.run_verifier("cold", "warm"), 1)

    def test_rejects_a_missing_presentation(self):
        write_record(self.root, "cold")
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_rejects_a_failed_presentation(self):
        capture_id = write_record(self.root, "cold")
        write_presented(self.root, capture_id, status="Failed: write error")
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_rejects_a_presentation_without_a_persisted_record(self):
        write_record(self.root, "cold")
        write_presented(self.root, str(uuid.uuid4()))
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_rejects_a_screen_that_shows_another_entry(self):
        earlier_id = write_record(self.root, "cold")
        current_id = write_record(self.root, "warm")
        write_presented(self.root, current_id, lines=[f"Capture ID: {current_id}", f"Earlier: {earlier_id}"])
        self.assertEqual(self.run_verifier("cold", "warm"), 1)

    def test_rejects_an_invalid_capture_id(self):
        (self.root / "records").mkdir()
        record = {
            "captureId": "not-a-uuid",
            "source": "directLaunch",
            "launchKind": "cold",
            "protectedDataAvailable": True,
            "committedAt": "2026-03-01T09:30:00.000Z",
            "syntheticText": "Synthetic probe capture",
        }
        (self.root / "records" / "not-a-uuid.json").write_text(json.dumps(record))
        write_presented(self.root, "not-a-uuid")
        self.assertEqual(self.run_verifier("cold"), 1)


if __name__ == "__main__":
    unittest.main()
