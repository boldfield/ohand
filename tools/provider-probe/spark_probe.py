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
from datetime import datetime
from dataclasses import dataclass, asdict
from typing import Optional, Dict, Any
import hashlib
import urllib.request
import urllib.error
import ssl


@dataclass
class ProbeResult:
    """Sanitized result of a single probe request."""
    timestamp: str
    request_type: str
    endpoint_hash: str
    success: bool
    status_code: Optional[int]
    protocol: str
    auth_required: bool
    auth_success: Optional[bool]
    response_structure: Optional[str]
    model_count: Optional[int]
    error_message: Optional[str]


class SparkProbe:
    """Probe Spark endpoint compatibility and reachability."""

    def __init__(self, config_path: str = "/etc/ohand-provider/models.json"):
        """Initialize probe with provider configuration."""
        self.config = self._load_config(config_path)
        self.provider_config = self.config.get("providers", {}).get("ollama", {})
        self.base_url = self.provider_config.get("baseUrl", "")
        self.api_key = self.provider_config.get("apiKey", "")
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

    def _hash_endpoint(self) -> str:
        """Create a sanitized hash of the endpoint for audit."""
        return hashlib.sha256(self.base_url.encode()).hexdigest()[:16]

    def _make_request(self, endpoint: str, method: str = "POST",
                     data: Optional[str] = None, headers: Optional[Dict] = None) -> tuple[int, str]:
        """Make HTTP request to endpoint."""
        if not headers:
            headers = {}

        headers["User-Agent"] = "spark-probe/1.0"
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"

        try:
            req = urllib.request.Request(
                endpoint,
                data=data.encode() if data else None,
                headers=headers,
                method=method
            )

            # Create SSL context (allows self-signed certs for testing)
            ctx = ssl.create_default_context()

            try:
                with urllib.request.urlopen(req, context=ctx, timeout=10) as resp:
                    return resp.status, resp.read().decode('utf-8')
            except urllib.error.HTTPError as e:
                return e.code, e.read().decode('utf-8')
        except Exception as e:
            raise Exception(f"Request failed: {e}")

    def probe_reachability(self) -> bool:
        """Test basic endpoint reachability."""
        try:
            status, _ = self._make_request(
                f"{self.base_url}/models",
                method="GET",
                headers={"Accept": "application/json"}
            )
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="reachability",
                endpoint_hash=self._hash_endpoint(),
                success=status == 200,
                status_code=status,
                protocol="https",
                auth_required=False,
                auth_success=None,
                response_structure=None,
                model_count=None,
                error_message=None
            )
            self.results.append(result)
            return result.success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="reachability",
                endpoint_hash=self._hash_endpoint(),
                success=False,
                status_code=None,
                protocol="https",
                auth_required=False,
                auth_success=None,
                response_structure=None,
                model_count=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def probe_models_list(self) -> bool:
        """Test models list endpoint with authentication."""
        try:
            status, resp_body = self._make_request(
                f"{self.base_url}/models",
                method="GET",
                headers={"Accept": "application/json"}
            )

            success = status == 200
            model_count = None
            resp_structure = None
            auth_success = None

            if success:
                resp_json = json.loads(resp_body)
                if "data" in resp_json:
                    model_count = len(resp_json.get("data", []))
                    resp_structure = "openai-list"
                else:
                    resp_structure = "unknown"
            else:
                auth_success = status != 401 and status != 403

            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="models_list",
                endpoint_hash=self._hash_endpoint(),
                success=success,
                status_code=status,
                protocol="https",
                auth_required=True,
                auth_success=auth_success,
                response_structure=resp_structure,
                model_count=model_count,
                error_message=None if success else f"HTTP {status}"
            )
            self.results.append(result)
            return success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="models_list",
                endpoint_hash=self._hash_endpoint(),
                success=False,
                status_code=None,
                protocol="https",
                auth_required=True,
                auth_success=None,
                response_structure=None,
                model_count=None,
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

            status, resp_body = self._make_request(
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
                endpoint_hash=self._hash_endpoint(),
                success=success,
                status_code=status,
                protocol="https",
                auth_required=True,
                auth_success=status != 401 and status != 403,
                response_structure=resp_structure,
                model_count=None,
                error_message=None if success else f"HTTP {status}"
            )
            self.results.append(result)
            return success
        except Exception as e:
            result = ProbeResult(
                timestamp=datetime.utcnow().isoformat() + "Z",
                request_type="chat_completion",
                endpoint_hash=self._hash_endpoint(),
                success=False,
                status_code=None,
                protocol="https",
                auth_required=True,
                auth_success=None,
                response_structure=None,
                model_count=None,
                error_message=str(e)
            )
            self.results.append(result)
            return False

    def get_sanitized_results(self) -> Dict[str, Any]:
        """Return sanitized results for documentation."""
        return {
            "collection_time": datetime.utcnow().isoformat() + "Z",
            "endpoint_identifier": self._hash_endpoint(),
            "protocol": "https",
            "api_type": "openai-compatible",
            "probes": [asdict(r) for r in self.results],
            "note": "Endpoint address and credentials are omitted from this output."
        }

    def save_raw_evidence(self, path: str) -> None:
        """Save raw evidence locally for audit (includes credentials/addresses)."""
        evidence = {
            "timestamp": datetime.utcnow().isoformat() + "Z",
            "endpoint": self.base_url,
            "api_type": self.provider_config.get("api"),
            "models_count": len(self.provider_config.get("models", [])),
            "model_ids": [m.get("id") for m in self.provider_config.get("models", [])],
            "probes": [asdict(r) for r in self.results],
            "note": "Raw evidence containing private endpoint and configuration details"
        }

        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, 'w') as f:
            json.dump(evidence, f, indent=2)

        # Set restrictive permissions
        os.chmod(path, 0o600)


def main():
    """Run the Spark endpoint probe."""
    probe = SparkProbe()

    print("Starting Spark endpoint probe...")
    print(f"Testing endpoint (sanitized as: {probe._hash_endpoint()})")

    # Run probes
    print("- Testing reachability...", end=" ")
    if probe.probe_reachability():
        print("OK")
    else:
        print("FAILED")
        print("Error: Could not reach endpoint")
        sys.exit(1)

    print("- Testing models list...", end=" ")
    if probe.probe_models_list():
        print("OK")
    else:
        print("FAILED")

    print("- Testing chat completion...", end=" ")
    if probe.probe_chat_completion():
        print("OK")
    else:
        print("FAILED")

    # Output sanitized results
    sanitized = probe.get_sanitized_results()
    print("\nSanitized results:")
    print(json.dumps(sanitized, indent=2))

    # Save raw evidence
    evidence_path = "/tmp/spark-probe-evidence.json"
    probe.save_raw_evidence(evidence_path)
    print(f"\nRaw evidence saved to: {evidence_path}")
    print(f"Evidence reference: spark-probe-{datetime.utcnow().strftime('%Y%m%d-%H%M%S')}")


if __name__ == "__main__":
    main()
