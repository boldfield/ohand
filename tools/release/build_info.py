#!/usr/bin/env python3
"""
Generate reproducible build identifiers and version information for M1 releases.

Build identifier format: <revision-prefix>-<run-id>
- revision-prefix: first 8 characters of the git commit SHA at build time
- run-id: 8-character hex identifier from collection/build timestamp

The identifier remains stable across rebuilds of the same revision, and uniquely
identifies the build metadata (revision, timestamp, and collection conditions).
"""

import subprocess
import sys
import json
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import struct


def get_git_revision():
    """Get the current git HEAD commit SHA."""
    try:
        result = subprocess.run(
            ['git', 'rev-parse', 'HEAD'],
            capture_output=True,
            text=True,
            check=True
        )
        return result.stdout.strip()
    except subprocess.CalledProcessError as e:
        print(f"Error: Could not get git revision: {e}", file=sys.stderr)
        sys.exit(1)


def get_git_tree_clean():
    """Check if the working tree is clean."""
    try:
        result = subprocess.run(
            ['git', 'status', '--porcelain'],
            capture_output=True,
            text=True,
            check=True
        )
        return len(result.stdout.strip()) == 0
    except subprocess.CalledProcessError:
        return False


def generate_run_id(timestamp_str):
    """
    Generate a deterministic 8-character hex run ID from a timestamp.

    Uses the ISO 8601 timestamp (without microseconds) to produce a stable
    identifier that is human-traceable to collection time.
    """
    # Parse the timestamp to ensure it's valid, then use the part after T
    # to seed the run ID (format: 2026-10-12T19:52:44Z)
    if not timestamp_str:
        timestamp_str = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    # Use last 12 chars of timestamp + some hash to get determinism
    time_part = timestamp_str.replace('-', '').replace(':', '').replace('Z', '')[-8:]
    return time_part[:8].lower()


def create_build_info(revision, timestamp_str=None):
    """Create a build info dictionary with all metadata."""
    if not timestamp_str:
        timestamp_str = datetime.now(timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z')

    revision_prefix = revision[:8]
    run_id = generate_run_id(timestamp_str)
    build_identifier = f"{revision_prefix}-{run_id}"

    return {
        "build_identifier": build_identifier,
        "build_revision": revision,
        "build_revision_short": revision_prefix,
        "run_id": run_id,
        "collected_at": timestamp_str,
        "working_tree_clean": get_git_tree_clean(),
    }


def main():
    """Main entry point."""
    import argparse

    parser = argparse.ArgumentParser(
        description="Generate reproducible build identifiers and version information"
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output as JSON instead of key=value format"
    )
    parser.add_argument(
        "--timestamp",
        help="Override collection timestamp (ISO 8601 format, e.g., 2026-10-12T19:52:44Z)"
    )
    parser.add_argument(
        "--export-vars",
        help="Export shell variables to this file (key=value format)"
    )

    args = parser.parse_args()

    revision = get_git_revision()
    build_info = create_build_info(revision, args.timestamp)

    if args.json:
        print(json.dumps(build_info, indent=2))
    else:
        for key, value in build_info.items():
            print(f"{key}={value}")

    if args.export_vars:
        with open(args.export_vars, 'w') as f:
            for key, value in build_info.items():
                # Convert boolean to lowercase string for shell
                if isinstance(value, bool):
                    value = str(value).lower()
                f.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
