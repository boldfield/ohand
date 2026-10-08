#!/usr/bin/env python3
"""Build, sign and optionally install a probe on a physical iPhone (macOS with Xcode only).

Route: Apple Development identity plus a Development provisioning profile that lists the device.
The probe is archived unsigned, then `xcodebuild -exportArchive` signs it from an explicit export
options file, so each bundle identifier gets its own profile and frameworks are never given one.

Signing inputs come only from the environment or injected files outside the repository:
  OHAND_SIGNING_PROFILE_PATH         required  .mobileprovision file
  OHAND_SIGNING_CERT_PATH            optional  .p12 identity; imported into a per-run temporary keychain.
                                               Without it, exactly one Apple Development identity must
                                               already be in the user's keychains.
  OHAND_SIGNING_CERT_PASSWORD        .p12 password (or OHAND_SIGNING_CERT_PASSWORD_FILE)
  OHAND_DEVICE_ID                    devicectl device identifier; required with --install
  OHAND_DEVICE_LABEL                 optional content-free label for evidence (a-z, 0-9, '-')
  OHAND_PRIVATE_EVIDENCE_DIR         optional; raw redacted logs, must be outside the repository

Stdout and stderr carry only fixed messages. Child tool output goes to a redacted log in the private
evidence directory. The sanitized evidence record contains fixed fields only.

Exit status: 0 success, 1 build/signing/install failure, 2 missing or invalid input.
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

EVIDENCE_SCHEMA = "ohand.apple-signing-evidence.v1"
ROUTE = "development"
IDENTITY_CLASS = "Apple Development"
EXPORT_METHOD = "debugging"
DEFAULT_SCHEME = "BridgeProbe"
# Single-application probes only: schemes with embedded extensions would need one profile per bundle identifier.
SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS = {
    "BridgeProbe": "com.boldfield.ohand.probes.bridge",
    "AudioProbe": "com.boldfield.ohand.probes.audio",
    "TranscriptionProbe": "com.boldfield.ohand.probes.transcription",
    "NotificationProbe": "com.boldfield.ohand.probes.notification",
}

PROFILE_PATH_VARIABLE = "OHAND_SIGNING_PROFILE_PATH"
CERT_PATH_VARIABLE = "OHAND_SIGNING_CERT_PATH"
CERT_PASSWORD_VARIABLE = "OHAND_SIGNING_CERT_PASSWORD"
CERT_PASSWORD_FILE_VARIABLE = "OHAND_SIGNING_CERT_PASSWORD_FILE"
DEVICE_ID_VARIABLE = "OHAND_DEVICE_ID"
DEVICE_LABEL_VARIABLE = "OHAND_DEVICE_LABEL"
PRIVATE_EVIDENCE_VARIABLE = "OHAND_PRIVATE_EVIDENCE_DIR"
INJECTED_VARIABLE_PREFIXES = ("OHAND_SIGNING_", "OHAND_DEVICE_")

DEVICE_LABEL_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{0,31}$")
XCODE_VERSION_PATTERN = re.compile(r"^Xcode \d+(\.\d+){0,2}$")
XCODE_BUILD_PATTERN = re.compile(r"^Build version [0-9A-Za-z]+$")
CODESIGNING_IDENTITY_PATTERN = re.compile(r'^\s*\d+\)\s+[0-9A-Fa-f]{40}\s+"([^"]*)"')
KEYCHAIN_PATH_PATTERN = re.compile(r'^\s*"(.*)"\s*$')
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
    def __init__(self, name: str, uuid: str, team_identifier: str, expires_at: datetime.datetime,
                 provisioned_devices: List[str]):
        self.name = name
        self.uuid = uuid
        self.team_identifier = team_identifier
        self.expires_at = expires_at
        self.provisioned_devices = provisioned_devices


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


def parse_profile(profile: dict, bundle_identifier: str, now: datetime.datetime) -> ProfileInfo:
    required_fields = ("Name", "UUID", "TeamIdentifier", "ExpirationDate", "Entitlements")
    missing = [field for field in required_fields if not profile.get(field)]
    if missing:
        raise input_error("provisioning profile is missing required field(s): " + ", ".join(missing))
    team_identifiers = profile["TeamIdentifier"]
    if not isinstance(team_identifiers, list) or not team_identifiers or not isinstance(team_identifiers[0], str):
        raise input_error("provisioning profile has no usable TeamIdentifier")
    entitlements = profile["Entitlements"]
    if profile.get("IsXcodeManaged"):
        raise input_error("provisioning profile is Xcode managed; export signs manually, so create a manual Development "
                          "profile in the developer portal for the bundle identifier and device")
    if profile.get("ProvisionsAllDevices"):
        raise input_error("provisioning profile is an enterprise profile; the development route needs a device-scoped profile")
    if not profile.get("ProvisionedDevices"):
        raise input_error("provisioning profile lists no devices; the development route needs a Development profile")
    if entitlements.get("get-task-allow") is not True:
        raise input_error("provisioning profile is not a Development profile (get-task-allow is not true)")
    if not profile_covers_bundle(entitlements.get("application-identifier", ""), team_identifiers[0], bundle_identifier):
        raise input_error("provisioning profile does not cover the probe's bundle identifier")
    expires_at = profile["ExpirationDate"]
    if not isinstance(expires_at, datetime.datetime):
        raise input_error("provisioning profile ExpirationDate is not a date")
    if expires_at.tzinfo is None:
        expires_at = expires_at.replace(tzinfo=datetime.timezone.utc)
    if expires_at <= now:
        raise input_error("provisioning profile has expired")
    devices = [device for device in profile["ProvisionedDevices"] if isinstance(device, str)]
    return ProfileInfo(profile["Name"], profile["UUID"], team_identifiers[0], expires_at, devices)


class SigningRun:
    def __init__(self, scheme: str, install: bool, evidence_dir: Path, environment: Dict[str, str]):
        self.scheme = scheme
        self.bundle_identifier = SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS[scheme]
        self.install = install
        self.evidence_dir = evidence_dir
        self.environment = environment
        self.run_identifier = secrets.token_hex(4)
        self.redactor = Redactor()
        self.started_at = datetime.datetime.now(datetime.timezone.utc)

        self.profile_path: Optional[Path] = None
        self.certificate_path: Optional[Path] = None
        self.certificate_password: Optional[str] = None
        self.device_id: Optional[str] = None
        self.device_label: Optional[str] = None
        self.private_run_dir: Optional[Path] = None
        self.log_path: Optional[Path] = None

        self.work_dir: Optional[Path] = None
        self.keychain_path: Optional[Path] = None
        self.original_keychains: Optional[List[str]] = None
        self.installed_profile_path: Optional[Path] = None
        self.profile_info: Optional[ProfileInfo] = None
        self.toolchain: Dict[str, str] = {"xcode_version": "unknown", "xcode_build": "unknown"}
        self.install_performed = False
        self.install_succeeded = False
        self.failed_stage: Optional[str] = None

    # ----- inputs -----

    def read_inputs(self) -> None:
        env = self.environment
        profile_value = env.get(PROFILE_PATH_VARIABLE)
        if not profile_value:
            raise input_error(f"{PROFILE_PATH_VARIABLE} is required (path to a Development .mobileprovision outside the repository)")
        self.profile_path = self._outside_repository_file(profile_value, PROFILE_PATH_VARIABLE)

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
        label = env.get(DEVICE_LABEL_VARIABLE) or f"device-{secrets.token_hex(4)}"
        if not DEVICE_LABEL_PATTERN.match(label):
            raise input_error(f"{DEVICE_LABEL_VARIABLE} must be 1-32 characters from a-z, 0-9 and '-'")
        self.device_label = label

    def _outside_repository_file(self, value: str, variable: str) -> Path:
        path = Path(value).expanduser().resolve()
        if not path.is_file():
            raise input_error(f"{variable} does not point to a readable file")
        if path == REPO_ROOT or REPO_ROOT in path.parents:
            raise input_error(f"{variable} must point outside the repository")
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

    def load_profile(self) -> None:
        completed = self.run_tool("read-profile", ["security", "cms", "-D", "-i", str(self.profile_path)],
                                  capture_output_in_log=False)
        try:
            profile = plistlib.loads(completed.stdout.encode("utf-8"))
        except Exception:
            raise input_error("provisioning profile could not be decoded")
        self.profile_info = parse_profile(profile, self.bundle_identifier, datetime.datetime.now(datetime.timezone.utc))
        self.redactor.register(self.profile_info.name, "profile-name", forbid_in_evidence=False)
        self.redactor.register(self.profile_info.uuid, "profile-uuid")
        self.redactor.register(self.profile_info.team_identifier, "team-id")
        for device in self.profile_info.provisioned_devices:
            self.redactor.register(device, "device-udid")

    def profiles_directory(self) -> Path:
        return Path.home() / "Library" / "Developer" / "Xcode" / "UserData" / "Provisioning Profiles"

    def install_profile(self) -> None:
        directory = self.profiles_directory()
        directory.mkdir(parents=True, exist_ok=True)
        destination = directory / f"{self.profile_info.uuid}.mobileprovision"
        if destination.exists():
            return
        shutil.copyfile(self.profile_path, destination)
        self.installed_profile_path = destination

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
        if self.installed_profile_path is not None:
            with contextlib.suppress(OSError):
                self.installed_profile_path.unlink()
            if self.installed_profile_path.exists():
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

    def build_and_export(self) -> Path:
        self.run_tool("generate-project", [str(IOS_ROOT / "scripts" / "generate.sh")])
        archive_path = self.work_dir / f"{self.scheme}.xcarchive"
        export_path = self.work_dir / "export"
        derived_data_path = self.work_dir / "derived"
        self.run_tool("archive", [
            "xcodebuild", "archive",
            "-project", str(IOS_ROOT / "OhAnd.xcodeproj"),
            "-scheme", self.scheme,
            "-configuration", "Release",
            "-destination", "generic/platform=iOS",
            "-archivePath", str(archive_path),
            "-derivedDataPath", str(derived_data_path),
            "CODE_SIGNING_ALLOWED=NO",
        ])

        options_path = self.work_dir / "ExportOptions.plist"
        options = {
            "method": EXPORT_METHOD,
            "signingStyle": "manual",
            "signingCertificate": IDENTITY_CLASS,
            "teamID": self.profile_info.team_identifier,
            "provisioningProfiles": {self.bundle_identifier: self.profile_info.uuid},
        }
        options_path.write_bytes(plistlib.dumps(options))
        os.chmod(options_path, 0o600)
        self.run_tool("export", [
            "xcodebuild", "-exportArchive",
            "-archivePath", str(archive_path),
            "-exportPath", str(export_path),
            "-exportOptionsPlist", str(options_path),
        ])
        return self._extract_signed_app(export_path)

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
        return apps[0]

    def install_on_device(self, app_path: Path) -> None:
        self.install_performed = True
        self.run_tool("install", ["xcrun", "devicectl", "device", "install", "app", "--device", self.device_id, str(app_path)])
        self.install_succeeded = True

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
        if self.profile_info is not None:
            profile_expiry = self.profile_info.expires_at.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
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
            "profile": {"expires_at": profile_expiry, "device_scoped": self.profile_info is not None},
            "device": {"label": self.device_label},
            "install": {
                "requested": self.install,
                "performed": self.install_performed,
                "succeeded": self.install_succeeded,
            },
            "private_evidence_reference": self.run_identifier,
        }
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
        run.load_profile()
        reached_build = True
        run.record_toolchain()
        run.prepare_identity()
        run.install_profile()
        app_path = run.build_and_export()
        if run.install:
            run.install_on_device(app_path)
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

    if reached_build and run.profile_info is not None:
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
    parser = argparse.ArgumentParser(description="Archive, sign (Development route) and optionally install a probe.")
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
