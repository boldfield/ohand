#!/usr/bin/env python3
"""Static checks for ios/project.yml that run without macOS or Xcode.

These checks do not prove the project builds. They catch structural drift
(owned paths missing from targets, signing material, invalid plists) on any
host; the native generate/build result is collected by macOS CI (F05).
"""
import plistlib
import re
import sys
from pathlib import Path

import yaml

IOS_ROOT = Path(__file__).resolve().parent.parent
BUNDLE_PREFIX = "com.boldfield.ohand"

# F01-owned native roots that must be reachable from a target without editing project.yml.
OWNED_SOURCE_ROOTS = {
    "OhAndCoreBridge": "OhAndCoreBridge",
    "Services": "OhAndServices",
    "Capture": "OhAndApp",
    "AppAssembly": "OhAndApp",
    "Tests": "OhAndTests",
    "BridgeProbe": "BridgeProbe",
    "NotificationProbe": "NotificationProbe",
    "AudioProbe": "AudioProbe",
    "TranscriptionProbe": "TranscriptionProbe",
    "CredentialProbe": "CredentialProbe",
    "CaptureProbe": "CaptureProbe",
}

PROBE_USAGE_STRINGS = {
    "AudioProbe": ["NSMicrophoneUsageDescription"],
    "TranscriptionProbe": ["NSMicrophoneUsageDescription", "NSSpeechRecognitionUsageDescription"],
}
APP_USAGE_STRINGS = ["NSMicrophoneUsageDescription", "NSSpeechRecognitionUsageDescription"]

FORBIDDEN_COMMITTED_SUFFIXES = (".mobileprovision", ".p12", ".p8", ".cer", ".certSigningRequest")


def load_project():
    return yaml.safe_load((IOS_ROOT / "project.yml").read_text())


def source_paths(target):
    return [entry["path"] if isinstance(entry, dict) else entry for entry in target.get("sources", [])]


def check_project(project, root=IOS_ROOT):
    errors = []
    targets = project["targets"]

    for owned_root, expected_target in OWNED_SOURCE_ROOTS.items():
        owners = [name for name, target in targets.items() if owned_root in source_paths(target)]
        if expected_target not in owners:
            errors.append(f"{owned_root}/ is not a source root of {expected_target}")
        if owned_root == "Services" and len(owners) != 1:
            errors.append(f"Services/ must be compiled into exactly one module, found {owners}")

    for target_name, target in targets.items():
        for entry in target.get("sources", []):
            if isinstance(entry, dict) and entry.get("optional") is not True and not (root / entry["path"]).exists():
                errors.append(f"{target_name} source path {entry['path']} is missing and not optional")

    test_target = targets.get("OhAndTests", {})
    if test_target.get("type") != "bundle.unit-test":
        errors.append("OhAndTests must be a bundle.unit-test target")
    scheme_tests = project.get("schemes", {}).get("OhAndTests", {}).get("test", {}).get("targets", [])
    if "OhAndTests" not in scheme_tests:
        errors.append("OhAndTests scheme has no test action")

    for target_name, target in targets.items():
        if target["type"] != "framework" and target_name not in project.get("schemes", {}):
            errors.append(f"{target_name} has no scheme")

    bundle_ids = [t.get("settings", {}).get("PRODUCT_BUNDLE_IDENTIFIER") for t in targets.values()]
    for bundle_id in bundle_ids:
        if not bundle_id or not bundle_id.startswith(BUNDLE_PREFIX + "."):
            errors.append(f"bundle identifier {bundle_id!r} is not under {BUNDLE_PREFIX}")
    if len(set(bundle_ids)) != len(bundle_ids):
        errors.append("bundle identifiers are not unique")

    control = targets.get("OhAndControl", {})
    conditions = control.get("settings", {}).get("SWIFT_ACTIVE_COMPILATION_CONDITIONS", "")
    if "$(inherited)" not in conditions or "OHAND_BUILD_TARGET_CONTROL" not in conditions or "=" in conditions:
        errors.append("OhAndControl must append a valueless OHAND_BUILD_TARGET_CONTROL Swift condition")

    flattened = yaml.safe_dump(project)
    for forbidden in ("CODE_SIGN_IDENTITY", "DEVELOPMENT_TEAM", "PROVISIONING_PROFILE"):
        if forbidden in flattened:
            errors.append(f"project.yml must not set {forbidden}; signing is supplied at build time")
    if project.get("configFiles") != {"Debug": "Config/Base.xcconfig", "Release": "Config/Base.xcconfig"}:
        errors.append("project configFiles must point at Config/Base.xcconfig")
    return errors


def check_xcconfig(root=IOS_ROOT):
    errors = []
    base = (root / "Config/Base.xcconfig").read_text()
    if not re.search(r"^CODE_SIGNING_ALLOWED\[sdk=iphonesimulator\*\]\s*=\s*NO\s*$", base, re.M):
        errors.append("Base.xcconfig must disable code signing for the simulator SDK")
    if re.search(r"^DEVELOPMENT_TEAM\s*=\s*\S", base, re.M):
        errors.append("Base.xcconfig must not set DEVELOPMENT_TEAM")
    if "#include? \"Local.xcconfig\"" not in base:
        errors.append("Base.xcconfig must optionally include Local.xcconfig")
    if "Config/Local.xcconfig" not in (root / ".gitignore").read_text():
        errors.append("Config/Local.xcconfig must be git-ignored")
    return errors


def check_plists(root=IOS_ROOT):
    errors = []
    plist_paths = sorted(root.glob("*/Info.plist"))
    if len(plist_paths) < 7:
        errors.append(f"expected app and six probe Info.plists, found {len(plist_paths)}")
    for plist_path in plist_paths:
        owner = plist_path.parent.name
        with plist_path.open("rb") as handle:
            plist = plistlib.load(handle)
        required = PROBE_USAGE_STRINGS.get(owner, APP_USAGE_STRINGS if owner == "AppAssembly" else [])
        for key in required:
            if not plist.get(key):
                errors.append(f"{owner}/Info.plist is missing {key}")
        manifest = plist.get("UIApplicationSceneManifest", {})
        configurations = manifest.get("UISceneConfigurations", {}).get("UIWindowSceneSessionRoleApplication", [])
        for configuration in configurations:
            delegate_name = configuration.get("UISceneDelegateClassName", "")
            if not delegate_name.startswith("$(PRODUCT_MODULE_NAME)."):
                errors.append(f"{owner}/Info.plist scene delegate {delegate_name!r} is not module-qualified")
        if not configurations:
            errors.append(f"{owner}/Info.plist has no scene configuration")
        if "armv7" in plist.get("UIRequiredDeviceCapabilities", []):
            errors.append(f"{owner}/Info.plist requires armv7")
        for key in ("UILaunchStoryboardName", "UIMainStoryboardFile", "NSUserNotificationUsageDescription"):
            if key in plist:
                errors.append(f"{owner}/Info.plist sets unsupported or dangling key {key}")
    return errors


def check_no_signing_material(root=IOS_ROOT):
    return [
        f"signing material committed: {path.relative_to(root)}"
        for path in root.rglob("*")
        if path.is_file() and path.suffix in FORBIDDEN_COMMITTED_SUFFIXES
    ]


def run_all_checks():
    return check_project(load_project()) + check_xcconfig() + check_plists() + check_no_signing_material()


if __name__ == "__main__":
    problems = run_all_checks()
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    if problems:
        sys.exit(1)
    print("ios project config static checks passed (no native build was performed)")
