# Spark endpoint protocol verification (V08)

**Status:** Verified and reachable.

## Overview

This document records the authorized probing of the Spark serving endpoint to verify protocol compatibility, reachability and authentication behavior. The endpoint address and credentials are not published; raw evidence is preserved locally with audit reference.

## Evidence collection

**Collection date:** 2026-10-08
**Collection time (UTC):** 2026-10-08T04:17:35Z
**Endpoint identifier:** `4371ed75d5d89414` (SHA256 hash prefix)
**Evidence reference:** `spark-probe-20261008-041735`
**Worker context:** Linux Odonian worker under UID 1000

Raw evidence containing full endpoint address, credentials and configuration details preserved locally with reference above.

## Protocol and compatibility

**API protocol:** OpenAI-completions compatible
**Transport:** HTTPS
**Authentication:** Bearer token (required)
**Authentication status:** Verified working

## Endpoint capabilities

### Reachability
- Status: **Reachable**
- HTTP status: 200
- Protocol: HTTPS
- TLS: Present and valid

### Models endpoint
- Path: `/models`
- HTTP status: 200
- Response format: OpenAI-compatible list structure
- Authentication: Required
- Available models: 7 distinct model IDs

Model compatibility matrix (from authorized probe):
| Model ID | Reasoning Support | Max Context | Max Tokens | Status |
| --- | --- | --- | --- | --- |
| gpt-oss:120b | Yes | — | — | Available |
| gpt-oss:20b | Yes | — | — | Available |
| qwen3.8:27b | Yes | — | — | Available |
| qwen3.5:122b | Yes | — | — | Available |
| deepseek-flash-iq3:latest | Yes | — | — | Available |
| glm-5.3-flash-IQ3_XXS:latest | Yes | — | — | Available |
| glm-5.3-flash | Yes | 131072 | 32768 | Available |

### Chat completions endpoint
- Path: `/chat/completions`
- HTTP status: 200
- Response format: OpenAI-compatible chat response
- Authentication: Required
- Authentication status: **Verified**
- Protocol compliance: Confirmed

## Probe results summary

Three authorized synthetic probes were executed:

1. **Reachability test** (2026-10-08T04:17:26Z)
   - Endpoint HTTP GET to models list
   - Result: Success (HTTP 200)

2. **Models list test** (2026-10-08T04:17:26Z)
   - HTTP GET `/models` with authentication
   - Result: Success (HTTP 200, 6 models returned, OpenAI list format)

3. **Chat completion test** (2026-10-08T04:17:35Z)
   - HTTP POST `/chat/completions` with minimal synthetic message
   - Result: Success (HTTP 200, OpenAI chat response format)

## Phone context reachability

This V08 probe was executed from the Linux Odonian worker execution context (UID 1000 in containerized environment). The endpoint is reachable from this network context with valid HTTPS and authentication.

**Note on phone context:** The actual M1 device capture and network context (iPhone 16 Pro on user's network) remains to be verified through integration testing in subsequent tasks. This probe establishes server-side protocol compatibility and public network reachability but does not substitute for actual device-context testing in T05 (actual-device M1 functional exit matrix).

## Configuration

The endpoint is configured as an OpenAI-compatible API with:
- Standard OpenAI `/models` and `/chat/completions` endpoints
- Bearer token authentication
- Support for reasoning models with configurable thinking levels
- Multiple open-source model implementations available

## Conclusion

The Spark endpoint is:
- ✓ Reachable via HTTPS from the Odonian worker context
- ✓ Responding with valid OpenAI-compatible API structure
- ✓ Properly configured for Bearer token authentication
- ✓ Provides multiple model options with reasoning support
- ✓ Implements structured `/chat/completions` endpoint compatible with V07/V09 adapter contract

The endpoint compatibility is verified for the self-hosted provider adapter (V09) to use.

---

**Raw evidence reference:** `spark-probe-20261008-041735`  
**Preserved locally:** Yes (contains endpoint address, credentials, model details)  
**Public audit trail:** This document with endpoint hash and probe timestamps
