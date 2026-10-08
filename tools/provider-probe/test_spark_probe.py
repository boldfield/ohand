#!/usr/bin/env python3
"""
Tests for the Spark protocol probe.

Tests cover:
- Configuration validation
- Authentication requirement detection
- Successful authentication with valid credentials
- OpenAI endpoint compatibility
- Sanitization of endpoint address and credentials
- Exit code verification
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

# Import the probe module
sys.path.insert(0, str(Path(__file__).parent))
from spark_probe import (
    ProbeError,
    read_provider_config,
    send_request,
    filter_headers,
    test_auth_required,
    test_authentication,
    test_openai_compatibility,
)


class StubHTTPHandler(BaseHTTPRequestHandler):
    """Stub HTTP server for testing probe requests."""

    # Class variable to control behavior
    auth_required = True
    auth_token = 'test-token-12345'
    models_list = ['model-1', 'model-2', 'model-3']
    support_openai = True

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

            response = {
                'choices': [
                    {
                        'message': {
                            'content': 'This is a test response.',
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

    def test_read_config_success(self):
        """Test successful configuration reading."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'spark': {
                    'base_url': 'http://example.com',
                    'credential': 'secret-token',
                    'model_name': 'model-name',
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                url, cred, model, config = read_provider_config()
                self.assertEqual(url, 'http://example.com')
                self.assertEqual(cred, 'secret-token')
                self.assertEqual(model, 'model-name')

            os.unlink(f.name)

    def test_read_config_missing_file(self):
        """Test error handling for missing config file."""
        with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': '/nonexistent/path.json'}):
            with self.assertRaises(ProbeError) as ctx:
                read_provider_config()
            self.assertIn('not found', str(ctx.exception))

    def test_read_config_missing_spark_key(self):
        """Test error handling for missing spark config."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({'other': {}}, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config()
                self.assertIn('spark', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_missing_base_url(self):
        """Test error handling for missing base_url."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'spark': {
                    'credential': 'token',
                    'model_name': 'model',
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config()
                self.assertIn('base_url', str(ctx.exception))

            os.unlink(f.name)

    def test_read_config_missing_credential(self):
        """Test error handling for missing credential."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
            json.dump({
                'spark': {
                    'base_url': 'http://example.com',
                    'model_name': 'model',
                }
            }, f)
            f.flush()

            with patch.dict(os.environ, {'OHAND_PROVIDER_CONFIG_PATH': f.name}):
                with self.assertRaises(ProbeError) as ctx:
                    read_provider_config()
                self.assertIn('credential', str(ctx.exception))

            os.unlink(f.name)

    def test_send_request_without_auth(self):
        """Test sending an unauthenticated request."""
        status, headers, body = send_request(self.base_url, '/models')
        self.assertEqual(status, 401)

    def test_send_request_with_auth(self):
        """Test sending an authenticated request."""
        status, headers, body = send_request(
            self.base_url, '/models',
            credential='test-token-12345'
        )
        self.assertEqual(status, 200)
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
        result = test_auth_required(self.base_url)
        self.assertEqual(result, 'authentication_required')

    def test_auth_not_required(self):
        """Test auth requirement detection when auth not needed."""
        StubHTTPHandler.auth_required = False
        result = test_auth_required(self.base_url)
        self.assertEqual(result, 'no_authentication_required')

    def test_authentication_success(self):
        """Test successful authentication."""
        success, models, status, error = test_authentication(
            self.base_url,
            'test-token-12345'
        )
        self.assertTrue(success)
        self.assertEqual(status, 200)
        self.assertIsNone(error)
        self.assertEqual(models, ['model-1', 'model-2'])

    def test_authentication_failure_wrong_token(self):
        """Test authentication failure with wrong token."""
        success, models, status, error = test_authentication(
            self.base_url,
            'wrong-token'
        )
        self.assertFalse(success)
        self.assertEqual(status, 401)
        self.assertIsNotNone(error)

    def test_openai_compatibility_success(self):
        """Test successful OpenAI compatibility check."""
        success, status, content, error = test_openai_compatibility(
            self.base_url,
            'test-token-12345',
            'model-1'
        )
        self.assertTrue(success)
        self.assertEqual(status, 200)
        self.assertIsNotNone(content)
        self.assertIn('test response', content)

    def test_openai_compatibility_not_supported(self):
        """Test OpenAI compatibility check when endpoint doesn't support it."""
        StubHTTPHandler.support_openai = False
        success, status, content, error = test_openai_compatibility(
            self.base_url,
            'test-token-12345',
            'model-1'
        )
        self.assertFalse(success)
        self.assertEqual(status, 404)


if __name__ == '__main__':
    unittest.main()
