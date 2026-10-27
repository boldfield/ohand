#!/usr/bin/env python3
"""Judge a simulator upgrade run from the facts tools/release/simulator_upgrade_check.sh collected, and write its record.

Inputs are files the script wrote: what the seeding run did (seed-expectations.json), the app's own content-free upgrade
reports after the upgrade and after a relaunch (and before it, when the prior revision already had the report), and
`simctl appinfo` / data-container observations before seeding, after the upgrade and after the relaunch.

Every check is listed in the record with its outcome. The exit status is 0 only when all of them pass. The record holds
revisions, build numbers, identifiers the synthetic run created, counts, outcomes, the simulator's name and runtime, and
the collection time. It holds no capture text, no device identifier and no signing material.

Usage: verify_upgrade_report.py --facts FACTS.json --expectations seed-expectations.json --report-after R.json
           --report-relaunch R.json [--report-before R.json] --output RECORD.json
"""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

RECORD_SCHEMA = "ohand.upgrade-simulator-record.v1"
REPORT_SCHEMA = "ohand.upgrade-report.v1"
CAPABILITY = "text_interpretation"
REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")

BUILD_NUMBER_PATTERN = re.compile(r'CFBundleVersion\s*=\s*"?([0-9A-Za-z.\-]+)"?\s*;')
VERSION_PATTERN = re.compile(r'CFBundleShortVersionString\s*=\s*"?([0-9A-Za-z.\-]+)"?\s*;')


def parse_appinfo(text: str) -> Dict[str, Optional[str]]:
    """The installed build's version and build number from `xcrun simctl appinfo <device> <bundle id>` output."""
    build = BUILD_NUMBER_PATTERN.search(text)
    version = VERSION_PATTERN.search(text)
    return {"build_number": build.group(1) if build else None, "version": version.group(1) if version else None}


class Checks:
    def __init__(self) -> None:
        self.results: List[Dict[str, Any]] = []

    def check(self, name: str, passed: bool, detail: str = "") -> bool:
        self.results.append({"check": name, "passed": bool(passed), **({"detail": detail} if detail else {})})
        return bool(passed)

    @property
    def all_passed(self) -> bool:
        return bool(self.results) and all(result["passed"] for result in self.results)


def items_by_id(report: Dict[str, Any]) -> Dict[str, Dict[str, Any]]:
    return {item["item_id"]: item for item in report.get("items", [])}


def reminder_summary(report: Dict[str, Any], item_id: str) -> Optional[Dict[str, Any]]:
    item = items_by_id(report).get(item_id)
    reminder = item.get("reminder") if item else None
    if not reminder:
        return None
    return {key: reminder.get(key) for key in ("reminder_id", "schedule_generation", "resolved_instant")}


def check_report(checks: Checks, label: str, report: Dict[str, Any], expectations: Dict[str, Any],
                 candidate: Dict[str, Any]) -> None:
    checks.check(f"{label}: report schema", report.get("schema") == REPORT_SCHEMA, str(report.get("schema")))
    build = report.get("build") or {}
    checks.check(f"{label}: report names the candidate build",
                 build.get("revision") == candidate["revision"] and str(build.get("build_number")) == str(candidate["build_number"]),
                 f"build {build.get('build_number')}")
    items = items_by_id(report)

    kept = expectations["kept"]
    entry = items.get(kept["itemID"])
    checks.check(f"{label}: kept capture present and active",
                 bool(entry) and entry["lifecycle_state"] == "active" and entry["text_basis"] == "original"
                 and entry["text_length"] == kept["textLength"])

    corrected = expectations["corrected"]
    entry = items.get(corrected["itemID"])
    checks.check(f"{label}: correction present and authoritative",
                 bool(entry) and entry["lifecycle_state"] == "active" and entry["text_basis"] == "corrected"
                 and entry["text_length"] == corrected["correctedTextLength"]
                 and entry.get("corrected_text_length") == corrected["correctedTextLength"],
                 "")
    checks.check(f"{label}: original of the corrected capture kept",
                 bool(entry) and entry.get("original_text_length") == corrected["originalTextLength"])

    deleted = expectations["deleted"]["itemID"]
    probe = next((probe for probe in report.get("probes", []) if probe["item_id"] == deleted), None)
    checks.check(f"{label}: deleted item stays deleted", bool(probe) and probe["state"] == "not_found")
    checks.check(f"{label}: deleted item absent from the recents", deleted not in items)

    reminder_expected = expectations["reminder"]
    entry = items.get(reminder_expected["itemID"])
    reminder = entry.get("reminder") if entry else None
    checks.check(f"{label}: reminder item present with its reminder",
                 bool(entry) and entry["lifecycle_state"] == "active" and entry["text_length"] == reminder_expected["textLength"]
                 and bool(reminder) and bool(reminder.get("resolved_instant")))

    seeded_ids = {kept["itemID"], corrected["itemID"], reminder_expected["itemID"]}
    checks.check(f"{label}: exactly the seeded captures are listed",
                 set(items) == seeded_ids and report.get("recent_item_count") == len(seeded_ids),
                 f"{report.get('recent_item_count')} listed")

    requests = report.get("pending_reminder_requests", [])
    checks.check(f"{label}: exactly one pending notification request, for the reminder",
                 bool(reminder) and len(requests) == 1 and requests[0]["reminder_id"] == reminder["reminder_id"]
                 and requests[0]["schedule_generation"] == reminder["schedule_generation"],
                 f"{len(requests)} pending")

    configured = [provider for provider in report.get("providers", [])
                  if CAPABILITY in provider.get("default_for", []) and not provider.get("revoked")]
    checks.check(f"{label}: provider profile configured with its credential",
                 len(configured) == 1 and configured[0]["credential_bound"] and configured[0]["credential_status"] == "present")
    checks.check(f"{label}: processing routes to the provider",
                 len(configured) == 1 and configured[0]["route_covered"])
    checks.check(f"{label}: the provider names its model and the destination classes its route allows",
                 len(configured) == 1 and bool(configured[0].get("model")) and bool(configured[0].get("destination_classes")))


def provider_summary(report: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    configured = [provider for provider in report.get("providers", [])
                  if CAPABILITY in provider.get("default_for", []) and not provider.get("revoked")]
    if len(configured) != 1:
        return None
    return {key: configured[0].get(key) for key in
            ("profile_version", "protocol", "model", "destination_classes", "credential_bound", "credential_status")
            if key in configured[0]}


def provider_unchanged(before: Dict[str, Any], after: Dict[str, Any]) -> bool:
    """Every field the prior revision's report carried is the same after the upgrade (an older report has fewer)."""
    prior, now = provider_summary(before), provider_summary(after)
    return prior is not None and now is not None and all(now.get(key) == value for key, value in prior.items())


def evaluate(facts: Dict[str, Any], expectations: Dict[str, Any], report_after: Dict[str, Any],
             report_relaunch: Dict[str, Any], report_before: Optional[Dict[str, Any]]) -> Tuple[Checks, Dict[str, Any]]:
    checks = Checks()
    prior, candidate = facts["prior"], facts["candidate"]

    checks.check("prior and candidate are different full revisions",
                 REVISION_PATTERN.match(prior["revision"]) is not None and REVISION_PATTERN.match(candidate["revision"]) is not None
                 and prior["revision"] != candidate["revision"])
    checks.check("prior revision is an ancestor of the candidate", facts.get("prior_is_ancestor") is True)
    installed = facts["installed"]
    before, after, relaunch = (str(installed[key]["build_number"]) for key in ("before_seeding", "after_upgrade", "after_relaunch"))
    checks.check("build number before seeding is the prior build's", before == str(prior["build_number"]), before)
    checks.check("build number after the upgrade is the candidate's", after == str(candidate["build_number"]), after)
    checks.check("the upgrade raised the build number",
                 before.isdigit() and after.isdigit() and int(after) > int(before), f"{before} -> {after}")
    checks.check("build number after relaunch is unchanged", relaunch == after, relaunch)
    containers = facts["data_container"]
    checks.check("the data container's contents carried over the upgrade (marker file written before it)",
                 containers.get("marker_carried") is True)
    checks.check("data container is the same after relaunch", containers["after_upgrade"] == containers["after_relaunch"])
    checks.check("the candidate was installed over the prior build, never uninstalled", facts.get("uninstalled") is False)

    check_report(checks, "after upgrade", report_after, expectations, candidate)
    check_report(checks, "after relaunch", report_relaunch, expectations, candidate)

    reminder_id = expectations["reminder"]["itemID"]
    checks.check("reminder identity and fire time are unchanged by the relaunch",
                 reminder_summary(report_after, reminder_id) is not None
                 and reminder_summary(report_after, reminder_id) == reminder_summary(report_relaunch, reminder_id))
    checks.check("pending notification requests are unchanged by the relaunch",
                 report_after.get("pending_reminder_requests") == report_relaunch.get("pending_reminder_requests"))

    prior_report = "absent"
    if report_before is not None:
        checks.check("the provider profile version, type, model, destinations and credential are unchanged by the upgrade",
                     provider_unchanged(report_before, report_after))
        prior_report = "present"
        checks.check("prior report: reminder identifier and fire time equal after the upgrade",
                     reminder_summary(report_before, reminder_id) is not None
                     and reminder_summary(report_before, reminder_id) == reminder_summary(report_after, reminder_id))
        checks.check("prior report: pending notification requests equal after the upgrade",
                     report_before.get("pending_reminder_requests") == report_after.get("pending_reminder_requests"))
    ui = facts.get("ui_checks", {})
    checks.check("UI run after the upgrade passed (captures, correction, search, recents, reminder time)",
                 ui.get("verify_exit_status") == 0)
    checks.check("UI seeding run passed", ui.get("seed_exit_status") == 0)

    record = {
        "schema": RECORD_SCHEMA,
        "result": "passed" if checks.all_passed else "failed",
        "collected_at": facts["collected_at"],
        "run": facts.get("run"),
        "prior": prior,
        "candidate": candidate,
        "installed_build_numbers": {"before_seeding": before, "after_upgrade": after, "after_relaunch": relaunch},
        # The simulator's installer can move the app's data to a new container directory when it installs over an app; the
        # contents (marker file, then everything the app reports) carry over. Recorded as it happened, not asserted.
        "data_container_path_unchanged_by_install": containers["before_upgrade"] == containers["after_upgrade"],
        "data_container_unchanged_after_relaunch": containers["after_upgrade"] == containers["after_relaunch"],
        "simulator": facts["simulator"],
        "prior_revision_has_upgrade_report": prior_report,
        "seeded": {"captures": 4, "corrections": 1, "deleted": 1, "provider_profiles": 1, "reminders": 1},
        "checks": checks.results,
    }
    return checks, record


def load(path: Path) -> Dict[str, Any]:
    return json.loads(path.read_text())


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--facts", required=True)
    parser.add_argument("--expectations", required=True)
    parser.add_argument("--report-after", required=True)
    parser.add_argument("--report-relaunch", required=True)
    parser.add_argument("--report-before")
    parser.add_argument("--output", required=True)
    arguments = parser.parse_args(argv)
    try:
        report_before = load(Path(arguments.report_before)) if arguments.report_before and Path(arguments.report_before).is_file() else None
        checks, record = evaluate(load(Path(arguments.facts)), load(Path(arguments.expectations)),
                                  load(Path(arguments.report_after)), load(Path(arguments.report_relaunch)), report_before)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: the upgrade facts could not be read ({type(error).__name__}: {error})", file=sys.stderr)
        return 2
    Path(arguments.output).write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    for result in checks.results:
        print(("PASS " if result["passed"] else "FAIL ") + result["check"] + (f" ({result['detail']})" if result.get("detail") else ""))
    print(f"upgrade check {record['result']}")
    return 0 if checks.all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
