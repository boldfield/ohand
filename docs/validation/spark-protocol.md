# Spark serving protocol verification

Status: **Pending worker-context evidence** — V08a implementation complete. Phone-context reachability evidence is owned by V08b and collected on the maintainer's iPhone over Tailscale, not from the Odonian worker.

## Protocol classification (Worker context)

This section records the Spark endpoint protocol observations collected by the V08a probe, running in the Odonian worker environment from authorized synthetic requests to the provider configuration.

### Authentication requirement

Spark endpoint authentication verification:

- **Status**: Pending evidence artifact
- **Evidence identifier**: [To be populated by probe run]
- **Probe revision**: [To be populated by probe run]
- **Collection time**: [To be populated by probe run]

The probe sends an unauthenticated GET request to the endpoint's `/models` path. A 401 or 403 response indicates authentication is required. A 200 response with valid JSON model list indicates authentication is not required.

### Model listing

Spark endpoint model enumeration:

- **Status**: Pending evidence artifact
- **Evidence identifier**: [To be populated by probe run]
- **Models**: [To be populated by probe run]

The probe sends an authenticated GET request to the endpoint's `/models` path using the configured bearer credential. The response body is parsed to extract available model identifiers.

### OpenAI compatibility

Spark endpoint OpenAI /chat/completions compatibility:

- **Status**: Pending evidence artifact
- **Evidence identifier**: [To be populated by probe run]

The probe sends an authenticated POST request to the endpoint's `/chat/completions` path with a minimal message. The response is parsed to verify the expected structure (choices array with message objects containing role and content fields).

### TLS and transport

Spark endpoint transport configuration:

- **Status**: Worker context only — scheme and certificate validation as configured
- **Evidence identifier**: [To be populated by probe run]

The probe records the transport scheme configured in the provider configuration file and whether the endpoint's TLS certificate validates against the system's default trust store. The endpoint may be served over cleartext HTTP on a private network; the recorded scheme reflects the configuration as observed, not an assumption.

Cleartext HTTP endpoints require an explicit, reviewed architectural allowance when used by adapters like V09. This evidence records what is configured; V09 implementation and security review are separate gates.

## Phone-context reachability

**This document records only what the Odonian worker can observe over its own network.**

Phone-context reachability — reaching the Spark endpoint from the maintainer's iPhone over Tailscale on both Wi-Fi and cellular — is an external input owned by V08b. The worker, Mac and simulator network contexts are not phone evidence and are not documented here. Phone reachability does not gate V08a approval; it is a distinct evidence input that gates V09 implementation.

See [M1 external prerequisites: Phone-context evidence for V08b](../features/m1-external-prerequisites.md#phone-context-evidence-for-v08b) for the phone-context collection procedure and its role in V08b and V09.

## Evidence artifacts

Sanitized evidence collected by the probe is committed under `docs/validation/evidence/spark-probe/` as JSON files named with the collection timestamp and evidence identifier.

Artifacts contain only:
- Evidence identifier, probe revision, collection time
- Request details: method, path, credential sent (yes/no), HTTP status
- Response headers: server, content-type, date (allowlisted)
- Parsed model identifiers from /models endpoint
- Structured response result from /chat/completions

Artifacts never contain:
- Endpoint address
- Credential value
- Any hash of the credential or address

The Kubernetes Secret `ohand-spark-provider` and its mount revision remain the auditable private reference (see [M1 external prerequisites](../features/m1-external-prerequisites.md)).
