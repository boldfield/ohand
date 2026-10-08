# Spark serving protocol verification

Status: **Worker-context evidence collected (V08a)** — server-side protocol verification complete. Phone-context reachability evidence is owned by V08b and collected on the maintainer's iPhone over Tailscale, not from the Odonian worker.

## Protocol classification (Worker context)

This section records the Spark endpoint protocol observations collected by the V08a probe, running in the Odonian worker environment from authorized synthetic requests to the provider configuration.

### Authentication requirement

Spark endpoint authentication verification:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T08-28-03.761604z00-00
- **Probe revision**: a136f4ae4d3bdd4f301f036b8402d85bf7161761
- **Collection time**: 2026-10-08T08:28:03.761604+00:00

The probe sends an unauthenticated GET request to the endpoint's `/models` path. Response: **200 OK**. The endpoint does not require authentication for the models endpoint.

### Model listing

Spark endpoint model enumeration:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T08-28-03.761604z00-00
- **Models**: deepseek-flash-iq3:latest, qwen3.8:27b, qwen3.5:122b, gpt-oss:120b, gpt-oss:20b, glm-5.3-flash
- **Response status**: 200 OK
- **Response headers**: 
  - Content-Type: application/json
  - Date: Thu, 08 Oct 2026 08:28:03 GMT

The probe sends an authenticated GET request to the endpoint's `/models` path using the configured bearer credential. The response parses as a valid JSON object with a `data` array containing model identifiers.

### OpenAI compatibility

Spark endpoint OpenAI /chat/completions compatibility:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T08-28-03.761604z00-00
- **Model tested**: deepseek-flash-iq3:latest (first available from /models)
- **Response status**: 200 OK
- **Response headers**:
  - Server: nginx/1.27.5
  - Content-Type: application/json
  - Date: Thu, 08 Oct 2026 08:28:08 GMT
- **Content format**: The endpoint returns valid JSON structure with `choices`, `message`, `role`, and `content` fields. However, the message content is plain text, not JSON. The probe requests JSON response format implicitly but the endpoint returns natural language text.

The probe sends an authenticated POST request to the endpoint's `/chat/completions` path with a minimal message using the first model reported by the endpoint. The response parses as a valid JSON object with the expected structure: `choices` array containing message objects with `role` and `content` fields. The message content is valid but returns plain text (e.g., "Hello! How can I help you today?") rather than JSON-structured data.

### TLS and transport

Spark endpoint transport configuration:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T08-28-03.761604z00-00
- **Scheme**: HTTPS
- **TLS verified**: Yes (default trust store certificate validation successful)

The probe records the transport scheme configured in the provider configuration file. The endpoint is reachable over HTTPS with successful TLS certificate verification against the default trust store.

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
