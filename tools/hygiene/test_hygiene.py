#!/usr/bin/env python3
"""
Unit tests for the hygiene checker.

Tests verify that:
- Seeded test secrets/private-path fixtures fail the check
- Approved synthetic fixtures pass
- Real secrets would be detected
- E2E integration tests prove acceptance criteria
"""

import unittest
import tempfile
import os
import subprocess
import shutil
from pathlib import Path
from check_hygiene import (
    check_for_secrets,
    check_private_captures,
    check_path,
    is_synthetic_fixture_path,
    is_skipped_path,
    check_signing_material,
    has_provenance_file,
    check_repository,
    find_tracked_files,
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
        self.assertTrue(is_skipped_path('node_modules/package.json'))
        self.assertFalse(is_skipped_path('.env'))  # .env files are NOT skipped - they should be checked
        self.assertFalse(is_skipped_path('src/main.rs'))
        self.assertFalse(is_skipped_path('fixtures/test.wav'))

    def test_comment_secrets_detected(self):
        """Secrets in comments should be flagged - comments don't protect against leaks."""
        # Python comment with secret
        content_py = '# api_key = fake_key_1234567890abcdefghij'
        issues = check_for_secrets(content_py, 'script.py')
        self.assertTrue(any('API Key' in issue[0] or 'OpenAI-style' in issue[0] for issue in issues))

        # Rust comment with secret
        content_rs = '// password = super_secret'
        issues = check_for_secrets(content_rs, 'lib.rs')
        self.assertTrue(any('Password' in issue[0] for issue in issues))

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

    def test_env_file_secrets_detected(self):
        """Secrets in .env files should be detected."""
        content = 'DATABASE_PASSWORD=super_secret_password_123'
        issues = check_for_secrets(content, '.env')
        self.assertTrue(any('Password' in issue[0] for issue in issues))

    def test_env_production_file_secrets_detected(self):
        """Secrets in .env.production files should be detected."""
        content = 'API_KEY=sk-test-1234567890abcdefghij'
        issues = check_for_secrets(content, '.env.production')
        self.assertTrue(any('API Key' in issue[0] or 'OpenAI-style' in issue[0] for issue in issues))

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


class TestSigningMaterial(unittest.TestCase):
    """Test detection of signing material and certificates."""

    def test_p12_detected(self):
        """PKCS#12 certificate files should be detected."""
        issues = check_signing_material('certs/apple.p12')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_mobileprovision_detected(self):
        """iOS provisioning profile should be detected."""
        issues = check_signing_material('profile/signing.mobileprovision')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_pem_detected(self):
        """PEM files should be detected."""
        issues = check_signing_material('keys/private.pem')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_key_detected(self):
        """Private key files should be detected."""
        issues = check_signing_material('certs/server.key')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_keystore_detected(self):
        """Android keystore files should be detected."""
        issues = check_signing_material('android/release.keystore')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_cer_detected(self):
        """Certificate files should be detected."""
        issues = check_signing_material('certs/root.cer')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_gpg_detected(self):
        """GPG keys should be detected."""
        issues = check_signing_material('keys/signing.gpg')
        self.assertTrue(any('Signing material' in issue for issue in issues))

    def test_asc_detected(self):
        """ASCII-armored key files should be detected."""
        issues = check_signing_material('keys/public.asc')
        self.assertTrue(any('Signing material' in issue for issue in issues))


class TestProvenanceRequirement(unittest.TestCase):
    """Test that audio files require provenance documentation."""

    def test_audio_without_provenance_fails(self):
        """Audio in fixtures without provenance should fail."""
        with tempfile.TemporaryDirectory() as tmpdir:
            audio_file = os.path.join(tmpdir, 'fixtures', 'test.wav')
            os.makedirs(os.path.dirname(audio_file), exist_ok=True)

            with open(audio_file, 'wb') as f:
                f.write(b'fake audio data')

            os.chdir(tmpdir)
            issues = check_private_captures('fixtures/test.wav')
            self.assertTrue(any('provenance' in issue for issue in issues))

    def test_audio_with_provenance_passes(self):
        """Audio in fixtures with provenance sidecar should pass."""
        with tempfile.TemporaryDirectory() as tmpdir:
            audio_file = os.path.join(tmpdir, 'fixtures', 'synthetic.wav')
            provenance_file = audio_file + '.provenance'

            os.makedirs(os.path.dirname(audio_file), exist_ok=True)

            with open(audio_file, 'wb') as f:
                f.write(b'fake audio')

            with open(provenance_file, 'w') as f:
                f.write('This is synthetic generated audio from TTS.')

            os.chdir(tmpdir)
            issues = check_private_captures('fixtures/synthetic.wav')
            self.assertFalse(any('provenance' in issue for issue in issues))


class TestE2EIntegration(unittest.TestCase):
    """End-to-end integration tests with real git repositories."""

    def setUp(self):
        """Create a temporary git repository for testing."""
        self.test_dir = tempfile.mkdtemp()
        self.original_cwd = os.getcwd()

    def tearDown(self):
        """Clean up the temporary repository."""
        os.chdir(self.original_cwd)
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def init_git_repo(self):
        """Initialize a git repository in the test directory."""
        os.chdir(self.test_dir)
        subprocess.run(['git', 'init'], check=True, capture_output=True)
        subprocess.run(['git', 'config', 'user.email', 'test@example.com'], check=True, capture_output=True)
        subprocess.run(['git', 'config', 'user.name', 'Test User'], check=True, capture_output=True)

    def test_committed_secret_fails(self):
        """Committed secret should be detected and fail check."""
        self.init_git_repo()

        # Create a file with a secret
        secret_file = os.path.join(self.test_dir, 'config.py')
        with open(secret_file, 'w') as f:
            f.write('api_key = sk-1234567890abcdefghij\n')

        # Commit it
        subprocess.run(['git', 'add', 'config.py'], check=True, capture_output=True)
        subprocess.run(['git', 'commit', '-m', 'Initial commit'], check=True, capture_output=True)

        # Check should find the issue
        issues, exit_code = check_repository(scan_mode='tracked')
        self.assertEqual(exit_code, 1)
        self.assertTrue(any('config.py' in issue[0] and ('API Key' in issue[1] or 'OpenAI' in issue[1]) for issue in issues))

    def test_committed_audio_without_provenance_fails(self):
        """Audio file in fixtures without provenance should fail."""
        self.init_git_repo()

        # Create audio in fixtures without provenance
        audio_dir = os.path.join(self.test_dir, 'fixtures')
        os.makedirs(audio_dir, exist_ok=True)

        audio_file = os.path.join(audio_dir, 'test.wav')
        with open(audio_file, 'wb') as f:
            f.write(b'fake audio')

        # Commit it
        subprocess.run(['git', 'add', 'fixtures/test.wav'], check=True, capture_output=True)
        subprocess.run(['git', 'commit', '-m', 'Add audio'], check=True, capture_output=True)

        # Check should detect missing provenance
        issues, exit_code = check_repository(scan_mode='tracked')
        self.assertEqual(exit_code, 1)
        self.assertTrue(any('fixtures/test.wav' in issue[0] and 'provenance' in issue[1] for issue in issues))

    def test_documented_synthetic_fixture_passes(self):
        """Audio file in fixtures with provenance should pass."""
        self.init_git_repo()

        # Create audio with provenance
        audio_dir = os.path.join(self.test_dir, 'fixtures')
        os.makedirs(audio_dir, exist_ok=True)

        audio_file = os.path.join(audio_dir, 'synthetic.wav')
        provenance_file = audio_file + '.provenance'

        with open(audio_file, 'wb') as f:
            f.write(b'fake audio')

        with open(provenance_file, 'w') as f:
            f.write('Generated synthetic audio for testing.')

        # Commit both
        subprocess.run(['git', 'add', 'fixtures/'], check=True, capture_output=True)
        subprocess.run(['git', 'commit', '-m', 'Add synthetic audio'], check=True, capture_output=True)

        # Check should pass
        issues, exit_code = check_repository(scan_mode='tracked')
        self.assertEqual(exit_code, 0)

    def test_secret_not_in_output(self):
        """Secret values should not appear in check output."""
        self.init_git_repo()

        # Create a file with a secret
        secret_value = 'sk-test-1234567890abcdefghij'
        secret_file = os.path.join(self.test_dir, 'config.py')
        with open(secret_file, 'w') as f:
            f.write(f'api_key = {secret_value}\n')

        # Commit it
        subprocess.run(['git', 'add', 'config.py'], check=True, capture_output=True)
        subprocess.run(['git', 'commit', '-m', 'Initial commit'], check=True, capture_output=True)

        # Run check and capture output
        from check_hygiene import format_report
        issues, _ = check_repository(scan_mode='tracked')
        report = format_report(issues)

        # Secret value should not be in the report
        self.assertNotIn(secret_value, report)


if __name__ == '__main__':
    unittest.main(verbosity=2)
