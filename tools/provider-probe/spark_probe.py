#!/usr/bin/env python3
"""
Spark protocol probe: test endpoint reachability and protocol compliance from the worker.

This probe reads the provider configuration from /etc/ohand-provider/models.json
(or OHAND_PROVIDER_CONFIG_PATH), sends synthetic requests to the configured Spark
endpoint, and records observations without leaking credentials or endpoint addresses.

Exits non-zero if any probe fails. Records structured evidence as a JSON artifact.
"""

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse
import urllib.request
import urllib.error
import ssl


# Allowlisted headers to record in evidence
ALLOWLISTED_HEADERS = {'server', 'content-type', 'date'}


class ProbeError(Exception):
    """Raised when a probe encounters a configured failure condition."""
    pass


def read_provider_config():
    """Read and parse provider configuration from the mounted file or env var.

    Returns (base_url, credential, raw_config)
    Raises ProbeError if config is missing or invalid.
    """
    config_path = os.environ.get('OHAND_PROVIDER_CONFIG_PATH', '/etc/ohand-provider/models.json')

    if not Path(config_path).exists():
        raise ProbeError('Provider config file not found')

    try:
        with open(config_path, 'r') as f:
            config = json.load(f)
    except (json.JSONDecodeError, IOError) as e:
        raise ProbeError('Failed to read/parse provider config')

    # Expect config with 'spark' provider containing base_url and credential
    if 'spark' not in config:
        raise ProbeError('Provider config missing spark key')

    spark_config = config['spark']

    # Validate required fields
    base_url = spark_config.get('base_url', '').strip()
    credential = spark_config.get('credential', '').strip()

    if not base_url:
        raise ProbeError('Spark config missing or empty base_url')
    if not credential:
        raise ProbeError('Spark config missing or empty credential')

    # Validate URL scheme
    parsed = urlparse(base_url)
    if parsed.scheme not in ('http', 'https'):
        raise ProbeError('Spark base_url has unknown scheme')

    return base_url, credential, config


def send_request(base_url, endpoint, method='GET', credential=None, body=None):
    """Send an HTTP request and return (status_code, headers, body_text, tls_verified).

    Returns (None, {}, '', False) if the request fails entirely.
    tls_verified is True only if HTTPS and certificate verified successfully.
    """
    url = base_url.rstrip('/') + endpoint

    req = urllib.request.Request(url, method=method)

    if credential:
        req.add_header('Authorization', f'Bearer {credential}')

    if body:
        req.add_header('Content-Type', 'application/json')
        req.data = body.encode('utf-8')

    tls_verified = False

    try:
        # Use default context with certificate verification for HTTPS
        if url.startswith('https'):
            context = ssl.create_default_context()
            # verify_mode is CERT_REQUIRED by default
        else:
            context = None

        response = urllib.request.urlopen(req, context=context)
        status = response.status
        headers = dict(response.headers)
        response_body = response.read().decode('utf-8', errors='replace')

        # TLS was verified if HTTPS and no exception raised
        tls_verified = url.startswith('https')
        return status, headers, response_body, tls_verified

    except urllib.error.HTTPError as e:
        status = e.code
        headers = dict(e.headers) if hasattr(e, 'headers') else {}
        try:
            response_body = e.read().decode('utf-8', errors='replace')
        except:
            response_body = ''
        # For HTTP errors, TLS verification succeeded if HTTPS (certificate error would raise SSLError)
        tls_verified = url.startswith('https')
        return status, headers, response_body, tls_verified

    except ssl.SSLError:
        # Certificate verification failed
        return None, {}, '', False

    except Exception as e:
        # Request failed entirely (connection refused, DNS failure, etc.)
        return None, {}, '', False


def filter_headers(headers):
    """Return only allowlisted headers, with values preserved."""
    return {k.lower(): v for k, v in headers.items() if k.lower() in ALLOWLISTED_HEADERS}


def test_auth_required(base_url):
    """Test whether authentication is required by sending an unauthenticated request.

    Returns:
        - 'authentication_required' if 401/403
        - 'no_authentication_required' if 200 and response parses as valid models list
        - None if request failed (unauthenticated request failure is a probe error)
    """
    status, headers, body, tls_verified = send_request(base_url, '/models')

    if status is None:
        return None

    if status in (401, 403):
        return 'authentication_required'

    if status == 200:
        # Check if it's valid JSON with data field
        try:
            models = json.loads(body)
            if isinstance(models, dict) and 'data' in models:
                if isinstance(models['data'], list):
                    return 'no_authentication_required'
        except:
            pass

    # Any other status or malformed response is a failure
    return None


def test_authentication(base_url, credential):
    """Test authentication with the provided credential.

    Returns (success: bool, models: list, status_code: int, headers_dict: dict, tls_verified: bool, error: str)
    """
    status, headers, body, tls_verified = send_request(base_url, '/models', credential=credential)

    if status is None:
        return False, [], None, {}, False, 'Request failed'

    if status != 200:
        return False, [], status, {}, tls_verified, None

    try:
        response = json.loads(body)
        if isinstance(response, dict) and 'data' in response and isinstance(response['data'], list):
            models = []
            for m in response['data']:
                if isinstance(m, dict) and 'id' in m:
                    models.append(m['id'])
            if models:
                filtered_headers = filter_headers(headers)
                return True, models, status, filtered_headers, tls_verified, None

        return False, [], status, {}, tls_verified, 'Invalid response structure'
    except json.JSONDecodeError:
        return False, [], status, {}, tls_verified, 'Response not valid JSON'


def test_structured_response(base_url, credential, model_name):
    """Test OpenAI /chat/completions endpoint with structured response validation.

    Returns (success: bool, status_code: int, headers_dict: dict, tls_verified: bool, content: str, error: str)
    """
    body = json.dumps({
        'model': model_name,
        'messages': [{'role': 'user', 'content': 'Hello'}],
    })

    status, headers, response_body, tls_verified = send_request(
        base_url, '/chat/completions', method='POST',
        credential=credential, body=body
    )

    if status is None:
        return False, None, {}, False, '', 'Request failed'

    if status != 200:
        return False, status, {}, tls_verified, '', None

    try:
        response = json.loads(response_body)

        # Validate structure: must have choices array with message objects
        if not isinstance(response, dict):
            return False, status, {}, tls_verified, '', 'Response not a JSON object'

        if 'choices' not in response or not isinstance(response['choices'], list):
            return False, status, {}, tls_verified, '', 'Response missing choices array'

        if not response['choices']:
            return False, status, {}, tls_verified, '', 'Choices array is empty'

        choice = response['choices'][0]
        if not isinstance(choice, dict) or 'message' not in choice:
            return False, status, {}, tls_verified, '', 'Choice missing message object'

        message = choice['message']
        if not isinstance(message, dict):
            return False, status, {}, tls_verified, '', 'Message is not a JSON object'

        # Message must have role and content fields
        if 'role' not in message or 'content' not in message:
            return False, status, {}, tls_verified, '', 'Message missing role or content'

        content = message['content']
        if not isinstance(content, str):
            return False, status, {}, tls_verified, '', 'Message content is not a string'

        # Validate content is not empty
        if not content.strip():
            return False, status, {}, tls_verified, '', 'Message content is empty'

        filtered_headers = filter_headers(headers)
        return True, status, filtered_headers, tls_verified, content, None

    except json.JSONDecodeError:
        return False, status, {}, tls_verified, '', 'Response not valid JSON'


def get_probe_revision():
    """Get the current git commit hash and dirty flag.

    Returns "commit[+dirty]"
    """
    try:
        import subprocess
        commit = subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'],
            stderr=subprocess.DEVNULL,
            cwd=Path(__file__).parent.parent.parent
        ).decode().strip()

        # Check for dirty tree
        status = subprocess.check_output(
            ['git', 'status', '--porcelain'],
            stderr=subprocess.DEVNULL,
            cwd=Path(__file__).parent.parent.parent
        ).decode().strip()

        dirty = '+dirty' if status else ''
        return f'{commit}{dirty}'
    except:
        return 'unknown'


def main():
    try:
        # Read configuration
        base_url, credential, config = read_provider_config()
    except ProbeError as e:
        sys.exit(1)

    # Parse URL to extract scheme
    parsed_url = urlparse(base_url)
    scheme = parsed_url.scheme

    # Collect probe observations
    collection_time = datetime.now(timezone.utc).isoformat()
    evidence_id = f'spark-{collection_time.replace(":", "-").replace("+", "z")}'

    observations = {
        'evidence_id': evidence_id,
        'probe_revision': get_probe_revision(),
        'collection_time': collection_time,
        'scheme': scheme,
        'probes': {}
    }

    # Probe 1: Check if authentication is required (unauthenticated request)
    auth_requirement = test_auth_required(base_url)

    if auth_requirement is None:
        sys.exit(1)

    observations['probes']['unauthenticated_models'] = {
        'method': 'GET',
        'path': '/models',
        'credential_sent': False,
        'authentication_requirement': auth_requirement,
    }

    # Probe 2: Test authentication and get models list
    auth_success, models, auth_status, auth_headers, tls_verified, auth_error = test_authentication(base_url, credential)

    if not auth_success:
        sys.exit(1)

    observations['authentication_status'] = 200
    observations['authentication_response_headers'] = auth_headers
    observations['tls_verified'] = tls_verified
    observations['models'] = models

    observations['probes']['authenticated_models'] = {
        'method': 'GET',
        'path': '/models',
        'credential_sent': True,
        'status': auth_status,
        'headers': auth_headers,
        'models': models,
    }

    # Select first model from endpoint for structured response test
    if not models:
        sys.exit(1)

    selected_model = models[0]

    # Probe 3: Test structured response on /chat/completions
    resp_success, resp_status, resp_headers, resp_tls_verified, resp_content, resp_error = test_structured_response(
        base_url, credential, selected_model
    )

    if not resp_success:
        sys.exit(1)

    observations['structured_response_status'] = resp_status
    observations['structured_response_headers'] = resp_headers
    observations['structured_response_content'] = resp_content

    observations['probes']['structured_response'] = {
        'method': 'POST',
        'path': '/chat/completions',
        'credential_sent': True,
        'status': resp_status,
        'headers': resp_headers,
        'model_requested': selected_model,
        'content_valid': True,
    }

    # Write evidence artifact
    evidence_dir = Path(__file__).parent.parent.parent / 'docs' / 'validation' / 'evidence' / 'spark-probe'
    evidence_dir.mkdir(parents=True, exist_ok=True)

    evidence_file = evidence_dir / f'{evidence_id}.json'
    try:
        with open(evidence_file, 'w') as f:
            json.dump(observations, f, indent=2)
    except Exception:
        sys.exit(1)

    sys.exit(0)


if __name__ == '__main__':
    main()
