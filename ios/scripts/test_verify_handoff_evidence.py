import json
import tempfile
import unittest
from pathlib import Path

import verify_handoff_evidence as verifier

COLD_ID = "0F8FAD5B-D9CB-469F-A165-70867728950E"
WARM_ID = "6BA7B810-9DAD-41D1-80B4-00C04FD430C8"
LARGE_TEXT_ID = "123E4567-E89B-42D3-A456-426614174000"
LOG = f"""noise
HANDOFF-PHASE cold {COLD_ID} notRunning
HANDOFF-PHASE warm {WARM_ID} background
HANDOFF-PHASE rejected - 2
HANDOFF-PHASE large-text-capture {LARGE_TEXT_ID} scrolls=0
HANDOFF-PHASE large-text-management - ok
"""


class VerifyHandoffEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.handoff_dir = root / "handoffs"
        self.capture_root = root / "CaptureProbe"
        self.handoff_dir.mkdir()
        (self.capture_root / "records").mkdir(parents=True)
        for capture_id, ready, received in ((COLD_ID, False, 10), (WARM_ID, True, 20)):
            self.write_inbox_record(capture_id, ready, received)
        for capture_id in (COLD_ID, WARM_ID, LARGE_TEXT_ID):
            self.write_native_record(capture_id)
        self.write_rejections(2, "invalid_capture_id")

    def write_inbox_record(self, capture_id, ready, received, **overrides):
        record = {"captureId": capture_id, "receivedAtUnixMs": received, "webviewReady": ready}
        record.update(overrides)
        (self.handoff_dir / f"{capture_id}.json").write_text(json.dumps(record))

    def write_native_record(self, capture_id):
        (self.capture_root / "records" / f"{capture_id}.json").write_text(json.dumps({"captureId": capture_id}))

    def write_rejections(self, count, reason):
        summary = {"count": count, "lastReason": reason, "lastReceivedAtUnixMs": 30}
        (self.handoff_dir / "rejections.json").write_text(json.dumps(summary))

    def errors(self, log=LOG, **kwargs):
        return verifier.check(self.handoff_dir, self.capture_root, log, **kwargs)

    def test_accepts_matching_evidence(self):
        self.assertEqual(self.errors(), [])

    def test_rejects_missing_or_reordered_phases(self):
        self.assertTrue(any("phases" in error for error in self.errors(LOG.replace("HANDOFF-PHASE warm", "HANDOFF-PHASE other"))))

    def test_rejects_a_cold_phase_that_began_with_a_running_shell(self):
        self.assertTrue(any("cold phase" in error for error in self.errors(LOG.replace("notRunning", "runningForeground"))))

    def test_rejects_an_identifier_the_native_entry_never_saved(self):
        (self.capture_root / "records" / f"{WARM_ID}.json").unlink()
        self.assertTrue(any("no saved record" in error for error in self.errors()))

    def test_rejects_an_extra_record_in_the_shell_inbox(self):
        self.write_inbox_record(LARGE_TEXT_ID, True, 40)
        self.assertTrue(any("exactly" in error for error in self.errors()))

    def test_rejects_a_missing_handoff_record(self):
        (self.handoff_dir / f"{COLD_ID}.json").unlink()
        self.assertTrue(any("exactly" in error for error in self.errors()))

    def test_rejects_extra_fields_in_a_shell_record(self):
        self.write_inbox_record(COLD_ID, False, 10, captureText="private words")
        self.assertTrue(any("only identifier and delivery facts" in error for error in self.errors()))

    def test_rejects_a_cold_record_written_after_the_web_ui_loaded(self):
        self.write_inbox_record(COLD_ID, True, 10)
        self.assertTrue(any("webview-free" in error for error in self.errors()))
        self.assertEqual(self.errors(require_cold_without_webview=False), [])

    def test_rejects_a_warm_record_written_before_the_web_ui_loaded(self):
        self.write_inbox_record(WARM_ID, False, 20)
        self.assertTrue(any("warm handoff" in error for error in self.errors()))

    def test_rejects_the_wrong_rejection_count(self):
        self.write_rejections(1, "invalid_capture_id")
        self.assertTrue(any("rejection count" in error for error in self.errors()))

    def test_rejects_an_unknown_rejection_reason(self):
        self.write_rejections(2, "because")
        self.assertTrue(any("not a known code" in error for error in self.errors()))

    def test_rejects_a_non_canonical_identifier_in_the_log(self):
        self.assertTrue(any("canonical" in error for error in self.errors(LOG.replace(COLD_ID, COLD_ID.lower()))))


if __name__ == "__main__":
    unittest.main()
