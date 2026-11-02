#!/usr/bin/env bash
# Export failure diagnostics from .xcresult bundles so a failed job uploads kilobytes, not the bundle.
#
# usage: export-failure-diagnostics.sh OUTPUT_DIR BUNDLE...
#
# Per bundle: the test summary and the test list as JSON (xcresulttool get test-results summary|tests, Xcode 16+),
# then the attachments of the failed tests only, in test order, until OUTPUT_DIR reaches CAP_BYTES (shared across the
# bundles; default 100 MB). MANIFEST.txt lists every file kept and every attachment omitted, with sizes.
# The cap is hard: the JSON files, the attachments and MANIFEST.txt together never exceed it (room is reserved for the
# manifest before anything else is written; the manifest is cut only if even that reserve is exceeded). A run-wide cap is shared by giving
# each failing job of the run its own share through CAP_BYTES.
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
# Room kept back for MANIFEST.txt so the listing survives a cap that attachments or metadata would otherwise fill.
manifest_reserve = min(8 * 1024 * 1024, cap_bytes // 4)
# Once an attachment does not fit, attachments stop: every later one is only listed as omitted.
cap_reached = False


def note(line):
    manifest.append(line)
    print(line)


def directory_size(directory):
    return sum(path.stat().st_size for path in directory.rglob("*") if path.is_file())


def bytes_available():
    """Bytes a payload may still take: the cap minus what the directory holds minus room for the manifest so far."""
    # 512 bytes of slack covers the line that notes an omitted payload.
    manifest_so_far = len(("\n".join(manifest) + "\n").encode()) + 512
    return cap_bytes - max(manifest_reserve, manifest_so_far) - directory_size(output_dir)


def run_tool(arguments, destination=None):
    result = subprocess.run(tool + arguments, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip().splitlines()[-1] if result.stderr.strip() else f"exit {result.returncode}")
    if destination is not None:
        size = len(result.stdout.encode())
        if size > bytes_available():
            note(f"omitted {destination.name} ({size} bytes): {cap_bytes} byte cap reached")
            return result.stdout
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
    global cap_reached
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
        if not written.exists():
            continue
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
                candidates = (case.get("nodeIdentifier"), case.get("nodeIdentifierURL"), case.get("name"))
                if entry.get("testIdentifier") in candidates or entry.get("testIdentifierURL") in candidates:
                    return index
            return len(failed)
        entries = [entry for entry in entries if entry.get("testIdentifier") in wanted or entry.get("testIdentifierURL") in wanted]
        entries.sort(key=rank)
        target = output_dir / f"{label}-attachments"
        candidates = []
        for entry in entries:
            for attachment in entry.get("attachments", []):
                exported = work / attachment["exportedFileName"]
                if not exported.is_file():
                    continue
                readable = attachment.get("suggestedHumanReadableName") or exported.name
                name = f"{entry['testIdentifier'].replace('/', '_')}--{exported.name}"
                candidates.append((exported, name, exported.stat().st_size, readable))

        def omitted_line(name, size):
            return f"omitted {label}-attachments/{name} ({size} bytes): cap reached"

        # Worst-case manifest bytes for every attachment still to come in this bundle (each listed as omitted or kept).
        pending_lines = [len(omitted_line(name, size)) + len(readable) + 32 for _, name, size, readable in candidates]
        for index, (exported, name, size, readable) in enumerate(candidates):
            pending_manifest = sum(pending_lines[index:])
            manifest_so_far = len(("\n".join(manifest) + "\n").encode())
            room = cap_bytes - directory_size(output_dir) - max(manifest_reserve, manifest_so_far + pending_manifest)
            if cap_reached or size > room:
                cap_reached = True
                note(omitted_line(name, size))
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
manifest_bytes = ("\n".join(manifest) + "\n").encode()
manifest_room = max(cap_bytes - directory_size(output_dir), 0)
if len(manifest_bytes) > manifest_room:
    manifest_bytes = manifest_bytes[:manifest_room]
(output_dir / "MANIFEST.txt").write_bytes(manifest_bytes)
PY
exit 0
