# Contributing to Oh And

## Public repository standards

Oh And is a public open-source repository. Contributions must maintain strict privacy and security standards to prevent accidental exposure of secrets, credentials, private captures, or other sensitive material.

### Forbidden in commits

The following must never be committed, even in test fixtures or documentation:

- **Real credentials and keys:**
  - AWS keys, API keys, OAuth tokens
  - Private cryptographic keys (RSA, EC, OpenSSH)
  - Passwords and authentication tokens
  - Database URLs with embedded credentials
  - Anthropic, OpenAI, or other provider API credentials

- **Private captures and recordings:**
  - Real audio recordings of user speech
  - Actual session data or personal notes
  - Real user identities or private conversations
  - Private videos or screen recordings
  - Signing certificates for app distribution (unless intentionally redacted for documentation)

- **Endpoints and addresses:**
  - Private or internal server addresses
  - VPN or private network configuration
  - Non-public phone numbers or contact information

### Approved synthetic fixtures

The following paths are explicitly approved for synthetic test fixtures:

- `fixtures/` — General synthetic test data
- `core/tests/` — Rust core unit tests and test fixtures
- `ios/Tests/` — Swift unit tests and test fixtures
- `tests/` — End-to-end and integration test fixtures
- `tools/evaluation/` — Evaluation and validation fixtures (synthetic only)
- `docs/validation/` — Validation documentation with sanitized examples

Synthetic fixtures in these paths must:
- Use obviously fake/placeholder data
- Clearly label any audio as synthetic or generated
- Include inline documentation of provenance if non-obvious
- Never include transcripts or derivatives of real private content

### Automated hygiene checks

All pull requests are automatically checked for:

1. **Secret pattern detection** — Common patterns matching API keys, passwords, and private key material
2. **Private capture policy** — Audio files and recordings outside documented fixture paths
3. **Credential isolation** — No credentials passed to scripts or embedded in URLs

The hygiene check runs as part of the standard CI suite and will block merging if violations are detected. See `tools/hygiene/check_hygiene.py` for the exact rules.

### If you accidentally committed secrets

If you discover you've committed secrets or private material:

1. **Do not force-push to main** — Notify a maintainer immediately
2. **Create a follow-up PR** — Remove the sensitive content and add to `.gitignore` if needed
3. **Document the issue** — Link the PR to any audit findings or follow-up actions

Note: Removing from git history does not guarantee the data is unrecoverable from backups or caches. Prevention is far better than cleanup.

### Audio and media guidelines

Audio fixtures are essential for testing speech capture, transcription, and voice processing:

- **Synthetic generated audio** — Always allowed in `fixtures/`, `tests/`, and `**/Tests/` paths
- **Real recordings** — Never commit real speech or personal audio
- **Downloaded/purchased samples** — Only with clear licensing; include provenance in fixture documentation
- **Redacted or scrambled** — For documentation only, clearly marked as redacted

Example fixture documentation:

```python
# fixtures/audio/synthetic_reminder.wav — Synthetic audio:
# Generated from TTS using a neutral system voice. No real speech.
# Provenance: ttsx3 library, en-US voice (not identifiable human)
# Use: Unit tests for audio processing pipelines
```

### Contributing without secrets

**Before submitting a PR:**

1. Review all committed paths — do they contain real credentials, audio, or private data?
2. Run `make hygiene-check` locally — fix any detected issues before pushing
3. Use `.gitignore` for local configuration with real secrets (never commit them)
4. Replace real provider keys with obvious placeholders in tests (e.g., `sk-test-1234...`)
5. Use fixture generators or synthetic data instead of captured real data

**Example `.gitignore` for local development:**

```
# Local configuration with real secrets
.env
.env.local
*.key
*.pem
credentials.json
config.local.json

# Generated build artifacts
target/
build/
.build/
Pods/

# Test and coverage reports
coverage/
.coverage
*.profdata
```

### Review process

Maintainers will review contributions for:

- No real secrets or private credentials
- Audio and media restricted to approved paths
- Fixture documentation is clear and correct
- No private captures disguised as test data

Questions about what belongs in a public repository? Open an issue to discuss before submitting a large contribution.
