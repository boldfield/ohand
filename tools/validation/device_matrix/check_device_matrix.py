#!/usr/bin/env python3
"""Check an ohand.device-matrix.v1 record (T05a). Pure Python 3, no dependencies.

Usage: check_device_matrix.py RECORD [RECORD ...]
       check_device_matrix.py --all-in DIRECTORY   (every *.json record in it; passes when there is none)

Exits 1 and names every problem found; exits 0 when each record follows the format in
docs/validation/evidence/device-matrix/README.md.
"""

import argparse
import json
import re
import sys
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPOSITORY_ROOT = HERE.parents[2]
REQUIRED_CASES_PATH = HERE / "required_cases.json"
DEFAULT_RECORD_DIRECTORY = REPOSITORY_ROOT / "docs" / "validation" / "evidence" / "device-matrix"
CANDIDATE_DIRECTORY = "docs/releases/m1-trial"

SCHEMA = "ohand.device-matrix.v1"
TOP_LEVEL_KEYS = {
    "schema", "collected_at", "candidate_record", "build_number", "revision", "device", "capabilities_enabled",
    "cases", "latency", "private_evidence_reference", "issues",
}
DEVICE_KEYS = {"model", "ios_version"}
CASE_KEYS = {"outcome", "artifacts", "observed_at", "reason", "notes"}
OUTCOMES = ("passed", "failed", "blocked", "not_applicable")
LATENCY_KEYS = {"sample_count", "median_ms", "p95_ms", "min_ms", "max_ms", "second_source_artifact"}
LATENCY_STATISTICS = ("median_ms", "p95_ms", "min_ms", "max_ms")
MINIMUM_LATENCY_SAMPLES = 5
ISSUE_KEYS = {"severity", "summary", "follow_up_task", "acceptance"}
SEVERITIES = ("low", "medium", "high", "critical")
MAXIMUM_NOTES_LENGTH = 240
ARTIFACT_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
TIMESTAMP = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
REVISION = re.compile(r"^[0-9a-f]{40}$")
# A string that looks like a hash, a certificate or profile fingerprint, or a device identifier. The revision is the only
# 40-hex value the record may hold, and it is checked by name before this scan.
IDENTIFIER_PATTERNS = (
    ("a hex run of 32 or more digits (a hash)", re.compile(r"(?<![0-9A-Fa-f])[0-9A-Fa-f]{32,}(?![0-9A-Fa-f])")),
    ("a device UDID", re.compile(r"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{16}\b")),
    ("a UUID", re.compile(r"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b")),
    ("a serial-number-like or ECID value", re.compile(r"\b(?:ECID|UDID|serial)\b[^A-Za-z]*[0-9A-Fa-f]{8,}", re.IGNORECASE)),
)


class RecordObject(dict):
    """A JSON object that remembers keys it saw more than once, which json.load would otherwise collapse silently."""
    repeated_keys = ()


def read_object(pairs):
    result = RecordObject()
    repeated = []
    for key, value in pairs:
        if key in result:
            repeated.append(key)
        result[key] = value
    result.repeated_keys = tuple(repeated)
    return result


def load_required(path=REQUIRED_CASES_PATH):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def is_timestamp(value):
    if not isinstance(value, str) or not TIMESTAMP.match(value):
        return False
    try:
        datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return False
    return True


def walk_strings(value, location):
    if isinstance(value, str):
        yield location, value
    elif isinstance(value, dict):
        for key, child in value.items():
            yield from walk_strings(child, f"{location}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_strings(child, f"{location}[{index}]")


def check_unknown_keys(mapping, allowed, location, problems):
    for key in sorted(set(mapping) - allowed):
        problems.append(f"{location}: key '{key}' is outside the format")


def check_identifiers(record, problems):
    for location, text in walk_strings({k: v for k, v in record.items() if k != "revision"}, "record"):
        for description, pattern in IDENTIFIER_PATTERNS:
            if pattern.search(text):
                problems.append(f"{location}: holds {description}; the record holds no team, profile, certificate or device identifier and no hash of one")


def check_candidate(record, repository_root, problems):
    reference = record.get("candidate_record")
    if not isinstance(reference, str) or not reference:
        problems.append("candidate_record: missing; name the candidate record under docs/releases/m1-trial/")
        return
    relative = Path(reference)
    if relative.is_absolute() or ".." in relative.parts or relative.parent.as_posix() != CANDIDATE_DIRECTORY or relative.suffix != ".json":
        problems.append(f"candidate_record: '{reference}' is not a .json file directly under {CANDIDATE_DIRECTORY}/")
        return
    path = repository_root / relative
    if not path.is_file():
        problems.append(f"candidate_record: '{reference}' is not an existing file")
        return
    try:
        candidate = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        problems.append(f"candidate_record: '{reference}' is not readable JSON ({error})")
        return
    if not isinstance(candidate, dict):
        problems.append(f"candidate_record: '{reference}' is not a JSON object")
        return
    for field in ("build_number", "revision"):
        if str(candidate.get(field)) != str(record.get(field)):
            problems.append(f"candidate_record: {field} '{record.get(field)}' does not match '{candidate.get(field)}' in {reference}")


def check_header(record, problems):
    if record.get("schema") != SCHEMA:
        problems.append(f"schema: must be '{SCHEMA}'")
    if not is_timestamp(record.get("collected_at")):
        problems.append("collected_at: must be an ISO 8601 timestamp with a time zone")
    build_number = record.get("build_number")
    if not isinstance(build_number, str) or not build_number.isdigit():
        problems.append("build_number: must be the candidate's build number as a string of digits")
    if not isinstance(record.get("revision"), str) or not REVISION.match(record["revision"]):
        problems.append("revision: must be the full 40-digit lowercase git SHA")
    device = record.get("device")
    if not isinstance(device, dict):
        problems.append("device: must be an object holding model and ios_version")
    else:
        check_unknown_keys(device, DEVICE_KEYS, "device", problems)
        for field in sorted(DEVICE_KEYS):
            if not isinstance(device.get(field), str) or not device[field].strip():
                problems.append(f"device.{field}: must be a non-empty string")
    reference = record.get("private_evidence_reference")
    if not isinstance(reference, str) or not reference.strip():
        problems.append("private_evidence_reference: must be a non-empty description of where the private evidence is kept")


def check_capabilities(record, required, problems):
    enabled = record.get("capabilities_enabled")
    if not isinstance(enabled, list) or not all(isinstance(item, str) for item in enabled):
        problems.append("capabilities_enabled: must be a list of capability names (an empty list when the candidate enables none)")
        return []
    for name in enabled:
        if name not in required["capabilities"]:
            problems.append(f"capabilities_enabled: '{name}' is not one of {', '.join(required['capabilities'])}")
    if len(set(enabled)) != len(enabled):
        problems.append("capabilities_enabled: repeats a capability")
    return enabled


def check_case(identifier, case, spec, enabled, required, problems):
    where = f"cases.{identifier}"
    if not isinstance(case, dict):
        problems.append(f"{where}: must be an object")
        return
    check_unknown_keys(case, CASE_KEYS, where, problems)
    outcome = case.get("outcome")
    if outcome not in OUTCOMES:
        problems.append(f"{where}.outcome: '{outcome}' is outside the enumeration {', '.join(OUTCOMES)}")
        return
    artifacts = case.get("artifacts")
    if artifacts is not None and (not isinstance(artifacts, list) or not all(isinstance(name, str) and ARTIFACT_NAME.match(name) for name in artifacts)):
        problems.append(f"{where}.artifacts: must be a list of artifact file names (letters, digits, '.', '_' and '-' only)")
        artifacts = []
    elif isinstance(artifacts, list):
        for name in artifacts:
            if not name.startswith(identifier + "-"):
                problems.append(f"{where}.artifacts: '{name}' does not follow the pattern '{identifier}-<detail>.<extension>'")
    if case.get("observed_at") is not None and not is_timestamp(case["observed_at"]):
        problems.append(f"{where}.observed_at: must be an ISO 8601 timestamp with a time zone")
    reason = case.get("reason")
    if reason is not None and (not isinstance(reason, str) or not reason.strip()):
        problems.append(f"{where}.reason: must be a non-empty string")
        reason = None
    notes = case.get("notes")
    if notes is not None and (not isinstance(notes, str) or len(notes) > MAXIMUM_NOTES_LENGTH):
        problems.append(f"{where}.notes: must be a string of at most {MAXIMUM_NOTES_LENGTH} characters holding counts and outcomes only")

    if outcome in ("passed", "failed"):
        if not artifacts:
            problems.append(f"{where}: a {outcome} case needs at least one artifact")
        if not case.get("observed_at"):
            problems.append(f"{where}: a {outcome} case needs observed_at")
    else:
        if not reason:
            problems.append(f"{where}: a {outcome} case needs a reason")
    capability = spec.get("capability")
    if outcome == "not_applicable":
        allowed = []
        if capability and capability not in enabled:
            allowed.append(required["not_applicable_reasons"]["capability"])
        if spec.get("mac_inlet"):
            allowed.append(required["not_applicable_reasons"]["mac_inlet"])
        if not allowed:
            problems.append(f"{where}: not_applicable is not allowed for this case" + (f" because the candidate enables {capability}" if capability else ""))
        elif reason and reason not in allowed:
            problems.append(f"{where}.reason: must be exactly '{allowed[0]}'" if len(allowed) == 1 else f"{where}.reason: must be one of '{allowed[0]}' or '{allowed[1]}'")
    elif capability and capability not in enabled:
        problems.append(f"{where}: the candidate does not enable {capability}, so the case must be not_applicable with reason '{required['not_applicable_reasons']['capability']}'")


def check_cases(record, enabled, required, problems):
    cases = record.get("cases")
    if isinstance(cases, list):
        # A list can repeat an identifier; the format is an object keyed by identifier, so report repeats by name.
        names = [entry.get("id") for entry in cases if isinstance(entry, dict)]
        for name in sorted({n for n in names if names.count(n) > 1}):
            problems.append(f"cases: identifier '{name}' is repeated")
        problems.append("cases: must be an object keyed by case identifier, not a list")
        return
    if not isinstance(cases, dict):
        problems.append("cases: must be an object keyed by case identifier")
        return
    for name in sorted(set(getattr(cases, "repeated_keys", ()))):
        problems.append(f"cases: identifier '{name}' is repeated")
    specs = {spec["id"]: spec for spec in required["cases"]}
    for identifier in specs:
        if identifier not in cases:
            problems.append(f"cases: required identifier '{identifier}' is missing")
    for identifier in cases:
        if identifier not in specs:
            problems.append(f"cases: identifier '{identifier}' is not a required case (key outside the format)")
    for identifier, spec in specs.items():
        if identifier in cases:
            check_case(identifier, cases[identifier], spec, enabled, required, problems)


def check_latency(record, required, problems):
    latency = record.get("latency")
    if not isinstance(latency, dict):
        problems.append("latency: must be an object keyed by series name")
        return
    for series in required["latency_series"]:
        if series not in latency:
            problems.append(f"latency: series '{series}' is missing")
    for series, values in latency.items():
        where = f"latency.{series}"
        if series not in required["latency_series"]:
            problems.append(f"latency: series '{series}' is outside the format")
            continue
        if not isinstance(values, dict):
            problems.append(f"{where}: must be an object")
            continue
        check_unknown_keys(values, LATENCY_KEYS, where, problems)
        count = values.get("sample_count")
        if isinstance(count, bool) or not isinstance(count, int):
            problems.append(f"{where}.sample_count: missing or not an integer")
        elif count < MINIMUM_LATENCY_SAMPLES:
            problems.append(f"{where}.sample_count: {count} is under the minimum of {MINIMUM_LATENCY_SAMPLES} trials")
        statistics = {}
        for field in LATENCY_STATISTICS:
            if not is_number(values.get(field)) or values[field] < 0:
                problems.append(f"{where}.{field}: must be a non-negative number of milliseconds")
            else:
                statistics[field] = values[field]
        if len(statistics) == len(LATENCY_STATISTICS) and not (
                statistics["min_ms"] <= statistics["median_ms"] <= statistics["p95_ms"] <= statistics["max_ms"]):
            problems.append(f"{where}: must satisfy min_ms <= median_ms <= p95_ms <= max_ms")
        second = values.get("second_source_artifact")
        if second is not None and (not isinstance(second, str) or not ARTIFACT_NAME.match(second)):
            problems.append(f"{where}.second_source_artifact: must be an artifact file name")


def check_issues(record, problems):
    issues = record.get("issues")
    if not isinstance(issues, list):
        problems.append("issues: must be a list (empty when the run found none)")
        return
    for index, issue in enumerate(issues):
        where = f"issues[{index}]"
        if not isinstance(issue, dict):
            problems.append(f"{where}: must be an object")
            continue
        check_unknown_keys(issue, ISSUE_KEYS, where, problems)
        if issue.get("severity") not in SEVERITIES:
            problems.append(f"{where}.severity: must be one of {', '.join(SEVERITIES)}")
        if not isinstance(issue.get("summary"), str) or not issue["summary"].strip():
            problems.append(f"{where}.summary: must be a non-empty string")
        follow_up, acceptance = issue.get("follow_up_task"), issue.get("acceptance")
        if bool(follow_up) == bool(acceptance):
            problems.append(f"{where}: needs exactly one of follow_up_task or acceptance (an explicit acceptance for the trial)")


def check_record(record, repository_root=REPOSITORY_ROOT, required=None):
    """Return the list of problems in the record; an empty list means it conforms."""
    required = required or load_required()
    if not isinstance(record, dict):
        return ["record: must be a JSON object"]
    problems = []
    check_unknown_keys(record, TOP_LEVEL_KEYS, "record", problems)
    for key in sorted(TOP_LEVEL_KEYS - set(record)):
        problems.append(f"record: required key '{key}' is missing")
    check_header(record, problems)
    check_candidate(record, Path(repository_root), problems)
    enabled = check_capabilities(record, required, problems)
    check_cases(record, enabled, required, problems)
    check_latency(record, required, problems)
    check_issues(record, problems)
    check_identifiers(record, problems)
    return problems


def check_file(path, repository_root=REPOSITORY_ROOT, required=None):
    try:
        record = json.loads(Path(path).read_text(encoding="utf-8"), object_pairs_hook=read_object)
    except (OSError, ValueError) as error:
        return [f"record: cannot be read as JSON ({error})"]
    return check_record(record, repository_root, required)


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("records", nargs="*", help="record files to check")
    parser.add_argument("--all-in", metavar="DIRECTORY", help="check every *.json record in DIRECTORY (none is fine)")
    parser.add_argument("--repository-root", default=str(REPOSITORY_ROOT), help=argparse.SUPPRESS)
    options = parser.parse_args(arguments)
    paths = [Path(record) for record in options.records]
    if options.all_in:
        paths += sorted(Path(options.all_in).glob("*.json"))
    if not paths and not options.all_in:
        parser.error("name a RECORD or use --all-in")
    failed = False
    for path in paths:
        problems = check_file(path, options.repository_root)
        for problem in problems:
            print(f"{path}: {problem}", file=sys.stderr)
        failed = failed or bool(problems)
        if not problems:
            print(f"{path}: ok")
    if options.all_in and not paths:
        print(f"{options.all_in}: no device-matrix record yet; nothing to check")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
