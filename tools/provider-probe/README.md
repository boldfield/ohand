# Provider Probe Tool

Authorized synthetic testing of provider endpoints to verify protocol compatibility and reachability.

## Purpose

This tool is used to validate that a configured provider endpoint:
- Is reachable over the network
- Responds with valid API protocol messages
- Properly implements authentication (requires credentials, rejects unauthenticated requests)
- Returns expected response structures
- Supports structured response formats (JSON mode)
- Identifies serving software

The probe is designed to work with externally supplied configuration files and to preserve raw evidence locally while publishing only sanitized findings.

## Usage

```bash
python3 spark_probe.py
```

Requires:
- Python 3.8+
- Provider configuration at `/etc/ohand-provider/models.json` (or override with `OHAND_PROVIDER_CONFIG_PATH`)
- Optional: Provider key via `OHAND_PROVIDER_KEY` (defaults to "ollama")

## Configuration

The probe reads from the location specified by `OHAND_PROVIDER_CONFIG_PATH` or defaults to:
`/etc/ohand-provider/models.json`

This file must contain a JSON structure with provider configuration, including:
- `providers[provider-name].baseUrl`: The base API endpoint URL (required, must not be empty)
- `providers[provider-name].apiKey`: Authentication credential (required, must not be empty)
- `providers[provider-name].api`: API type identifier
- `providers[provider-name].models`: List of available models

The tool fails with exit code 1 if required fields are missing or empty.

## Output

### Sanitized output (stdout)

The tool prints sanitized results suitable for documentation:
- Evidence ID (opaque identifier, no endpoint hash)
- Protocol type (extracted from URL scheme)
- API compatibility information
- Probe results with success/failure status for each test
- No credentials, actual endpoint addresses, or endpoint hashes

### Raw evidence (file)

Raw evidence containing full endpoint address, credentials and configuration is saved to:
- Directory: `~/.local/share/ohand-provider-probe/`
- Filename: `{evidence_id}.json`
- The evidence file includes:
  - Evidence ID
  - Probe script revision (git commit)
  - Collection timestamp
  - Full endpoint and configuration (credentials present)
  - All probe results with response bodies and headers

## Results

The probe records:
- Reachability test (HTTP GET to models endpoint, unauthenticated)
- Models list test (authenticated)
- Chat completion test (synthetic message)
- Response format test (structured JSON response)

Each probe captures:
- Timestamp (UTC)
- Request type
- HTTP status code
- Response structure type
- Authentication status
- Serving software (from Server header)
- Success/failure indication

## Security

- Credentials are loaded from config file only, validated as non-empty
- Raw evidence file is created with restrictive permissions (0600)
- Sanitized output excludes endpoint addresses and credentials
- Endpoint is identified only by opaque evidence ID, never by hash
- Full audit trail preserved in raw evidence with evidence ID reference
- Evidence stored in home directory with per-evidence files (not overwritten)
- Protocol extracted from actual URL, not hardcoded

## Exit codes

- 0: Core reachability tests passed (unauthenticated, models, chat)
- 1: Core reachability tests failed or configuration invalid
