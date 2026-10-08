#!/usr/bin/env python3
"""
Tests for the Spark protocol probe.

Tests cover:
- Configuration validation with providers schema
- Authentication requirement detection
- Successful authentication with valid credentials
- Structured response validation with JSON parsing
- TLS verification detection
- Exit code verification
- Sanitization of endpoint address and credentials
"""

import json
import os
import sys
import tempfile
import unittest
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path
from threading import Thread
from unittest.mock import patch
import subprocess

# Import the probe module
sys.path.insert(0, str(Path(__file__).parent))
from spark_probe import (
    ProbeError,
    read_provider_config,
    send_request,
    filter_headers,
    test_auth_required,
    test_authentication,
    test_structured_response,
)


class StubHTTPHandler(BaseHTTPRequestHandler):
    """Stub HTTP server for testing probe requests."""

    # Class variable to control behavior
    auth_required = True
    auth_token = 'test-token-12345'
    models_list = ['model-1', 'model-2', 'model-3']
    support_openai = True
    return_json_content = True

    def do_GET(self):
        """Handle GET requests."""
        if self.path == '/models':
            # Check authentication
            auth_header = self.headers.get('Authorization', '')
            has_valid_auth = auth_header == f'Bearer {self.auth_token}'

            if self.auth_required and not has_valid_auth:
                self.send_response(401)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                return

            # Return models
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Server', 'spark-server/1.0')
            self.send_header('Date', 'Mon, 08 Oct 2026 10:00:00 GMT')
            self.end_headers()

            response = {
                'data': [{'id': m} for m in self.models_list],
            }
            self.wfile.write(json.dumps(response).encode())
            return

        self.send_error(404)

    def do_POST(self):
        """Handle POST requests."""
        if self.path == '/chat/completions':
            # Check authentication
            auth_header = self.headers.get('Authorization', '')
            has_valid_auth = auth_header == f'Bearer {self.auth_token}'

            if not has_valid_auth:
                self.send_response(401)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                return

            if not self.support_openai:
                self.send_response(404)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                return

            # Parse request
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')

            try:
                request = json.loads(body)
            except:
                self.send_response(400)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                return

            # Return structured response
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Server', 'spark-server/1.0')
            self.send_header('Date', 'Mon, 08 Oct 2026 10:00:00 GMT')
            self.end_headers()

            if self.return_json_content:
                # Return valid JSON content in the message
                content = json.dumps({'test': 'response', 'model': request.get('model', 'unknown')})
            else:
                # Return plain text (invalid for JSON response format)
                content = 'This is a test response.'

            response = {
                'choices': [
                    {
                        'message': {
                            'content': content,
                            'role': 'assistant',
                        }
                    }
                ],
                'model': request.get('model', 'unknown'),
            }
            self.wfile.write(json.dumps(response).encode())
            return

        self.send_error(404)

    def log_message(self, format, *args):
        """Suppress log messages during tests."""
        pass


class TestProviderProbe(unittest.TestCase):
    """Test suite for the Spark protocol probe."""

    @classmethod
    def setUpClass(cls):
        """Start the stub HTTP server."""
        cls.server = HTTPServer(('127.0.0.1', 0), StubHTTPHandler)
        cls.port = cls.server.server_address[1]
        cls.base_url = f'http://127.0.0.1:{cls.port}'

        # Start server in a thread
        cls.server_thread = Thread(target=cls.server.serve_forever)
        cls.server_thread.daemon = True
        cls.server_thread.start()

    @classmethod
    def tearDownClass(cls):
        """Stop the stub HTTP server."""
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        """Reset server state before each test."""
        StubHTTPHandler.auth_required = True
        StubHTTPHandler.auth_token = 'test-token-12345'
        StubHTTPHandler.models_list = ['model-1', 'model-2']
        StubHTTPHandler.support_openai = True
        StubHTTPHandler.return_json_content = True

    def test_read_config_success(self):
        """Test successful configuration reading with providers schema."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test-provider': {
                        'baseUrl': 'http://example.com',
                        'apiKey': 'secret-token',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                url, cred, models = read_provider_config('test-provider')
                self.assertEqual(url, 'http://example.com')
                self.assertEqual(cred, 'secret-token')
                self.assertEqual(len(models), 1)

            os.unlink(f.name)

    def test_read_config_missing_file(self):
        """Test error handling for missing config file."""
        with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': '/nonexistent/path.json'}):
            with self.assertRaises(ProbeError) as ctx:
                read_provider_config('test-provider')
            self.assertIn('not found', str(ctx.exception))

    def test_read_config_missing_providers_key(self):
        """Test error handling for missing providers key."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({'other': {}}, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config('test-provider')
                self.assertIn('providers', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_missing_provider_key(self):
        """Test error handling for missing specific provider key."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'other-provider': {}
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config('test-provider')
                self.assertIn('not found', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_missing_base_url(self):
        """Test error handling for missing baseUrl."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test-provider': {
                        'apiKey': 'token',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config('test-provider')
                self.assertIn('baseUrl', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_missing_api_key(self):
        """Test error handling for missing apiKey."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test-provider': {
                        'baseUrl': 'http://example.com',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config('test-provider')
                self.assertIn('apiKey', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_invalid_scheme(self):
        """Test error handling for invalid URL scheme."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test-provider': {
                        'baseUrl': 'ftp://example.com',
                        'apiKey': 'token',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config('test-provider')
                self.assertIn('scheme', str(ctx.exception))

            os.unlink(f.name)

    def test_send_request_without_auth(self):
        """Test sending an unauthenticated request."""
        status, headers, body, tls_verified, error = send_request(self.base_url, '/models')
        self.assertEqual(status, 401)
        self.assertFalse(tls_verified)  # HTTP, so TLS not verified
        self.assertIsNone(error)

    def test_send_request_with_auth(self):
        """Test sending an authenticated request."""
        status, headers, body, tls_verified, error = send_request(
            self.base_url, '/models',
            credential='test-token-12345'
        )
        self.assertEqual(status, 200)
        self.assertFalse(tls_verified)  # HTTP, so TLS not verified
        self.assertIsNone(error)
        data = json.loads(body)
        self.assertIn('data', data)

    def test_filter_headers(self):
        """Test that only allowlisted headers are preserved."""
        headers = {
            'Server': 'test-server',
            'Content-Type': 'application/json',
            'Date': 'Mon, 08 Oct 2026 10:00:00 GMT',
            'X-Custom-Header': 'secret',
            'Authorization': 'Bearer token',
        }
        filtered = filter_headers(headers)
        self.assertIn('server', filtered)
        self.assertIn('content-type', filtered)
        self.assertIn('date', filtered)
        self.assertNotIn('x-custom-header', filtered)
        self.assertNotIn('authorization', filtered)

    def test_auth_required_when_auth_needed(self):
        """Test auth requirement detection with 401."""
        StubHTTPHandler.auth_required = True
        auth_req, status, headers, tls_verified, error = test_auth_required(self.base_url)
        self.assertEqual(auth_req, 'authentication_required')
        self.assertEqual(status, 401)
        self.assertIsNone(error)

    def test_auth_not_required(self):
        """Test auth requirement detection when auth not needed."""
        StubHTTPHandler.auth_required = False
        auth_req, status, headers, tls_verified, error = test_auth_required(self.base_url)
        self.assertEqual(auth_req, 'no_authentication_required')
        self.assertEqual(status, 200)
        self.assertIsNone(error)

    def test_authentication_success(self):
        """Test successful authentication."""
        success, models, status, headers, tls_verified, error = test_authentication(
            self.base_url,
            'test-token-12345'
        )
        self.assertTrue(success)
        self.assertEqual(status, 200)
        self.assertIsNone(error)
        self.assertEqual(models, ['model-1', 'model-2'])
        self.assertFalse(tls_verified)  # HTTP

    def test_authentication_failure_wrong_token(self):
        """Test authentication failure with wrong token."""
        success, models, status, headers, tls_verified, error = test_authentication(
            self.base_url,
            'wrong-token'
        )
        self.assertFalse(success)
        self.assertEqual(status, 401)
        self.assertIsNone(error)  # No error message for HTTP errors

    def test_structured_response_success_with_json_content(self):
        """Test successful structured response validation with JSON content."""
        StubHTTPHandler.return_json_content = True
        success, status, headers, tls_verified, content, content_valid, error = test_structured_response(
            self.base_url,
            'test-token-12345',
            'model-1'
        )
        self.assertTrue(success)
        self.assertEqual(status, 200)
        self.assertTrue(content_valid)  # Content should parse as JSON
        self.assertIsNone(error)
        self.assertIsNotNone(content)
        self.assertFalse(tls_verified)  # HTTP

    def test_structured_response_plain_text_content(self):
        """Test structured response with plain text content (invalid for JSON format)."""
        StubHTTPHandler.return_json_content = False
        success, status, headers, tls_verified, content, content_valid, error = test_structured_response(
            self.base_url,
            'test-token-12345',
            'model-1'
        )
        # This should still be marked as success (200 response with valid structure)
        # but content_valid should be False since it doesn't parse as JSON
        self.assertTrue(success)
        self.assertEqual(status, 200)
        self.assertFalse(content_valid)  # Content is plain text, not JSON

    def test_structured_response_not_supported(self):
        """Test structured response when endpoint doesn't support it."""
        StubHTTPHandler.support_openai = False
        success, status, headers, tls_verified, content, content_valid, error = test_structured_response(
            self.base_url,
            'test-token-12345',
            'model-1'
        )
        self.assertFalse(success)
        self.assertEqual(status, 404)

    def test_structured_response_no_auth(self):
        """Test structured response fails without authentication."""
        success, status, headers, tls_verified, content, content_valid, error = test_structured_response(
            self.base_url,
            'wrong-token',
            'model-1'
        )
        self.assertFalse(success)
        self.assertEqual(status, 401)


class TestMainExitCodes(unittest.TestCase):
    """Test exit codes and sanitization of the main function."""

    def test_main_exit_code_missing_provider_key(self):
        """Test that main exits with non-zero when provider key not provided."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test': {
                        'baseUrl': 'http://127.0.0.1:9999',
                        'apiKey': 'token',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                result = subprocess.run(
                    [sys.executable, str(Path(__file__).parent / 'spark_probe.py')],
                    capture_output=True
                )
                # argparse exits with 2 for argument errors
                self.assertNotEqual(result.returncode, 0)

            os.unlink(f.name)

    def test_main_exit_code_missing_config(self):
        """Test that main exits with 1 on missing config."""
        with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': '/nonexistent/path.json'}):
            result = subprocess.run(
                [sys.executable, str(Path(__file__).parent / 'spark_probe.py'), 'test-provider'],
                capture_output=True
            )
            self.assertEqual(result.returncode, 1)

    def test_credential_not_in_stderr(self):
        """Test that credentials don't appear in stderr."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'providers': {
                    'test': {
                        'baseUrl': 'http://127.0.0.1:9999',
                        'apiKey': 'super-secret-credential-12345',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                result = subprocess.run(
                    [sys.executable, str(Path(__file__).parent / 'spark_probe.py'), 'test'],
                    capture_output=True,
                    text=True
                )
                # Credential should not appear in stderr
                self.assertNotIn('super-secret-credential-12345', result.stderr)

            os.unlink(f.name)

    def test_endpoint_address_not_in_error_output(self):
        """Test that endpoint addresses don't appear in error output."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            endpoint = 'http://192.168.1.100:8080'
            json.dump({
                'providers': {
                    'test': {
                        'baseUrl': endpoint,
                        'apiKey': 'token',
                        'models': [{'id': 'model-1'}]
                    }
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                result = subprocess.run(
                    [sys.executable, str(Path(__file__).parent / 'spark_probe.py'), 'test'],
                    capture_output=True,
                    text=True
                )
                # Endpoint should not appear in stderr
                self.assertNotIn('192.168.1.100', result.stderr)

            os.unlink(f.name)


if __name__ == '__main__':
    unittest.main()
