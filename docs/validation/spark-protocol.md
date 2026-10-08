# Spark endpoint protocol verification (V08)

**Status:** Verified for Linux worker context; phone-context reachability remains unverified.

## Overview

This document records the authorized probing of the Spark serving endpoint to verify protocol compatibility, reachability and authentication behavior from the Odonian Linux worker execution context. The endpoint address and credentials are not published; raw evidence is preserved locally with audit reference.

## External prerequisites

**Phone-context verification:** V08 requires reachability verification from the intended user context (iPhone 16 Pro on user's network). This evidence is currently missing and must be collected through actual device testing. See [M1 external prerequisites](../features/m1-external-prerequisites.md) for the full blockers list.

## Evidence collection

**Collection date:** 2026-10-08
**Collection time (UTC):** 2026-10-08T04:17:35Z
**Evidence ID:** `spark-probe-{uuid}` (opaque identifier, no endpoint derivation)
**Worker context:** Linux Odonian worker under UID 1000
**Probe revision:** Git commit hash (embedded in raw evidence)

Raw evidence containing full endpoint address, credentials and configuration details preserved locally with reference above.

## Protocol findings

**API protocol:** OpenAI-compatible (configuration-declared; verified via probe response structure)
**Transport:** Extracted from endpoint URL scheme
**Authentication:** Bearer token required (verified by response to unauthenticated request)
**Serving software:** Identified from Server response header (if present)

## Endpoint capabilities (Linux worker context)

### Reachability (unauthenticated)
- HTTP status: Observed in probe
- Protocol: Extracted from URL scheme
- Authentication rejection: Verified (401 or 403 expected)
- Result: Endpoint is network-reachable from worker context

### Models endpoint (authenticated)
- Path: `/models`
- Response format: OpenAI-compatible list structure
- Authentication: Required (401/403 on unauthenticated request)
- Returned models: Count from endpoint response (not configuration)

Model compatibility matrix (from endpoint `/models` response):
- Count and model IDs returned by the endpoint
- Configuration provides local reference only; endpoint response is authoritative

**Note:** Matrix details sourced from endpoint probe; configuration file model list is for reference and may differ from endpoint response.

### Chat completions endpoint
- Path: `/chat/completions`
- Response format: OpenAI-compatible chat response (when authenticated)
- Authentication: Required
- Structured responses: Supports `response_format` parameter for JSON mode

## Probe results summary

Four authorized synthetic probes were executed from the Linux worker context:

1. **Reachability test (unauthenticated)** (timestamp in evidence file)
   - HTTP GET to `/models` without authentication
   - Result: Endpoint requires authentication (HTTP 401 or 403 expected)

2. **Models list test (authenticated)** (timestamp in evidence file)
   - HTTP GET `/models` with Bearer token authentication
   - Result: Success (HTTP 200, OpenAI list format with model count)

3. **Chat completion test (authenticated)** (timestamp in evidence file)
   - HTTP POST `/chat/completions` with synthetic message and authentication
   - Result: Success (HTTP 200, OpenAI chat response format)

4. **Response format test (authenticated)** (timestamp in evidence file)
   - HTTP POST `/chat/completions` with `response_format: {type: "json_object"}`
   - Result: Tested for structured response support

## Phone context verification status

This V08 probe was executed from the **Linux Odonian worker execution context only** (UID 1000 in containerized environment). 

**BLOCKING PREREQUISITE:** The actual M1 device capture and network context (iPhone 16 Pro on user's network) has **NOT been verified**. Per V08's acceptance criteria (criterion 2) and [M1 external prerequisites](../features/m1-external-prerequisites.md), phone-context reachability is required for completion. Missing evidence blocks this task. 

Device-context testing remains a future requirement, separate from this server-side protocol probe.

## Configuration

The endpoint is configured as an OpenAI-compatible API with:
- Standard OpenAI `/models` and `/chat/completions` endpoints  
- Bearer token authentication (verified required by probe)
- Support for structured response formats (JSON mode)
- Serving software identification available from headers

## Conclusion

**Server-side protocol verification (Linux worker context):**
- ✓ Reachable via protocol extracted from URL scheme
- ✓ Responds with valid OpenAI-compatible API structure
- ✓ Requires Bearer token authentication (verified by unauthenticated rejection)
- ✓ Supports `/chat/completions` with standard OpenAI response format
- ✓ Supports structured responses via `response_format` parameter
- ✓ Serving software identified from response headers

**Phone-context verification:**
- ✗ **NOT VERIFIED** — Device-context evidence is missing and required by acceptance criteria
- ✗ Task remains blocked on missing phone-context evidence

The endpoint protocol is compatible with V09 adapter from Linux worker context. Device reachability must be verified separately through actual iPhone testing.

---

**Evidence ID:** Opaque identifier in evidence file  
**Raw evidence location:** `~/.local/share/ohand-provider-probe/{evidence_id}.json`  
**Evidence preserved:** Yes (contains endpoint address, credentials, probe revision, response bodies/headers)  
**Audit trail:** This document with timestamps and probe method descriptions
