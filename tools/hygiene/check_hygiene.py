#!/usr/bin/env python3
"""
Fixture hygiene checker: enforce public-repository secret and privacy standards.

Prevents committed real secrets, private credentials, private captures, and
undocumented raw audio while allowing synthetic fixtures in documented paths.
"""

import re
import sys
import os
from pathlib import Path
from typing import List, Tuple, Set

# Patterns that indicate potential secrets or private content
SECRET_PATTERNS = [
    # AWS credentials
    (r'AKIA[0-9A-Z]{16}', 'AWS Access Key ID'),
    (r'aws_secret_access_key\s*=\s*[^\s]+', 'AWS Secret Access Key'),

    # API keys and tokens
    (r'api[_-]?key\s*=\s*[^\s]+', 'API Key'),
    (r'auth[_-]?token\s*=\s*[^\s]+', 'Auth Token'),
    (r'x-api-key\s*=\s*[^\s]+', 'X-API-Key'),
    (r'bearer\s+[a-zA-Z0-9._\-]+', 'Bearer Token'),

    # OAuth tokens
    (r'oauth[_-]?token\s*=\s*[^\s]+', 'OAuth Token'),
    (r'access[_-]?token\s*=\s*[^\s]+', 'Access Token'),
    (r'refresh[_-]?token\s*=\s*[^\s]+', 'Refresh Token'),

    # Passwords
    (r'password\s*=\s*[^\s]+', 'Password'),
    (r'passwd\s*=\s*[^\s]+', 'Password'),

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

    # Anthropic/OpenAI API keys (common patterns)
    (r'sk-[a-zA-Z0-9]{20,}', 'OpenAI-style API Key'),
    (r'sk-ant-[a-zA-Z0-9]{20,}', 'Anthropic API Key'),
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

# Files to skip checking entirely
SKIP_PATTERNS = {
    r'\.git/',
    r'\.github/',
    r'target/',
    r'Cargo.lock',
    r'\.DS_Store',
    r'node_modules/',
    r'\.venv/',
    r'\.env',
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


def check_for_secrets(content: str, path: str) -> List[Tuple[str, int]]:
    """
    Check content for patterns matching secrets or private keys.

    Returns list of (issue_description, line_number) tuples.
    """
    issues = []
    lines = content.split('\n')

    for line_num, line in enumerate(lines, 1):
        # Skip comments in source files
        if path.endswith(('.py', '.rs', '.ts', '.tsx', '.swift', '.java')):
            stripped = line.strip()
            if stripped.startswith('#') or stripped.startswith('//'):
                continue

        for pattern, secret_type in SECRET_PATTERNS:
            if re.search(pattern, line, re.IGNORECASE):
                issues.append((f'Potential {secret_type} detected', line_num))
                break  # Only report first match per line

    return issues


def check_private_captures(path: str) -> List[str]:
    """
    Check if private audio/capture files exist outside approved paths.

    Returns list of issue descriptions.
    """
    issues = []

    for pattern in PRIVATE_CAPTURE_PATTERNS:
        if re.search(pattern, path, re.IGNORECASE):
            # Audio files outside fixtures are not allowed
            if not is_synthetic_fixture_path(path):
                issues.append(f'Audio file outside documented fixtures: {path}')
            break

    for pattern in RECORDING_FILE_PATTERNS:
        if re.search(pattern, path, re.IGNORECASE):
            # Recording files outside fixtures should be documented
            if not is_synthetic_fixture_path(path):
                # Audio files require explicit fixture path
                if any(re.search(ext, path) for ext in PRIVATE_CAPTURE_PATTERNS):
                    issues.append(f'Recording outside fixtures requires documentation: {path}')

    return issues


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

    # Skip secret checking for hygiene checker implementation and tests
    # (they intentionally contain pattern definitions and test data)
    if path.startswith('tools/hygiene/'):
        return issues

    # Check for secrets in content
    secret_issues = check_for_secrets(content, path)
    issues.extend(secret_issues)

    return issues


def find_uncommitted_files() -> Set[str]:
    """Find files that are not in git index (untracked or uncommitted)."""
    try:
        import subprocess
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


def check_repository(paths: List[str] = None) -> Tuple[List[Tuple[str, str, int | None]], int]:
    """
    Check repository for hygiene issues.

    Args:
        paths: Specific paths to check. If None, checks all modified/untracked files.

    Returns:
        (issues: List of (file_path, issue_description, line_number), exit_code)
    """
    if paths is None:
        # Check uncommitted files
        paths = list(find_uncommitted_files())

    issues = []

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
    paths = sys.argv[1:] if len(sys.argv) > 1 else None
    issues, exit_code = check_repository(paths)
    print(format_report(issues), file=sys.stderr if exit_code else sys.stdout)
    sys.exit(exit_code)
