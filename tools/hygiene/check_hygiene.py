#!/usr/bin/env python3
"""
Fixture hygiene checker: enforce public-repository secret and privacy standards.

Prevents committed real secrets, private credentials, private captures, and
undocumented raw audio while allowing synthetic fixtures in documented paths.
"""

import re
import sys
import os
import subprocess
import json
from pathlib import Path
from typing import List, Tuple, Set, Optional

# Patterns that indicate potential secrets or private content
SECRET_PATTERNS = [
    # AWS credentials
    (r'AKIA[0-9A-Z]{16}', 'AWS Access Key ID'),
    (r'aws_secret_access_key\s*[:=]\s*[^\s]+', 'AWS Secret Access Key'),

    # API keys and tokens (support both = and : separators)
    (r'api[_-]?key\s*[:=]\s*[^\s]+', 'API Key'),
    (r'auth[_-]?token\s*[:=]\s*[^\s]+', 'Auth Token'),
    (r'x-api-key\s*[:=]\s*[^\s]+', 'X-API-Key'),
    (r'bearer\s+[a-zA-Z0-9._\-]+', 'Bearer Token'),

    # OAuth tokens
    (r'oauth[_-]?token\s*[:=]\s*[^\s]+', 'OAuth Token'),
    (r'access[_-]?token\s*[:=]\s*[^\s]+', 'Access Token'),
    (r'refresh[_-]?token\s*[:=]\s*[^\s]+', 'Refresh Token'),

    # Passwords
    (r'password\s*[:=]\s*[^\s]+', 'Password'),
    (r'passwd\s*[:=]\s*[^\s]+', 'Password'),

    # Private keys
    (r'-----BEGIN RSA PRIVATE KEY-----', 'RSA Private Key'),
    (r'-----BEGIN PRIVATE KEY-----', 'Private Key'),
    (r'-----BEGIN EC PRIVATE KEY-----', 'EC Private Key'),
    (r'-----BEGIN OPENSSH PRIVATE KEY-----', 'SSH Private Key'),

    # Apple/iOS specific
    (r'-----BEGIN CERTIFICATE-----', 'Certificate'),
    (r'-----BEGIN PKCS8 PRIVATE KEY-----', 'PKCS8 Private Key'),

    # Database URLs with credentials
    (r'(postgres|mysql|mongodb)://[^/\s]*:[^@\s]+@', 'Database URL with Credentials'),

    # GitHub tokens
    (r'ghp_[a-zA-Z0-9_]{36,255}', 'GitHub Personal Access Token'),
    (r'ghs_[a-zA-Z0-9_]{36,255}', 'GitHub OAuth Token'),

    # Anthropic/OpenAI API keys (more specific patterns first)
    (r'sk-ant-[a-zA-Z0-9_\-]{20,}', 'Anthropic API Key'),
    (r'sk-[a-zA-Z0-9_\-]{20,}', 'OpenAI-style API Key'),
]

# File extensions and paths that should not contain private captures
PRIVATE_CAPTURE_PATTERNS = [
    r'\.m4a$',    # Audio files
    r'\.wav$',    # Audio files
    r'\.mp3$',    # Audio files
    r'\.aac$',    # Audio files
    r'\.flac$',   # Audio files
    r'\.ogg$',    # Audio files
    r'\.wma$',    # Audio files
]

# Patterns for recording files that may contain private content
RECORDING_FILE_PATTERNS = [
    r'recording\.',
    r'capture\.',
    r'audio\.',
    r'session\.',
]

# Paths that are allowed for documented synthetic fixtures
SYNTHETIC_FIXTURE_PATHS = {
    'fixtures/',
    'ios/Tests/',
    'core/tests/',
    'tests/',
    'tools/evaluation/',
    'docs/validation/',
}

# Signing material that must not be committed
SIGNING_MATERIAL_PATTERNS = [
    r'\.p12$',                           # PKCS#12 certificates
    r'\.mobileprovision$',               # iOS provisioning profiles
    r'\.cer$',                           # Certificates
    r'\.pem$',                           # PEM encoded keys/certs
    r'\.key$',                           # Private keys
    r'\.jks$',                           # Java keystores
    r'\.keystore$',                      # Android keystores
    r'\.gpg$',                           # GPG keys
    r'\.asc$',                           # ASCII-armored keys
]

# Files to skip checking entirely
SKIP_PATTERNS = {
    r'\.git/',
    r'target/',
    r'Cargo.lock',
    r'\.DS_Store',
    r'node_modules/',
    r'\.venv/',
}


def is_skipped_path(path: str) -> bool:
    """Check if a path should be skipped."""
    for pattern in SKIP_PATTERNS:
        if re.search(pattern, path):
            return True
    return False


def is_synthetic_fixture_path(path: str) -> bool:
    """Check if a path is in an approved synthetic fixture location."""
    for fixture_path in SYNTHETIC_FIXTURE_PATHS:
        if path.startswith(fixture_path):
            return True
    return False


def has_provenance_file(audio_path: str) -> bool:
    """Check if audio file has a provenance sidecar file documenting it as synthetic."""
    path_obj = Path(audio_path)

    # Look for .provenance sidecar (e.g., audio.m4a.provenance)
    provenance_path = Path(str(path_obj) + '.provenance')
    if provenance_path.exists():
        try:
            with open(provenance_path, 'r') as f:
                content = f.read().lower()
                # Check that the provenance file documents it as synthetic
                if 'synthetic' in content or 'generated' in content:
                    return True
        except (OSError, IOError):
            pass

    # Look for adjacent README or MANIFEST file
    parent = path_obj.parent
    for manifest_name in ['README.md', 'MANIFEST.md', 'FIXTURES.md']:
        manifest_path = parent / manifest_name
        if manifest_path.exists():
            try:
                with open(manifest_path, 'r') as f:
                    content = f.read().lower()
                    # Check if the manifest documents this file as synthetic
                    if path_obj.name in content.lower() and ('synthetic' in content or 'generated' in content):
                        return True
            except (OSError, IOError):
                pass

    return False


def check_for_secrets(content: str, path: str) -> List[Tuple[str, int]]:
    """
    Check content for patterns matching secrets or private keys.

    Returns list of (issue_description, line_number) tuples.
    """
    issues = []
    lines = content.split('\n')

    for line_num, line in enumerate(lines, 1):
        for pattern, secret_type in SECRET_PATTERNS:
            if re.search(pattern, line, re.IGNORECASE):
                issues.append((f'Potential {secret_type} detected', line_num))
                break

    return issues


def check_private_captures(path: str) -> List[str]:
    """
    Check if private audio/capture files exist outside approved paths or without provenance.

    Returns list of issue descriptions.
    """
    issues = []

    for pattern in PRIVATE_CAPTURE_PATTERNS:
        if re.search(pattern, path, re.IGNORECASE):
            # Audio files outside fixtures are not allowed
            if not is_synthetic_fixture_path(path):
                issues.append(f'Audio file outside documented fixtures: {path}')
            else:
                # Inside fixtures, must have provenance
                if not has_provenance_file(path):
                    issues.append(f'Audio file in fixtures must have provenance documentation: {path}')
            break

    for pattern in RECORDING_FILE_PATTERNS:
        if re.search(pattern, path, re.IGNORECASE):
            if not is_synthetic_fixture_path(path):
                if any(re.search(ext, path) for ext in PRIVATE_CAPTURE_PATTERNS):
                    issues.append(f'Recording outside fixtures requires documentation: {path}')

    return issues


def check_signing_material(path: str) -> List[str]:
    """Check if signing material or private certificates are committed."""
    issues = []

    for pattern in SIGNING_MATERIAL_PATTERNS:
        if re.search(pattern, path, re.IGNORECASE):
            issues.append(f'Signing material or private certificate must not be committed: {path}')
            break

    return issues


def run_gitleaks_check() -> Tuple[List[Tuple[str, str, None]], bool]:
    """
    Run gitleaks to detect secrets in the repository.

    Returns (issues list, has_gitleaks) - issues as (path, description, None) tuples.
    Exit code 1 indicates secrets found, 0 indicates none found.
    """
    try:
        # Create a temporary file for the JSON report
        import tempfile
        report_fd, report_path = tempfile.mkstemp(suffix='.json')
        os.close(report_fd)

        try:
            result = subprocess.run(
                ['gitleaks', 'detect', '--redact', '--report-path', report_path],
                capture_output=True,
                text=True,
                cwd=os.getcwd(),
                timeout=60
            )

            issues = []
            # Parse the JSON report if it exists
            if os.path.exists(report_path) and os.path.getsize(report_path) > 0:
                try:
                    with open(report_path, 'r') as f:
                        report = json.load(f)
                    # gitleaks returns a list directly
                    matches = report if isinstance(report, list) else report.get('Matches', [])
                    for match in matches:
                        path = match.get('File', 'unknown')
                        secret_type = match.get('RuleID', 'Secret')
                        issues.append((path, f'Secret detected by gitleaks: {secret_type}', None))
                except (json.JSONDecodeError, IOError, KeyError, TypeError) as e:
                    raise RuntimeError(f'Failed to parse gitleaks report: {str(e)}')

            # gitleaks returns 1 if findings, 0 if none, non-zero other on error
            # If exit code is 1 but we have issues, that's expected
            # If exit code is 1 but we have no issues, the report was invalid
            if result.returncode == 1 and not issues:
                raise RuntimeError('gitleaks found secrets but report is empty or invalid')
            elif result.returncode not in (0, 1):
                raise RuntimeError(f'gitleaks exited with code {result.returncode}: {result.stderr}')

            return issues, True
        finally:
            if os.path.exists(report_path):
                os.unlink(report_path)
    except FileNotFoundError:
        # gitleaks not installed - fail closed
        raise RuntimeError('gitleaks is not installed or not in PATH. Install gitleaks and add to PATH.')
    except subprocess.TimeoutExpired:
        raise RuntimeError('gitleaks scan timed out after 60 seconds.')


def check_path(path: str) -> List[Tuple[str, int | None]]:
    """
    Check a single file path for hygiene issues.

    Returns list of (issue_description, line_number or None) tuples.
    """
    issues = []

    if is_skipped_path(path):
        return issues

    # Skip non-files
    if not os.path.isfile(path):
        return issues

    # Check signing material first
    signing_issues = check_signing_material(path)
    for issue in signing_issues:
        issues.append((issue, None))
        if signing_issues:
            return issues

    # Check file-path based issues (before reading content)
    capture_issues = check_private_captures(path)
    for issue in capture_issues:
        issues.append((issue, None))

    # Skip binary and large files for content checking
    try:
        file_size = os.path.getsize(path)
        if file_size > 10 * 1024 * 1024:  # 10MB
            return issues
    except OSError:
        return issues

    # Try to read and check content
    try:
        with open(path, 'r', encoding='utf-8', errors='replace') as f:
            content = f.read()
    except (OSError, IOError):
        return issues

    # Skip secret checking for test fixtures and test files
    # These contain deliberately seeded test secrets for unit test verification
    if path.startswith('tools/hygiene/fixtures/') or re.search(r'test_.*\.py$', path):
        return issues

    # Check for secrets in content
    secret_issues = check_for_secrets(content, path)
    issues.extend(secret_issues)

    return issues


def find_pr_diff_files() -> Set[str]:
    """Find files in the PR diff (compared to origin/main)."""
    try:
        result = subprocess.run(
            ['git', 'diff', 'origin/main...HEAD', '--name-only'],
            capture_output=True,
            text=True,
            cwd=os.getcwd()
        )
        files = set(result.stdout.strip().split('\n')) if result.stdout.strip() else set()
        return files - {''}
    except Exception:
        return set()


def find_tracked_files() -> Set[str]:
    """Find all tracked files in the repository."""
    try:
        result = subprocess.run(
            ['git', 'ls-files'],
            capture_output=True,
            text=True,
            cwd=os.getcwd()
        )
        files = set(result.stdout.strip().split('\n')) if result.stdout.strip() else set()
        return files - {''}
    except Exception:
        return set()


def find_uncommitted_files() -> Set[str]:
    """Find files that are not in git index (untracked or uncommitted)."""
    try:
        # Get untracked files
        result = subprocess.run(
            ['git', 'ls-files', '--others', '--exclude-standard'],
            capture_output=True,
            text=True,
            cwd=os.getcwd()
        )
        untracked = set(result.stdout.strip().split('\n')) if result.stdout.strip() else set()

        # Get uncommitted changes (modified files)
        result = subprocess.run(
            ['git', 'diff', '--name-only'],
            capture_output=True,
            text=True,
            cwd=os.getcwd()
        )
        modified = set(result.stdout.strip().split('\n')) if result.stdout.strip() else set()

        return (untracked | modified) - {''}
    except Exception:
        return set()


def check_repository(paths: List[str] = None, scan_mode: str = 'auto', require_gitleaks: bool = True) -> Tuple[List[Tuple[str, str, int | None]], int]:
    """
    Check repository for hygiene issues.

    Args:
        paths: Specific paths to check. If None, auto-detects based on scan_mode.
        scan_mode: 'uncommitted' (untracked/modified), 'tracked' (all tracked), 'pr-diff' (PR changes), 'auto' (PR if origin/main exists, else uncommitted)
        require_gitleaks: If True, fail if gitleaks is not available. If False, continue with custom checks only.

    Returns:
        (issues: List of (file_path, issue_description, line_number), exit_code)
    """
    if paths is None:
        if scan_mode == 'auto':
            # In CI, scan PR diff if origin/main exists, else scan tracked files
            try:
                subprocess.run(['git', 'rev-parse', 'origin/main'], capture_output=True, check=True, cwd=os.getcwd())
                paths = list(find_pr_diff_files())
                if not paths:
                    # No diff from main, scan all tracked files
                    paths = list(find_tracked_files())
            except subprocess.CalledProcessError:
                # origin/main doesn't exist, scan uncommitted files
                paths = list(find_uncommitted_files())
        elif scan_mode == 'uncommitted':
            paths = list(find_uncommitted_files())
        elif scan_mode == 'tracked':
            paths = list(find_tracked_files())
        elif scan_mode == 'pr-diff':
            paths = list(find_pr_diff_files())

    issues = []

    # Run gitleaks first - required in CI to ensure maintained scanner runs
    try:
        gitleaks_issues, _ = run_gitleaks_check()
        issues.extend(gitleaks_issues)
    except RuntimeError as e:
        if require_gitleaks:
            # In CI, we must have gitleaks installed
            print(f'CRITICAL: {str(e)}', file=sys.stderr)
            return [(str(e), '', None)], 1
        # In local testing, optional

    # Then run custom checks on specified paths
    for path in paths:
        file_issues = check_path(path)
        for issue_desc, line_num in file_issues:
            issues.append((path, issue_desc, line_num))

    return issues, 0 if not issues else 1


def format_report(issues: List[Tuple[str, str, int | None]]) -> str:
    """Format issues for human-readable output."""
    if not issues:
        return 'No hygiene issues detected.'

    report = ['Hygiene check failed with issues:']
    report.append('')

    by_file = {}
    for path, issue, line_num in issues:
        if path not in by_file:
            by_file[path] = []
        by_file[path].append((issue, line_num))

    for path in sorted(by_file.keys()):
        report.append(f'  {path}')
        for issue, line_num in by_file[path]:
            if line_num is not None:
                report.append(f'    Line {line_num}: {issue}')
            else:
                report.append(f'    {issue}')

    return '\n'.join(report)


if __name__ == '__main__':
    import argparse

    parser = argparse.ArgumentParser(description='Check repository fixture hygiene and secret policies')
    parser.add_argument('paths', nargs='*', help='Specific paths to check (if none, auto-detects)')
    parser.add_argument('--scan-mode', choices=['auto', 'uncommitted', 'tracked', 'pr-diff'], default='auto',
                        help='Which files to scan: auto (default), uncommitted (untracked/modified), tracked (all), pr-diff (PR changes)')

    args = parser.parse_args()

    # In tracked/auto/pr-diff modes, require gitleaks scanner
    require_gitleaks = args.scan_mode in ['auto', 'tracked', 'pr-diff']

    paths = args.paths if args.paths else None
    issues, exit_code = check_repository(paths, scan_mode=args.scan_mode, require_gitleaks=require_gitleaks)
    print(format_report(issues), file=sys.stderr if exit_code else sys.stdout)
    sys.exit(exit_code)
