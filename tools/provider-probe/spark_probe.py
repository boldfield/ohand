#!/usr/bin/env python3
"""
Spark protocol probe: test endpoint reachability and protocol compliance from the worker.

This probe reads the provider configuration from /etc/ohand-provider/models.json
(or OHAND_PROVIDER_CONFIG_PATH), sends synthetic requests to the configured Spark
endpoint, and records observations without leaking credentials or endpoint addresses.

Exits non-zero if any probe fails. Records structured evidence as a JSON artifact.
"""

import argparse
import json
import os
import sys
import socket
import ssl
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse
import urllib.request
import urllib.error


# Allowlisted headers to record in evidence
ALLOWLISTED_HEADERS = {'server', 'content-type', 'date'}


class ProbeError(Exception):
    """Raised when a probe encounters a configured failure condition."""
    pass


def read_provider_config(provider_key):
    """Read and parse provider configuration from the mounted file or env var.

    Args:
        provider_key: The provider key to look up (e.g., 'ollama')

    Returns (base_url, credential, models_list)
    Raises ProbeError if config is missing or invalid.
    """
    config_path = os.environ.get('OHAND_PROVIDER_CONFIG_PATH', '/etc/ohand-provider/models.json')

    if not Path(config_path).exists():
        raise ProbeError(f'Provider config file not found at {config_path}')

    try:
        with open(config_path, 'r') as f:
            config = json.load(f)
    except (json.JSONDecodeError, IOError) as e:
        raise ProbeError(f'Failed to read/parse provider config: {e}')

    # Validate top-level structure
    if 'providers' not in config:
        raise ProbeError('Provider config missing "providers" key')

    if not isinstance(config['providers'], dict):
        raise ProbeError('Provider config "providers" is not a dict')

    # Look up the specific provider
    if provider_key not in config['providers']:
        raise ProbeError(f'Provider "{provider_key}" not found in config')

    provider_config = config['providers'][provider_key]

    # Validate required fields
    if not isinstance(provider_config, dict):
        raise ProbeError(f'Provider "{provider_key}" config is not a dict')

    base_url = provider_config.get('baseUrl', '').strip()
    credential = provider_config.get('apiKey', '').strip()
    models = provider_config.get('models', [])

    if not base_url:
        raise ProbeError(f'Provider "{provider_key}" config missing or empty baseUrl')
    if not credential:
        raise ProbeError(f'Provider "{provider_key}" config missing or empty apiKey')
    if not isinstance(models, list):
        raise ProbeError(f'Provider "{provider_key}" config "models" is not a list')
    if not models:
        raise ProbeError(f'Provider "{provider_key}" config "models" list is empty')

    # Validate URL scheme
    parsed = urlparse(base_url)
    if parsed.scheme not in ('http', 'https'):
        raise ProbeError(f'Provider "{provider_key}" baseUrl has unknown scheme: {parsed.scheme}')

    return base_url, credential, models


def send_request(base_url, endpoint, method='GET', credential=None, body=None, timeout=10):
    """Send an HTTP request and return (status_code, headers, body_text, tls_verified, error_detail).

    Returns (None, {}, '', False, error_detail) if the request fails entirely.
    tls_verified is True only if HTTPS and certificate verified successfully.
    error_detail is None on success, or a classification string on failure.
    """
    url = base_url.rstrip('/') + endpoint

    req = urllib.request.Request(url, method=method)

    if credential:
        req.add_header('Authorization', f'Bearer {credential}')

    if body:
        req.add_header('Content-Type', 'application/json')
        req.data = body.encode('utf-8')

    tls_verified = False
    scheme = urlparse(url).scheme

    try:
        # Use default context with certificate verification for HTTPS
        if url.startswith('https'):
            context = ssl.create_default_context()
            # verify_mode is CERT_REQUIRED by default
        else:
            context = None

        response = urllib.request.urlopen(req, context=context, timeout=timeout)
        status = response.status
        headers = dict(response.headers)
        response_body = response.read().decode('utf-8', errors='replace')

        # TLS was verified if HTTPS and no exception raised
        tls_verified = url.startswith('https')
        return status, headers, response_body, tls_verified, None

    except urllib.error.HTTPError as e:
        status = e.code
        headers = dict(e.headers) if hasattr(e, 'headers') else {}
        try:
            response_body = e.read().decode('utf-8', errors='replace')
        except:
            response_body = ''
        # For HTTP errors, TLS verification succeeded if HTTPS (certificate error would raise SSLError)
        tls_verified = url.startswith('https')
        return status, headers, response_body, tls_verified, None

    except ssl.SSLError as e:
        # Certificate verification failed
        return None, {}, '', False, 'tls-verification-failed'

    except socket.timeout:
        return None, {}, '', False, 'request-timeout'

    except Exception as e:
        # Request failed entirely (connection refused, DNS failure, etc.)
        return None, {}, '', False, 'request-failed'


def filter_headers(headers):
    """Return only allowlisted headers, with values preserved."""
    return {k.lower(): v for k, v in headers.items() if k.lower() in ALLOWLISTED_HEADERS}


def test_auth_required(base_url):
    """Test whether authentication is required by sending an unauthenticated request.

    Returns (auth_requirement, status, headers, tls_verified, error):
        - auth_requirement: 'authentication_required' if 401/403, 'no_authentication_required' if 200
        - status: HTTP status code (None if request failed)
        - headers: Response headers dict
        - tls_verified: Whether TLS was verified (HTTPS only)
        - error: Error detail if request failed, None otherwise
    """
    status, headers, body, tls_verified, error = send_request(base_url, '/models')

    if error:
        # Request completely failed
        return None, status, headers, tls_verified, error

    if status in (401, 403):
        return 'authentication_required', status, headers, tls_verified, None

    if status == 200:
        # For OpenAI-compatible endpoints, check if it's valid JSON with data field
        try:
            response = json.loads(body)
            if isinstance(response, dict) and 'data' in response:
                if isinstance(response['data'], list):
                    return 'no_authentication_required', status, headers, tls_verified, None
        except:
            pass
        # 200 but response not recognized
        return None, status, headers, tls_verified, 'unrecognized-response-format'

    # Any other status is unexpected
    return None, status, headers, tls_verified, f'unexpected-status-{status}'


def test_authentication(base_url, credential):
    """Test authentication with the provided credential.

    Returns (success: bool, models: list, status_code: int, headers_dict: dict, tls_verified: bool, error: str)
    """
    status, headers, body, tls_verified, error = send_request(base_url, '/models', credential=credential)

    if error:
        return False, [], status, {}, tls_verified, error

    if status != 200:
        return False, [], status, filter_headers(headers), tls_verified, None

    try:
        response = json.loads(body)
        if isinstance(response, dict) and 'data' in response and isinstance(response['data'], list):
            models = []
            for m in response['data']:
                if isinstance(m, dict) and 'id' in m:
                    models.append(m['id'])
                elif isinstance(m, dict):
                    # Model entry without 'id' field
                    pass
            # Accept response even if models list is empty, as long as structure is valid
            filtered_headers = filter_headers(headers)
            return True, models, status, filtered_headers, tls_verified, None

        return False, [], status, {}, tls_verified, 'invalid-response-structure'
    except json.JSONDecodeError:
        return False, [], status, {}, tls_verified, 'response-not-json'


def test_structured_response(base_url, credential, model_name, response_format='json'):
    """Test OpenAI /chat/completions endpoint with structured response validation.

    Sends a chat request with a requested JSON response format and validates that
    the returned content parses as JSON matching the requested shape.

    Returns (success: bool, status_code: int, headers_dict: dict, tls_verified: bool,
             content: str, content_valid: bool, error: str)
    """
    body = json.dumps({
        'model': model_name,
        'messages': [{'role': 'user', 'content': 'Respond with valid JSON only: {"status": "ok"}'}],
        'response_format': {'type': response_format},
    })

    status, headers, response_body, tls_verified, error = send_request(
        base_url, '/chat/completions', method='POST',
        credential=credential, body=body, timeout=30
    )

    if error:
        return False, status, {}, tls_verified, '', False, error

    if status != 200:
        return False, status, filter_headers(headers), tls_verified, '', False, None

    try:
        response = json.loads(response_body)

        # Validate structure: must have choices array with message objects
        if not isinstance(response, dict):
            return False, status, {}, tls_verified, '', False, 'response-not-object'

        if 'choices' not in response or not isinstance(response['choices'], list):
            return False, status, {}, tls_verified, '', False, 'missing-choices-array'

        if not response['choices']:
            return False, status, {}, tls_verified, '', False, 'empty-choices-array'

        choice = response['choices'][0]
        if not isinstance(choice, dict) or 'message' not in choice:
            return False, status, {}, tls_verified, '', False, 'choice-missing-message'

        message = choice['message']
        if not isinstance(message, dict):
            return False, status, {}, tls_verified, '', False, 'message-not-object'

        # Message must have role and content fields
        if 'role' not in message or 'content' not in message:
            return False, status, {}, tls_verified, '', False, 'message-missing-fields'

        content = message['content']
        if not isinstance(content, str):
            return False, status, {}, tls_verified, '', False, 'content-not-string'

        if not content.strip():
            return False, status, {}, tls_verified, '', False, 'content-empty'

        # Try to parse content as JSON to validate it matches the requested format
        content_valid = False
        try:
            json.loads(content)
            content_valid = True
        except json.JSONDecodeError:
            # Content is not JSON, which violates the response_format requirement
            pass

        filtered_headers = filter_headers(headers)
        # Fail if content was not valid JSON when JSON format was requested
        return content_valid, status, filtered_headers, tls_verified, content, content_valid, None

    except json.JSONDecodeError:
        return False, status, {}, tls_verified, '', False, 'response-not-json'


def get_probe_revision():
    """Get the current git commit hash and dirty flag.

    Returns "commit[+dirty]". Excludes the evidence directory from dirty check.
    """
    try:
        import subprocess
        commit = subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'],
            stderr=subprocess.DEVNULL,
            cwd=Path(__file__).parent.parent.parent
        ).decode().strip()

        # Check for dirty tree, excluding the evidence directory
        status = subprocess.check_output(
            ['git', 'status', '--porcelain', '--', '.', ':!docs/validation/evidence/'],
            stderr=subprocess.DEVNULL,
            cwd=Path(__file__).parent.parent.parent
        ).decode().strip()

        dirty = '+dirty' if status else ''
        return f'{commit}{dirty}'
    except:
        return 'unknown'


def main():
    parser = argparse.ArgumentParser(
        description='Spark protocol probe: test endpoint reachability and protocol compliance'
    )
    parser.add_argument(
        'provider_key',
        help='Provider key to probe (e.g., "spark")'
    )
    args = parser.parse_args()

    try:
        # Read configuration
        base_url, credential, models_list = read_provider_config(args.provider_key)
    except ProbeError as e:
        # Explicit configuration errors with sanitized messages to stderr
        print(f'Probe error: {str(e)}', file=sys.stderr)
        sys.exit(1)

    # Parse URL to extract scheme
    parsed_url = urlparse(base_url)
    scheme = parsed_url.scheme

    # Collect probe observations
    collection_time = datetime.now(timezone.utc).isoformat()
    # Replace colons and plus signs to make valid filename
    time_str = collection_time.replace(':', '-').replace('+', 'z')
    evidence_id = f'spark-{time_str}'

    observations = {
        'evidence_id': evidence_id,
        'probe_revision': get_probe_revision(),
        'collection_time': collection_time,
        'scheme': scheme,
        'probes': {}
    }

    # Probe 1: Check if authentication is required (unauthenticated request)
    auth_req, unauth_status, unauth_headers, unauth_tls, unauth_error = test_auth_required(base_url)

    if unauth_error:
        # Unauthenticated request failed - this is a probe error, exit non-zero
        # Don't write evidence for a completely failed network request
        print(f'Unauthenticated models probe failed: {unauth_error}', file=sys.stderr)
        sys.exit(1)

    observations['probes']['unauthenticated_models'] = {
        'method': 'GET',
        'path': '/models',
        'credential_sent': False,
        'status': unauth_status,
        'headers': filter_headers(unauth_headers),
        'authentication_requirement': auth_req,
        'tls_verified': unauth_tls,
    }

    # Probe 2: Test authentication and get models list
    auth_success, models, auth_status, auth_headers, auth_tls, auth_error = test_authentication(
        base_url, credential
    )

    if not auth_success:
        # Authentication probe failed - write evidence before exiting
        observations['probes']['authenticated_models'] = {
            'method': 'GET',
            'path': '/models',
            'credential_sent': True,
            'status': auth_status,
            'headers': filter_headers(auth_headers) if auth_headers else {},
            'models': [],
            'tls_verified': auth_tls,
            'error': auth_error,
        }

        # Write failed evidence artifact
        evidence_dir = Path(__file__).parent.parent.parent / 'docs' / 'validation' / 'evidence' / 'spark-probe'
        evidence_dir.mkdir(parents=True, exist_ok=True)
        evidence_file = evidence_dir / f'{evidence_id}.json'
        try:
            with open(evidence_file, 'w') as f:
                json.dump(observations, f, indent=2)
                f.write('\n')
        except Exception as e:
            print(f'Failed to write evidence artifact: {e}', file=sys.stderr)

        sys.exit(1)

    observations['probes']['authenticated_models'] = {
        'method': 'GET',
        'path': '/models',
        'credential_sent': True,
        'status': auth_status,
        'headers': auth_headers,
        'models': models,
        'tls_verified': auth_tls,
    }

    # Chat probe must use a model the endpoint listed; cannot fall back to config
    if not models:
        observations['probes']['authenticated_models']['error'] = 'endpoint-returned-no-models'
        # Write evidence before exiting
        evidence_dir = Path(__file__).parent.parent.parent / 'docs' / 'validation' / 'evidence' / 'spark-probe'
        evidence_dir.mkdir(parents=True, exist_ok=True)
        evidence_file = evidence_dir / f'{evidence_id}.json'
        try:
            with open(evidence_file, 'w') as f:
                json.dump(observations, f, indent=2)
                f.write('\n')
        except Exception as e:
            print(f'Failed to write evidence artifact: {e}', file=sys.stderr)
        print('Endpoint returned no model IDs in /models response', file=sys.stderr)
        sys.exit(1)

    selected_model = models[0]

    # Probe 3: Test structured response on /chat/completions
    resp_success, resp_status, resp_headers, resp_tls, resp_content, content_valid, resp_error = test_structured_response(
        base_url, credential, selected_model
    )

    observations['probes']['structured_response'] = {
        'method': 'POST',
        'path': '/chat/completions',
        'credential_sent': True,
        'status': resp_status,
        'headers': resp_headers if resp_headers else {},
        'model_requested': selected_model,
        'content_valid': content_valid,
        'tls_verified': resp_tls,
    }

    if resp_content:
        observations['probes']['structured_response']['sample_content'] = resp_content[:100]

    if resp_error:
        observations['probes']['structured_response']['error'] = resp_error

    # Write evidence artifact
    evidence_dir = Path(__file__).parent.parent.parent / 'docs' / 'validation' / 'evidence' / 'spark-probe'
    evidence_dir.mkdir(parents=True, exist_ok=True)

    evidence_file = evidence_dir / f'{evidence_id}.json'
    try:
        with open(evidence_file, 'w') as f:
            json.dump(observations, f, indent=2)
            f.write('\n')
    except Exception as e:
        print(f'Failed to write evidence artifact: {e}', file=sys.stderr)
        sys.exit(1)

    # Exit with error if any probe failed
    if not auth_success or not resp_success:
        sys.exit(1)

    sys.exit(0)


if __name__ == '__main__':
    main()
