# Provider Probe Tool

Authorized synthetic testing of provider endpoints to verify protocol compatibility and reachability.

## Purpose

This tool is used to validate that a configured provider endpoint:
- Is reachable over the network
- Responds with valid API protocol messages
- Properly implements authentication
- Returns expected response structures

The probe is designed to work with externally supplied configuration files and to preserve raw evidence locally while publishing only sanitized findings.

## Usage

```bash
python3 spark_probe.py
```

Requires:
- Python 3.8+
- Provider configuration at `/etc/ohand-provider/models.json` (or override with environment)

## Configuration

The probe reads from the standard location: `/etc/ohand-provider/models.json`

This file must contain a JSON structure with provider configuration, including:
- `providers[provider-name].baseUrl`: The base API endpoint URL
- `providers[provider-name].apiKey`: Authentication credential
- `providers[provider-name].api`: API type identifier
- `providers[provider-name].models`: List of available models

## Output

### Sanitized output (stdout)

The tool prints sanitized results suitable for documentation:
- Endpoint identifier (hash, not the actual URL)
- Protocol type
- API compatibility information
- Probe results with success/failure status
- No credentials or actual endpoint addresses

### Raw evidence (file)

Raw evidence containing full endpoint address, credentials and configuration is saved to:
- Default: `/tmp/spark-probe-evidence.json`
- The evidence includes an audit reference (timestamp-based)

## Results

The probe records:
- Reachability test (HTTP GET to models endpoint)
- Models list test (with authentication)
- Chat completion test (synthetic message)

Each probe captures:
- Timestamp (UTC)
- HTTP status code
- Response structure type
- Authentication status
- Success/failure indication

## Security

- Credentials are loaded from config file only
- Raw evidence file is created with restrictive permissions (0600)
- Sanitized output excludes endpoint addresses and credentials
- Endpoint is identified only by secure hash in output
- Full audit trail preserved in raw evidence with timestamp reference

## Exit codes

- 0: All reachability tests passed
- 1: Could not reach endpoint
