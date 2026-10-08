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
        "source": "shortcutURL",
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


def write_idle(root):
    write_presented(root, None, status="Ready", lines=["No capture entry."])


class VerifyCaptureRecordsTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def run_verifier(self, *kinds, known=(), presented="saved", source=None):
        argv = [str(self.root), "--quiet", "--expect-presented", presented, "--expect-kinds", *kinds]
        if known:
            argv += ["--known-ids", *known]
        if source:
            argv += ["--expect-source", source]
        return verifier.main(argv)

    def test_accepts_a_new_record_that_the_screen_presents(self):
        cold_id = write_record(self.root, "cold")
        warm_id = write_record(self.root, "warm")
        write_presented(self.root, warm_id)
        self.assertEqual(self.run_verifier("cold", "warm", known=[cold_id], source="shortcutURL"), 0)

    def test_rejects_a_presented_record_that_is_not_new(self):
        cold_id = write_record(self.root, "cold")
        write_presented(self.root, cold_id)
        self.assertEqual(self.run_verifier("cold", known=[cold_id]), 1)

    def test_rejects_two_new_records_for_one_handoff(self):
        write_record(self.root, "cold")
        warm_id = write_record(self.root, "warm")
        write_presented(self.root, warm_id)
        self.assertEqual(self.run_verifier("cold", "warm"), 1)

    def test_rejects_a_disappeared_record(self):
        current_id = write_record(self.root, "cold")
        write_presented(self.root, current_id)
        self.assertEqual(self.run_verifier("cold", known=[str(uuid.uuid4())]), 1)

    def test_accepts_an_idle_screen_with_no_new_records(self):
        earlier_id = write_record(self.root, "cold")
        write_idle(self.root)
        self.assertEqual(self.run_verifier("cold", known=[earlier_id], presented="idle"), 0)

    def test_accepts_an_idle_first_launch_with_no_records(self):
        write_idle(self.root)
        self.assertEqual(self.run_verifier(presented="idle"), 0)

    def test_rejects_a_plain_launch_that_created_a_record(self):
        write_record(self.root, "cold")
        write_idle(self.root)
        self.assertEqual(self.run_verifier("cold", presented="idle"), 1)

    def test_rejects_an_idle_expectation_when_an_entry_is_shown(self):
        capture_id = write_record(self.root, "cold")
        write_presented(self.root, capture_id)
        self.assertEqual(self.run_verifier("cold", known=[capture_id], presented="idle"), 1)

    def test_rejects_a_leftover_pending_entry(self):
        capture_id = write_record(self.root, "cold")
        write_presented(self.root, capture_id)
        (self.root / "pending-entry.json").write_text("{}")
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_rejects_an_unexpected_source(self):
        capture_id = write_record(self.root, "cold", source="controlIntent")
        write_presented(self.root, capture_id)
        self.assertEqual(self.run_verifier("cold", source="shortcutURL"), 1)

    def test_rejects_an_unknown_source(self):
        capture_id = write_record(self.root, "cold", source="directLaunch")
        write_presented(self.root, capture_id)
        self.assertEqual(self.run_verifier("cold"), 1)

    def test_accepts_records_matching_what_the_screen_rendered(self):
        cold_id = write_record(self.root, "cold")
        warm_id = write_record(self.root, "warm")
        write_presented(self.root, warm_id)
        argv = [str(self.root), "--quiet", "--expect-kinds", "cold", "warm", "--known-ids", cold_id,
                "--expect-record", f"{cold_id}=cold", "--expect-record", f"{warm_id}=warm"]
        self.assertEqual(verifier.main(argv), 0)

    def test_rejects_a_rendered_id_without_a_record_or_with_another_kind(self):
        cold_id = write_record(self.root, "cold")
        write_presented(self.root, cold_id)
        base = [str(self.root), "--quiet", "--expect-kinds", "cold"]
        self.assertEqual(verifier.main(base + ["--expect-record", f"{cold_id}=warm"]), 1)
        self.assertEqual(verifier.main(base + ["--expect-record", f"{uuid.uuid4()}=cold"]), 1)

    def test_rejects_missing_records(self):
        (self.root / "records").mkdir()
        write_idle(self.root)
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
        self.assertEqual(self.run_verifier("cold", "warm", known=[earlier_id]), 1)

    def test_rejects_an_invalid_capture_id(self):
        (self.root / "records").mkdir()
        record = {
            "captureId": "not-a-uuid",
            "source": "shortcutURL",
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
