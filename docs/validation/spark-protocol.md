# Spark serving protocol verification

Status: **Worker-context evidence collected (V08a)** — server-side protocol verification complete. Phone-context reachability evidence is owned by V08b and collected on the maintainer's iPhone over Tailscale, not from the Odonian worker.

## Protocol classification (Worker context)

This section records the Spark endpoint protocol observations collected by the V08a probe, running in the Odonian worker environment from authorized synthetic requests to the provider configuration.

### Authentication requirement

Spark endpoint authentication verification:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T07-54-20.140534z00-00
- **Probe revision**: 4fee1f6ec9faa36820cbf99a9ba91edd88c9173a (worker tree dirty at collection time)
- **Collection time**: 2026-10-08T07:54:20+00:00

The probe sends an unauthenticated GET request to the endpoint's `/models` path. Response: **401 Unauthorized**. The endpoint requires authentication.

### Model listing

Spark endpoint model enumeration:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T07-54-20.140534z00-00
- **Models**: qwen-long, qwen-max
- **Response status**: 200 OK
- **Response headers**: 
  - Server: BaseHTTP/0.6 Python/3.11.2
  - Date: Thu, 08 Oct 2026 07:54:20 GMT
  - Content-Type: application/json

The probe sends an authenticated GET request to the endpoint's `/models` path using the configured bearer credential. The response parses as a valid JSON object with a `data` array containing model identifiers.

### OpenAI compatibility

Spark endpoint OpenAI /chat/completions compatibility:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T07-54-20.140534z00-00
- **Model tested**: qwen-long (first available from /models)
- **Response status**: 200 OK
- **Response headers**:
  - Server: BaseHTTP/0.6 Python/3.11.2
  - Date: Thu, 08 Oct 2026 07:54:20 GMT
  - Content-Type: application/json

The probe sends an authenticated POST request to the endpoint's `/chat/completions` path with a minimal message using the first model reported by the endpoint. The response parses as a valid JSON object with the expected structure: `choices` array containing message objects with `role` and `content` fields. The message content is non-empty and valid.

### TLS and transport

Spark endpoint transport configuration:

- **Status**: Observed
- **Evidence identifier**: spark-2026-10-08T07-54-20.140534z00-00
- **Scheme**: HTTP (cleartext)
- **TLS verified**: No (cleartext HTTP endpoint)

The probe records the transport scheme configured in the provider configuration file. The endpoint is reachable over cleartext HTTP on the private network. The endpoint may require an explicit architectural allowance for cleartext communication when used by adapters like V09.

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
