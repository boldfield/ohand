#!/usr/bin/env python3
"""Build and sign the M1 trial candidate (OhAndApp with its embedded capture extension) and write its candidate record.

This wraps tools/apple-build/sign_probe.py, which owns every signing step (profiles, keychain, archive, export). The
wrapper adds three things around it:

1. It refuses a dirty or shallow tree before any Apple tool runs (tools/release/build_info.py --require-clean).
2. It keeps the exported .ipa outside the repository and hashes it (SHA-256) before and after the copy.
3. It writes the candidate record described in docs/releases/m1-trial/README.md: revision, version and build number,
   the archive's SHA-256, the signing route and the profile expiry date. The record never holds a team identifier,
   profile name or UUID, certificate name or hash, device identifier, or any hash of them; it is checked against every
   sensitive value the signing run registered before it is written.

Signing inputs are the ones sign_probe.py reads from the environment (OHAND_SIGNING_PROFILE_PATH or
OHAND_SIGNING_PROFILE_MAP_PATH, OHAND_SIGNING_CERT_PATH, OHAND_SIGNING_CERT_PASSWORD[_FILE], ...).

  sign_trial_candidate.py --output-dir DIR [--record-dir DIR] [--evidence-dir DIR]
  sign_trial_candidate.py --verify RECORD --archive IPA

--verify recomputes the archive's SHA-256 and compares it with the record (exit 1 on a mismatch).

Exit status: sign_probe.py's (2 input, 3 missing profile for a bundle, 4 archive integrity, 1 other), 1 for a dirty or
shallow tree, a checksum mismatch or a record that would hold a sensitive value.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import sys
from pathlib import Path
from typing import List, Optional

RELEASE_DIRECTORY = Path(__file__).resolve().parent
REPO_ROOT = RELEASE_DIRECTORY.parents[1]
sys.path.insert(0, str(RELEASE_DIRECTORY))
sys.path.insert(0, str(REPO_ROOT / "tools" / "apple-build"))

import build_info  # noqa: E402
import sign_probe  # noqa: E402

CANDIDATE_SCHEMA = "ohand.trial-candidate.v1"
SCHEME = "OhAndApp"
DEFAULT_RECORD_DIRECTORY = REPO_ROOT / "docs" / "releases" / "m1-trial"
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
RECORD_KEYS = {
    "schema", "revision", "version", "build_number", "archive_file", "archive_sha256", "signing_route",
    "profile_expires_at", "created_at",
}


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


class CandidateRun(sign_probe.SigningRun):
    """A signing run that keeps the exported archive: copies it to `output_dir` and records its checksum."""

    def __init__(self, output_dir: Path, archive_name: str, *arguments):
        super().__init__(*arguments)
        self.output_dir = output_dir
        self.archive_name = archive_name
        self.archive_sha256: Optional[str] = None

    def _extract_signed_app(self, export_path: Path) -> Path:
        application = super()._extract_signed_app(export_path)
        exported = sorted(export_path.glob("*.ipa"))
        exported_checksum = sha256_of(exported[0])
        destination = self.output_dir / self.archive_name
        self.output_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(exported[0], destination)
        if sha256_of(destination) != exported_checksum:
            destination.unlink(missing_ok=True)
            raise sign_probe.SigningError("the kept archive does not match the exported archive", sign_probe.EXIT_FAILURE, "export")
        self.archive_sha256 = exported_checksum
        return application


def outside_repository(path: Path, label: str, repository: Path) -> Path:
    resolved = path.resolve()
    if any(resolved == root or root in resolved.parents for root in (REPO_ROOT, repository.resolve())):
        raise sign_probe.input_error(f"{label} must be outside the repository (the archive embeds a provisioning profile)")
    return resolved


def candidate_record(info: dict, run: CandidateRun, created_at: str) -> dict:
    profile = run.evidence_record("succeeded")["profile"]
    return {
        "schema": CANDIDATE_SCHEMA,
        "revision": info["revision"],
        "version": info["version"],
        "build_number": str(info["build_number"]),
        "archive_file": run.archive_name,
        "archive_sha256": run.archive_sha256,
        "signing_route": sign_probe.ROUTE,
        "profile_expires_at": profile["expires_at"],
        "created_at": created_at,
    }


def write_record(record: dict, record_dir: Path, forbidden_values: List[str]) -> Path:
    text = json.dumps(record, indent=2, sort_keys=True) + "\n"
    for value in forbidden_values:
        if value and value in text:
            raise sign_probe.SigningError("the candidate record would contain a sensitive value; not written",
                                          sign_probe.EXIT_FAILURE, "record")
    record_dir.mkdir(parents=True, exist_ok=True)
    destination = record_dir / f"candidate-{record['build_number']}-{record['revision'][:8]}.json"
    temporary = destination.with_suffix(".json.tmp")
    temporary.write_text(text)
    os.replace(temporary, destination)
    return destination


def verify(record_path: Path, archive_path: Path) -> int:
    try:
        record = json.loads(record_path.read_text())
        recorded = record["archive_sha256"]
    except (OSError, ValueError, KeyError):
        print("error: the candidate record cannot be read", file=sys.stderr)
        return 2
    if not isinstance(recorded, str) or not SHA256_PATTERN.match(recorded):
        print("error: the candidate record has no valid archive checksum", file=sys.stderr)
        return 2
    try:
        actual = sha256_of(archive_path)
    except OSError:
        print("error: the archive cannot be read", file=sys.stderr)
        return 2
    if actual != recorded:
        print("error: the archive's SHA-256 does not match the candidate record", file=sys.stderr)
        return 1
    print("archive matches the candidate record")
    return 0


def sign(arguments: argparse.Namespace) -> int:
    repository = Path(arguments.repo)
    try:
        info = build_info.derive(repository, require_clean=True)
    except build_info.BuildInfoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    try:
        output_dir = outside_repository(Path(arguments.output_dir), "--output-dir", repository)
    except sign_probe.SigningError as error:
        print(f"error: {error.message}", file=sys.stderr)
        return error.exit_code
    archive_name = f"OhAnd-{info['version']}-{info['build_number']}-{info['revision'][:8]}.ipa"
    # sign_probe.py archives the checkout its module globals name; point them at the certified checkout so the record
    # and the archive cannot describe different trees.
    certified_root = repository.resolve()
    sign_probe.REPO_ROOT = certified_root
    sign_probe.IOS_ROOT = certified_root / "ios"
    run = CandidateRun(output_dir, archive_name, SCHEME, False, Path(arguments.evidence_dir), dict(os.environ))
    sign_probe.install_signal_handlers()
    exit_code = sign_probe.execute(run)
    if exit_code != sign_probe.EXIT_OK:
        return exit_code
    try:
        # Signing writes its sanitized evidence record into the repository, so only the revision is compared here.
        if build_info.derive(repository)["revision"] != info["revision"]:
            raise sign_probe.SigningError("the checkout moved while the candidate was built", sign_probe.EXIT_FAILURE, "record")
        created_at = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        record = candidate_record(info, run, created_at)
        destination = write_record(record, Path(arguments.record_dir), run.redactor.values_forbidden_in_evidence())
    except build_info.BuildInfoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except sign_probe.SigningError as error:
        print(f"error: {error.message}", file=sys.stderr)
        return error.exit_code
    print(f"candidate record written: {destination.name}")
    print(f"candidate archive: {output_dir / archive_name}")
    return sign_probe.EXIT_OK


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Build, sign and record the M1 trial candidate.")
    parser.add_argument("--output-dir", help="directory outside the repository that receives the exported .ipa")
    parser.add_argument("--record-dir", default=str(DEFAULT_RECORD_DIRECTORY), help="directory for the candidate record")
    parser.add_argument("--evidence-dir", default=str(sign_probe.DEFAULT_EVIDENCE_DIR),
                        help="directory for sign_probe.py's sanitized evidence record")
    parser.add_argument("--repo", default=str(REPO_ROOT),
                        help="checkout that is both archived and certified in the record (default: this one)")
    parser.add_argument("--verify", metavar="RECORD", help="check --archive against RECORD's checksum instead of signing")
    parser.add_argument("--archive", help="the .ipa to check with --verify")
    arguments = parser.parse_args(argv)
    if arguments.verify:
        if not arguments.archive:
            parser.error("--verify needs --archive")
        return verify(Path(arguments.verify), Path(arguments.archive))
    if not arguments.output_dir:
        parser.error("--output-dir is required")
    return sign(arguments)


if __name__ == "__main__":
    sys.exit(main())
