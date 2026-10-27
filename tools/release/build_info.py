#!/usr/bin/env python3
"""Derive the identity of an Oh And build from its git checkout.

Every app build carries four values in its Info.plist: the marketing version, a build number, the full git revision
and a dirty flag.

* The marketing version is `CFBundleShortVersionString` of `ios/AppAssembly/Info.plist`.
* The build number is the first-parent commit count of HEAD. It is a pure function of the revision, and it grows by
  at least one with every commit on `main`, so a later build always installs over an earlier one.
* The revision is the full 40-digit SHA of HEAD.
* The dirty flag is true when `git status --porcelain` lists anything (tracked changes or untracked files).

A checkout that cannot name its revision fails. No placeholder is ever written. A candidate (`--require-clean`) also
refuses a dirty tree and a shallow clone, whose commit count would be wrong.

Usage: build_info.py [--repo PATH] [--require-clean] [--format json|plist-keys]
Exit status: 0 on success, 1 when the identity cannot be derived or the candidate rules refuse it.
"""

import argparse
import json
import plistlib
import re
import subprocess
import sys
from pathlib import Path
from typing import Dict, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
INFO_PLIST = Path("ios") / "AppAssembly" / "Info.plist"
REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_PATTERN = re.compile(r"^[0-9A-Za-z.\-]{1,32}$")

REVISION_KEY = "OhAndBuildRevision"
DIRTY_KEY = "OhAndBuildDirty"


class BuildInfoError(Exception):
    """The build identity cannot be derived, or the candidate rules refuse it."""


def run_git(repo: Path, *arguments: str) -> str:
    try:
        completed = subprocess.run(
            ["git", "-C", str(repo), *arguments], capture_output=True, text=True, check=False)
    except OSError as error:
        raise BuildInfoError(f"git cannot be run: {error.strerror or error}") from None
    if completed.returncode != 0:
        raise BuildInfoError(f"git {arguments[0]} failed: {completed.stderr.strip() or 'no output'}")
    return completed.stdout


def read_marketing_version(repo: Path) -> str:
    plist_path = repo / INFO_PLIST
    try:
        version = plistlib.loads(plist_path.read_bytes())["CFBundleShortVersionString"]
    except (OSError, ValueError, KeyError):
        raise BuildInfoError(f"{INFO_PLIST} does not give CFBundleShortVersionString") from None
    if not isinstance(version, str) or not VERSION_PATTERN.match(version):
        raise BuildInfoError(f"the marketing version in {INFO_PLIST} is not a plain version label")
    return version


def derive(repo: Path = REPO_ROOT, require_clean: bool = False) -> Dict[str, object]:
    """The build identity of `repo`'s HEAD. Raises BuildInfoError rather than guessing."""
    if run_git(repo, "rev-parse", "--is-inside-work-tree").strip() != "true":
        raise BuildInfoError(f"{repo} is not a git working tree")
    revision = run_git(repo, "rev-parse", "HEAD").strip()
    if not REVISION_PATTERN.match(revision):
        raise BuildInfoError("the git revision is not a full 40-digit SHA")
    count_text = run_git(repo, "rev-list", "--first-parent", "--count", "HEAD").strip()
    if not count_text.isdigit() or int(count_text) < 1:
        raise BuildInfoError("the first-parent commit count is not a positive number")
    shallow = run_git(repo, "rev-parse", "--is-shallow-repository").strip() == "true"
    dirty = run_git(repo, "status", "--porcelain").strip() != ""
    if require_clean and dirty:
        raise BuildInfoError("the working tree has uncommitted changes; a candidate build needs a clean tree")
    if require_clean and shallow:
        raise BuildInfoError("the checkout is shallow, so its commit count (the build number) is wrong; fetch full history")
    return {
        "version": read_marketing_version(repo),
        "build_number": int(count_text),
        "revision": revision,
        "dirty": dirty,
        "shallow": shallow,
    }


def plist_keys(info: Dict[str, object]) -> Dict[str, object]:
    """The Info.plist entries a build carries."""
    return {
        "CFBundleShortVersionString": info["version"],
        "CFBundleVersion": str(info["build_number"]),
        REVISION_KEY: info["revision"],
        DIRTY_KEY: bool(info["dirty"]),
    }


def main(argv: Optional[list] = None) -> int:
    parser = argparse.ArgumentParser(description="Derive the build identity from the git checkout.")
    parser.add_argument("--repo", default=str(REPO_ROOT), help="repository to read (default: this one)")
    parser.add_argument("--require-clean", action="store_true", help="refuse a dirty tree or a shallow clone (candidate builds)")
    parser.add_argument("--format", choices=("json", "plist-keys"), default="json")
    arguments = parser.parse_args(argv)
    try:
        info = derive(Path(arguments.repo), arguments.require_clean)
    except BuildInfoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    document = plist_keys(info) if arguments.format == "plist-keys" else info
    print(json.dumps(document, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
