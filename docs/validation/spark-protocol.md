# Spark serving protocol verification

Status: **Worker-context evidence collection pending (V08a)** — the Spark endpoint is not reachable from the Odonian worker environment at this time. Phone-context reachability evidence is owned by V08b and collected on the maintainer's iPhone over Tailscale, not from the Odonian worker.

## Protocol classification (Worker context)

This section will record the Spark endpoint protocol observations collected by the V08a probe, running in the Odonian worker environment from authorized synthetic requests to the provider configuration.

### Authentication requirement

Spark endpoint authentication verification:

- **Status**: [To be populated by probe run]
- **Evidence identifier**: [To be populated by probe run]
- **Probe revision**: [To be populated by probe run]
- **Collection time**: [To be populated by probe run]

The probe sends an unauthenticated GET request to the endpoint's `/models` path. The response status and authentication requirement will be recorded.

### Model listing

Spark endpoint model enumeration:

- **Status**: [To be populated by probe run]
- **Evidence identifier**: [To be populated by probe run]
- **Models**: [To be populated by probe run]
- **Response status**: [To be populated by probe run]
- **Response headers**: [To be populated by probe run]

The probe sends an authenticated GET request to the endpoint's `/models` path using the configured bearer credential. The response will be parsed as a valid JSON object with a `data` array containing model identifiers.

### Structured response (OpenAI-compatible JSON format)

Spark endpoint OpenAI /chat/completions compatibility with JSON response format:

- **Status**: [To be populated by probe run]
- **Evidence identifier**: [To be populated by probe run]
- **Model tested**: [To be populated by probe run]
- **Response status**: [To be populated by probe run]
- **Response headers**: [To be populated by probe run]
- **Content format**: [To be populated by probe run]

The probe sends an authenticated POST request to the endpoint's `/chat/completions` path with a message and an explicit JSON response format request. The endpoint must return valid JSON structure with `choices`, `message`, `role`, and `content` fields, and the message content must parse as valid JSON matching the requested schema.

### TLS and transport

Spark endpoint transport configuration:

- **Status**: [To be populated by probe run]
- **Evidence identifier**: [To be populated by probe run]
- **Scheme**: [To be populated by probe run]
- **TLS verified**: [To be populated by probe run]

The probe records the transport scheme configured in the provider configuration file. For HTTPS endpoints, TLS certificate verification against the default trust store is performed and recorded.

This evidence records what is configured and observed; V09 implementation and security review are separate gates.

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
