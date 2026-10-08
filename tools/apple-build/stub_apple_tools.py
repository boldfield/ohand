#!/usr/bin/env python3
"""Synthetic stand-ins for security, xcodebuild, xcrun, devicectl and mint, used only by the behaviour tests.

Invoked as `stub_apple_tools.py <tool> <arguments...>` from tiny wrapper scripts placed on PATH.
Every call is appended to $STUB_STATE_DIR/calls.jsonl. Behaviour is selected through environment variables:
  STUB_FAIL=<tool>:<first argument>[,...]   exit 1 for that call
  STUB_HANG=<tool>:<first argument>         write $STUB_STATE_DIR/hanging, then sleep
  STUB_ECHO_ARGS=1                          print the full argument vector to stderr (simulates a leaky tool)
  STUB_IDENTITIES / STUB_LOGIN_IDENTITIES   comma separated identity names in an imported / the user keychain
  STUB_P12_PASSWORD                         password the synthetic .p12 expects
  STUB_UNSIGNED_EXPORT=1                    export an .ipa without _CodeSignature
  STUB_LEAK_TEXT=<text>                     print this text to stderr on every call (simulates a tool echoing identifiers)
  STUB_EMPTY_KEYCHAIN_LIST=1                `security list-keychains -d user` prints nothing parseable
  STUB_FAIL_RESTORE_SEARCH_LIST=1           fail only the call that restores the original keychain search list
"""

import json
import os
import plistlib
import sys
import time
import zipfile
from pathlib import Path

STATE_DIR = Path(os.environ["STUB_STATE_DIR"])
DEFAULT_LOGIN_KEYCHAIN = "/synthetic/Library/Keychains/login.keychain-db"
SYNTHETIC_IDENTITY_NAME = "Apple Development: Synthetic Person (SYNTHETIC1)"


def record_call(tool, arguments):
    entry = {"tool": tool, "argv": arguments}
    if tool == "xcodebuild" and "-exportArchive" in arguments:
        home = Path(os.environ["HOME"])
        entry["installed_profiles_at_call"] = sorted(
            str(path.relative_to(home)) for path in (home / "Library").rglob("*.mobileprovision"))
    with open(STATE_DIR / "calls.jsonl", "a") as handle:
        handle.write(json.dumps(entry) + "\n")


def configured(variable, tool, arguments):
    first = arguments[0] if arguments else ""
    return f"{tool}:{first}" in os.environ.get(variable, "").split(",")


def identities_text(names):
    return "".join(f'  {index}) {index:040X} "{name}"\n' for index, name in enumerate(names, start=1))


def split_names(value):
    return [name for name in value.split(",") if name]


def keychain_search_list():
    path = STATE_DIR / "search-list.json"
    if path.exists():
        return json.loads(path.read_text())
    return [DEFAULT_LOGIN_KEYCHAIN]


def option_value(arguments, option):
    return arguments[arguments.index(option) + 1]


def security(arguments):
    command = arguments[0]
    if command == "cms":
        sys.stdout.write(Path(option_value(arguments, "-i")).read_text())
    elif command == "list-keychains":
        if "-s" in arguments:
            restored_list = arguments[arguments.index("-s") + 1:]
            if os.environ.get("STUB_FAIL_RESTORE_SEARCH_LIST") == "1" and not any("ohand-signing-" in path for path in restored_list):
                print("synthetic restore failure", file=sys.stderr)
                sys.exit(1)
            (STATE_DIR / "search-list.json").write_text(json.dumps(arguments[arguments.index("-s") + 1:]))
        elif os.environ.get("STUB_EMPTY_KEYCHAIN_LIST") != "1":
            for path in keychain_search_list():
                print(f'    "{path}"')
    elif command == "create-keychain":
        keychain = Path(arguments[-1])
        keychain.write_text("synthetic keychain")
        (STATE_DIR / "keychain-password").write_text(option_value(arguments, "-p"))
    elif command == "unlock-keychain":
        expected = (STATE_DIR / "keychain-password").read_text() if (STATE_DIR / "keychain-password").exists() else None
        if not Path(arguments[-1]).exists() or option_value(arguments, "-p") != expected:
            sys.exit(1)
    elif command == "import":
        keychain = Path(option_value(arguments, "-k"))
        if not keychain.exists() or option_value(arguments, "-P") != os.environ.get("STUB_P12_PASSWORD", "synthetic-p12-password"):
            sys.exit(1)
        names = split_names(os.environ.get("STUB_IDENTITIES", SYNTHETIC_IDENTITY_NAME))
        Path(str(keychain) + ".identities").write_text("\n".join(names))
    elif command == "find-identity":
        keychain = arguments[4] if len(arguments) > 4 else None
        if keychain:
            identity_file = Path(keychain + ".identities")
            names = identity_file.read_text().splitlines() if identity_file.exists() else []
        else:
            names = split_names(os.environ.get("STUB_LOGIN_IDENTITIES", ""))
        sys.stdout.write(identities_text(names))
    elif command == "delete-keychain":
        for suffix in ("", ".identities"):
            Path(arguments[-1] + suffix).unlink(missing_ok=True)


def xcodebuild(arguments):
    if arguments == ["-version"]:
        print("Xcode 26.6\nBuild version 17A000")
    elif arguments[0] == "archive":
        archive = Path(option_value(arguments, "-archivePath"))
        (archive / "Products" / "Applications").mkdir(parents=True)
        (archive / "Info.plist").write_bytes(plistlib.dumps({"Name": "Synthetic"}))
    elif arguments[0] == "-exportArchive":
        options = plistlib.loads(Path(option_value(arguments, "-exportOptionsPlist")).read_bytes())
        (STATE_DIR / "export-options.json").write_text(json.dumps(options))
        export_path = Path(option_value(arguments, "-exportPath"))
        export_path.mkdir(parents=True)
        with zipfile.ZipFile(export_path / "Probe.ipa", "w") as archive:
            archive.writestr("Payload/Probe.app/Info.plist", "synthetic")
            if os.environ.get("STUB_UNSIGNED_EXPORT") != "1":
                archive.writestr("Payload/Probe.app/_CodeSignature/CodeResources", "synthetic")


def devicectl(arguments):
    print("Synthetic install complete")


def main():
    tool, arguments = sys.argv[1], sys.argv[2:]
    if tool == "xcrun":
        tool, arguments = arguments[0], arguments[1:]
    record_call(tool, arguments)
    if os.environ.get("STUB_ECHO_ARGS") == "1":
        print("stub saw: " + " ".join(arguments), file=sys.stderr)
    if os.environ.get("STUB_LEAK_TEXT"):
        print(os.environ["STUB_LEAK_TEXT"], file=sys.stderr)
    if configured("STUB_HANG", tool, arguments):
        (STATE_DIR / "hanging").write_text("hanging")
        time.sleep(120)
    if configured("STUB_FAIL", tool, arguments):
        print(f"synthetic {tool} failure", file=sys.stderr)
        sys.exit(1)
    handlers = {"security": security, "xcodebuild": xcodebuild, "devicectl": devicectl, "mint": lambda a: None}
    handlers[tool](arguments)


if __name__ == "__main__":
    main()
