#!/usr/bin/env python3
"""Fail-closed protocol probe for the user-controlled Spark (OpenAI-style) endpoint.

Reads one named provider from the Pi-style models.json mounted in the Odonian worker,
sends a few bounded synthetic requests, and writes a sanitized evidence artifact.
The endpoint address and credential are never printed or written anywhere.

Exit status: 0 every probe passed, 1 at least one probe failed (artifact still written),
2 usage or configuration error (no requests sent, no artifact written).
"""

import argparse
import http.client
import json
import os
import ssl
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit

DEFAULT_CONFIG_PATH = "/etc/ohand-provider/models.json"
CONFIG_PATH_ENV_VAR = "OHAND_PROVIDER_CONFIG_PATH"
REPO_ROOT = Path(__file__).resolve().parents[2]
EVIDENCE_DIRECTORY_RELATIVE = "docs/validation/evidence/spark-probe"
DEFAULT_OUTPUT_DIRECTORY = REPO_ROOT / EVIDENCE_DIRECTORY_RELATIVE

EXIT_PASSED = 0
EXIT_PROBE_FAILED = 1
EXIT_CONFIGURATION_ERROR = 2

SCHEMA_VERSION = 1
ALLOWLISTED_RESPONSE_HEADERS = ("server", "content-type", "date")
MODEL_ENTRY_ALLOWLISTED_FIELDS = ("object",)
MAX_RESPONSE_BYTES = 1024 * 1024
MODELS_TIMEOUT_SECONDS = 15
CHAT_TIMEOUT_SECONDS = 120
CHAT_MAX_TOKENS = 1024

REQUESTED_STATUS_VALUE = "ok"
REQUESTED_COUNT_VALUE = 3
STRUCTURED_PROMPT = (
    "Return a JSON object with exactly two keys: \"status\" set to the string \"ok\" "
    "and \"count\" set to the integer 3. Return only the JSON object."
)
STRUCTURED_RESPONSE_FORMAT = {
    "type": "json_schema",
    "json_schema": {
        "name": "probe_status",
        "strict": True,
        "schema": {
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": [REQUESTED_STATUS_VALUE]},
                "count": {"type": "integer", "enum": [REQUESTED_COUNT_VALUE]},
            },
            "required": ["status", "count"],
            "additionalProperties": False,
        },
    },
}


class ConfigurationError(Exception):
    """A usage or provider-configuration problem; the message is safe to print."""


class ProviderSettings:
    def __init__(self, base_url, credential, scheme, host, port, base_path):
        self.base_url = base_url
        self.credential = credential
        self.scheme = scheme
        self.host = host
        self.port = port
        self.base_path = base_path

    def sensitive_values(self):
        values = {self.credential, self.base_url, self.base_url.rstrip("/"), self.host}
        if self.host and self.port:
            values.add(f"{self.host}:{self.port}")
        return [value for value in values if value]


def load_provider_settings(config_path, provider_key):
    if not provider_key or not provider_key.strip():
        raise ConfigurationError("provider key must not be empty")
    path = Path(config_path)
    if not path.is_file():
        raise ConfigurationError(f"provider configuration file not found at {config_path}")
    try:
        with open(path, "r", encoding="utf-8") as config_file:
            config = json.load(config_file)
    except (OSError, ValueError) as error:
        raise ConfigurationError(
            f"provider configuration at {config_path} is unreadable or not valid JSON ({type(error).__name__})"
        )
    providers = config.get("providers") if isinstance(config, dict) else None
    if not isinstance(providers, dict):
        raise ConfigurationError("provider configuration has no 'providers' object")
    provider = providers.get(provider_key)
    if not isinstance(provider, dict):
        raise ConfigurationError(f"provider '{provider_key}' is not defined in the provider configuration")
    base_url = provider.get("baseUrl")
    credential = provider.get("apiKey")
    if not isinstance(base_url, str) or not base_url.strip():
        raise ConfigurationError(f"provider '{provider_key}' has a missing or empty baseUrl")
    if not isinstance(credential, str) or not credential.strip():
        raise ConfigurationError(f"provider '{provider_key}' has a missing or empty apiKey")
    base_url = base_url.strip()
    credential = credential.strip()
    try:
        parts = urlsplit(base_url)
        port = parts.port
        host = parts.hostname
    except ValueError:
        raise ConfigurationError(f"provider '{provider_key}' baseUrl is not a valid URL")
    if parts.scheme.lower() in ("http", "https") and not host:
        raise ConfigurationError(f"provider '{provider_key}' baseUrl has no host")
    scheme = parts.scheme.lower() if parts.scheme.lower() in ("http", "https") else "unknown"
    return ProviderSettings(base_url, credential, scheme, host, port, parts.path.rstrip("/"))


def classify_transport(scheme):
    if scheme == "http":
        return "cleartext-http"
    if scheme == "https":
        return "https"
    return "unknown-scheme"


def send_request(settings, method, path, send_credential, timeout, json_body=None):
    """Perform one request without following redirects. Returns an observation dict plus the body bytes."""
    observation = {
        "method": method,
        "path": path,
        "credential_sent": send_credential,
        "status": None,
        "response_headers": {},
        "tls_verified": False,
        "error": None,
    }
    headers = {"Accept": "application/json"}
    if send_credential:
        headers["Authorization"] = f"Bearer {settings.credential}"
    body_bytes = None
    if json_body is not None:
        body_bytes = json.dumps(json_body).encode("utf-8")
        headers["Content-Type"] = "application/json"

    connection = None
    try:
        if settings.scheme == "https":
            context = ssl.create_default_context()
            connection = http.client.HTTPSConnection(settings.host, settings.port, timeout=timeout, context=context)
        else:
            connection = http.client.HTTPConnection(settings.host, settings.port, timeout=timeout)
        connection.request(method, settings.base_path + path, body=body_bytes, headers=headers)
        response = connection.getresponse()
        observation["status"] = response.status
        observation["response_headers"] = {
            name: response.getheader(name) for name in ALLOWLISTED_RESPONSE_HEADERS if response.getheader(name) is not None
        }
        body = response.read(MAX_RESPONSE_BYTES + 1)
        if len(body) > MAX_RESPONSE_BYTES:
            observation["error"] = "response-too-large"
            return observation, b""
        observation["tls_verified"] = settings.scheme == "https"
        return observation, body
    except ssl.SSLCertVerificationError:
        observation["error"] = "tls-verification-failed"
    except ssl.SSLError:
        observation["error"] = "tls-error"
    except TimeoutError:
        observation["error"] = "timeout"
    except OSError:
        observation["error"] = "connection-failed"
    except http.client.HTTPException:
        observation["error"] = "http-protocol-error"
    finally:
        if connection is not None:
            connection.close()
    return observation, b""


def parse_json_object(body_bytes):
    try:
        parsed = json.loads(body_bytes.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    return parsed if isinstance(parsed, dict) else None


def extract_model_entries(body_bytes):
    """Return (entries, reason). Entries are validated /models records; reason is set on failure."""
    parsed = parse_json_object(body_bytes)
    if parsed is None:
        return None, "body-not-a-json-object"
    data = parsed.get("data")
    if not isinstance(data, list) or not data:
        return None, "data-missing-or-empty"
    entries = []
    for entry in data:
        if not isinstance(entry, dict):
            return None, "model-entry-not-an-object"
        model_id = entry.get("id")
        if not isinstance(model_id, str) or not model_id.strip():
            return None, "model-id-missing-or-not-a-nonempty-string"
        recorded = {"id": model_id}
        for field in MODEL_ENTRY_ALLOWLISTED_FIELDS:
            if isinstance(entry.get(field), str):
                recorded[field] = entry[field]
        entries.append(recorded)
    return entries, None


def extract_chat_message_content(body_bytes):
    """Return (content, reason) for an OpenAI-style chat completion body."""
    parsed = parse_json_object(body_bytes)
    if parsed is None:
        return None, "body-not-a-json-object"
    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices or not isinstance(choices[0], dict):
        return None, "choices-missing-or-empty"
    message = choices[0].get("message")
    if not isinstance(message, dict):
        return None, "message-missing"
    if message.get("role") != "assistant":
        return None, "message-role-not-assistant"
    if not isinstance(message.get("content"), str):
        return None, "message-content-not-a-string"
    return message["content"], None


def validate_structured_content(content):
    """Return (observed_keys, reason): reason is None only when content matches the requested shape."""
    try:
        parsed = json.loads(content)
    except ValueError:
        return None, "content-not-json"
    if not isinstance(parsed, dict):
        return None, "content-not-a-json-object"
    observed_keys = sorted(parsed.keys())
    if observed_keys != ["count", "status"]:
        return observed_keys, "content-keys-do-not-match-requested-shape"
    if parsed["status"] != REQUESTED_STATUS_VALUE or not isinstance(parsed["status"], str):
        return observed_keys, "status-value-does-not-match-requested-shape"
    count = parsed["count"]
    if isinstance(count, bool) or not isinstance(count, int) or count != REQUESTED_COUNT_VALUE:
        return observed_keys, "count-value-does-not-match-requested-shape"
    return observed_keys, None


def classify_unauthenticated(observation, label, failures):
    """Classify a request sent without a credential; only a 401/403 counts as authentication being enforced."""
    if observation["error"]:
        failures.append(f"unauthenticated-{label}-{observation['error']}")
        return "request-failed"
    if observation["status"] in (401, 403):
        return "authentication-required"
    if 200 <= observation["status"] < 300:
        failures.append(f"authentication-not-required-{label}")
        return "authentication-not-required"
    failures.append(f"unauthenticated-{label}-status-{observation['status']}")
    return "unexpected-status"


def run_probes(settings, requested_model):
    """Run the probes and return the evidence body (without identification fields)."""
    requests = []
    failures = []
    classifications = {}
    models = []
    structured_response = {"attempted": False, "passed": False, "reason": "not-attempted"}

    if classify_transport(settings.scheme) == "unknown-scheme":
        failures.append("unknown-scheme")
        return {
            "configured_scheme": "unknown",
            "requests": requests,
            "models": models,
            "classifications": {"transport": "unknown-scheme"},
            "structured_response": structured_response,
            "failures": failures,
        }

    # 1. Unauthenticated models request.
    observation, _ = send_request(settings, "GET", "/models", send_credential=False, timeout=MODELS_TIMEOUT_SECONDS)
    observation["name"] = "models-unauthenticated"
    requests.append(observation)
    classifications["authentication_models"] = classify_unauthenticated(observation, "models", failures)

    # 2. Authenticated models request.
    observation, body = send_request(settings, "GET", "/models", send_credential=True, timeout=MODELS_TIMEOUT_SECONDS)
    observation["name"] = "models-authenticated"
    requests.append(observation)
    if observation["error"]:
        classifications["models"] = "request-failed"
        failures.append(f"authenticated-models-{observation['error']}")
    elif observation["status"] != 200:
        classifications["models"] = "credential-rejected" if observation["status"] in (401, 403) else "unexpected-status"
        failures.append(f"authenticated-models-status-{observation['status']}")
    else:
        entries, reason = extract_model_entries(body)
        if entries is None:
            classifications["models"] = "not-openai-compatible"
            failures.append(f"models-{reason}")
        else:
            classifications["models"] = "openai-compatible"
            models = entries

    # 3. Structured chat request against a model the endpoint listed.
    listed_ids = [entry["id"] for entry in models]
    if not listed_ids:
        classifications["chat"] = "not-attempted"
        failures.append("no-endpoint-listed-model-for-chat")
    elif requested_model is not None and requested_model not in listed_ids:
        classifications["chat"] = "not-attempted"
        failures.append("requested-model-not-listed-by-endpoint")
    else:
        selected_model = requested_model if requested_model is not None else listed_ids[0]
        structured_response = {"attempted": True, "model": selected_model, "passed": False, "reason": None}
        chat_body = {
            "model": selected_model,
            "messages": [{"role": "user", "content": STRUCTURED_PROMPT}],
            "response_format": STRUCTURED_RESPONSE_FORMAT,
            "max_tokens": CHAT_MAX_TOKENS,
            "temperature": 0,
            "stream": False,
        }
        observation, body = send_request(
            settings, "POST", "/chat/completions", send_credential=True, json_body=chat_body, timeout=CHAT_TIMEOUT_SECONDS
        )
        observation["name"] = "chat-structured"
        requests.append(observation)
        if observation["error"]:
            classifications["chat"] = "request-failed"
            structured_response["reason"] = observation["error"]
            failures.append(f"chat-{observation['error']}")
        elif observation["status"] != 200:
            classifications["chat"] = "unexpected-status"
            structured_response["reason"] = f"status-{observation['status']}"
            failures.append(f"chat-status-{observation['status']}")
        else:
            content, reason = extract_chat_message_content(body)
            if content is None:
                classifications["chat"] = "not-openai-compatible"
                structured_response["reason"] = reason
                failures.append(f"chat-{reason}")
            else:
                classifications["chat"] = "openai-compatible"
                observed_keys, reason = validate_structured_content(content)
                if observed_keys is not None:
                    structured_response["observed_keys"] = observed_keys
                if reason is None:
                    structured_response["passed"] = True
                    structured_response["reason"] = "content-matches-requested-shape"
                else:
                    structured_response["reason"] = reason
                    failures.append(f"structured-response-{reason}")

    # 4. Unauthenticated chat request: whether the generation path enforces authentication.
    if structured_response["attempted"]:
        unauthenticated_chat_body = {
            "model": structured_response["model"],
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": False,
        }
        observation, _ = send_request(
            settings, "POST", "/chat/completions", send_credential=False,
            json_body=unauthenticated_chat_body, timeout=CHAT_TIMEOUT_SECONDS,
        )
        observation["name"] = "chat-unauthenticated"
        requests.append(observation)
        classifications["authentication_chat"] = classify_unauthenticated(observation, "chat", failures)

    tls_verified_values = {request["tls_verified"] for request in requests if request["status"] is not None}
    if settings.scheme == "https":
        classifications["transport"] = (
            "https-verified" if tls_verified_values == {True} else "https-not-verified"
        )
        if any(request["error"] == "tls-verification-failed" for request in requests):
            classifications["transport"] = "https-verification-failed"
    else:
        classifications["transport"] = "cleartext-http"

    return {
        "configured_scheme": settings.scheme,
        "requests": requests,
        "models": models,
        "classifications": classifications,
        "structured_response": structured_response,
        "failures": failures,
    }


def read_probe_revision():
    """Return (commit, dirty). Changes under the evidence directory are probe output, not probe code."""

    def run_git(arguments):
        return subprocess.run(
            ["git", *arguments], cwd=REPO_ROOT, capture_output=True, text=True, timeout=30, check=False
        )

    try:
        head = run_git(["rev-parse", "HEAD"])
        status = run_git(["status", "--porcelain", "--untracked-files=normal", "--", ".", f":(exclude){EVIDENCE_DIRECTORY_RELATIVE}"])
    except (OSError, subprocess.SubprocessError):
        return "unknown", True
    if head.returncode != 0 or status.returncode != 0:
        return "unknown", True
    return head.stdout.strip(), bool(status.stdout.strip())


def build_evidence(settings, requested_model, now):
    commit, dirty = read_probe_revision()
    evidence_id = f"spark-probe-{now.strftime('%Y%m%dT%H%M%SZ')}-{commit[:8]}{'-dirty' if dirty else ''}"
    body = run_probes(settings, requested_model)
    evidence = {
        "schema_version": SCHEMA_VERSION,
        "evidence_id": evidence_id,
        "probe_revision": {"commit": commit, "dirty": dirty},
        "collected_at": now.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "collection_context": "odonian-worker",
        **body,
        "result": "pass" if not body["failures"] else "fail",
        "unverified": ["phone-context reachability (owned by V08b)"],
    }
    return evidence


def assert_sanitized(serialized, settings):
    for sensitive_value in settings.sensitive_values():
        if sensitive_value in serialized:
            raise ConfigurationError("refusing to write evidence: it would contain the endpoint address or credential")


def write_evidence(evidence, settings, output_directory):
    serialized = json.dumps(evidence, indent=2, sort_keys=True) + "\n"
    assert_sanitized(serialized, settings)
    try:
        output_directory.mkdir(parents=True, exist_ok=True)
        artifact_path = output_directory / f"{evidence['evidence_id']}.json"
        with open(artifact_path, "x", encoding="utf-8") as artifact_file:
            artifact_file.write(serialized)
    except OSError as error:
        raise ConfigurationError(f"could not write the evidence artifact ({type(error).__name__})")
    return artifact_path


def parse_arguments(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--provider", required=True, help="provider key under 'providers' in the configuration file")
    parser.add_argument("--config", help=f"configuration path (default: ${CONFIG_PATH_ENV_VAR} or {DEFAULT_CONFIG_PATH})")
    parser.add_argument("--model", help="model id for the chat probe; must be listed by the endpoint (default: first listed)")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIRECTORY, help="directory for the evidence artifact")
    return parser.parse_args(argv)


def main(argv=None):
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    config_path = arguments.config or os.environ.get(CONFIG_PATH_ENV_VAR) or DEFAULT_CONFIG_PATH
    try:
        settings = load_provider_settings(config_path, arguments.provider)
        evidence = build_evidence(settings, arguments.model, datetime.now(timezone.utc))
        artifact_path = write_evidence(evidence, settings, arguments.output_dir)
    except ConfigurationError as error:
        print(f"error: {error}", file=sys.stderr)
        return EXIT_CONFIGURATION_ERROR

    print(f"evidence: {evidence['evidence_id']}")
    print(f"artifact: {artifact_path.name}")
    for name, value in sorted(evidence["classifications"].items()):
        print(f"{name}: {value}")
    print(f"result: {evidence['result']}")
    for failure in evidence["failures"]:
        print(f"failure: {failure}", file=sys.stderr)
    return EXIT_PASSED if evidence["result"] == "pass" else EXIT_PROBE_FAILED


if __name__ == "__main__":
    sys.exit(main())
