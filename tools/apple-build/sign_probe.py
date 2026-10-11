#!/usr/bin/env python3
"""Build, sign and optionally install a probe on a physical iPhone (macOS with Xcode only).

Route: Apple Development identity plus a Development provisioning profile that lists the device.
The probe is archived unsigned, then `xcodebuild -exportArchive` signs it from an explicit export
options file, so each bundle identifier gets its own profile and frameworks are never given one.

Schemes that embed app extensions (OhAndApp) sign one profile per signed bundle: the bundle identifiers are read
from the unsigned archive, each is matched to a supplied profile that covers it, and the export options map every
signed bundle (application and extensions, never frameworks) to its profile UUID.

Signing inputs come only from the environment or injected files outside the repository:
  OHAND_SIGNING_PROFILE_PATH         required  .mobileprovision file (one profile may cover several bundles)
  OHAND_SIGNING_PROFILE_MAP_PATH     alternative to the above for OhAndApp: JSON file outside the repository mapping
                                               each bundle identifier to its own .mobileprovision file
  OHAND_SIGNING_CERT_PATH            optional  .p12 identity; imported into a per-run temporary keychain.
                                               Without it, exactly one Apple Development identity must
                                               already be in the user's keychains.
  OHAND_SIGNING_CERT_PASSWORD        .p12 password (or OHAND_SIGNING_CERT_PASSWORD_FILE)
  OHAND_DEVICE_ID                    devicectl device identifier; required with --install
  OHAND_DEVICE_UDID                  optional; hardware identifier to check against the profiles when devicectl
                                               cannot report it (OhAndApp with --install)
  OHAND_DEVICE_LABEL                 optional content-free label for evidence (a-z, 0-9, '-')
  OHAND_PRIVATE_EVIDENCE_DIR         optional; raw redacted logs, must be outside the repository

Stdout and stderr carry only fixed messages. Child tool output goes to a redacted log in the private
evidence directory. The sanitized evidence record contains fixed fields only.

Exit status: 0 success, 1 build/signing/install/launch failure, 2 missing or invalid input, 3 a bundle in an
embedding scheme has no usable covering profile (the message names only the bundle identifier).
"""

import argparse
import contextlib
import datetime
import json
import os
import plistlib
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Dict, List, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
IOS_ROOT = REPO_ROOT / "ios"
DEFAULT_EVIDENCE_DIR = REPO_ROOT / "docs" / "validation" / "evidence" / "apple-signing"

EXIT_OK = 0
EXIT_FAILURE = 1
EXIT_INPUT = 2
EXIT_BUNDLE_PROFILE = 3

EVIDENCE_SCHEMA = "ohand.apple-signing-evidence.v1"
EMBEDDED_EVIDENCE_SCHEMA = "ohand.apple-signing-evidence.v2"
ROUTE = "development"
IDENTITY_CLASS = "Apple Development"
EXPORT_METHOD = "debugging"
DEFAULT_SCHEME = "BridgeProbe"
# Application identifier of each supported scheme. The single-application probes keep one profile and the
# schema v1 record; schemes in EMBEDDING_SCHEMES embed extensions and sign one entry per bundle found in the archive.
SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS = {
    "BridgeProbe": "com.boldfield.ohand.probes.bridge",
    "AudioProbe": "com.boldfield.ohand.probes.audio",
    "TranscriptionProbe": "com.boldfield.ohand.probes.transcription",
    "NotificationProbe": "com.boldfield.ohand.probes.notification",
    "OhAndApp": "com.boldfield.ohand.app",
}
EMBEDDING_SCHEMES = frozenset({"OhAndApp"})
EXTENSION_DIRECTORY_NAMES = ("PlugIns", "Extensions")

PROFILE_PATH_VARIABLE = "OHAND_SIGNING_PROFILE_PATH"
PROFILE_MAP_VARIABLE = "OHAND_SIGNING_PROFILE_MAP_PATH"
CERT_PATH_VARIABLE = "OHAND_SIGNING_CERT_PATH"
CERT_PASSWORD_VARIABLE = "OHAND_SIGNING_CERT_PASSWORD"
CERT_PASSWORD_FILE_VARIABLE = "OHAND_SIGNING_CERT_PASSWORD_FILE"
DEVICE_ID_VARIABLE = "OHAND_DEVICE_ID"
DEVICE_LABEL_VARIABLE = "OHAND_DEVICE_LABEL"
DEVICE_UDID_VARIABLE = "OHAND_DEVICE_UDID"
PRIVATE_EVIDENCE_VARIABLE = "OHAND_PRIVATE_EVIDENCE_DIR"
INJECTED_VARIABLE_PREFIXES = ("OHAND_SIGNING_", "OHAND_DEVICE_")

DEVICE_LABEL_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{0,31}$")
XCODE_VERSION_PATTERN = re.compile(r"^Xcode \d+(\.\d+){0,2}$")
XCODE_BUILD_PATTERN = re.compile(r"^Build version [0-9A-Za-z]+$")
CODESIGNING_IDENTITY_PATTERN = re.compile(r'^\s*\d+\)\s+[0-9A-Fa-f]{40}\s+"([^"]*)"')
BUNDLE_IDENTIFIER_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$")
MAXIMUM_MAPPED_BUNDLES = 16
KEYCHAIN_PATH_PATTERN = re.compile(r'^\s*"(.*)"\s*$')
# The profile UUID names the installed file, so only the canonical 8-4-4-4-12 hex form is accepted.
PROFILE_UUID_PATTERN = re.compile(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}")
KEYCHAIN_TIMEOUT_SECONDS = 21600


class SigningError(Exception):
    """A failure whose message is fixed text and safe to print."""

    def __init__(self, message: str, exit_code: int = EXIT_FAILURE, stage: str = "preflight"):
        super().__init__(message)
        self.message = message
        self.exit_code = exit_code
        self.stage = stage


class Interrupted(BaseException):
    """Raised from a signal handler so every cleanup path runs."""


def input_error(message: str) -> SigningError:
    return SigningError(message, EXIT_INPUT, "preflight")


class Redactor:
    def __init__(self):
        self._labels: Dict[str, str] = {}
        self._evidence_forbidden: List[str] = []

    def register(self, value: Optional[str], label: str, forbid_in_evidence: bool = True) -> None:
        if value:
            self._labels[value] = label
            if forbid_in_evidence:
                self._evidence_forbidden.append(value)

    def apply(self, text: str) -> str:
        for value in sorted(self._labels, key=len, reverse=True):
            text = text.replace(value, f"<{self._labels[value]}>")
        return text

    def values_forbidden_in_evidence(self) -> List[str]:
        return list(self._evidence_forbidden)


class ProfileInfo:
    def __init__(self, name: str, uuid: str, team_identifier: str, expires_at, provisioned_devices: List[str],
                 application_identifier: str = "", xcode_managed: bool = False, provisions_all_devices: bool = False,
                 has_devices: bool = True, development: bool = True):
        self.name = name
        self.uuid = uuid
        self.team_identifier = team_identifier
        self.expires_at = expires_at
        self.provisioned_devices = provisioned_devices
        self.application_identifier = application_identifier
        self.xcode_managed = xcode_managed
        self.provisions_all_devices = provisions_all_devices
        self.has_devices = has_devices
        self.development = development
        self.source_path: Optional[Path] = None

    def covers(self, bundle_identifier: str) -> bool:
        return profile_covers_bundle(self.application_identifier, self.team_identifier, bundle_identifier)

    def expires_text(self) -> str:
        return self.expires_at.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


class EmbeddedBundle:
    def __init__(self, identifier: str, kind: str):
        self.identifier = identifier
        self.kind = kind


def profile_covers_bundle(application_identifier: str, team_identifier: str, bundle_identifier: str) -> bool:
    team_prefix = team_identifier + "."
    if not application_identifier.startswith(team_prefix):
        return False
    pattern = application_identifier[len(team_prefix):]
    if pattern == "*":
        return True
    if pattern.endswith(".*"):
        return bundle_identifier.startswith(pattern[:-1])
    return pattern == bundle_identifier


def read_profile_basics(profile: dict) -> ProfileInfo:
    required_fields = ("Name", "UUID", "TeamIdentifier", "ExpirationDate", "Entitlements")
    missing = [field for field in required_fields if not profile.get(field)]
    if missing:
        raise input_error("provisioning profile is missing required field(s): " + ", ".join(missing))
    if not isinstance(profile["UUID"], str) or not PROFILE_UUID_PATTERN.fullmatch(profile["UUID"]):
        raise input_error("provisioning profile UUID is not a canonical UUID")
    if not isinstance(profile["Name"], str):
        raise input_error("provisioning profile Name is not a string")
    team_identifiers = profile["TeamIdentifier"]
    if not isinstance(team_identifiers, list) or not team_identifiers or not isinstance(team_identifiers[0], str):
        raise input_error("provisioning profile has no usable TeamIdentifier")
    entitlements = profile["Entitlements"]
    provisioned = profile.get("ProvisionedDevices")
    devices = [device for device in provisioned if isinstance(device, str)] if isinstance(provisioned, list) else []
    return ProfileInfo(profile["Name"], profile["UUID"], team_identifiers[0], profile["ExpirationDate"], devices,
                       application_identifier=entitlements.get("application-identifier", ""),
                       xcode_managed=bool(profile.get("IsXcodeManaged")),
                       provisions_all_devices=bool(profile.get("ProvisionsAllDevices")),
                       has_devices=bool(provisioned), development=entitlements.get("get-task-allow") is True)


def normalize_expiry(info: ProfileInfo) -> None:
    expires_at = info.expires_at
    if not isinstance(expires_at, datetime.datetime):
        raise input_error("provisioning profile ExpirationDate is not a date")
    if expires_at.tzinfo is None:
        expires_at = expires_at.replace(tzinfo=datetime.timezone.utc)
    info.expires_at = expires_at


def parse_profile(profile: dict, bundle_identifier: str, now: datetime.datetime) -> ProfileInfo:
    info = read_profile_basics(profile)
    if info.xcode_managed:
        raise input_error("provisioning profile is Xcode managed; export signs manually, so create a manual Development "
                          "profile in the developer portal for the bundle identifier and device")
    if info.provisions_all_devices:
        raise input_error("provisioning profile is an enterprise profile; the development route needs a device-scoped profile")
    if not info.has_devices:
        raise input_error("provisioning profile lists no devices; the development route needs a Development profile")
    if not info.development:
        raise input_error("provisioning profile is not a Development profile (get-task-allow is not true)")
    if not info.covers(bundle_identifier):
        raise input_error("provisioning profile does not cover the probe's bundle identifier")
    normalize_expiry(info)
    if info.expires_at <= now:
        raise input_error("provisioning profile has expired")
    return info


def covering_profile_problem(info: ProfileInfo, now: datetime.datetime) -> Optional[str]:
    if info.xcode_managed:
        return "is Xcode managed; export signs manually, so create a manual Development profile in the developer portal"
    if info.provisions_all_devices:
        return "is an enterprise profile; the development route needs a device-scoped profile"
    if not info.has_devices:
        return "lists no devices; the development route needs a Development profile"
    if not info.development:
        return "is not a Development profile (get-task-allow is not true)"
    if info.expires_at <= now:
        return "has expired"
    return None


def bundle_profile_error(message: str) -> SigningError:
    return SigningError(message, EXIT_BUNDLE_PROFILE, "bundle-coverage")


class SigningRun:
    def __init__(self, scheme: str, install: bool, evidence_dir: Path, environment: Dict[str, str]):
        self.scheme = scheme
        self.bundle_identifier = SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS[scheme]
        self.embedding = scheme in EMBEDDING_SCHEMES
        self.install = install
        self.evidence_dir = evidence_dir
        self.environment = environment
        self.run_identifier = secrets.token_hex(4)
        self.redactor = Redactor()
        self.started_at = datetime.datetime.now(datetime.timezone.utc)

        self.profile_path: Optional[Path] = None
        self.profile_map: Dict[str, Path] = {}
        self.certificate_path: Optional[Path] = None
        self.certificate_password: Optional[str] = None
        self.device_id: Optional[str] = None
        self.device_udid: Optional[str] = None
        self.device_label: Optional[str] = None
        self.private_run_dir: Optional[Path] = None
        self.log_path: Optional[Path] = None

        self.work_dir: Optional[Path] = None
        self.keychain_path: Optional[Path] = None
        self.original_keychains: Optional[List[str]] = None
        self.installed_profile_paths: List[Path] = []
        self.partial_profile_paths: List[Path] = []
        self.profiles: List[ProfileInfo] = []
        self.bundles: List[EmbeddedBundle] = []
        self.assignments: Dict[str, ProfileInfo] = {}
        self.archive_path: Optional[Path] = None
        self.toolchain: Dict[str, str] = {"xcode_version": "unknown", "xcode_build": "unknown"}
        self.install_performed = False
        self.install_succeeded = False
        self.launch_performed = False
        self.launched = False
        self.running_confirmed = False
        self.failed_stage: Optional[str] = None

    # ----- inputs -----

    def read_inputs(self) -> None:
        env = self.environment
        profile_value = env.get(PROFILE_PATH_VARIABLE)
        map_value = env.get(PROFILE_MAP_VARIABLE)
        if map_value and not self.embedding:
            raise input_error(f"{PROFILE_MAP_VARIABLE} is only supported for schemes that embed extensions; "
                              f"{self.scheme} takes one profile in {PROFILE_PATH_VARIABLE}")
        if profile_value and map_value:
            raise input_error(f"set only one of {PROFILE_PATH_VARIABLE} and {PROFILE_MAP_VARIABLE}")
        if map_value:
            self.profile_map = self._read_profile_map(map_value)
        elif profile_value:
            self.profile_path = self._outside_repository_file(profile_value, PROFILE_PATH_VARIABLE)
        elif self.embedding:
            raise input_error(f"{PROFILE_PATH_VARIABLE} is required (path to a Development .mobileprovision outside the "
                              f"repository that covers every signed bundle), or {PROFILE_MAP_VARIABLE} for one profile per bundle")
        else:
            raise input_error(f"{PROFILE_PATH_VARIABLE} is required (path to a Development .mobileprovision outside the repository)")

        certificate_value = env.get(CERT_PATH_VARIABLE)
        if certificate_value:
            self.certificate_path = self._outside_repository_file(certificate_value, CERT_PATH_VARIABLE)
            self.certificate_password = self._read_certificate_password()
            self.redactor.register(self.certificate_password, "certificate-password")

        if self.install:
            self.device_id = env.get(DEVICE_ID_VARIABLE) or None
            if not self.device_id:
                raise input_error(f"--install requires {DEVICE_ID_VARIABLE} (the devicectl device identifier)")
            self.redactor.register(self.device_id, "device-id")
            if self.embedding:
                self.device_udid = env.get(DEVICE_UDID_VARIABLE) or None
                self.redactor.register(self.device_udid, "device-udid")
        label = env.get(DEVICE_LABEL_VARIABLE) or f"device-{secrets.token_hex(4)}"
        if not DEVICE_LABEL_PATTERN.match(label):
            raise input_error(f"{DEVICE_LABEL_VARIABLE} must be 1-32 characters from a-z, 0-9 and '-'")
        self.device_label = label

    def _read_profile_map(self, map_value: str) -> Dict[str, Path]:
        map_path = self._outside_repository_file(map_value, PROFILE_MAP_VARIABLE)
        try:
            entries = json.loads(map_path.read_text())
        except (OSError, ValueError):
            raise input_error(f"{PROFILE_MAP_VARIABLE} must hold a JSON object mapping bundle identifiers to profile files") from None
        if not isinstance(entries, dict) or not entries or len(entries) > MAXIMUM_MAPPED_BUNDLES:
            raise input_error(f"{PROFILE_MAP_VARIABLE} must hold a JSON object of 1-{MAXIMUM_MAPPED_BUNDLES} bundle identifier to profile file entries")
        mapping: Dict[str, Path] = {}
        for bundle_identifier, profile_value in entries.items():
            if not BUNDLE_IDENTIFIER_PATTERN.match(bundle_identifier) or not isinstance(profile_value, str) or not profile_value:
                raise input_error(f"{PROFILE_MAP_VARIABLE} has an entry that is not a bundle identifier mapped to a profile file path")
            mapping[bundle_identifier] = self._outside_repository_file(
                profile_value, f"{PROFILE_MAP_VARIABLE} entry for {bundle_identifier}", "ohand-signing-profile-map-entry")
        return mapping

    def _outside_repository_file(self, value: str, variable: str, label: Optional[str] = None) -> Path:
        path = Path(value).expanduser().resolve()
        if not path.is_file():
            raise input_error(f"{variable} does not point to a readable file")
        if path == REPO_ROOT or REPO_ROOT in path.parents:
            raise input_error(f"{variable} must point outside the repository")
        label = label or f"{variable.lower().replace('_', '-')}"
        self.redactor.register(value, label)
        self.redactor.register(str(path), label)
        self.redactor.register(path.name, f"{label}-file-name", forbid_in_evidence=False)
        return path

    def _read_certificate_password(self) -> str:
        env = self.environment
        password_file = env.get(CERT_PASSWORD_FILE_VARIABLE)
        if password_file:
            path = self._outside_repository_file(password_file, CERT_PASSWORD_FILE_VARIABLE)
            password = path.read_text().rstrip("\r\n")
        else:
            password = env.get(CERT_PASSWORD_VARIABLE, "")
        if not password:
            raise input_error(f"{CERT_PASSWORD_VARIABLE} or {CERT_PASSWORD_FILE_VARIABLE} is required with {CERT_PATH_VARIABLE}")
        return password

    # ----- private evidence and command execution -----

    def prepare_private_log(self) -> None:
        base_value = self.environment.get(PRIVATE_EVIDENCE_VARIABLE)
        base = Path(base_value).expanduser() if base_value else Path.home() / ".ohand-private-evidence"
        resolved = base.resolve()
        if resolved == REPO_ROOT or REPO_ROOT in resolved.parents:
            raise input_error(f"{PRIVATE_EVIDENCE_VARIABLE} must be outside the repository")
        self.private_run_dir = resolved / self.run_identifier
        self.private_run_dir.mkdir(parents=True, mode=0o700, exist_ok=True)
        os.chmod(self.private_run_dir, 0o700)
        self.log_path = self.private_run_dir / "run.log"
        self.log_path.touch(mode=0o600)

    def _log(self, text: str) -> None:
        if self.log_path is not None:
            with open(self.log_path, "a") as handle:
                handle.write(self.redactor.apply(text) + "\n")

    def _child_environment(self) -> Dict[str, str]:
        return {key: value for key, value in self.environment.items() if not key.startswith(INJECTED_VARIABLE_PREFIXES)}

    def run_tool(self, stage: str, argv: List[str], *, capture_output_in_log: bool = True,
                 allow_failure: bool = False, cwd: Optional[Path] = None) -> subprocess.CompletedProcess:
        self._log(f"$ [{stage}] " + " ".join(argv))
        try:
            completed = subprocess.run(argv, capture_output=True, text=True, env=self._child_environment(),
                                       cwd=str(cwd) if cwd else None, check=False)
        except FileNotFoundError:
            raise SigningError(f"{argv[0]} is not available on PATH (macOS with Xcode required)", EXIT_INPUT, stage)
        self._log(f"[{stage}] exit status {completed.returncode}")
        if capture_output_in_log:
            self._log(completed.stdout + completed.stderr)
        if completed.returncode != 0 and not allow_failure:
            raise SigningError(f"{stage} failed (exit status {completed.returncode}); redacted log in private evidence "
                               f"run {self.run_identifier}", EXIT_FAILURE, stage)
        return completed

    # ----- profile -----

    def load_profiles(self) -> None:
        paths = [self.profile_path] if self.profile_path is not None else list(dict.fromkeys(self.profile_map.values()))
        now = datetime.datetime.now(datetime.timezone.utc)
        for path in paths:
            completed = self.run_tool("read-profile", ["security", "cms", "-D", "-i", str(path)], capture_output_in_log=False)
            try:
                profile = plistlib.loads(completed.stdout.encode("utf-8"))
            except Exception:
                raise input_error("provisioning profile could not be decoded")
            if self.embedding:
                info = read_profile_basics(profile)
                normalize_expiry(info)
            else:
                info = parse_profile(profile, self.bundle_identifier, now)
            info.source_path = path
            self.profiles.append(info)
            self.redactor.register(info.name, "profile-name", forbid_in_evidence=False)
            self.redactor.register(info.uuid, "profile-uuid")
            self.redactor.register(info.team_identifier, "team-id")
            for device in info.provisioned_devices:
                self.redactor.register(device, "device-udid")
        if len({info.team_identifier for info in self.profiles}) > 1:
            raise input_error("the supplied provisioning profiles belong to different teams; export signs with one team")

    def profiles_directory(self) -> Path:
        return Path.home() / "Library" / "Developer" / "Xcode" / "UserData" / "Provisioning Profiles"

    def profiles_to_install(self) -> List[ProfileInfo]:
        used = list(self.assignments.values()) if self.embedding else self.profiles
        return list({info.uuid: info for info in used}.values())

    def install_profiles(self) -> None:
        for info in self.profiles_to_install():
            self._install_profile(info)

    def _install_profile(self, info: ProfileInfo) -> None:
        directory = self.profiles_directory()
        directory.mkdir(parents=True, exist_ok=True)
        destination = directory / f"{info.uuid}.mobileprovision"
        if destination.parent != directory:
            raise input_error("provisioning profile UUID does not name a file in Xcode's profile directory")
        if destination.exists():
            if destination.read_bytes() != info.source_path.read_bytes():
                raise input_error("a different provisioning profile with the same UUID is already installed in Xcode's "
                                  "profile directory; remove it or replace it with the supplied profile")
            return
        # Copy to a run-specific partial file and rename it onto the UUID name, so that the UUID name never holds a
        # truncated profile. Both paths are tracked before the copy starts so cleanup removes whatever was created.
        partial = directory / f".ohand-signing-{self.run_identifier}-{len(self.partial_profile_paths)}.partial"
        self.partial_profile_paths.append(partial)
        self.installed_profile_paths.append(destination)
        try:
            shutil.copyfile(info.source_path, partial)
            os.replace(partial, destination)
        except OSError:
            raise SigningError("could not install the provisioning profile", EXIT_FAILURE, "profile-install") from None

    # ----- keychain -----

    def parse_keychain_list(self, output: str) -> List[str]:
        paths = []
        for line in output.splitlines():
            match = KEYCHAIN_PATH_PATTERN.match(line)
            if match:
                paths.append(match.group(1))
        return paths

    def count_development_identities(self, extra_arguments: List[str]) -> int:
        completed = self.run_tool("find-identity", ["security", "find-identity", "-v", "-p", "codesigning"] + extra_arguments,
                                  capture_output_in_log=False)
        names = [m.group(1) for m in map(CODESIGNING_IDENTITY_PATTERN.match, completed.stdout.splitlines()) if m]
        distribution_only = names and not any(name.startswith(IDENTITY_CLASS) for name in names)
        if distribution_only:
            raise input_error(f"no {IDENTITY_CLASS} identity found; the development route cannot use other identity classes")
        return sum(1 for name in names if name.startswith(IDENTITY_CLASS))

    def prepare_identity(self) -> None:
        if self.certificate_path is None:
            count = self.count_development_identities([])
            if count != 1:
                raise input_error(f"expected exactly one valid {IDENTITY_CLASS} identity in the user keychains, found {count}")
            return

        keychain_password = secrets.token_urlsafe(24)
        self.redactor.register(keychain_password, "keychain-password")
        self.keychain_path = self.work_dir / f"ohand-signing-{self.run_identifier}.keychain-db"
        keychain = str(self.keychain_path)
        listing = self.run_tool("list-keychains", ["security", "list-keychains", "-d", "user"], capture_output_in_log=False)
        self.original_keychains = self.parse_keychain_list(listing.stdout)
        if not self.original_keychains:
            self.original_keychains = None
            raise input_error("could not read the user keychain search list; refusing to change it because it could not be restored")

        self.run_tool("create-keychain", ["security", "create-keychain", "-p", keychain_password, keychain])
        self.run_tool("keychain-settings", ["security", "set-keychain-settings", "-lut", str(KEYCHAIN_TIMEOUT_SECONDS), keychain])
        self.run_tool("unlock-keychain", ["security", "unlock-keychain", "-p", keychain_password, keychain])
        self.run_tool("import-identity", ["security", "import", str(self.certificate_path), "-k", keychain, "-f", "pkcs12",
                                          "-P", self.certificate_password, "-T", "/usr/bin/codesign", "-T", "/usr/bin/security"])
        self.run_tool("key-partition-list", ["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:",
                                             "-s", "-k", keychain_password, keychain])
        self.run_tool("search-list", ["security", "list-keychains", "-d", "user", "-s", keychain] + self.original_keychains)
        count = self.count_development_identities([keychain])
        if count != 1:
            raise input_error(f"the supplied certificate must provide exactly one valid {IDENTITY_CLASS} identity, found {count}")

    def cleanup(self) -> List[str]:
        problems = []
        if self.keychain_path is not None:
            if self.original_keychains is not None:
                restored = self._cleanup_step(["security", "list-keychains", "-d", "user", "-s"] + self.original_keychains)
                if not restored:
                    problems.append("could not restore the keychain search list")
            self._cleanup_step(["security", "delete-keychain", str(self.keychain_path)])
            with contextlib.suppress(OSError):
                self.keychain_path.unlink()
            if self.keychain_path.exists():
                problems.append("temporary keychain could not be removed")
        for profile_path in self.partial_profile_paths + self.installed_profile_paths:
            with contextlib.suppress(OSError):
                profile_path.unlink()
            if profile_path.exists():
                problems.append("installed provisioning profile could not be removed")
        if self.work_dir is not None:
            shutil.rmtree(self.work_dir, ignore_errors=True)
            if self.work_dir.exists():
                problems.append("temporary working directory could not be removed")
        return problems

    def _cleanup_step(self, argv: List[str]) -> bool:
        try:
            completed = subprocess.run(argv, capture_output=True, text=True, env=self._child_environment(), check=False)
        except OSError:
            return False
        return completed.returncode == 0

    # ----- build, export, install -----

    def record_toolchain(self) -> None:
        completed = self.run_tool("xcode-version", ["xcodebuild", "-version"], allow_failure=True)
        lines = completed.stdout.splitlines()
        if lines and XCODE_VERSION_PATTERN.match(lines[0].strip()):
            self.toolchain["xcode_version"] = lines[0].strip()
        if len(lines) > 1 and XCODE_BUILD_PATTERN.match(lines[1].strip()):
            self.toolchain["xcode_build"] = lines[1].strip().split(" ", 2)[2]

    def generate_and_archive(self) -> None:
        self.run_tool("generate-project", [str(IOS_ROOT / "scripts" / "generate.sh")])
        self.archive_path = self.work_dir / f"{self.scheme}.xcarchive"
        derived_data_path = self.work_dir / "derived"
        self.run_tool("archive", [
            "xcodebuild", "archive",
            "-project", str(IOS_ROOT / "OhAnd.xcodeproj"),
            "-scheme", self.scheme,
            "-configuration", "Release",
            "-destination", "generic/platform=iOS",
            "-archivePath", str(self.archive_path),
            "-derivedDataPath", str(derived_data_path),
            "CODE_SIGNING_ALLOWED=NO",
        ])

    def export_signed_app(self) -> Path:
        export_path = self.work_dir / "export"
        if self.embedding:
            provisioning_profiles = {bundle.identifier: self.assignments[bundle.identifier].uuid for bundle in self.bundles}
        else:
            provisioning_profiles = {self.bundle_identifier: self.profiles[0].uuid}
        options_path = self.work_dir / "ExportOptions.plist"
        options = {
            "method": EXPORT_METHOD,
            "signingStyle": "manual",
            "signingCertificate": IDENTITY_CLASS,
            "teamID": self.profiles[0].team_identifier,
            "provisioningProfiles": provisioning_profiles,
        }
        options_path.write_bytes(plistlib.dumps(options))
        os.chmod(options_path, 0o600)
        self.run_tool("export", [
            "xcodebuild", "-exportArchive",
            "-archivePath", str(self.archive_path),
            "-exportPath", str(export_path),
            "-exportOptionsPlist", str(options_path),
        ])
        return self._extract_signed_app(export_path)

    # ----- embedded bundles -----

    def inspect_archive(self) -> None:
        applications = sorted((self.archive_path / "Products" / "Applications").glob("*.app"))
        if len(applications) != 1:
            raise SigningError("archive does not contain exactly one app", EXIT_FAILURE, "archive-inspect")
        self.bundles = self._signed_bundles(applications[0])
        if self.bundles[0].identifier != self.bundle_identifier:
            raise SigningError("archived app does not have the scheme's bundle identifier", EXIT_FAILURE, "archive-inspect")

    @staticmethod
    def _signed_bundles(application: Path) -> List[EmbeddedBundle]:
        def identifier_of(bundle: Path) -> str:
            try:
                identifier = plistlib.loads((bundle / "Info.plist").read_bytes())["CFBundleIdentifier"]
            except Exception:
                raise SigningError(f"embedded bundle {bundle.relative_to(application)} has no readable bundle identifier",
                                   EXIT_FAILURE, "archive-inspect") from None
            if not isinstance(identifier, str) or not BUNDLE_IDENTIFIER_PATTERN.match(identifier):
                raise SigningError(f"embedded bundle {bundle.relative_to(application)} has an invalid bundle identifier",
                                   EXIT_FAILURE, "archive-inspect")
            return identifier

        bundles = [EmbeddedBundle(identifier_of(application), "application")]
        extensions = [path for name in EXTENSION_DIRECTORY_NAMES for path in sorted((application / name).glob("*.appex"))]
        bundles += sorted((EmbeddedBundle(identifier_of(extension), "app-extension") for extension in extensions),
                          key=lambda bundle: bundle.identifier)
        if len({bundle.identifier for bundle in bundles}) != len(bundles):
            raise SigningError("archive embeds two bundles with the same bundle identifier", EXIT_FAILURE, "archive-inspect")
        return bundles

    def assign_profiles(self) -> None:
        now = datetime.datetime.now(datetime.timezone.utc)
        if self.profile_map:
            unknown = sorted(set(self.profile_map) - {bundle.identifier for bundle in self.bundles})
            if unknown:
                raise input_error(f"{PROFILE_MAP_VARIABLE} names bundle identifier {unknown[0]}, which is not embedded in the archive")
        by_path = {info.source_path: info for info in self.profiles}
        for bundle in self.bundles:
            if self.profile_map:
                if bundle.identifier not in self.profile_map:
                    raise bundle_profile_error(f"no supplied provisioning profile is mapped to bundle identifier {bundle.identifier}")
                info = by_path[self.profile_map[bundle.identifier]]
            else:
                info = self.profiles[0]
            if not info.covers(bundle.identifier):
                raise bundle_profile_error(f"no supplied provisioning profile covers bundle identifier {bundle.identifier}")
            problem = covering_profile_problem(info, now)
            if problem:
                raise bundle_profile_error(f"provisioning profile covering bundle identifier {bundle.identifier} {problem}")
            self.assignments[bundle.identifier] = info

    def check_target_device_listed(self) -> None:
        if not self.install:
            return
        udid = self.device_udid or self._read_device_udid()
        self.redactor.register(udid, "device-udid")
        for bundle in self.bundles:
            listed = {device.lower() for device in self.assignments[bundle.identifier].provisioned_devices}
            if udid.lower() not in listed:
                raise bundle_profile_error(
                    f"provisioning profile covering bundle identifier {bundle.identifier} does not list the target device")

    def _read_device_udid(self) -> str:
        output_path = self.work_dir / "device-details.json"
        completed = self.run_tool("device-details", ["xcrun", "devicectl", "device", "info", "details", "--device", self.device_id,
                                                     "--json-output", str(output_path)], allow_failure=True)
        hint = f"could not read the target device's hardware identifier with devicectl; set {DEVICE_UDID_VARIABLE}"
        udid = None
        if completed.returncode == 0:
            try:
                udid = json.loads(output_path.read_text())["result"]["hardwareProperties"]["udid"]
            except Exception:
                udid = None
        if not isinstance(udid, str) or not udid:
            raise SigningError(hint, EXIT_INPUT, "device-details")
        return udid

    def _extract_signed_app(self, export_path: Path) -> Path:
        archives = sorted(export_path.glob("*.ipa")) if export_path.is_dir() else []
        if len(archives) != 1:
            raise SigningError("export did not produce exactly one .ipa", EXIT_FAILURE, "export")
        extract_root = self.work_dir / "payload"
        with zipfile.ZipFile(archives[0]) as archive:
            names = archive.namelist()
            for name in names:
                target = (extract_root / name).resolve()
                if extract_root.resolve() not in target.parents and target != extract_root.resolve():
                    raise SigningError("exported .ipa contains an unsafe path", EXIT_FAILURE, "export")
            archive.extractall(extract_root)
        apps = sorted((extract_root / "Payload").glob("*.app"))
        if len(apps) != 1:
            raise SigningError("exported .ipa does not contain exactly one app", EXIT_FAILURE, "export")
        if not (apps[0] / "_CodeSignature" / "CodeResources").is_file():
            raise SigningError("exported app is not code-signed", EXIT_FAILURE, "export")
        if self.embedding:
            self._verify_exported_bundles(apps[0])
        return apps[0]

    def _verify_exported_bundles(self, application: Path) -> None:
        exported = self._signed_bundles(application)
        if [bundle.identifier for bundle in exported] != [bundle.identifier for bundle in self.bundles]:
            raise SigningError("exported app embeds different bundles than the archive", EXIT_FAILURE, "export")
        for extension in [path for name in EXTENSION_DIRECTORY_NAMES for path in (application / name).glob("*.appex")]:
            if not (extension / "_CodeSignature" / "CodeResources").is_file():
                raise SigningError("an exported app extension is not code-signed", EXIT_FAILURE, "export")

    def install_on_device(self, app_path: Path) -> None:
        self.install_performed = True
        self.run_tool("install", ["xcrun", "devicectl", "device", "install", "app", "--device", self.device_id, str(app_path)])
        self.install_succeeded = True

    def launch_on_device(self) -> None:
        self.launch_performed = True
        launch_output = self.work_dir / "launch.json"
        self.run_tool("launch", ["xcrun", "devicectl", "device", "process", "launch", "--device", self.device_id,
                                 "--terminate-existing", "--json-output", str(launch_output), self.bundle_identifier])
        process_identifier = self._read_json(launch_output, "launch", ("result", "process", "processIdentifier"))
        if not isinstance(process_identifier, int) or isinstance(process_identifier, bool):
            raise SigningError("launch did not report a process identifier", EXIT_FAILURE, "launch")
        self.launched = True
        processes_output = self.work_dir / "processes.json"
        self.run_tool("running-check", ["xcrun", "devicectl", "device", "info", "processes", "--device", self.device_id,
                                        "--json-output", str(processes_output)])
        running = self._read_json(processes_output, "running-check", ("result", "runningProcesses"))
        if not isinstance(running, list) or not any(
                isinstance(entry, dict) and entry.get("processIdentifier") == process_identifier for entry in running):
            raise SigningError("the launched app is not in the device's running process list", EXIT_FAILURE, "running-check")
        self.running_confirmed = True

    @staticmethod
    def _read_json(path: Path, stage: str, keys: tuple):
        try:
            value = json.loads(path.read_text())
            for key in keys:
                value = value[key]
            return value
        except Exception:
            raise SigningError(f"devicectl output for {stage} could not be read", EXIT_FAILURE, stage) from None

    # ----- evidence -----

    def build_revision(self) -> Dict[str, object]:
        def git(*arguments: str) -> Optional[str]:
            try:
                completed = subprocess.run(["git", "-C", str(REPO_ROOT), *arguments], capture_output=True, text=True, check=False)
            except OSError:
                return None
            return completed.stdout.strip() if completed.returncode == 0 else None

        commit = git("rev-parse", "HEAD") or "unknown"
        porcelain = git("status", "--porcelain")
        return {"commit": commit, "working_tree_clean": porcelain == ""}

    def evidence_record(self, status: str) -> dict:
        revision = self.build_revision()
        profile_expiry = None
        if self.profiles:
            profile_expiry = min(self.profiles if not self.assignments else self.assignments.values(),
                                 key=lambda info: info.expires_at).expires_text()
        commit = revision["commit"]
        record = {
            "schema": EVIDENCE_SCHEMA,
            "status": status,
            "failed_stage": self.failed_stage,
            "collected_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "build_revision": commit,
            "working_tree_clean": revision["working_tree_clean"],
            "build_identifier": f"{commit[:8]}-{self.run_identifier}",
            "scheme": self.scheme,
            "bundle_identifier": self.bundle_identifier,
            "route": ROUTE,
            "identity_class": IDENTITY_CLASS,
            "signing_style": "manual",
            "export_method": EXPORT_METHOD,
            "toolchain": dict(self.toolchain),
            "profile": {"expires_at": profile_expiry, "device_scoped": bool(self.profiles)},
            "device": {"label": self.device_label},
            "install": {
                "requested": self.install,
                "performed": self.install_performed,
                "succeeded": self.install_succeeded,
            },
            "private_evidence_reference": self.run_identifier,
        }
        if self.embedding:
            record["schema"] = EMBEDDED_EVIDENCE_SCHEMA
            record["embedded_bundles"] = [
                {"bundle_identifier": bundle.identifier, "kind": bundle.kind,
                 "profile_expires_at": self.assignments[bundle.identifier].expires_text() if bundle.identifier in self.assignments else None}
                for bundle in self.bundles]
            record["launch"] = {"requested": self.install, "performed": self.launch_performed, "launched": self.launched,
                                "running_confirmed": self.running_confirmed}
        return record

    def write_evidence(self, status: str) -> Optional[Path]:
        record = self.evidence_record(status)
        text = json.dumps(record, indent=2, sort_keys=True) + "\n"
        for value in self.redactor.values_forbidden_in_evidence():
            if value in text:
                raise SigningError("sanitized evidence would contain a sensitive value; not written", EXIT_FAILURE, "evidence")
        self.evidence_dir.mkdir(parents=True, exist_ok=True)
        stamp = self.started_at.strftime("%Y%m%dT%H%M%SZ")
        destination = self.evidence_dir / f"apple-signing-{stamp}-{self.run_identifier}.json"
        temporary = destination.with_suffix(".json.tmp")
        temporary.write_text(text)
        os.replace(temporary, destination)
        return destination


def install_signal_handlers() -> None:
    def raise_interrupted(signal_number, frame):
        raise Interrupted()

    for signal_number in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(signal_number, raise_interrupted)


def execute(run: SigningRun) -> int:
    exit_code = EXIT_OK
    reached_build = False
    try:
        run.read_inputs()
        run.prepare_private_log()
        run.work_dir = Path(tempfile.mkdtemp(prefix="ohand-signing-"))
        os.chmod(run.work_dir, 0o700)
        run.load_profiles()
        reached_build = True
        run.record_toolchain()
        if run.embedding:
            # Bundle identifiers exist only in the archive; the unsigned archive needs no keychain or profile, so every
            # coverage refusal happens before any keychain work or signing.
            run.generate_and_archive()
            run.inspect_archive()
            run.assign_profiles()
            run.check_target_device_listed()
            run.prepare_identity()
            run.install_profiles()
        else:
            run.prepare_identity()
            run.install_profiles()
            run.generate_and_archive()
        app_path = run.export_signed_app()
        if run.install:
            run.install_on_device(app_path)
            if run.embedding:
                run.launch_on_device()
    except SigningError as error:
        run.failed_stage = error.stage
        print(f"error: {error.message}", file=sys.stderr)
        exit_code = error.exit_code
    except Interrupted:
        run.failed_stage = "interrupted"
        print("error: interrupted; cleaning up", file=sys.stderr)
        exit_code = EXIT_FAILURE
    except BaseException:
        run.failed_stage = "internal"
        print("error: unexpected internal failure; cleaning up", file=sys.stderr)
        exit_code = EXIT_FAILURE
    finally:
        for signal_number in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            signal.signal(signal_number, signal.SIG_IGN)
        problems = run.cleanup()
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        if problems and exit_code == EXIT_OK:
            exit_code = EXIT_FAILURE
            run.failed_stage = "cleanup"

    if reached_build and run.profiles:
        try:
            status = "succeeded" if exit_code == EXIT_OK else "failed"
            destination = run.write_evidence(status)
            print(f"sanitized evidence written: {destination.name}")
        except SigningError as error:
            print(f"error: {error.message}", file=sys.stderr)
            exit_code = EXIT_FAILURE
    if exit_code == EXIT_OK:
        print("signed build complete" + ("; installed on device" if run.install else "; install not requested"))
    return exit_code


def parse_arguments(argv: List[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Archive, sign (Development route), and optionally install and launch a probe or OhAndApp.")
    parser.add_argument("--scheme", default=DEFAULT_SCHEME, choices=sorted(SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS))
    parser.add_argument("--install", action="store_true", help=f"install on the device named by {DEVICE_ID_VARIABLE}")
    parser.add_argument("--evidence-dir", default=str(DEFAULT_EVIDENCE_DIR),
                        help="directory for the sanitized evidence record (default: docs/validation/evidence/apple-signing)")
    return parser.parse_args(argv)


def main(argv: Optional[List[str]] = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    install_signal_handlers()
    run = SigningRun(arguments.scheme, arguments.install, Path(arguments.evidence_dir), dict(os.environ))
    return execute(run)


if __name__ == "__main__":
    sys.exit(main())
