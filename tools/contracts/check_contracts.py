#!/usr/bin/env python3
"""Validate the M1 architecture contract against the M1 task manifest.

Checks performed:
- every task key referenced by depends_on exists and the dependency graph is acyclic;
- every (file_scope path, task) pair in the manifest appears in the contract ownership map;
- the ownership map assigns no owner the manifest does not, except declared F01 bootstrap reservations;
- every path with more than one owner is named in the Shared File Ordering section, and each
  consecutive pair of editors is serialized by a dependency path in the manifest;
- the contract contains no fenced code blocks (F01 requires prose without implementation snippets).

Exits non-zero and prints each problem when any check fails. Standard library only.
"""

import json
import re
import sys
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CONTRACT_PATH = REPOSITORY_ROOT / "docs/architecture/m1-contracts.md"
DEFAULT_MANIFEST_PATH = REPOSITORY_ROOT / "docs/features/m1-tasks.json"

# Paths F01 owns in addition to its manifest file_scope: the contract checks themselves.
F01_BOOTSTRAP_RESERVATIONS = {"Makefile", "tools/contracts/"}

OWNERSHIP_ROW_PATTERN = re.compile(r"^\|\s*`(?P<path>[^`]+)`\s*\|[^|]*\|\s*(?P<owners>[^|]*?)\s*\|\s*$")
TASK_KEY_PATTERN = re.compile(r"^[A-Z][0-9]{2}$")
SHARED_ORDERING_HEADING = "## Shared File Ordering"


def parse_ownership_map(contract_text):
    """Return {path: set(task keys)} from ownership table rows; reference rows are skipped."""
    ownership_by_path = {}
    for line in contract_text.splitlines():
        row_match = OWNERSHIP_ROW_PATTERN.match(line)
        if not row_match:
            continue
        owner_tokens = [token.strip() for token in row_match.group("owners").split(",")]
        task_keys = {token for token in owner_tokens if TASK_KEY_PATTERN.match(token)}
        if not task_keys:
            continue
        ownership_by_path.setdefault(row_match.group("path"), set()).update(task_keys)
    return ownership_by_path


def manifest_ownership(tasks):
    ownership_by_path = {}
    for task in tasks:
        for path in task["file_scope"]:
            ownership_by_path.setdefault(path, set()).add(task["key"])
    return ownership_by_path


def dependency_problems(tasks):
    problems = []
    dependencies_by_key = {task["key"]: list(task["depends_on"]) for task in tasks}
    for task_key, dependency_keys in dependencies_by_key.items():
        for dependency_key in dependency_keys:
            if dependency_key not in dependencies_by_key:
                problems.append(f"{task_key} depends on unknown task {dependency_key}")
    visit_state = {}

    def visit(task_key, stack):
        visit_state[task_key] = "active"
        for dependency_key in dependencies_by_key.get(task_key, []):
            if visit_state.get(dependency_key) == "active":
                cycle = stack[stack.index(dependency_key):] + [dependency_key]
                problems.append("dependency cycle: " + " -> ".join(cycle))
            elif dependency_key in dependencies_by_key and dependency_key not in visit_state:
                visit(dependency_key, stack + [dependency_key])
        visit_state[task_key] = "done"

    for task_key in dependencies_by_key:
        if task_key not in visit_state:
            visit(task_key, [task_key])
    return problems


def is_ancestor(ancestor_key, descendant_key, dependencies_by_key):
    pending = list(dependencies_by_key.get(descendant_key, []))
    seen = set()
    while pending:
        current_key = pending.pop()
        if current_key == ancestor_key:
            return True
        if current_key in seen:
            continue
        seen.add(current_key)
        pending.extend(dependencies_by_key.get(current_key, []))
    return False


def topological_order(task_keys, dependencies_by_key):
    """Order task keys so that ancestors come first; ties keep sorted order."""
    return sorted(task_keys, key=lambda key: (sum(is_ancestor(other, key, dependencies_by_key) for other in task_keys), key))


def ownership_problems(contract_ownership, expected_ownership):
    problems = []
    for path, expected_keys in sorted(expected_ownership.items()):
        mapped_keys = contract_ownership.get(path, set())
        for missing_key in sorted(expected_keys - mapped_keys):
            problems.append(f"ownership map is missing {missing_key} for {path}")
    for path, mapped_keys in sorted(contract_ownership.items()):
        expected_keys = expected_ownership.get(path, set())
        for extra_key in sorted(mapped_keys - expected_keys):
            if extra_key == "F01" and path in F01_BOOTSTRAP_RESERVATIONS:
                continue
            problems.append(f"ownership map assigns {path} to {extra_key}, which the manifest does not")
    return problems


def shared_ordering_problems(contract_text, contract_ownership, dependencies_by_key):
    problems = []
    ordering_section = contract_text.split(SHARED_ORDERING_HEADING, 1)
    if len(ordering_section) < 2:
        return [f"contract has no '{SHARED_ORDERING_HEADING}' section"]
    ordering_text = ordering_section[1].split("\n## ", 1)[0]
    for path, owner_keys in sorted(contract_ownership.items()):
        if len(owner_keys) < 2:
            continue
        if path not in ordering_text:
            problems.append(f"shared path {path} has owners {sorted(owner_keys)} but no Shared File Ordering entry")
        ordered_keys = topological_order(owner_keys, dependencies_by_key)
        for earlier_key, later_key in zip(ordered_keys, ordered_keys[1:]):
            if not is_ancestor(earlier_key, later_key, dependencies_by_key):
                problems.append(f"shared path {path}: editors {earlier_key} and {later_key} are not serialized by a dependency path")
    return problems


def code_block_problems(contract_text):
    return [
        f"contract line {line_number} opens a fenced code block; F01 must stay prose"
        for line_number, line in enumerate(contract_text.splitlines(), start=1)
        if line.lstrip().startswith(("```", "~~~"))
    ]


def validate(contract_text, manifest):
    tasks = manifest["tasks"]
    dependencies_by_key = {task["key"]: list(task["depends_on"]) for task in tasks}
    contract_ownership = parse_ownership_map(contract_text)
    problems = []
    problems += dependency_problems(tasks)
    problems += ownership_problems(contract_ownership, manifest_ownership(tasks))
    problems += shared_ordering_problems(contract_text, contract_ownership, dependencies_by_key)
    problems += code_block_problems(contract_text)
    return problems


def main(argument_values):
    contract_path = Path(argument_values[1]) if len(argument_values) > 1 else DEFAULT_CONTRACT_PATH
    manifest_path = Path(argument_values[2]) if len(argument_values) > 2 else DEFAULT_MANIFEST_PATH
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    problems = validate(contract_path.read_text(encoding="utf-8"), manifest)
    for problem in problems:
        print(f"contract check: {problem}", file=sys.stderr)
    if problems:
        return 1
    path_count = len(parse_ownership_map(contract_path.read_text(encoding="utf-8")))
    print(f"contract check: {len(manifest['tasks'])} tasks and {path_count} mapped paths reconcile")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
