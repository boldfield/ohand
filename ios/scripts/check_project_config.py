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

# Control extension target -> host app target, and the owned directory each extension compiles.
CONTROL_EXTENSIONS = {"CaptureProbe": "CaptureProbeControl", "OhAndApp": "OhAndCaptureControl"}
CONTROL_ROOTS = {"CaptureProbeControl": "CaptureProbe/Control", "OhAndCaptureControl": "Capture/Entry/Control"}
# Directory compiled into BOTH the host app and its control extension; it holds the app-opening OpenIntent.
SHARED_INTENT_ROOTS = {"CaptureProbeControl": "CaptureProbe/Shared", "OhAndCaptureControl": "Capture/Entry/Shared"}

PROBE_USAGE_STRINGS = {
    "AudioProbe": ["NSMicrophoneUsageDescription"],
    "TranscriptionProbe": ["NSSpeechRecognitionUsageDescription"],
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
        if target["type"] not in ("framework", "app-extension") and target_name not in project.get("schemes", {}):
            errors.append(f"{target_name} has no scheme")

    bundle_ids = [t.get("settings", {}).get("PRODUCT_BUNDLE_IDENTIFIER") for t in targets.values()]
    for bundle_id in bundle_ids:
        if not bundle_id or not bundle_id.startswith(BUNDLE_PREFIX + "."):
            errors.append(f"bundle identifier {bundle_id!r} is not under {BUNDLE_PREFIX}")
    if len(set(bundle_ids)) != len(bundle_ids):
        errors.append("bundle identifiers are not unique")

    if "OhAndControl" in targets:
        errors.append("OhAndControl must not be a second application; controls are app extensions")
    errors += check_control_extensions(project, root)

    flattened = yaml.safe_dump(project)
    for forbidden in ("CODE_SIGN_IDENTITY", "DEVELOPMENT_TEAM", "PROVISIONING_PROFILE"):
        if forbidden in flattened:
            errors.append(f"project.yml must not set {forbidden}; signing is supplied at build time")
    if project.get("configFiles") != {"Debug": "Config/Base.xcconfig", "Release": "Config/Base.xcconfig"}:
        errors.append("project configFiles must point at Config/Base.xcconfig")
    return errors


def check_control_extensions(project, root=IOS_ROOT):
    errors = []
    targets = project["targets"]
    for host_name, extension_name in CONTROL_EXTENSIONS.items():
        host = targets.get(host_name, {})
        extension = targets.get(extension_name, {})
        if extension.get("type") != "app-extension":
            errors.append(f"{extension_name} must be an app-extension target")
            continue
        host_id = host.get("settings", {}).get("PRODUCT_BUNDLE_IDENTIFIER", "")
        extension_id = extension.get("settings", {}).get("PRODUCT_BUNDLE_IDENTIFIER", "")
        if not extension_id.startswith(host_id + "."):
            errors.append(f"{extension_name} bundle identifier must be prefixed by {host_name}'s")
        if extension_name not in [d.get("target") for d in host.get("dependencies", [])]:
            errors.append(f"{host_name} must depend on (embed) {extension_name}")
        control_root = CONTROL_ROOTS[extension_name]
        if control_root not in source_paths(extension):
            errors.append(f"{extension_name} must include {control_root}/")
        owner_root, _, relative = control_root.partition("/")
        host_entries = [e for e in host.get("sources", []) if isinstance(e, dict) and e["path"] == owner_root]
        excluded = [x for e in host_entries for x in e.get("excludes", [])]
        if f"{relative}/**" not in excluded:
            errors.append(f"{host_name} must exclude {control_root}/ so extension code is not compiled into the host")
        errors += check_shared_intent(host_name, host, extension_name, extension, root)
        plist_path = root / control_root / "Info.plist"
        with plist_path.open("rb") as handle:
            plist = plistlib.load(handle)
        if plist.get("NSExtension", {}).get("NSExtensionPointIdentifier") != "com.apple.widgetkit-extension":
            errors.append(f"{control_root}/Info.plist must declare the WidgetKit extension point")
        if not list((root / control_root).glob("*.swift")):
            errors.append(f"{control_root}/ has no Swift source")
    return errors


def check_shared_intent(host_name, host, extension_name, extension, root=IOS_ROOT):
    errors = []
    shared_root = SHARED_INTENT_ROOTS[extension_name]
    control_root = CONTROL_ROOTS[extension_name]
    owner_root, _, relative = shared_root.partition("/")
    if shared_root not in source_paths(extension):
        errors.append(f"{extension_name} must include {shared_root}/ (shared app-opening intent)")
    host_entries = [e for e in host.get("sources", []) if isinstance(e, dict) and e["path"] == owner_root]
    if not host_entries:
        errors.append(f"{host_name} must compile {owner_root}/, which contains {shared_root}/")
    for entry in host_entries:
        for pattern in entry.get("excludes", []):
            excluded_prefix = pattern.rstrip("*").rstrip("/")
            if excluded_prefix and (relative == excluded_prefix or relative.startswith(excluded_prefix + "/")):
                errors.append(f"{host_name} must not exclude {shared_root}/")
    intent_names = []
    for intent_path in sorted((root / shared_root).glob("*.swift")):
        intent_text = intent_path.read_text()
        if "openAppWhenRun" in intent_text:
            errors.append(f"{intent_path.relative_to(root)} uses legacy openAppWhenRun; control handoff must use OpenIntent")
        for intent_name in re.findall(r"struct\s+(\w+)\s*:[^{]*\bOpenIntent\b", intent_text):
            if not re.search(r"@Parameter\b[^\n]*\n\s*var\s+target\s*:", intent_text):
                errors.append(f"{intent_path.relative_to(root)}: OpenIntent {intent_name} must declare an @Parameter target")
            intent_names.append(intent_name)
    if not intent_names:
        errors.append(f"{shared_root}/ must contain an OpenIntent that launches {host_name}")
    control_text = "\n".join(path.read_text() for path in (root / control_root).glob("*.swift"))
    for intent_name in intent_names:
        if re.search(rf"struct\s+{intent_name}\b", control_text):
            errors.append(f"{control_root}/ must not redefine shared intent {intent_name}")
    if intent_names and not any(re.search(rf"ControlWidgetButton\(action:\s*{name}\(", control_text) for name in intent_names):
        errors.append(f"{control_root}/ control button must run a shared OpenIntent from {shared_root}/")
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
