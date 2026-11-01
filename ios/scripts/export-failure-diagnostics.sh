#!/usr/bin/env bash
# Export failure diagnostics from .xcresult bundles so a failed job uploads kilobytes, not the bundle.
#
# usage: export-failure-diagnostics.sh OUTPUT_DIR BUNDLE...
#
# Per bundle: the test summary and the test list as JSON (xcresulttool get test-results summary|tests, Xcode 16+),
# then the attachments of the failed tests only, in test order, until OUTPUT_DIR reaches CAP_BYTES (shared across the
# bundles; default 100 MB). MANIFEST.txt lists every file kept and every attachment omitted, with sizes.
# Always exits 0: the job has already failed and a diagnostics problem must not change its conclusion.
# See docs/features/ci-artifact-budget.md.
set -uo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 OUTPUT_DIR BUNDLE..." >&2
  exit 0
fi
output_dir="$1"
shift
mkdir -p "${output_dir}"
export CAP_BYTES="${CAP_BYTES:-104857600}"
export XCRESULTTOOL="${XCRESULTTOOL:-xcrun xcresulttool}"

python3 -I - "${output_dir}" "$@" <<'PY' || echo "export-failure-diagnostics: unexpected error (ignored)" >&2
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

output_dir = Path(sys.argv[1]).resolve()
bundles = sys.argv[2:]
cap_bytes = int(os.environ["CAP_BYTES"])
tool = shlex.split(os.environ["XCRESULTTOOL"])
manifest = []


def note(line):
    manifest.append(line)
    print(line)


def directory_size(directory):
    return sum(path.stat().st_size for path in directory.rglob("*") if path.is_file())


def run_tool(arguments, destination=None):
    result = subprocess.run(tool + arguments, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip().splitlines()[-1] if result.stderr.strip() else f"exit {result.returncode}")
    if destination is not None:
        destination.write_text(result.stdout)
    return result.stdout


def failed_test_cases(nodes):
    """Failed 'Test Case' nodes in document (test) order."""
    found = []
    for node in nodes:
        if node.get("nodeType") == "Test Case" and node.get("result") == "Failed":
            found.append(node)
        found.extend(failed_test_cases(node.get("children", [])))
    return found


def export_bundle(bundle, label):
    path = Path(bundle)
    if not path.is_dir():
        note(f"{bundle}: missing or not a bundle, nothing exported")
        return
    summary = output_dir / f"{label}-summary.json"
    tests = output_dir / f"{label}-tests.json"
    try:
        run_tool(["get", "test-results", "summary", "--path", bundle, "--compact"], summary)
        tests_json = run_tool(["get", "test-results", "tests", "--path", bundle, "--compact"], tests)
    except Exception as error:
        for partial in (summary, tests):
            partial.unlink(missing_ok=True)
        note(f"{bundle}: unreadable ({error}), nothing exported")
        return
    for written in (summary, tests):
        note(f"kept {written.name} ({written.stat().st_size} bytes)")
    failed = failed_test_cases(json.loads(tests_json).get("testNodes", []))
    if not failed:
        note(f"{bundle}: no failed test cases in the test list")
        return
    for case in failed:
        note(f"{bundle}: failed test {case.get('nodeIdentifier') or case.get('name')}")
    wanted = {value for case in failed for value in (case.get("nodeIdentifier"), case.get("nodeIdentifierURL"), case.get("name")) if value}
    work = Path(tempfile.mkdtemp(prefix="attachments-"))
    try:
        try:
            run_tool(["export", "attachments", "--path", bundle, "--output-path", str(work)])
            entries = json.loads((work / "manifest.json").read_text())
        except Exception as error:
            note(f"{bundle}: attachments could not be exported ({error})")
            return
        # Test order: follow the failed-test list, then anything the manifest attributes to a failed test.
        def rank(entry):
            for index, case in enumerate(failed):
                if entry.get("testIdentifier") in (case.get("nodeIdentifier"), case.get("nodeIdentifierURL"), case.get("name")):
                    return index
            return len(failed)
        entries = [entry for entry in entries if entry.get("testIdentifier") in wanted or entry.get("testIdentifierURL") in wanted]
        entries.sort(key=rank)
        target = output_dir / f"{label}-attachments"
        for entry in entries:
            for attachment in entry.get("attachments", []):
                exported = work / attachment["exportedFileName"]
                if not exported.is_file():
                    continue
                size = exported.stat().st_size
                readable = attachment.get("suggestedHumanReadableName") or exported.name
                name = f"{entry['testIdentifier'].replace('/', '_')}--{exported.name}"
                if directory_size(output_dir) + size > cap_bytes:
                    note(f"omitted {label}-attachments/{name} ({size} bytes, {readable}): {cap_bytes} byte cap reached")
                    continue
                target.mkdir(exist_ok=True)
                shutil.copy2(exported, target / name)
                note(f"kept {label}-attachments/{name} ({size} bytes, {readable})")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if not bundles:
    note("no bundles given")
for number, bundle in enumerate(bundles):
    label = f"{number:02d}-{Path(bundle).stem}"
    try:
        export_bundle(bundle, label)
    except Exception as error:
        note(f"{bundle}: export failed ({error})")
(output_dir / "MANIFEST.txt").write_text("\n".join(manifest) + "\n")
PY
exit 0
