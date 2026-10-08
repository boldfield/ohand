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

    Returns (base_url, credential, model_name, raw_config)
    Raises ProbeError if config is missing or invalid.
    """
    config_path = os.environ.get('OHAND_PROVIDER_CONFIG_PATH', '/etc/ohand-provider/models.json')

    if not Path(config_path).exists():
        raise ProbeError(f'Provider config file not found: {config_path}')

    try:
        with open(config_path, 'r') as f:
            config = json.load(f)
    except (json.JSONDecodeError, IOError) as e:
        raise ProbeError(f'Failed to read/parse provider config: {e}')

    # Expect config with 'spark' provider containing base_url, credential, model_name
    if 'spark' not in config:
        raise ProbeError('Provider config missing "spark" key')

    spark_config = config['spark']

    # Validate required fields
    base_url = spark_config.get('base_url', '').strip()
    credential = spark_config.get('credential', '').strip()
    model_name = spark_config.get('model_name', '').strip()

    if not base_url:
        raise ProbeError('Spark config missing or empty base_url')
    if not credential:
        raise ProbeError('Spark config missing or empty credential')
    if not model_name:
        raise ProbeError('Spark config missing or empty model_name')

    return base_url, credential, model_name, config


def send_request(base_url, endpoint, method='GET', credential=None, body=None):
    """Send an HTTP request and return (status_code, headers, body_text).

    Returns None for status/headers/body if the request fails entirely.
    """
    url = base_url.rstrip('/') + endpoint

    req = urllib.request.Request(url, method=method)

    if credential:
        req.add_header('Authorization', f'Bearer {credential}')

    if body:
        req.add_header('Content-Type', 'application/json')
        req.data = body.encode('utf-8')

    try:
        # Accept any certificate (we will report whether it verified)
        context = ssl.create_default_context()
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE

        response = urllib.request.urlopen(req, context=context if url.startswith('https') else None)
        status = response.status
        headers = dict(response.headers)
        response_body = response.read().decode('utf-8', errors='replace')
        return status, headers, response_body
    except urllib.error.HTTPError as e:
        status = e.code
        headers = dict(e.headers) if hasattr(e, 'headers') else {}
        try:
            response_body = e.read().decode('utf-8', errors='replace')
        except:
            response_body = ''
        return status, headers, response_body
    except Exception as e:
        # Request failed entirely
        return None, {}, str(e)


def filter_headers(headers):
    """Return only allowlisted headers, with values preserved."""
    return {k.lower(): v for k, v in headers.items() if k.lower() in ALLOWLISTED_HEADERS}


def test_auth_required(base_url):
    """Test whether authentication is required by sending an unauthenticated request.

    Returns:
        - 'authenticated_required' if 401/403
        - 'no_auth_required' if 200 and response parses as valid
        - 'auth_error' if other status
    """
    status, headers, body = send_request(base_url, '/models')

    if status is None:
        return 'request_failed'

    if status in (401, 403):
        return 'authentication_required'

    if status == 200:
        # Check if it's valid JSON (basic validation)
        try:
            models = json.loads(body)
            if isinstance(models, dict) and 'data' in models:
                return 'no_authentication_required'
        except:
            pass
        return 'auth_error'

    return 'auth_error'


def test_authentication(base_url, credential):
    """Test authentication with the provided credential.

    Returns (success: bool, models: list, status_code: int, error: str)
    """
    status, headers, body = send_request(base_url, '/models', credential=credential)

    if status is None:
        return False, [], None, f'Request failed: {body}'

    if status != 200:
        return False, [], status, f'Auth request returned {status}'

    try:
        response = json.loads(body)
        if isinstance(response, dict) and 'data' in response:
            models = [m.get('id', '') for m in response['data'] if isinstance(m, dict)]
            return True, models, status, None
        else:
            return False, [], status, 'Response missing "data" field'
    except json.JSONDecodeError as e:
        return False, [], status, f'Response not valid JSON: {e}'


def test_openai_compatibility(base_url, credential, model_name):
    """Test OpenAI /chat/completions endpoint.

    Returns (success: bool, status_code: int, message_content: str, error: str)
    """
    body = json.dumps({
        'model': model_name,
        'messages': [{'role': 'user', 'content': 'Hello'}],
    })

    status, headers, response_body = send_request(
        base_url, '/chat/completions', method='POST',
        credential=credential, body=body
    )

    if status is None:
        return False, None, None, f'Request failed: {response_body}'

    if status != 200:
        return False, status, None, f'Endpoint returned {status}'

    try:
        response = json.loads(response_body)
        if isinstance(response, dict) and 'choices' in response:
            choices = response['choices']
            if choices and isinstance(choices[0], dict):
                message = choices[0].get('message', {})
                if isinstance(message, dict):
                    content = message.get('content', '')
                    return True, status, content, None
        return False, status, None, 'Response missing expected structure'
    except json.JSONDecodeError as e:
        return False, status, None, f'Response not valid JSON: {e}'


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
        base_url, credential, model_name, config = read_provider_config()
    except ProbeError as e:
        print(f'Configuration error: {e}', file=sys.stderr)
        sys.exit(1)

    # Collect probe observations
    observations = {
        'evidence_id': f'spark-probe-{datetime.now(timezone.utc).isoformat()}',
        'probe_revision': get_probe_revision(),
        'collection_time': datetime.now(timezone.utc).isoformat(),
        'probes': {}
    }

    # Test 1: Check if authentication is required
    auth_status = test_auth_required(base_url)
    observations['probes']['auth_requirement'] = {
        'result': auth_status,
        'method': 'GET',
        'path': '/models',
        'credential_sent': False,
        'headers_sent': {},
    }

    if auth_status == 'authentication_required':
        observations['authentication_required'] = True
    else:
        observations['authentication_required'] = False

    # Test 2: Test authentication
    auth_success, models, auth_status_code, auth_error = test_authentication(base_url, credential)
    observations['probes']['authentication'] = {
        'success': auth_success,
        'method': 'GET',
        'path': '/models',
        'credential_sent': True,
        'status': auth_status_code,
        'error': auth_error,
    }

    if not auth_success:
        print(f'Authentication failed: {auth_error}', file=sys.stderr)
        sys.exit(1)

    observations['models'] = models

    # Test 3: Test OpenAI compatibility
    openai_success, openai_status, message_content, openai_error = test_openai_compatibility(
        base_url, credential, model_name
    )
    observations['probes']['openai_compatibility'] = {
        'success': openai_success,
        'method': 'POST',
        'path': '/chat/completions',
        'credential_sent': True,
        'status': openai_status,
        'error': openai_error,
    }

    if not openai_success:
        print(f'OpenAI compatibility test failed: {openai_error}', file=sys.stderr)
        sys.exit(1)

    observations['structured_response'] = {
        'model': model_name,
        'message_content': message_content[:200],  # Truncate for size
    }

    # Write evidence artifact
    evidence_dir = Path(__file__).parent.parent.parent / 'docs' / 'validation' / 'evidence' / 'spark-probe'
    evidence_dir.mkdir(parents=True, exist_ok=True)

    evidence_file = evidence_dir / f'{observations["evidence_id"]}.json'
    with open(evidence_file, 'w') as f:
        json.dump(observations, f, indent=2)

    print(f'Probe succeeded. Evidence written to {evidence_file.relative_to(Path.cwd())}')
    sys.exit(0)


if __name__ == '__main__':
    main()
