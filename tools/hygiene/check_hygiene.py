#!/usr/bin/env python3
"""Public-repository hygiene check.

Combines a pinned, maintained secret scanner (gitleaks) over the reachable git
history with project policy checks over every tracked file and every path still reachable in
history: no signing material, no committed environment files, no files in
private capture/recording/transcript directories, and no audio/video recordings
unless they sit under a documented fixture root next to a valid provenance record.

Exit status: 0 clean, 1 policy or secret findings, 2 the scanner could not run.
Output never includes matched secret values.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import PurePosixPath
from typing import List, NamedTuple, Optional, Set

GITLEAKS_VERSION = "8.21.2"
GITLEAKS_FINDINGS_EXIT_CODE = 2
GITLEAKS_TIMEOUT_SECONDS = 600
GITLEAKS_CONFIG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gitleaks.toml")

EXIT_CLEAN = 0
EXIT_FINDINGS = 1
EXIT_SCANNER_ERROR = 2

SIGNING_MATERIAL_SUFFIXES = (
    ".p12", ".pfx", ".p8", ".mobileprovision", ".provisionprofile",
    ".cer", ".crt", ".der", ".pem", ".key", ".jks", ".keystore",
    ".gpg", ".asc", ".certsigningrequest", ".keychain", ".keychain-db",
)
SSH_PRIVATE_KEY_NAMES = {"id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"}
ENV_FILE_TEMPLATE_SUFFIXES = (".example", ".sample", ".template")
SCANNER_SUPPRESSION_FILE_NAME = ".gitleaksignore"

MEDIA_SUFFIXES = (
    ".m4a", ".wav", ".mp3", ".aac", ".flac", ".ogg", ".oga", ".opus", ".caf",
    ".aif", ".aiff", ".amr", ".wma", ".3gp",
    ".mp4", ".mov", ".m4v", ".webm", ".mkv",
)
SYNTHETIC_FIXTURE_ROOTS = (
    "fixtures/",
    "core/tests/fixtures/",
    "ios/Tests/Fixtures/",
)
PROVENANCE_SUFFIX = ".provenance.json"
# Matched as a whole directory name, case-insensitively, only at the repository
# root or directly beneath a fixture root: deeper source modules such as
# core/src/store/captures/ are ordinary code.
PRIVATE_DIRECTORY_NAMES = {
    "captures", "recordings", "private", "personal", "transcripts",
    "conversations", "voice-memos", "voicememos", "voice_memos",
}


class Finding(NamedTuple):
    path: str
    message: str
    line: Optional[int] = None


class ScannerError(RuntimeError):
    """The maintained scanner could not produce a trustworthy result."""


def list_tracked_files(repo_dir: str) -> List[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"], capture_output=True, cwd=repo_dir, check=True
    )
    return sorted(entry.decode("utf-8", "surrogateescape") for entry in result.stdout.split(b"\0") if entry)


def check_signing_material(path: str) -> List[str]:
    name = PurePosixPath(path).name
    lowered = name.lower()
    if lowered.endswith(SIGNING_MATERIAL_SUFFIXES) or name in SSH_PRIVATE_KEY_NAMES:
        return ["signing material or private key file must not be committed"]
    if (lowered == ".env" or lowered.startswith(".env.")) and not lowered.endswith(ENV_FILE_TEMPLATE_SUFFIXES):
        return ["environment file must not be committed (use a .env.example template)"]
    return []


def check_scanner_suppression(path: str) -> List[str]:
    if PurePosixPath(path).name == SCANNER_SUPPRESSION_FILE_NAME:
        return ["scanner suppression file must not be committed; adjust tools/hygiene/gitleaks.toml in review instead"]
    return []


def check_private_path(path: str) -> List[str]:
    posix_path = PurePosixPath(path)
    directory_parts = posix_path.parts[:-1]
    if not directory_parts:
        return []
    candidate_names = {directory_parts[0].lower()}
    for fixture_root in SYNTHETIC_FIXTURE_ROOTS:
        root_parts = PurePosixPath(fixture_root).parts
        if directory_parts[:len(root_parts)] == root_parts and len(directory_parts) > len(root_parts):
            candidate_names.add(directory_parts[len(root_parts)].lower())
    if candidate_names & PRIVATE_DIRECTORY_NAMES:
        return ["files under private capture, recording or transcript directories must not be committed"]
    return []


def is_under_fixture_root(path: str) -> bool:
    return path.startswith(SYNTHETIC_FIXTURE_ROOTS)


def sha256_of_file(file_path: str) -> str:
    digest = hashlib.sha256()
    with open(file_path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def check_path_policy(path: str) -> List[str]:
    return check_signing_material(path) + check_scanner_suppression(path) + check_private_path(path)


def check_provenance_record(path: str, repo_dir: str, tracked: Set[str]) -> List[str]:
    sidecar_path = path + PROVENANCE_SUFFIX
    if sidecar_path not in tracked:
        return [f"missing tracked provenance record {PurePosixPath(sidecar_path).name}"]
    try:
        with open(os.path.join(repo_dir, sidecar_path), "r", encoding="utf-8") as handle:
            record = json.load(handle)
    except (OSError, ValueError):
        return [f"provenance record {PurePosixPath(sidecar_path).name} is not readable JSON"]
    if not isinstance(record, dict):
        return [f"provenance record {PurePosixPath(sidecar_path).name} must be a JSON object"]

    problems = []
    if record.get("synthetic") is not True:
        problems.append('"synthetic" must be true')
    if record.get("contains_personal_data") is not False:
        problems.append('"contains_personal_data" must be false')
    for required_text_field in ("generator", "description"):
        value = record.get(required_text_field)
        if not isinstance(value, str) or not value.strip():
            problems.append(f'"{required_text_field}" must be a non-empty string')
    recorded_digest = record.get("sha256")
    if not isinstance(recorded_digest, str) or recorded_digest != sha256_of_file(os.path.join(repo_dir, path)):
        problems.append('"sha256" must equal the SHA-256 of the media file')
    if problems:
        return [f"invalid provenance record {PurePosixPath(sidecar_path).name}: " + "; ".join(problems)]
    return []


def check_media_file(path: str, repo_dir: str, tracked: Set[str]) -> List[str]:
    if not path.lower().endswith(MEDIA_SUFFIXES):
        return []
    if not is_under_fixture_root(path):
        return ["audio/video file outside documented synthetic fixture roots"]
    return check_provenance_record(path, repo_dir, tracked)


def list_historical_paths(repo_dir: str) -> List[str]:
    result = subprocess.run(
        ["git", "log", "--format=", "--name-only", "--no-renames", "-z", "HEAD"],
        capture_output=True, cwd=repo_dir, check=True,
    )
    names = (entry.decode("utf-8", "surrogateescape").strip("\n") for entry in result.stdout.split(b"\0"))
    return sorted({name for name in names if name})


def read_git_object(repo_dir: str, revision_and_path: str) -> Optional[bytes]:
    result = subprocess.run(["git", "show", revision_and_path], capture_output=True, cwd=repo_dir)
    return result.stdout if result.returncode == 0 else None


def check_deleted_media_provenance(path: str, repo_dir: str) -> List[str]:
    """Verify a media file that no longer exists using the last tree that contained it."""
    last_touch = subprocess.run(
        ["git", "--literal-pathspecs", "rev-list", "-n", "1", "HEAD", "--", path],
        capture_output=True, text=True, cwd=repo_dir,
    ).stdout.strip()
    media_bytes = read_git_object(repo_dir, f"{last_touch}^:{path}") if last_touch else None
    sidecar_bytes = read_git_object(repo_dir, f"{last_touch}^:{path}{PROVENANCE_SUFFIX}") if last_touch else None
    if media_bytes is None:
        return ["cannot verify provenance of removed audio/video file in reachable history"]
    if sidecar_bytes is None:
        return ["removed audio/video file had no provenance record and remains in reachable history"]
    try:
        record = json.loads(sidecar_bytes.decode("utf-8"))
    except ValueError:
        record = None
    if (
        not isinstance(record, dict)
        or record.get("synthetic") is not True
        or record.get("contains_personal_data") is not False
        or record.get("sha256") != hashlib.sha256(media_bytes).hexdigest()
    ):
        return ["removed audio/video file had an invalid provenance record and remains in reachable history"]
    return []


def check_history_paths(repo_dir: str, tracked: Set[str]) -> List[Finding]:
    """Apply path policy to files deleted from the tree but still reachable from HEAD."""
    findings = []
    for path in list_historical_paths(repo_dir):
        if path in tracked:
            continue
        messages = check_path_policy(path)
        if path.lower().endswith(MEDIA_SUFFIXES):
            if is_under_fixture_root(path):
                messages += check_deleted_media_provenance(path, repo_dir)
            else:
                messages.append("audio/video file outside documented synthetic fixture roots")
        for message in messages:
            findings.append(Finding(path, f"{message} (deleted, still in reachable history)"))
    return findings


def check_tracked_files(repo_dir: str) -> List[Finding]:
    tracked_files = list_tracked_files(repo_dir)
    tracked = set(tracked_files)
    findings = []
    for path in tracked_files:
        if not os.path.isfile(os.path.join(repo_dir, path)):
            continue
        messages = (
            check_path_policy(path) + check_media_file(path, repo_dir, tracked)
        )
        for message in messages:
            findings.append(Finding(path, message))
    findings.extend(check_history_paths(repo_dir, tracked))
    return findings


def run_gitleaks(repo_dir: str, binary: str = "gitleaks", log_opts: str = "HEAD") -> List[Finding]:
    """Scan history reachable from log_opts; raise ScannerError on anything but a clean or findings result."""
    with tempfile.TemporaryDirectory() as scratch_dir:
        report_path = os.path.join(scratch_dir, "report.json")
        command = [
            binary, "git",
            "--config", GITLEAKS_CONFIG_PATH,
            "--ignore-gitleaks-allow",
            "--redact", "--no-banner", "--no-color",
            "--log-level", "error",
            "--exit-code", str(GITLEAKS_FINDINGS_EXIT_CODE),
            "--report-format", "json",
            "--report-path", report_path,
            f"--log-opts={log_opts}",
            ".",
        ]
        try:
            completed = subprocess.run(
                command, capture_output=True, text=True, cwd=repo_dir, timeout=GITLEAKS_TIMEOUT_SECONDS
            )
        except FileNotFoundError:
            raise ScannerError(f"{binary} is not installed or not on PATH (pinned version {GITLEAKS_VERSION})")
        except subprocess.TimeoutExpired:
            raise ScannerError(f"{binary} timed out after {GITLEAKS_TIMEOUT_SECONDS} seconds")

        if completed.returncode not in (0, GITLEAKS_FINDINGS_EXIT_CODE):
            stderr_excerpt = (completed.stderr or "").strip()[:500]
            raise ScannerError(f"{binary} failed with exit code {completed.returncode}: {stderr_excerpt}")

        matches = []
        if os.path.exists(report_path) and os.path.getsize(report_path) > 0:
            try:
                with open(report_path, "r", encoding="utf-8") as handle:
                    matches = json.load(handle)
            except ValueError:
                raise ScannerError(f"{binary} wrote a malformed report")
            if not isinstance(matches, list) or not all(isinstance(match, dict) for match in matches):
                raise ScannerError(f"{binary} wrote an unexpected report shape")
        if completed.returncode == GITLEAKS_FINDINGS_EXIT_CODE and not matches:
            raise ScannerError(f"{binary} reported findings but wrote no readable report")

        return [
            Finding(
                str(match.get("File", "unknown")),
                f"secret detected by gitleaks rule {match.get('RuleID', 'unknown')} "
                f"(commit {str(match.get('Commit', ''))[:8] or 'unknown'})",
                match.get("StartLine") if isinstance(match.get("StartLine"), int) else None,
            )
            for match in matches
        ]


def check_repository(repo_dir: str, run_scanner: bool = True, gitleaks_binary: str = "gitleaks") -> List[Finding]:
    """Return all findings; raises ScannerError if the scanner is required but unusable."""
    findings = check_tracked_files(repo_dir)
    if run_scanner:
        findings.extend(run_gitleaks(repo_dir, binary=gitleaks_binary))
    return findings


def format_report(findings: List[Finding]) -> str:
    if not findings:
        return "No hygiene issues detected."
    lines = ["Hygiene check failed:"]
    for finding in sorted(findings, key=lambda item: (item.path, item.line or 0, item.message)):
        location = f"{finding.path}:{finding.line}" if finding.line is not None else finding.path
        lines.append(f"  {location}: {finding.message}")
    return "\n".join(lines)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Check public-repository secret and fixture hygiene.")
    parser.add_argument("--repo", default=".", help="repository to check (default: current directory)")
    parser.add_argument("--gitleaks", default="gitleaks", help="gitleaks binary (default: gitleaks on PATH)")
    args = parser.parse_args(argv)

    try:
        findings = check_repository(args.repo, gitleaks_binary=args.gitleaks)
    except ScannerError as error:
        print(f"Hygiene scanner error: {error}", file=sys.stderr)
        return EXIT_SCANNER_ERROR
    if findings:
        print(format_report(findings), file=sys.stderr)
        return EXIT_FINDINGS
    print(format_report(findings))
    return EXIT_CLEAN


if __name__ == "__main__":
    sys.exit(main())
