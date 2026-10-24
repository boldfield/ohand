# E02: Live interchangeable backends end-to-end validation

## Summary

This document summarizes the end-to-end validation of live interchangeable backends through the production adapter and proposal application path. The harness runs authorized synthetic captures through multiple configured backends, verifies semantic outcomes against oracle fixtures, exercises error handling, and demonstrates that raw captures remain unchanged when switching between backends.

**STATUS:** E02 acceptance criteria are satisfied. Live evidence collected 2026-10-24 from seven backends (two Anthropic, five self-hosted) at revision ae7444c. All backends demonstrate zero forbidden mutations and durable state under failure scenarios. Anthropic backends (haiku, sonnet) show strong semantic alignment (27/33, 29/33 oracle matches). Self-hosted backends show partial or failed semantic outcomes: deepseek-iq3 matched 24/33 with 7 invalid; others matched ≤16/33 with 16–28 invalid outputs. Three self-hosted models (gpt-oss:20b, gpt-oss:120b, qwen3.5:122b) are marked capability-replaced and unverified. Raw exchanges preserved and cited.

## Evidence collection

Two distinct evidence collection approaches are used:

### No-network proof (CI-runnable, local TLS fixture)

The test `no_network_proof_runs_the_whole_harness` in `core/tests/live_providers/main.rs` runs the complete harness against a local TLS server that mirrors the production protocol:

- Runs all fixtures in `fixtures/intent/contrastive-fixtures.json` (33 fixtures)
- Exercises both self-hosted and hosted (Anthropic) backend paths
- Verifies profile attribution, capture durability, and backend switching correctness
- Asserts all failure scenarios: unreachable endpoints, invalid credentials, revoked profile versions, and malformed responses
- Produces sanitized evidence artifacts with private raw exchanges recorded outside the repository
- Requires only `openssl`, `python3`, and `curl` — no API credentials
- Runs on every CI pipeline; no fixture server restart required; leak check included

**Pass status:** ✓ Confirmed by the Test suite job on the current PR (run 40118139406, commit 0095d0d). The test `no_network_proof_runs_the_whole_harness` exercises all fixtures and failure scenarios against the local TLS fixture server.

### Live provider runs (requires coordinator credentials)

Live end-to-end validation requires access to actual hosted provider APIs (Anthropic and/or OpenAI). The harness command:

```
OHAND_LIVE_PROFILES='self-hosted:<model>,anthropic:<model>,openai:<model>' \
OHAND_LIVE_CREDENTIAL_ENVS='anthropic=ANTHROPIC_API_KEY,openai=OPENAI_API_KEY' \
OHAND_PROVIDER_CONFIG_PATH=/path/to/models.json \
OHAND_LIVE_EVIDENCE_DIR=docs/validation/evidence/live-providers \
OHAND_PRIVATE_EVIDENCE_DIR=~/.ohand-private-evidence \
  cargo test --test live_providers live_providers_end_to_end -- --ignored --nocapture
```

**Current fleet status:** Coordinator-authorized live runs:

- **Run 3 (multi-backend, acceptance complete):** `live-providers-live-20261024T084039065Z-ae7444c8a42ca5b8fd6a46733f85de418e6b13c5.json`  
  Commit: `ae7444c8a42ca5b8fd6a46733f85de418e6b13c5` (current M1 head)  
  Collected: 2026-10-24T08:40:39Z  
  Backends: `anthropic:claude-haiku-5-5`, `anthropic:claude-sonnet-5-5`, `self-hosted:gpt-oss:20b`, `self-hosted:gpt-oss:120b`, `self-hosted:deepseek-flash-iq3:latest`, `self-hosted:qwen3.8:27b`, `self-hosted:qwen3.5:122b`  
  **Status:** Satisfies all acceptance criteria. Two genuinely different backends (Anthropic + self-hosted) both validated. All backends: zero forbidden mutations. Fixture count: 33. Semantic outcomes and latency collected. Instruction version: `0b3a70bffc3baa70ea93a08958b11b6537aea52ca7d4844be8eea54effccb671`. Private raw exchanges in evidence directory with SHA-256: `c98fc589c777aed90f684010905468f6b62dd68f0de0892a0356308183c5540d`.
  
- **Run 1 (cross-provider, pre-F23/F25):** `live-providers-live-20261019T161442499Z-b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d.json`  
  Commit: `b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d` (pre-F23/F25)  
  Collected: 2026-10-19  
  Backends: `self-hosted:gpt-oss:20b` and `anthropic:claude-haiku-5-5`  
  **Status:** Historical reference. Self-hosted leg failed extensively (25 invalid outputs of 33); collected before F23/F25 fixes. Run 3 supersedes this evidence.
  
- **Run 2 (single-provider, reference):** `live-providers-live-20261023T135717890Z-5af79fbdda69b989144ea1a2530f23fcd72c2abc.json`  
  Commit: `5af79fbdda69b989144ea1a2530f23fcd72c2abc`  
  Collected: 2026-10-23  
  Backends: `anthropic:claude-haiku-5-5` and `anthropic:claude-sonnet-5-5` (two models, same provider, same protocol)  
  **Status:** Historical reference. Single provider only; replaced by Run 3.

## Acceptance criteria verification

**All criteria satisfied by Run 3 (2026-10-24).** The run exercises two genuinely different backends (Anthropic protocol + self-hosted), demonstrates zero forbidden mutations across all backends, and provides full semantic outcome reporting with raw evidence preservation.

### 1. Provider/model/config versions, semantic outcomes, latency and cost

**From Run 3 (2026-10-24):**

**Backends tested:** 7 backends, 33 contrastive fixtures from `fixtures/intent/contrastive-fixtures.json`

**Results per backend:**

| Backend | Applied | Abstained | Invalid Output | Oracle Matches | Latency (ms, 30 calls) | Cost Estimate | Profile Version |
|---------|---------|-----------|----------------|----------------|------------------------|---------------|-----------------|
| `anthropic:claude-haiku-5-5` | 24 | 8 | 1 | 27/33 | 76,341 | $0.0166 (30 of 33) | `6c2cd59c-0327-417c-b4ce-9192bffceaaf` |
| `anthropic:claude-sonnet-5-5` | 26 | 7 | 0 | 29/33 | 91,877 | $0.2982 (30 of 33) | `d50074eb-4d61-41ad-8c99-771a54fa153f` |
| `self-hosted:gpt-oss:20b`† | 6 | 0 | 27 | 6/33 | 544,029 | no cost data | `a905348b-d4c3-4036-960b-3ba476ca19bb` |
| `self-hosted:gpt-oss:120b`† | 14 | 0 | 19 | 11/33 | 544,857 | no cost data | `f77581cc-2dc1-4a90-83c5-58ad203d2c30` |
| `self-hosted:deepseek-flash-iq3:latest` | 25 | 1 | 7 | 24/33 | 1,099,816 | no cost data | `fdbc106a-5a2d-470c-b1a5-636fb0ac91e7` |
| `self-hosted:qwen3.8:27b` | 13 | 4 | 16 | 16/33 | 976,129 | no cost data | `bbe89779-f859-4869-aa4d-c8567b60b87d` |
| `self-hosted:qwen3.5:122b`† | 3 | 2 | 28 | 4/33 | 1,333,274 | no cost data | `aac6cdad-8f26-473b-a00e-1e9f056ab296` |

† Marked as `capability_replaced_for_unverified_models` in the artifact; semantic failures prevent certification as a qualifying backend.

**Cost reporting:** Anthropic models report cost estimates from the supplied rates file; estimates are provided per-call and sum the total. Self-hosted models have no per-call cost data.

**Switching and durability:**
- Switching check count: 21 backend pairs, including 10 cross-protocol pairs (e.g., self-hosted:* → anthropic:claude-haiku-5-5)
- Captures changed when switching backends: 0 across all pairs
- Results changed when switching backends: 0 across all pairs
- Attributions changed when switching backends: 0 across all pairs
- Earlier results remain attributed to their own profile version

✓ **Status:** Two genuinely different backend protocols (Anthropic + self-hosted) satisfy zero-forbidden-mutation and durability criteria. Anthropic backends (haiku, sonnet) demonstrate strong semantic alignment (27–29/33 oracle matches). Self-hosted backends show semantic failures; the best-performing (deepseek-flash-iq3, 24/33 matches) is not capability-replaced but shows 7 invalid outputs. Durable state verified across all 21 switching pairs with zero captures or attributions changed.

### 2. Unavailable/invalid response and revoked-credential behavior

**Failure scenarios from Run 3 (2026-10-24):**

| Scenario | Backend | Status | Result | Retriable | Detail |
|----------|---------|--------|--------|-----------|--------|
| `unreachable_endpoint` | `self-hosted:gpt-oss:20b` | ran | backed_off | true | Endpoint unavailable triggers retriable backoff. |
| `invalid_hosted_credential` | `anthropic:claude-haiku-5-5` | ran | failed | false | Unauthorized response (401) is non-retriable. Revoked-credential behavior: provider rejects the credential. |
| `revoked_profile_version` | `self-hosted:gpt-oss:20b` | ran | not_run | false | Job retired by revoke path before any provider call; no request sent. Application-side profile revocation, distinct from provider credential revocation. |
| `malformed_response` | n/a | not_exercised | — | — | Live endpoint cannot be forced to answer with malformed body; no-network proof asserts it against the local fixture server. |

**Observations:**
- `unreachable_endpoint` and `revoked_profile_version` ran against self-hosted:gpt-oss:20b only.
- `invalid_hosted_credential` ran against anthropic:claude-haiku-5-5 only (not self-hosted).
- `malformed_response` was not exercised live; it is covered by the no-network proof test.
- Durability verified: no state changes under failure scenarios.

**Revoked-credential criterion:** The artifact exercises `invalid_hosted_credential` (provider rejects a credential with a 401 Unauthorized response), which demonstrates revoked-credential behavior. The `revoked_profile_version` scenario demonstrates application-side profile revocation (retiring a job before dispatch), which is distinct from credential revocation.

✓ **Status:** Failure scenarios demonstrate durability and correct error handling. Revoked-credential behavior is observed via invalid_hosted_credential (provider 401 on Anthropic). Captures remain durable under all tested failures.

### 3. Zero forbidden mutations for each claimed live backend

**Forbidden mutation counts from Run 3 (2026-10-24), all 33 fixtures per backend:**

| Backend | Forbidden Mutations | correction_changed | lifecycle_left_active | raw_capture_changed | scope_changed |
|---------|---------------------|-------------------|----------------------|---------------------|---------------|
| `anthropic:claude-haiku-5-5` | 0 | 0 | 0 | 0 | 0 |
| `anthropic:claude-sonnet-5-5` | 0 | 0 | 0 | 0 | 0 |
| `self-hosted:gpt-oss:20b` | 0 | 0 | 0 | 0 | 0 |
| `self-hosted:gpt-oss:120b` | 0 | 0 | 0 | 0 | 0 |
| `self-hosted:deepseek-flash-iq3:latest` | 0 | 0 | 0 | 0 | 0 |
| `self-hosted:qwen3.8:27b` | 0 | 0 | 0 | 0 | 0 |
| `self-hosted:qwen3.5:122b` | 0 | 0 | 0 | 0 | 0 |

✓ **Status:** Every backend demonstrates zero forbidden mutations across all 33 fixtures. This confirms the application layer correctly prevents mutation (no captures modified, no lifecycle leaks, no scope changes, no correction mutations) regardless of backend semantic outcome. Semantic performance varies: Anthropic backends show strong alignment (27–29/33 oracle), self-hosted backends show partial/failed outcomes (4–24/33 oracle; 7–28 invalid outputs).

### 4. Evidence artifacts with build revision and collection time

**Artifact inventory:**

| File | Commit | Collected | Backends | Fixtures | Status |
|------|--------|-----------|----------|----------|--------|
| `live-providers-live-20261024T084039065Z-ae7444c8a42ca5b8fd6a46733f85de418e6b13c5.json` | ae7444c8a42ca5b8… | 2026-10-24T08:40:39Z | 7 backends (Anthropic+self-hosted) | 33 | ✓ Acceptance complete |
| `live-providers-live-20261019T161442499Z-b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d.json` | b28ead12… (pre-F23/F25) | 2026-10-19 | self-hosted:gpt-oss:20b + anthropic:claude-haiku-5-5 | 33 | Historical reference |
| `live-providers-live-20261023T135717890Z-5af79fbdda69b989144ea1a2530f23fcd72c2abc.json` | 5af79fbd… | 2026-10-23 | anthropic:claude-haiku-5-5 + anthropic:claude-sonnet-5-5 | 33 | Historical reference |

**Run 3 artifact structure:**
- Public sanitized JSON: `live-providers-live-20261024T084039065Z-ae7444c8a42ca5b8fd6a46733f85de418e6b13c5.json` (committed to `docs/validation/evidence/live-providers/`)
- Private raw exchanges: OHAND_PRIVATE_EVIDENCE_DIR outside repository, mode 0600
  - File: `live-providers-live-20261024T084039065Z-ae7444c8a42ca5b8fd6a46733f85de418e6b13c5.raw.json`
  - Size: 2,555,091 bytes
  - SHA-256: `c98fc589c777aed90f684010905468f6b62dd68f0de0892a0356308183c5540d`
- Reference in artifact: `run_id`, `bytes`, `sha256`, `location` of private raw file
- No endpoint addresses, credentials, or captured text in public artifact
- Model output preserved in private file for leak verification

**No-network proof evidence:**
- Test `no_network_proof_runs_the_whole_harness` in `core/tests/live_providers/main.rs`
- Runs against local TLS fixture server (no live credentials required)
- Exercises all failure scenarios and verifies malformed response handling
- Confirmed passing by Test suite job on the current PR (run 40118139406, commit 0095d0d)

✓ **Status:** All evidence artifacts cited with full build revision, collection timestamp, and private-file hash. Raw evidence preserved outside repository with auditable reference. Two genuinely different backend protocols (Anthropic + self-hosted) satisfy zero-forbidden-mutation and durability criteria across 21 switching pairs. Semantic performance is reported separately: Anthropic backends show strong alignment (27–29/33 oracle matches); self-hosted backends show partial/failed semantic outcomes (4–24/33 oracle; 7–28 invalid outputs), with three models marked as capability-replaced and unverified.

## Implementation notes

### Provider protocol adapter

The harness exercises the production adapter path:
1. **Ingress:** Authorized synthetic capture flows through `InterpretationDispatcher`
2. **Profile configuration:** Route grant and provider selection via production configuration functions
3. **Adapter invocation:** Backend-specific HTTP/protocol handling via registered provider adapters (Anthropic, OpenAI, self-hosted)
4. **Proposal application:** Production domain layer applies or rejects proposals
5. **State durability:** Captures, corrections, and results remain unchanged across backend switches

### Fixture oracle

Contrastive fixtures include:
- Item type classification (task, idea, note, other)
- Source span extraction (note boundaries within audio)
- Reminder proposal (when/timezone for one-shot reminders, quality assessment)
- Session topic proposal (conversation context, source reference)
- Abstention cases (out of scope, malformed, missing context)

Oracle proposal is constructed from fixture expectations; production scorer compares outcomes.

### Configuration and credential binding

- Credential environment variables are named, never passed as values
- Binding validation prevents credential cross-origin leakage
- Self-hosted endpoint is read from provider config at key `ollama` (as in `providers_self_hosted/live_smoke.rs`)
- Profile versions prevent queued work from silently changing destination

## Deployment and future iterations

The no-network proof demonstrates that the entire code path functions correctly against a local TLS fixture server; the harness is ready to run against any configured backend provided credentials are bound in the environment.

**Completion status:** E02 acceptance criteria are satisfied as of 2026-10-24. Coordinator-authorized live run on commit ae7444c demonstrates:
- Two genuinely different backend protocols (Anthropic + self-hosted) with zero forbidden mutations and zero captures changed across all 21 switching pairs
- Full semantic outcome reporting: applied, abstained, invalid_output counts per backend with oracle match rates (Anthropic: 27–29/33; self-hosted: 4–24/33; 3 models capability-replaced as unverified)
- Failure scenario coverage: unreachable endpoints, invalid credentials, profile revocation all exercised correctly with durability verified
- Durable raw evidence: 2.5 MB private exchanges preserved outside repository with auditable hash
- Switching verification: captures, results, and attributions remain unchanged when backend configuration changes (21 pairs checked)
- Cost and latency collected and recorded with metadata for both protocol families (Anthropic: $0.0166–$0.2982; self-hosted: no cost data)

Future deployments can use this evidence as the foundation for ongoing monitoring and extend to additional backends by running the harness with new provider credentials and recording results with the same artifact structure.
