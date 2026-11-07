"""Tests for check_device_matrix.py and its required-case list. Every record here is synthetic."""

import copy
import io
import json
import re
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import check_device_matrix as checker

HERE = Path(__file__).resolve().parent
REPOSITORY_ROOT = HERE.parents[2]
RESULTS_DOCUMENT = REPOSITORY_ROOT / "docs" / "validation" / "m1-device-results.md"
REVISION = "0123456789abcdef0123456789abcdef01234567"
CANDIDATE_PATH = "docs/releases/m1-trial/candidate-412-01234567.json"
CAPABILITY_REASON = "capability not enabled in candidate"
MAC_REASON = "Mac inlet not included in candidate"


def synthetic_record(required, enabled=()):
    cases = {}
    for spec in required["cases"]:
        identifier = spec["id"]
        capability = spec.get("capability")
        if capability and capability not in enabled:
            cases[identifier] = {"outcome": "not_applicable", "reason": CAPABILITY_REASON}
        elif spec.get("mac_inlet"):
            cases[identifier] = {"outcome": "not_applicable", "reason": MAC_REASON}
        else:
            cases[identifier] = {
                "outcome": "passed",
                "artifacts": [f"{identifier}-01.png"],
                "observed_at": "2026-11-10T09:15:00-08:00",
                "notes": "2 of 2 captures kept",
            }
    latency = {
        series: {"sample_count": 5, "median_ms": 900, "p95_ms": 1100, "min_ms": 800, "max_ms": 1200}
        for series in required["latency_series"]
    }
    return {
        "schema": "ohand.device-matrix.v1",
        "collected_at": "2026-11-10T17:00:00Z",
        "candidate_record": CANDIDATE_PATH,
        "build_number": "412",
        "revision": REVISION,
        "device": {"model": "iPhone 16 Pro", "ios_version": "26.6.2"},
        "capabilities_enabled": list(enabled),
        "cases": cases,
        "latency": latency,
        "private_evidence_reference": "maintainer's private folder, session 1",
        "issues": [{"severity": "low", "summary": "synthetic cosmetic issue", "acceptance": "accepted for the trial"}],
    }


class CheckerTestCase(unittest.TestCase):
    def setUp(self):
        self.required = checker.load_required()
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        candidate = self.root / CANDIDATE_PATH
        candidate.parent.mkdir(parents=True)
        candidate.write_text(json.dumps({"build_number": "412", "revision": REVISION}))

    def problems(self, record):
        return checker.check_record(record, self.root, self.required)

    def assertProblem(self, record, fragment):
        found = self.problems(record)
        self.assertTrue(any(fragment in problem for problem in found), f"no problem mentioning {fragment!r} in {found}")

    def record(self, enabled=()):
        return synthetic_record(self.required, enabled)


class PassingRecordTests(CheckerTestCase):
    def test_a_complete_synthetic_record_passes(self):
        self.assertEqual(self.problems(self.record()), [])

    def test_a_record_enabling_every_capability_passes_when_those_cases_ran(self):
        self.assertEqual(self.problems(self.record(enabled=self.required["capabilities"])), [])

    def test_failed_blocked_and_the_second_source_are_accepted_when_complete(self):
        record = self.record()
        record["cases"]["offline-1"] = {"outcome": "failed", "artifacts": ["offline-1-02.mp4"], "observed_at": "2026-11-10T09:20:00Z"}
        record["cases"]["offline-2"] = {"outcome": "blocked", "reason": "phone out of battery before the step"}
        record["latency"]["trigger_to_ready.cold_launch"]["second_source_artifact"] = "metrics-export-01.json"
        self.assertEqual(self.problems(record), [])


class RuleTests(CheckerTestCase):
    def test_missing_identifier(self):
        record = self.record()
        del record["cases"]["reminder-8"]
        self.assertProblem(record, "required identifier 'reminder-8' is missing")

    def test_unknown_identifier_is_outside_the_format(self):
        record = self.record()
        record["cases"]["invented-1"] = {"outcome": "blocked", "reason": "x"}
        self.assertProblem(record, "'invented-1' is not a required case")

    def test_repeated_identifier_in_the_json_object(self):
        text = json.dumps(self.record())
        text = text.replace('"offline-1": {', '"offline-1": {"outcome": "blocked", "reason": "first"}, "offline-1": {', 1)
        path = self.root / "repeat.json"
        path.write_text(text)
        found = checker.check_file(path, self.root, self.required)
        self.assertTrue(any("'offline-1' is repeated" in problem for problem in found), found)

    def test_repeated_identifier_in_a_list(self):
        record = self.record()
        record["cases"] = [{"id": "offline-1"}, {"id": "offline-1"}]
        self.assertProblem(record, "'offline-1' is repeated")

    def test_outcome_outside_the_enumeration(self):
        record = self.record()
        record["cases"]["state-1"]["outcome"] = "skipped"
        self.assertProblem(record, "outside the enumeration")

    def test_passed_without_artifact(self):
        record = self.record()
        del record["cases"]["state-1"]["artifacts"]
        self.assertProblem(record, "cases.state-1: a passed case needs at least one artifact")

    def test_failed_without_observed_at(self):
        record = self.record()
        record["cases"]["state-2"] = {"outcome": "failed", "artifacts": ["state-2-01.png"]}
        self.assertProblem(record, "cases.state-2: a failed case needs observed_at")

    def test_blocked_without_reason(self):
        record = self.record()
        record["cases"]["state-3"] = {"outcome": "blocked"}
        self.assertProblem(record, "cases.state-3: a blocked case needs a reason")

    def test_not_applicable_without_reason(self):
        record = self.record()
        record["cases"]["state-6"] = {"outcome": "not_applicable"}
        self.assertProblem(record, "cases.state-6: a not_applicable case needs a reason")

    def test_not_applicable_is_refused_for_an_ordinary_case(self):
        record = self.record()
        record["cases"]["state-3"] = {"outcome": "not_applicable", "reason": "cannot lock the phone"}
        self.assertProblem(record, "not_applicable is not allowed for this case")

    def test_not_applicable_for_an_enabled_capability_is_refused(self):
        record = self.record(enabled=["shadow"])
        record["cases"]["capability-shadow"] = {"outcome": "not_applicable", "reason": CAPABILITY_REASON}
        self.assertProblem(record, "because the candidate enables shadow")

    def test_a_case_of_a_capability_that_is_not_enabled_must_be_not_applicable(self):
        record = self.record()
        record["cases"]["config-4"] = {"outcome": "passed", "artifacts": ["config-4-01.png"], "observed_at": "2026-11-10T09:15:00Z"}
        self.assertProblem(record, "does not enable self_hosted_provider")

    def test_not_applicable_with_a_reason_outside_the_allowed_ones(self):
        record = self.record()
        record["cases"]["config-4"]["reason"] = "no time"
        self.assertProblem(record, f"must be exactly '{CAPABILITY_REASON}'")

    def test_candidate_record_that_does_not_exist(self):
        record = self.record()
        record["candidate_record"] = "docs/releases/m1-trial/candidate-999-deadbeef.json"
        self.assertProblem(record, "is not an existing file")

    def test_candidate_record_outside_the_candidate_directory(self):
        record = self.record()
        record["candidate_record"] = "../elsewhere.json"
        self.assertProblem(record, "directly under docs/releases/m1-trial/")

    def test_candidate_record_with_another_build_number(self):
        record = self.record()
        record["build_number"] = "413"
        self.assertProblem(record, "build_number '413' does not match '412'")

    def test_candidate_record_with_another_revision(self):
        record = self.record()
        record["revision"] = "f" * 40
        self.assertProblem(record, "revision")

    def test_latency_series_without_a_sample_count(self):
        record = self.record()
        del record["latency"]["end_of_input_to_save.text"]["sample_count"]
        self.assertProblem(record, "latency.end_of_input_to_save.text.sample_count: missing")

    def test_latency_series_under_five_samples(self):
        record = self.record()
        record["latency"]["trigger_to_ready.cold_launch"]["sample_count"] = 4
        self.assertProblem(record, "under the minimum of 5 trials")

    def test_latency_series_missing(self):
        record = self.record()
        del record["latency"]["end_of_input_to_save.voice"]
        self.assertProblem(record, "series 'end_of_input_to_save.voice' is missing")

    def test_latency_statistics_out_of_order(self):
        record = self.record()
        record["latency"]["trigger_to_ready.warm_switch"]["min_ms"] = 5000
        self.assertProblem(record, "min_ms <= median_ms")

    def test_latency_holding_samples_is_outside_the_format(self):
        record = self.record()
        record["latency"]["trigger_to_ready.warm_switch"]["samples_ms"] = [1, 2, 3, 4, 5]
        self.assertProblem(record, "key 'samples_ms' is outside the format")

    def test_key_outside_the_format_at_the_top(self):
        record = self.record()
        record["device_udid"] = "x"
        self.assertProblem(record, "key 'device_udid' is outside the format")

    def test_key_outside_the_format_in_device_and_case(self):
        record = self.record()
        record["device"]["name"] = "Someone's phone"
        record["cases"]["offline-1"]["transcript"] = "text"
        self.assertProblem(record, "device: key 'name' is outside the format")
        self.assertProblem(record, "cases.offline-1: key 'transcript' is outside the format")

    def test_missing_top_level_key(self):
        record = self.record()
        del record["issues"]
        self.assertProblem(record, "required key 'issues' is missing")

    def test_an_issue_needs_a_follow_up_or_an_acceptance(self):
        record = self.record()
        record["issues"] = [{"severity": "high", "summary": "synthetic"}]
        self.assertProblem(record, "exactly one of follow_up_task or acceptance")

    def test_artifact_name_must_follow_the_case_pattern(self):
        record = self.record()
        record["cases"]["offline-1"]["artifacts"] = ["screenshot.png"]
        self.assertProblem(record, "does not follow the pattern 'offline-1-<detail>.<extension>'")

    def test_a_device_identifier_or_hash_is_refused(self):
        record = self.record()
        record["cases"]["offline-1"]["notes"] = "phone 00008140-001A2B3C4D5E6F78"
        record["private_evidence_reference"] = "sha " + "a" * 64
        self.assertProblem(record, "a device UDID")
        self.assertProblem(record, "a hex run of 32 or more digits")

    def test_long_notes_are_refused(self):
        record = self.record()
        record["cases"]["offline-1"]["notes"] = "x" * 300
        self.assertProblem(record, "counts and outcomes only")

    def test_wrong_schema_and_bad_timestamp(self):
        record = self.record()
        record["schema"] = "ohand.device-matrix.v2"
        record["collected_at"] = "yesterday"
        self.assertProblem(record, "schema: must be")
        self.assertProblem(record, "collected_at")


class CommandLineTests(CheckerTestCase):
    def run_main(self, *arguments):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = checker.main(["--repository-root", str(self.root), *arguments])
        return code, out.getvalue(), err.getvalue()

    def test_exit_status_names_the_problem(self):
        record = self.record()
        del record["cases"]["reminder-8"]
        path = self.root / "bad.json"
        path.write_text(json.dumps(record))
        code, _, err = self.run_main(str(path))
        self.assertEqual(code, 1)
        self.assertIn("'reminder-8' is missing", err)

    def test_a_passing_record_exits_zero(self):
        path = self.root / "good.json"
        path.write_text(json.dumps(self.record()))
        self.assertEqual(self.run_main(str(path))[0], 0)

    def test_an_empty_directory_passes(self):
        empty = self.root / "empty"
        empty.mkdir()
        code, out, _ = self.run_main("--all-in", str(empty))
        self.assertEqual(code, 0)
        self.assertIn("no device-matrix record yet", out)

    def test_a_directory_with_one_bad_record_fails(self):
        directory = self.root / "records"
        directory.mkdir()
        (directory / "good.json").write_text(json.dumps(self.record()))
        (directory / "bad.json").write_text("{not json")
        self.assertEqual(self.run_main("--all-in", str(directory))[0], 1)

    def test_the_script_runs_as_a_command(self):
        result = subprocess.run([sys.executable, str(HERE / "check_device_matrix.py"), "--all-in", str(self.root / "none")],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_committed_records_pass(self):
        directory = checker.DEFAULT_RECORD_DIRECTORY
        for path in sorted(directory.glob("*.json")):
            self.assertEqual(checker.check_file(path), [], str(path))


class RequiredCaseListTests(unittest.TestCase):
    def setUp(self):
        self.required = checker.load_required()
        self.identifiers = [spec["id"] for spec in self.required["cases"]]

    def test_identifiers_are_unique_and_stable_in_form(self):
        self.assertEqual(len(self.identifiers), len(set(self.identifiers)))
        for identifier in self.identifiers:
            self.assertRegex(identifier, r"^[a-z]+(-[a-z0-9]+)+$")

    def test_the_protocol_sections_are_complete(self):
        expected = {"offline": 6, "state": 6, "reminder": 10, "retrieval": 7, "config": 4, "processing": 3}
        for prefix, count in expected.items():
            self.assertEqual([f"{prefix}-{n}" for n in range(1, count + 1)],
                             [i for i in self.identifiers if i.startswith(prefix + "-")])

    def test_every_capability_gate_names_a_known_capability(self):
        for spec in self.required["cases"]:
            if "capability" in spec:
                self.assertIn(spec["capability"], self.required["capabilities"])

    def test_the_results_document_and_the_list_name_the_same_identifiers(self):
        text = RESULTS_DOCUMENT.read_text(encoding="utf-8")
        start = text.index("<!-- case-table:start -->")
        end = text.index("<!-- case-table:end -->")
        table = text[start:end]
        documented = re.findall(r"^\| `([a-z0-9-]+)` \|", table, re.MULTILINE)
        self.assertEqual(len(documented), len(set(documented)), "the case table repeats an identifier")
        self.assertEqual(sorted(documented), sorted(self.identifiers))

    def test_every_case_has_a_procedure_section(self):
        text = RESULTS_DOCUMENT.read_text(encoding="utf-8")
        headings = re.findall(r"^#### `([a-z0-9-]+)`", text, re.MULTILINE)
        self.assertEqual(sorted(headings), sorted(self.identifiers))

    def test_the_document_lists_every_latency_series(self):
        text = RESULTS_DOCUMENT.read_text(encoding="utf-8")
        for series in self.required["latency_series"]:
            self.assertIn(f"`{series}`", text)


if __name__ == "__main__":
    unittest.main()
