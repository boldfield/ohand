#!/usr/bin/env python3
"""
Unit tests for the hygiene checker.

Tests verify that:
- Seeded test secrets/private-path fixtures fail the check
- Approved synthetic fixtures pass
- Real secrets would be detected
"""

import unittest
import tempfile
import os
from pathlib import Path
from check_hygiene import (
    check_for_secrets,
    check_private_captures,
    check_path,
    is_synthetic_fixture_path,
    is_skipped_path,
)


class TestHygieneChecker(unittest.TestCase):
    """Test the hygiene checker against various scenarios."""

    def test_aws_access_key_detected(self):
        """Seeded AWS Access Key should be detected."""
        content = 'AKIAIOSFODNN7EXAMPLE'
        issues = check_for_secrets(content, 'example.txt')
        self.assertTrue(any('AWS Access Key' in issue[0] for issue in issues))

    def test_api_key_detected(self):
        """Seeded API key should be detected."""
        content = 'api_key=sk-1234567890abcdefghij'
        issues = check_for_secrets(content, 'config.py')
        self.assertTrue(any('API Key' in issue[0] or 'OpenAI-style' in issue[0] for issue in issues))

    def test_anthropic_key_detected(self):
        """Seeded Anthropic API key should be detected."""
        content = 'key = sk-ant-abcdefghijklmnopqrst'
        issues = check_for_secrets(content, 'config.py')
        self.assertTrue(any('Anthropic API Key' in issue[0] for issue in issues))

    def test_private_key_detected(self):
        """Private key material should be detected."""
        content = '''-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA1234567890...
-----END RSA PRIVATE KEY-----'''
        issues = check_for_secrets(content, 'key.pem')
        self.assertTrue(any('Private Key' in issue[0] for issue in issues))

    def test_password_detected(self):
        """Hardcoded password should be detected."""
        content = 'password=super_secret_password_123'
        issues = check_for_secrets(content, 'config.rs')
        self.assertTrue(any('Password' in issue[0] for issue in issues))

    def test_database_url_with_credentials_detected(self):
        """Database URL with embedded credentials should be detected."""
        content = 'postgres://user:password@localhost/db'
        issues = check_for_secrets(content, 'db.conf')
        self.assertTrue(any('Database URL' in issue[0] for issue in issues))

    def test_bearer_token_detected(self):
        """Bearer token should be detected."""
        content = 'Authorization: bearer abc123def456ghi789jkl'
        issues = check_for_secrets(content, 'request.py')
        self.assertTrue(any('Bearer Token' in issue[0] for issue in issues))

    def test_audio_file_outside_fixtures_fails(self):
        """Audio file outside documented fixtures should fail."""
        # Create a temporary audio file outside fixtures
        with tempfile.NamedTemporaryFile(suffix='.m4a', delete=False) as tmp:
            tmp.write(b'fake audio data')
            tmp_path = tmp.name

        try:
            issues = check_private_captures(tmp_path)
            self.assertTrue(any('Audio file outside' in issue for issue in issues))
        finally:
            os.unlink(tmp_path)

    def test_audio_file_in_fixtures_passes(self):
        """Audio file in fixtures/ passes."""
        issues = check_private_captures('fixtures/synthetic_audio.m4a')
        self.assertFalse(any('Audio file outside' in issue for issue in issues))

    def test_audio_file_in_tests_passes(self):
        """Audio file in tests/ passes."""
        issues = check_private_captures('core/tests/recording.wav')
        self.assertFalse(any('Audio file outside' in issue for issue in issues))

    def test_audio_file_in_ios_tests_passes(self):
        """Audio file in ios/Tests/ passes."""
        issues = check_private_captures('ios/Tests/fixtures/synthetic.mp3')
        self.assertFalse(any('Audio file outside' in issue for issue in issues))

    def test_synthetic_fixture_path_recognized(self):
        """Synthetic fixture paths should be recognized."""
        self.assertTrue(is_synthetic_fixture_path('fixtures/synthetic_audio.wav'))
        self.assertTrue(is_synthetic_fixture_path('core/tests/data.m4a'))
        self.assertTrue(is_synthetic_fixture_path('ios/Tests/probe.aac'))
        self.assertFalse(is_synthetic_fixture_path('src/audio.wav'))
        self.assertFalse(is_synthetic_fixture_path('app/capture.m4a'))

    def test_skipped_paths_ignored(self):
        """Skipped paths should not be checked."""
        self.assertTrue(is_skipped_path('.git/config'))
        self.assertTrue(is_skipped_path('target/release/app'))
        self.assertTrue(is_skipped_path('.env'))
        self.assertFalse(is_skipped_path('src/main.rs'))
        self.assertFalse(is_skipped_path('fixtures/test.wav'))

    def test_comment_secrets_ignored(self):
        """Secrets in comments should not be flagged."""
        # Python comment
        content_py = '# api_key = fake_key_1234567890abcdefghij'
        issues = check_for_secrets(content_py, 'script.py')
        self.assertEqual(len(issues), 0)

        # Rust comment
        content_rs = '// password = super_secret'
        issues = check_for_secrets(content_rs, 'lib.rs')
        self.assertEqual(len(issues), 0)

    def test_no_secrets_in_clean_content(self):
        """Clean content should pass."""
        content = '''
fn main() {
    println!("Hello, world!");
    let config = read_config();
    process(config);
}
'''
        issues = check_for_secrets(content, 'main.rs')
        self.assertEqual(len(issues), 0)

    def test_no_audio_in_clean_paths(self):
        """Clean paths should pass."""
        issues = check_private_captures('src/main.rs')
        self.assertEqual(len(issues), 0)

        issues = check_private_captures('README.md')
        self.assertEqual(len(issues), 0)


class TestFixtureDocumentation(unittest.TestCase):
    """Test fixture path documentation."""

    def test_fixtures_directory_approved(self):
        """fixtures/ directory is approved for synthetic content."""
        self.assertTrue(is_synthetic_fixture_path('fixtures/'))
        self.assertTrue(is_synthetic_fixture_path('fixtures/audio/synthetic.wav'))

    def test_core_tests_approved(self):
        """core/tests/ is approved for synthetic test fixtures."""
        self.assertTrue(is_synthetic_fixture_path('core/tests/'))
        self.assertTrue(is_synthetic_fixture_path('core/tests/data/recording.m4a'))

    def test_ios_tests_approved(self):
        """ios/Tests/ is approved for synthetic test fixtures."""
        self.assertTrue(is_synthetic_fixture_path('ios/Tests/'))
        self.assertTrue(is_synthetic_fixture_path('ios/Tests/fixtures/audio.aac'))

    def test_general_tests_approved(self):
        """tests/ directory is approved for synthetic test fixtures."""
        self.assertTrue(is_synthetic_fixture_path('tests/'))
        self.assertTrue(is_synthetic_fixture_path('tests/fixtures/capture.flac'))

    def test_validation_approved(self):
        """docs/validation/ is approved for evidence/probe documentation."""
        self.assertTrue(is_synthetic_fixture_path('docs/validation/'))
        self.assertTrue(is_synthetic_fixture_path('docs/validation/probe_audio.ogg'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
