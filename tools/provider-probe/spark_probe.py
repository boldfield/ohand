#!/usr/bin/env python3
"""
Spark provider endpoint probe.

Performs authorized synthetic requests to the configured Spark endpoint
to verify reachability, authentication, TLS and protocol compatibility.

Credentials and endpoint addresses are loaded from environment/config and
not included in output documentation. Raw evidence is preserved locally
with sanitized references for audit.
"""

import json
import sys
import os
import uuid
from datetime import datetime
from dataclasses import dataclass, asdict
from typing import Optional, Dict, Any
import urllib.request
import urllib.error
import ssl
from urllib.parse import urlparse


@dataclass
class ProbeResult:
    """Sanitized result of a single probe request."""
    timestamp: str
    request_type: str
    evidence_id: str
    success: bool
    status_code: Optional[int]
    protocol: Optional[str]
    auth_required: bool
    auth_success: Optional[bool]
    response_structure: Optional[str]
    model_count: Optional[int]
    serving_software: Optional[str]
    error_message: Optional[str]


class SparkProbe:
    """Probe Spark endpoint compatibility and reachability."""

    def __init__(self, config_path: Optional[str] = None, provider_key: Optional[str] = None):
        """Initialize probe with provider configuration."""
        if config_path is None:
            config_path = os.environ.get("OHAND_PROVIDER_CONFIG_PATH", "/etc/ohand-provider/models.json")

        self.evidence_id = f"spark-probe-{uuid.uuid4().hex[:12]}"
        self.config = self._load_config(config_path)

        if provider_key is None:
            provider_key = os.environ.get("OHAND_PROVIDER_KEY", "ollama")

        self.provider_config = self.config.get("providers", {}).get(provider_key, {})
        self.base_url = self.provider_config.get("baseUrl", "").strip()
        self.api_key = self.provider_config.get("apiKey", "").strip()

        if not self.base_url:
            print(f"Error: baseUrl not configured for provider '{provider_key}'", file=sys.stderr)
            sys.exit(1)
        if not self.api_key:
            print(f"Error: apiKey not configured for provider '{provider_key}'", file=sys.stderr)
            sys.exit(1)

        self.results: list[ProbeResult] = []

    def _load_config(self, path: str) -> Dict[str, Any]:
        """Load provider configuration from JSON file."""
        try:
            with open(path, 'r') as f:
                return json.load(f)
        except FileNotFoundError:
            print(f"Error: Config file not found at {path}", file=sys.stderr)
            sys.exit(1)
        except json.JSONDecodeError as e:
            print(f"Error: Failed to parse config: {e}", file=sys.stderr)
            sys.exit(1)

    def _get_protocol(self) -> str:
        """Extract protocol from base URL."""
        parsed = urlparse(self.base_url)
        return parsed.scheme if parsed.scheme in ("http", "https") else "https"

    def _make_request(self, endpoint: str, method: str = "POST",
                     data: Optional[str] = None, headers: Optional[Dict] = None,
                     include_auth: bool = True) -> tuple[int, str, Dict[str, str]]:
        """Make HTTP request to endpoint. Returns (status_code, body, headers)."""
        if not headers:
            headers = {}

        headers["User-Agent"] = "spark-probe/1.0"
        if include_auth and self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"

        try:
            req = urllib.request.Request(
                endpoint,
                data=data.encode() if data else None,
                headers=headers,
                method=method
            )

            ctx = ssl.create_default_context()

            try:
                with urllib.request.urlopen(req, context=ctx, timeout=10) as resp:
                    resp_headers = dict(resp.headers)
                    return resp.status, resp.read().decode('utf-8'), resp_headers
            except urllib.error.HTTPError as e:
                resp_headers = dict(e.headers) if hasattr(e, 'headers') else {}
                return e.code, e.read().decode('utf-8'), resp_headers
        except Exception as e:
            raise Exception(f"Request failed: {e}")

    def probe_reachability_unauthenticated(self) -> bool:
        """Test endpoint reachability without authentication to verify auth is required."""
        try:
            status, _, resp_headers = self._make_request(
                f"{self.base_url}/models",
                method="GET",
                headers={"Accept": "application/json"},
                include_auth=False
            )
            auth_required = status in (401, 403)
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="reachability_unauth",
                evidence_id=self.evidence_id,
                success=True,
                status_code=status,
                protocol=self._get_protocol(),
                auth_required=auth_required,
                auth_success=None,
                response_structure=None,
                model_count=None,
                serving_software=resp_headers.get("Server"),
                error_message=None
            )
            self.results.append(result)
            return True
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="reachability_unauth",
                evidence_id=self.evidence_id,
                success=False,
                status_code=None,
                protocol=self._get_protocol(),
                auth_required=None,
                auth_success=None,
                response_structure=None,
                model_count=None,
                serving_software=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def probe_models_list(self) -> bool:
        """Test models list endpoint with authentication."""
        try:
            status, resp_body, resp_headers = self._make_request(
                f"{self.base_url}/models",
                method="GET",
                headers={"Accept": "application/json"}
            )

            success = status == 200
            model_count = None
            resp_structure = None
            auth_success = status != 401 and status != 403

            if success:
                resp_json = json.loads(resp_body)
                if "data" in resp_json:
                    model_count = len(resp_json.get("data", []))
                    resp_structure = "openai-list"
                else:
                    resp_structure = "unknown"

            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="models_list",
                evidence_id=self.evidence_id,
                success=success,
                status_code=status,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=auth_success,
                response_structure=resp_structure,
                model_count=model_count,
                serving_software=resp_headers.get("Server"),
                error_message=None if success else f"HTTP {status}"
            )
            self.results.append(result)
            return success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="models_list",
                evidence_id=self.evidence_id,
                success=False,
                status_code=None,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=None,
                response_structure=None,
                model_count=None,
                serving_software=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def probe_chat_completion(self, model: str = "gpt-oss:20b") -> bool:
        """Test chat completion endpoint with a simple request."""
        try:
            request_body = {
                "model": model,
                "messages": [
                    {"role": "user", "content": "hello"}
                ],
                "max_tokens": 10,
                "stream": False
            }

            status, resp_body, resp_headers = self._make_request(
                f"{self.base_url}/chat/completions",
                method="POST",
                data=json.dumps(request_body),
                headers={"Content-Type": "application/json", "Accept": "application/json"}
            )

            success = status == 200
            resp_structure = None

            if success:
                resp_json = json.loads(resp_body)
                if "choices" in resp_json:
                    resp_structure = "openai-chat"

            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="chat_completion",
                evidence_id=self.evidence_id,
                success=success,
                status_code=status,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=status != 401 and status != 403,
                response_structure=resp_structure,
                model_count=None,
                serving_software=resp_headers.get("Server"),
                error_message=None if success else f"HTTP {status}"
            )
            self.results.append(result)
            return success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="chat_completion",
                evidence_id=self.evidence_id,
                success=False,
                status_code=None,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=None,
                response_structure=None,
                model_count=None,
                serving_software=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def probe_response_format(self, model: str = "gpt-oss:20b") -> bool:
        """Test structured response format (JSON mode) support."""
        try:
            request_body = {
                "model": model,
                "messages": [
                    {"role": "user", "content": "respond with json"}
                ],
                "response_format": {"type": "json_object"},
                "max_tokens": 20,
                "stream": False
            }

            status, resp_body, resp_headers = self._make_request(
                f"{self.base_url}/chat/completions",
                method="POST",
                data=json.dumps(request_body),
                headers={"Content-Type": "application/json", "Accept": "application/json"}
            )

            success = status == 200
            resp_structure = None

            if success:
                resp_json = json.loads(resp_body)
                if "choices" in resp_json:
                    resp_structure = "openai-chat-json"

            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="response_format",
                evidence_id=self.evidence_id,
                success=success,
                status_code=status,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=status != 401 and status != 403,
                response_structure=resp_structure,
                model_count=None,
                serving_software=resp_headers.get("Server"),
                error_message=None if success else f"HTTP {status}"
            )
            self.results.append(result)
            return success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="response_format",
                evidence_id=self.evidence_id,
                success=False,
                status_code=None,
                protocol=self._get_protocol(),
                auth_required=True,
                auth_success=None,
                response_structure=None,
                model_count=None,
                serving_software=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def get_sanitized_results(self) -> Dict[str, Any]:
        """Return sanitized results for documentation."""
        return {
            "collection_time": datetime.utcnow().isoformat() + "Z",
            "evidence_id": self.evidence_id,
            "protocol": self._get_protocol(),
            "api_type": "openai-compatible",
            "probes": [asdict(r) for r in self.results],
            "note": "Endpoint address and credentials are omitted from this output."
        }

    def save_raw_evidence(self, output_dir: str) -> str:
        """Save raw evidence locally for audit (includes credentials/addresses).

        Returns the path where evidence was saved.
        """
        evidence = {
            "evidence_id": self.evidence_id,
            "probe_revision": self._get_probe_revision(),
            "timestamp": datetime.utcnow().isoformat() + "Z",
            "endpoint": self.base_url,
            "api_type": self.provider_config.get("api"),
            "models_count": len(self.provider_config.get("models", [])),
            "model_ids": [m.get("id") for m in self.provider_config.get("models", [])],
            "probes": [asdict(r) for r in self.results],
            "note": "Raw evidence containing private endpoint and configuration details"
        }

        os.makedirs(output_dir, exist_ok=True)
        evidence_path = os.path.join(output_dir, f"{self.evidence_id}.json")

        with open(evidence_path, 'w') as f:
            json.dump(evidence, f, indent=2)

        os.chmod(evidence_path, 0o600)
        return evidence_path

    def _get_probe_revision(self) -> str:
        """Get probe script revision/build identifier."""
        try:
            result = os.popen("git rev-parse HEAD 2>/dev/null").read().strip()
            if result:
                return result[:12]
        except:
            pass
        return "unknown"


def main():
    """Run the Spark endpoint probe."""
    probe = SparkProbe()

    print("Starting Spark endpoint probe...")
    print(f"Evidence ID: {probe.evidence_id}")

    # Run probes
    all_passed = True

    print("- Testing reachability (unauthenticated)...", end=" ", flush=True)
    if probe.probe_reachability_unauthenticated():
        print("OK")
    else:
        print("FAILED")
        all_passed = False

    print("- Testing models list (authenticated)...", end=" ", flush=True)
    if probe.probe_models_list():
        print("OK")
    else:
        print("FAILED")
        all_passed = False

    print("- Testing chat completion...", end=" ", flush=True)
    if probe.probe_chat_completion():
        print("OK")
    else:
        print("FAILED")
        all_passed = False

    print("- Testing response format (structured)...", end=" ", flush=True)
    if probe.probe_response_format():
        print("OK")
    else:
        print("FAILED (optional)")

    # Output sanitized results
    sanitized = probe.get_sanitized_results()
    print("\nSanitized results:")
    print(json.dumps(sanitized, indent=2))

    # Save raw evidence to XDG location
    evidence_dir = os.path.expanduser("~/.local/share/ohand-provider-probe")
    evidence_path = probe.save_raw_evidence(evidence_dir)
    print(f"\nRaw evidence saved to: {evidence_path}")
    print(f"Evidence reference: {probe.evidence_id}")

    if not all_passed:
        sys.exit(1)


if __name__ == "__main__":
    main()
