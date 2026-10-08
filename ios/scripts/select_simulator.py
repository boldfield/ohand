#!/usr/bin/env python3
"""Select one available iPhone simulator from `xcrun simctl list -j devices available` JSON.

Reads the JSON from stdin (or --input FILE) and prints `udid=`, `name=`, `runtime=` and
`runtime_key=` lines suitable for appending to $GITHUB_OUTPUT. With --max-runtime, runtimes newer
than that iOS version (the SDK reported by the selected Xcode, never a literal) are ignored.
"""
import argparse
import json
import re
import sys

RUNTIME_PREFIX = "com.apple.CoreSimulator.SimRuntime.iOS-"


class SelectionError(Exception):
    pass


def parse_version(text):
    parts = re.findall(r"\d+", text)
    if not parts:
        raise ValueError(f"no numeric version in {text!r}")
    return tuple(int(part) for part in parts)


def select_simulator(simctl_json, max_runtime=None):
    devices_by_runtime = simctl_json.get("devices", {})
    ceiling = parse_version(max_runtime) if max_runtime else None

    candidates = []
    for runtime_key, devices in devices_by_runtime.items():
        if not runtime_key.startswith(RUNTIME_PREFIX):
            continue
        runtime_version = runtime_key[len(RUNTIME_PREFIX):].replace("-", ".")
        try:
            version_tuple = parse_version(runtime_version)
        except ValueError:
            continue
        if ceiling is not None and version_tuple > ceiling:
            continue
        for device in devices:
            if not device.get("isAvailable", False):
                continue
            if not device.get("name", "").startswith("iPhone"):
                continue
            if not device.get("udid"):
                continue
            candidates.append((version_tuple, runtime_version, runtime_key, device))

    if not candidates:
        suffix = f" at or below iOS {max_runtime}" if max_runtime else ""
        raise SelectionError(f"No available iPhone simulator found{suffix}")

    newest_version = max(candidate[0] for candidate in candidates)
    newest = [candidate for candidate in candidates if candidate[0] == newest_version]
    _, runtime_version, runtime_key, device = min(
        newest, key=lambda candidate: (candidate[3]["name"], candidate[3]["udid"])
    )
    return {
        "udid": device["udid"],
        "name": device["name"],
        "runtime": runtime_version,
        "runtime_key": runtime_key,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", help="simctl JSON file (default: stdin)")
    parser.add_argument("--max-runtime", help="highest iOS runtime version to consider, e.g. 26.6")
    args = parser.parse_args(argv)

    if args.input:
        with open(args.input, encoding="utf-8") as handle:
            simctl_json = json.load(handle)
    else:
        simctl_json = json.load(sys.stdin)

    try:
        selected = select_simulator(simctl_json, args.max_runtime)
    except SelectionError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    for key, value in selected.items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
