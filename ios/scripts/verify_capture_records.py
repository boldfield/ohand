#!/usr/bin/env python3
"""Checks the CaptureProbe records that the app persisted in its simulator data container.

Used by smoke-capture-simulator.sh after each phase. It reads the real files the app wrote
(records/*.json, pending-entry.json and last-presented.json); it never trusts the process being alive.
"""
import argparse
import json
import sys
import uuid
from pathlib import Path

RECORD_KEYS = {"captureId", "source", "launchKind", "protectedDataAvailable", "committedAt", "syntheticText"}
SOURCES = {"controlIntent", "shortcutURL"}
LAUNCH_KINDS = {"cold", "warm"}
IDLE_STATUS = "Ready"


def load_records(root):
    records = {}
    for path in sorted((root / "records").glob("*.json")):
        record = json.loads(path.read_text())
        records[path.stem] = record
    return records


def check_records(root, expected_kinds, expected_source):
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
        elif expected_source and record["source"] != expected_source:
            errors.append(f"record {file_id} has source {record['source']!r}, not {expected_source!r}")
        if record["launchKind"] not in LAUNCH_KINDS:
            errors.append(f"record {file_id} has unknown launchKind {record['launchKind']!r}")
        if not isinstance(record["protectedDataAvailable"], bool):
            errors.append(f"record {file_id} protectedDataAvailable is not a boolean")
        if not record["syntheticText"]:
            errors.append(f"record {file_id} has no synthetic text")
    actual_kinds = sorted(record.get("launchKind") for record in records.values())
    if actual_kinds != sorted(expected_kinds):
        errors.append(f"launch kinds {actual_kinds} != expected {sorted(expected_kinds)}")
    if (root / "pending-entry.json").exists():
        errors.append("pending-entry.json is still present: a handoff was registered but not committed")
    return errors, records


def check_presented(root, records, expected_presented, known_ids):
    presented_path = root / "last-presented.json"
    if not presented_path.exists():
        return ["last-presented.json is missing: the capture screen never rendered"]
    presented = json.loads(presented_path.read_text())
    errors = []
    new_ids = set(records) - set(known_ids)
    missing_known = set(known_ids) - set(records)
    if missing_known:
        errors.append(f"previously persisted records disappeared: {sorted(missing_known)}")
    if expected_presented == "idle":
        if presented.get("captureId") is not None:
            errors.append(f"idle screen expected but it shows capture {presented.get('captureId')!r}")
        if presented.get("statusText") != IDLE_STATUS:
            errors.append(f"presented status is {presented.get('statusText')!r}, not {IDLE_STATUS!r}")
        if new_ids:
            errors.append(f"a plain launch created records {sorted(new_ids)}")
        shown_id = None
    else:
        shown_id = presented.get("captureId")
        if shown_id not in records:
            errors.append(f"presented capture {shown_id!r} has no persisted record")
        if presented.get("statusText") != "Saved":
            errors.append(f"presented status is {presented.get('statusText')!r}, not 'Saved'")
        if new_ids != {shown_id}:
            errors.append(f"this handoff should add exactly the presented record; new records are {sorted(new_ids)}")
    lines = "\n".join(presented.get("lines", []))
    for other_id in records:
        if other_id != shown_id and other_id in lines:
            errors.append("the capture screen showed another entry's capture ID")
    return errors


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="CaptureProbe directory inside the app data container")
    parser.add_argument("--expect-kinds", nargs="*", default=[], help="expected launchKind of each record, any order")
    parser.add_argument("--expect-source", choices=sorted(SOURCES), help="source every record must have")
    parser.add_argument("--expect-presented", choices=["saved", "idle"], default="saved")
    parser.add_argument("--known-ids", nargs="*", default=[], help="capture IDs persisted before this phase")
    parser.add_argument("--quiet", action="store_true", help="print nothing; only the exit status (for polling)")
    args = parser.parse_args(argv)
    errors, records = check_records(args.root, args.expect_kinds, args.expect_source)
    errors += check_presented(args.root, records, args.expect_presented, args.known_ids)
    if not args.quiet:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
    if errors:
        return 1
    if not args.quiet:
        kinds = ", ".join(sorted(r["launchKind"] for r in records.values())) or "none"
        print(f"verified {len(records)} persisted record(s) [{kinds}], screen {args.expect_presented}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
