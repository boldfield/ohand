#!/usr/bin/env python3
"""Checks the files CaptureProbe and the Tauri management shell persisted during ManagementHandoffUITests.

Used by the tauri-probe CI job after the UI test has driven both apps. It reads the real files from the two simulator data
containers and the HANDOFF-PHASE lines the UI test printed:
  HANDOFF-PHASE cold <id> notRunning
  HANDOFF-PHASE warm <id> background
  HANDOFF-PHASE rejected - <count>
  HANDOFF-PHASE large-text-capture <id> scrolls=<n>
  HANDOFF-PHASE large-text-management <id> ok
It never trusts that a process is alive.
"""
import argparse
import json
import re
import sys
from pathlib import Path

CAPTURE_ID_PATTERN = re.compile(r"^[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}$")
INBOX_RECORD_KEYS = {"captureId", "receivedAtUnixMs", "webviewReady"}
REJECTION_KEYS = {"count", "lastReason", "lastReceivedAtUnixMs"}
REJECTION_REASONS = {"bad_scheme", "unknown_route", "missing_capture_id", "unexpected_component", "invalid_capture_id"}
EXPECTED_PHASES = ["cold", "warm", "rejected", "large-text-capture", "large-text-management"]


def parse_phases(log_text):
    """Returns {phase: (capture_id_or_None, detail)}, keeping the first line printed for each phase."""
    phases = {}
    order = []
    for match in re.finditer(r"HANDOFF-PHASE (\S+) (\S+) (\S+)", log_text):
        name, capture_id, detail = match.groups()
        if name not in phases:
            phases[name] = (None if capture_id == "-" else capture_id, detail)
            order.append(name)
    return phases, order


def check(handoff_dir, capture_root, log_text, require_cold_without_webview=True):
    errors = []
    phases, order = parse_phases(log_text)
    if order != EXPECTED_PHASES:
        errors.append(f"phases {order} != expected {EXPECTED_PHASES}")
        return errors

    cold_id, cold_detail = phases["cold"]
    warm_id, warm_detail = phases["warm"]
    if cold_detail != "notRunning":
        errors.append(f"cold phase started with the shell in state {cold_detail!r}")
    if warm_detail != "background":
        errors.append(f"warm phase started with the shell in state {warm_detail!r}")
    for label, capture_id in (("cold", cold_id), ("warm", warm_id), ("large-text-capture", phases["large-text-capture"][0])):
        if not capture_id or not CAPTURE_ID_PATTERN.match(capture_id):
            errors.append(f"{label} phase capture ID {capture_id!r} is not a canonical identifier")
    if len({cold_id, warm_id, phases["large-text-capture"][0]}) != 3:
        errors.append("the handoffs did not carry three distinct identifiers")
    if phases["large-text-management"] != (phases["large-text-capture"][0], "ok"):
        errors.append(f"large-text management phase {phases['large-text-management']} does not repeat the large-text capture")
    if phases["rejected"][1] != "2":
        errors.append(f"expected 2 hostile URLs, phase reports {phases['rejected'][1]!r}")
    if errors:
        return errors

    large_id = phases["large-text-capture"][0]
    expected_ids = {cold_id, warm_id, large_id}
    inbox_files = {path.name for path in handoff_dir.iterdir()} if handoff_dir.is_dir() else set()
    expected_files = {f"{capture_id}.json" for capture_id in expected_ids} | {"rejections.json"}
    if inbox_files != expected_files:
        errors.append(f"shell inbox holds {sorted(inbox_files)}, expected exactly {sorted(expected_files)}")
        return errors

    inbox_records = {}
    for capture_id in expected_ids:
        record = json.loads((handoff_dir / f"{capture_id}.json").read_text())
        inbox_records[capture_id] = record
        if set(record) != INBOX_RECORD_KEYS:
            errors.append(f"shell record {capture_id} has keys {sorted(record)}; only identifier and delivery facts are allowed")
            continue
        if record["captureId"] != capture_id:
            errors.append(f"shell record file {capture_id}.json holds captureId {record['captureId']}")
        if not isinstance(record["webviewReady"], bool):
            errors.append(f"shell record {capture_id} webviewReady is not a boolean")

    for capture_id in expected_ids:
        native_path = capture_root / "records" / f"{capture_id}.json"
        if not native_path.is_file():
            errors.append(f"the shell received {capture_id} but CaptureProbe has no saved record with that identifier")
            continue
        native = json.loads(native_path.read_text())
        if native.get("captureId") != capture_id:
            errors.append(f"CaptureProbe record {capture_id}.json holds captureId {native.get('captureId')}")

    if not errors:
        if inbox_records[warm_id]["webviewReady"] is not True:
            errors.append("warm handoff was recorded before the web UI had loaded")
        if require_cold_without_webview and inbox_records[cold_id]["webviewReady"] is not False:
            errors.append("cold handoff was recorded after the web UI had loaded, so it does not show a webview-free save")
        if inbox_records[cold_id]["receivedAtUnixMs"] > inbox_records[warm_id]["receivedAtUnixMs"]:
            errors.append("warm handoff is older than the cold handoff")

    summary = json.loads((handoff_dir / "rejections.json").read_text())
    if set(summary) != REJECTION_KEYS:
        errors.append(f"rejection summary has keys {sorted(summary)}")
    else:
        if summary["count"] != 2:
            errors.append(f"rejection count {summary['count']} != 2")
        if summary["lastReason"] not in REJECTION_REASONS:
            errors.append(f"rejection reason {summary['lastReason']!r} is not a known code")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--handoff-dir", required=True, type=Path)
    parser.add_argument("--capture-root", required=True, type=Path)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--allow-cold-webview", action="store_true", help="do not require the cold record to precede the web UI")
    args = parser.parse_args()

    errors = check(args.handoff_dir, args.capture_root, args.log.read_text(errors="replace"), not args.allow_cold_webview)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print("Handoff evidence verified: the shell stored exactly the cold, warm and large-text identifiers CaptureProbe saved, and rejected 2 hostile URLs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
