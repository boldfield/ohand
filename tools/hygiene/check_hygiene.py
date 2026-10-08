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
from typing import Dict, List, NamedTuple, Optional, Set

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
# ISO base media files share the ftyp box with still-image formats, which are not recordings.
IMAGE_FTYP_BRANDS = {b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1", b"avif", b"avis", b"crx "}
MEDIA_SNIFF_LENGTH = 12
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


class TreeEntry(NamedTuple):
    path: str
    mode: str
    object_id: str


REGULAR_FILE_MODES = {"100644", "100755"}


def list_tracked_entries(repo_dir: str) -> List[TreeEntry]:
    """Every index entry (regular file, symlink or gitlink), independent of what the worktree holds."""
    result = subprocess.run(
        ["git", "ls-files", "--stage", "-z"], capture_output=True, cwd=repo_dir, check=True
    )
    entries = {}
    for record in result.stdout.split(b"\0"):
        if not record:
            continue
        metadata, _, raw_path = record.partition(b"\t")
        mode, object_id, _stage = metadata.decode("ascii").split()
        path = raw_path.decode("utf-8", "surrogateescape")
        entries[path] = TreeEntry(path, mode, object_id)
    return [entries[path] for path in sorted(entries)]


def read_tree_entry(repo_dir: str, commit: str, path: str) -> Optional[TreeEntry]:
    """The blob or gitlink recorded for path in commit, or None if the commit has no such file."""
    result = subprocess.run(
        ["git", "--literal-pathspecs", "ls-tree", "-z", "--full-tree", commit, "--", path],
        capture_output=True, cwd=repo_dir, check=True,
    )
    for record in result.stdout.split(b"\0"):
        metadata, _, raw_path = record.partition(b"\t")
        if raw_path.decode("utf-8", "surrogateescape") != path:
            continue
        mode, object_type, object_id = metadata.decode("ascii").split()
        if object_type == "tree":
            return None
        return TreeEntry(path, mode, object_id)
    return None


def read_blob(repo_dir: str, object_id: str) -> bytes:
    return subprocess.run(
        ["git", "cat-file", "blob", object_id], capture_output=True, cwd=repo_dir, check=True
    ).stdout


def read_blob_headers(repo_dir: str, object_ids: List[str], length: int = MEDIA_SNIFF_LENGTH) -> Dict[str, bytes]:
    """The first length bytes of each blob, read through one git cat-file process."""
    if not object_ids:
        return {}
    completed = subprocess.run(
        ["git", "cat-file", "--batch"], input="".join(f"{object_id}\n" for object_id in object_ids).encode("ascii"),
        capture_output=True, cwd=repo_dir, check=True,
    )
    output = completed.stdout
    headers = {}
    offset = 0
    for object_id in object_ids:
        header_end = output.index(b"\n", offset)
        _, object_type, size = output[offset:header_end].decode("ascii").split()
        if object_type != "blob":
            raise ScannerError(f"git object {object_id} is a {object_type}, expected a blob")
        content_start = header_end + 1
        headers[object_id] = output[content_start:content_start + min(int(size), length)]
        offset = content_start + int(size) + 1
    return headers


def looks_like_media(header: bytes) -> bool:
    """Recognise common audio/video containers from their leading bytes, whatever the file is named."""
    if header.startswith((b"ID3", b"OggS", b"fLaC", b"caff", b"#!AMR", b"\x1a\x45\xdf\xa3", b"\x30\x26\xb2\x75")):
        return True
    if header.startswith(b"RIFF") and header[8:12] in (b"WAVE", b"AVI "):
        return True
    if header.startswith(b"FORM") and header[8:12] in (b"AIFF", b"AIFC"):
        return True
    return header[4:8] == b"ftyp" and header[8:12] not in IMAGE_FTYP_BRANDS


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


def check_path_policy(path: str) -> List[str]:
    return check_signing_material(path) + check_scanner_suppression(path) + check_private_path(path)


def validate_provenance_bytes(sidecar_bytes: bytes, media_digest: str, sidecar_name: str) -> List[str]:
    """Validate one provenance sidecar against the SHA-256 of the media it describes."""
    try:
        record = json.loads(sidecar_bytes.decode("utf-8"))
    except ValueError:
        return [f"provenance record {sidecar_name} is not readable JSON"]
    if not isinstance(record, dict):
        return [f"provenance record {sidecar_name} must be a JSON object"]

    problems = []
    if record.get("synthetic") is not True:
        problems.append('"synthetic" must be true')
    if record.get("contains_personal_data") is not False:
        problems.append('"contains_personal_data" must be false')
    for required_text_field in ("generator", "description"):
        value = record.get(required_text_field)
        if not isinstance(value, str) or not value.strip():
            problems.append(f'"{required_text_field}" must be a non-empty string')
    if record.get("sha256") != media_digest:
        problems.append('"sha256" must equal the SHA-256 of the media file')
    if problems:
        return [f"invalid provenance record {sidecar_name}: " + "; ".join(problems)]
    return []


def check_media_version(repo_dir: str, media_entry: TreeEntry, sidecar_entry: Optional[TreeEntry]) -> List[str]:
    """Check one recorded version of a fixture media file against the sidecar recorded beside it."""
    sidecar_name = PurePosixPath(media_entry.path + PROVENANCE_SUFFIX).name
    if media_entry.mode not in REGULAR_FILE_MODES:
        return [f"audio/video fixture must be a regular file, not git mode {media_entry.mode}"]
    if sidecar_entry is None:
        return [f"missing tracked provenance record {sidecar_name}"]
    if sidecar_entry.mode not in REGULAR_FILE_MODES:
        return [f"provenance record {sidecar_name} must be a regular file, not git mode {sidecar_entry.mode}"]
    media_digest = hashlib.sha256(read_blob(repo_dir, media_entry.object_id)).hexdigest()
    return validate_provenance_bytes(read_blob(repo_dir, sidecar_entry.object_id), media_digest, sidecar_name)


def check_media_file(
    entry: TreeEntry, repo_dir: str, tracked: Dict[str, TreeEntry], has_media_content: bool = False
) -> List[str]:
    if not entry.path.lower().endswith(MEDIA_SUFFIXES):
        if not has_media_content:
            return []
        if not is_under_fixture_root(entry.path):
            return ["audio/video content (detected from file contents) outside documented synthetic fixture roots"]
    elif not is_under_fixture_root(entry.path):
        return ["audio/video file outside documented synthetic fixture roots"]
    return check_media_version(repo_dir, entry, tracked.get(entry.path + PROVENANCE_SUFFIX))


def list_historical_paths(repo_dir: str) -> List[str]:
    result = subprocess.run(
        ["git", "log", "--format=", "--name-only", "--no-renames", "-z", "HEAD"],
        capture_output=True, cwd=repo_dir, check=True,
    )
    names = (entry.decode("utf-8", "surrogateescape").strip("\n") for entry in result.stdout.split(b"\0"))
    return sorted({name for name in names if name})


def list_commits_touching(repo_dir: str, *paths: str) -> List[str]:
    result = subprocess.run(
        ["git", "--literal-pathspecs", "log", "--format=%H", "--no-renames", "--full-history", "HEAD", "--", *paths],
        capture_output=True, text=True, cwd=repo_dir, check=True,
    )
    return result.stdout.split()


def check_fixture_media_history(path: str, repo_dir: str) -> List[Finding]:
    """Require a valid, hash-matching sidecar in the same tree for every reachable version of a fixture.

    Commits that change only the sidecar are walked too, so deleting or corrupting the record while the
    media stays put is a reachable unprovenanced state.
    """
    sidecar_path = path + PROVENANCE_SUFFIX
    reported_messages: Set[str] = set()
    findings = []
    for commit in list_commits_touching(repo_dir, path, sidecar_path):
        media_entry = read_tree_entry(repo_dir, commit, path)
        if media_entry is None:
            continue
        sidecar_entry = read_tree_entry(repo_dir, commit, sidecar_path)
        if sidecar_entry is None:
            problems = [f"audio/video file version had no provenance record {PurePosixPath(sidecar_path).name}"]
        else:
            problems = check_media_version(repo_dir, media_entry, sidecar_entry)
        for problem in problems:
            if problem not in reported_messages:
                reported_messages.add(problem)
                findings.append(Finding(path, f"{problem} (commit {commit[:8]}, still in reachable history)"))
    return findings


def check_history_paths(repo_dir: str, tracked: Dict[str, TreeEntry]) -> List[Finding]:
    """Apply path and media policy to every path reachable from HEAD, including deleted and overwritten versions.

    Path rules for a path still in the index are already reported by check_tracked_files, which checks
    every index entry whatever its type, so only paths gone from the index are reported here. Fixture
    media versions are checked from the recorded blobs whatever now occupies the path.
    """
    findings = []
    for path in list_historical_paths(repo_dir):
        if path not in tracked:
            for message in check_path_policy(path):
                findings.append(Finding(path, f"{message} (deleted, still in reachable history)"))
        if not path.lower().endswith(MEDIA_SUFFIXES):
            continue
        if is_under_fixture_root(path):
            findings.extend(check_fixture_media_history(path, repo_dir))
        elif path not in tracked:
            findings.append(Finding(
                path, "audio/video file outside documented synthetic fixture roots (deleted, still in reachable history)"
            ))
    return findings


def check_tracked_files(repo_dir: str) -> List[Finding]:
    tracked_entries = list_tracked_entries(repo_dir)
    tracked = {entry.path: entry for entry in tracked_entries}
    sniffed_object_ids = sorted({
        entry.object_id for entry in tracked_entries
        if entry.mode in REGULAR_FILE_MODES and not entry.path.lower().endswith(MEDIA_SUFFIXES)
    })
    headers = read_blob_headers(repo_dir, sniffed_object_ids)
    findings = []
    for entry in tracked_entries:
        has_media_content = entry.object_id in headers and looks_like_media(headers[entry.object_id])
        for message in check_path_policy(entry.path) + check_media_file(entry, repo_dir, tracked, has_media_content):
            findings.append(Finding(entry.path, message))
    findings.extend(check_history_paths(repo_dir, tracked))
    return findings


def run_gitleaks(repo_dir: str, binary: str = "gitleaks", log_opts: str = "-m HEAD") -> List[Finding]:
    """Scan history reachable from log_opts; raise ScannerError on anything but a clean or findings result.

    git log -p shows no diff for a merge commit unless -m is given, so the default includes it: content
    added while resolving a merge is scanned against each parent.
    """
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


def ensure_full_history(repo_dir: str) -> None:
    """Reachable-history checks are meaningless on a truncated clone, so refuse to certify one."""
    result = subprocess.run(
        ["git", "rev-parse", "--is-shallow-repository"], capture_output=True, text=True, cwd=repo_dir
    )
    if result.returncode != 0:
        raise ScannerError("cannot determine whether the repository has full history")
    if result.stdout.strip() != "false":
        raise ScannerError("repository is a shallow clone; fetch full history (git fetch --unshallow) before checking")


def check_repository(repo_dir: str, run_scanner: bool = True, gitleaks_binary: str = "gitleaks") -> List[Finding]:
    """Return all findings; raises ScannerError if the scanner is required but unusable."""
    ensure_full_history(repo_dir)
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
