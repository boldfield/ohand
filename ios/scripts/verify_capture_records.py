#!/usr/bin/env python3
"""Checks the CaptureProbe records that the app persisted in its simulator data container.

Used by smoke-capture-simulator.sh after each launch phase. It reads the real files the app wrote
(records/*.json and last-presented.json); it never trusts the process being alive.
"""
import argparse
import json
import sys
import uuid
from pathlib import Path

RECORD_KEYS = {"captureId", "source", "launchKind", "protectedDataAvailable", "committedAt", "syntheticText"}
SOURCES = {"directLaunch", "controlIntent"}
LAUNCH_KINDS = {"cold", "warm"}


def load_records(root):
    records = {}
    for path in sorted((root / "records").glob("*.json")):
        record = json.loads(path.read_text())
        records[path.stem] = record
    return records


def check_records(root, expected_kinds):
    errors = []
    records = load_records(root)
    for file_id, record in records.items():
        if set(record) != RECORD_KEYS:
            errors.append(f"record {file_id} has keys {sorted(record)}")
            continue
        try:
            uuid.UUID(record["captureId"])
        except ValueError:
            errors.append(f"record {file_id} captureId is not a UUID")
        if record["captureId"] != file_id:
            errors.append(f"record file {file_id}.json holds captureId {record['captureId']}")
        if record["source"] not in SOURCES:
            errors.append(f"record {file_id} has unknown source {record['source']!r}")
        if record["launchKind"] not in LAUNCH_KINDS:
            errors.append(f"record {file_id} has unknown launchKind {record['launchKind']!r}")
        if not isinstance(record["protectedDataAvailable"], bool):
            errors.append(f"record {file_id} protectedDataAvailable is not a boolean")
        if not record["syntheticText"]:
            errors.append(f"record {file_id} has no synthetic text")
    actual_kinds = sorted(record.get("launchKind") for record in records.values())
    if actual_kinds != sorted(expected_kinds):
        errors.append(f"launch kinds {actual_kinds} != expected {sorted(expected_kinds)}")
    if len(set(records)) != len(records):
        errors.append("capture IDs are not unique")
    return errors, records


def check_presented(root, records):
    presented_path = root / "last-presented.json"
    if not presented_path.exists():
        return ["last-presented.json is missing: the capture screen never rendered an outcome"]
    presented = json.loads(presented_path.read_text())
    errors = []
    if presented.get("captureId") not in records:
        errors.append(f"presented capture {presented.get('captureId')!r} has no persisted record")
    if presented.get("statusText") != "Saved":
        errors.append(f"presented status is {presented.get('statusText')!r}, not 'Saved'")
    lines = "\n".join(presented.get("lines", []))
    for other_id in records:
        if other_id != presented.get("captureId") and other_id in lines:
            errors.append("the capture screen showed another entry's capture ID")
    return errors


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="CaptureProbe directory inside the app data container")
    parser.add_argument("--expect-kinds", nargs="*", default=[], help="expected launchKind of each record, any order")
    args = parser.parse_args(argv)
    errors, records = check_records(args.root, args.expect_kinds)
    errors += check_presented(args.root, records)
    for error in errors:
        print(f"FAIL: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"verified {len(records)} persisted record(s): " + ", ".join(sorted(r["launchKind"] for r in records.values())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
