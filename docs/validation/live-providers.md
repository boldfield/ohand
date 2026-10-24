# E02: Live interchangeable backends end-to-end validation

## Summary

This document summarizes the end-to-end validation of live interchangeable backends through the production adapter and proposal application path. The harness runs authorized synthetic captures through multiple configured backends, verifies semantic outcomes against oracle fixtures, exercises error handling, and demonstrates that raw captures remain unchanged when switching between backends.

**BLOCKING STATUS:** E02 currently lacks valid evidence for two genuinely different live backends both passing acceptance criteria. See blocking condition at end of document.

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

**Pass status:** ✓ Confirmed on commit `5e6a169` (HEAD at test run time)

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

**Current fleet status:** The Odonian worker fleet holds no Anthropic or OpenAI credentials. Coordinator-authorized live runs have been completed previously:

- **Run 1 (cross-provider):** `live-providers-live-20261019T161442499Z-b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d.json`  
  Commit: `b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d` (pre-F23/F25)  
  Collected: 2026-10-19  
  Backends: `self-hosted:gpt-oss:20b` and `anthropic:claude-haiku-5-5`  
  **Status:** Provides the only evidence with two genuinely different backends, but self-hosted leg failed extensively (25 invalid outputs of 33). Cannot certify self-hosted by Anthropic agreement (spec criterion 3).
  
- **Run 2 (single-provider):** `live-providers-live-20261023T135717890Z-5af79fbdda69b989144ea1a2530f23fcd72c2abc.json`  
  Commit: `5af79fbdda69b989144ea1a2530f23fcd72c2abc`  
  Collected: 2026-10-23  
  Backends: `anthropic:claude-haiku-5-5` and `anthropic:claude-sonnet-5-5` (two models, same provider, same protocol)  
  **Status:** Does not satisfy "two genuinely different backends" requirement (both Anthropic protocol/adapter).

## Acceptance criteria verification

**CRITICAL: The task requires two genuinely different backends (e.g., Anthropic + self-hosted, or Anthropic + OpenAI). The available evidence does not satisfy this requirement:**

- **Run 1 (2026-10-19):** Contains two different backends (self-hosted:gpt-oss:20b + anthropic:claude-haiku-5-5) but self-hosted leg failed (25 invalid outputs of 33 fixtures). Cannot certify by another model's success.
- **Run 2 (2026-10-23):** Contains two Anthropic models (haiku + sonnet) from one provider/protocol, not genuinely different backends.

### 1. Provider/model/config versions, semantic outcomes, latency and cost

**From most recent live evidence (2026-10-23 run, single-provider only):**

**Backends tested:**
- `anthropic:claude-haiku-5-5` (model: claude-haiku-5-5)
- `anthropic:claude-sonnet-5-5` (model: claude-sonnet-5-5)

**Fixtures:** 33 contrastive fixtures from `fixtures/intent/contrastive-fixtures.json`

**Results per backend:**

| Backend | Applied | Abstained | Invalid Output | Oracle Matches | Instruction Version |
|---------|---------|-----------|----------------|----------------|---------------------|
| `anthropic:claude-haiku-5-5` | 25 | 7 | 1 | 28/33 | `89eff04b128f6e3b8a9ebff15186595bf6502f3aedf8fe2ecdecf048e9082f17` |
| `anthropic:claude-sonnet-5-5` | 26 | 7 | 0 | 29/33 | `89eff04b128f6e3b8a9ebff15186595bf6502f3aedf8fe2ecdecf048e9082f17` |

**Switching and durability:**
- Switching check count: 1
- Captures changed when switching backends: 0
- Earlier results remain attributed to their own profile version

⚠ **Partial status:** Single-provider (Anthropic) evidence collected with proper versions, latency, tokens, and outcome counts. **Blocked on:** Two genuinely different backends both passing acceptance criteria.

### 2. Unavailable/invalid response and revoked-credential behavior

**Failure scenarios from live evidence (2026-10-23 run):**

| Scenario | Backend | Status | Result | Retriable | Detail |
|----------|---------|--------|--------|-----------|--------|
| `unreachable_endpoint` | `self-hosted:claude-haiku-5-5` | ran | backed_off | true | Endpoint unavailable triggers retriable backoff. **Harness labeling issue:** `self-hosted:claude-haiku-5-5` is mislabeled; no self-hosted backend was configured in this run. |
| `invalid_hosted_credential` | `anthropic:claude-haiku-5-5` | ran | failed | false | Unauthorized response is non-retriable |
| `revoked_profile_version` | `anthropic:claude-haiku-5-5` | ran | not_run | false | Job retired by revoke path before provider call; no request sent. **Status:** Revoked profile version satisfies "revoked-credential" criterion. |
| `malformed_response` | n/a | not_exercised | — | — | Live endpoint cannot be forced to answer with malformed body; no-network proof asserts it |

**Cross-provider run (2026-10-19) failure scenarios:**
- `unreachable_endpoint`: Ran against self-hosted leg, triggered backoff (retriable)
- Other scenarios: Tested against Anthropic leg

**Absence of API credentials for fleet workers:**
- Odonian fleet containers hold no Anthropic or OpenAI credentials
- Live proof is blocked by missing required external input (non-Anthropic verified backend)
- This is recorded as a concrete operational constraint, not a fabricated completion

⚠ **Partial status:** Revoked credential behavior (via profile version) demonstrated against Anthropic. Missing: Non-Anthropic backend running successfully through failure scenarios to complete "two genuinely different backends" requirement.

### 3. Zero forbidden mutations for each claimed live backend

**Forbidden mutation counts (2026-10-23 single-provider run):**

| Backend | Forbidden Mutations | correction_changed | lifecycle_left_active | raw_capture_changed | scope_changed |
|---------|---------------------|-------------------|----------------------|---------------------|---------------|
| `anthropic:claude-haiku-5-5` | 0 | 0 | 0 | 0 | 0 |
| `anthropic:claude-sonnet-5-5` | 0 | 0 | 0 | 0 | 0 |

**Cross-provider run (2026-10-19) status:**
- **anthropic:claude-haiku-5-5:** 0 forbidden mutations (independent verification holds)
- **self-hosted:gpt-oss:20b:** 0 forbidden mutations reported, but backend failed on 25 of 33 fixtures (invalid_output). Cannot certify by another model's success; backend itself unverified.

⚠ **Partial status:** Anthropic backends (both runs) show zero forbidden mutations. Blocked on: Non-Anthropic backend with functional execution (not failed/invalid_output state) required to satisfy "two genuinely different backends" with proper verification.

### 4. Evidence artifacts with build revision and collection time

**Artifact inventory:**

| File | Commit | Collected | Backends | Status |
|------|--------|-----------|----------|--------|
| `live-providers-live-20261019T161442499Z-b28ead121c5c9217c9cc26bc1cd4ed8d4a51fc0d.json` | b28ead12… (pre-F23/F25) | 2026-10-19 | self-hosted:gpt-oss:20b + anthropic:claude-haiku-5-5 | Cross-provider pair, but self-hosted leg invalid (25/33 failed) |
| `live-providers-live-20261023T135717890Z-5af79fbdda69b989144ea1a2530f23fcd72c2abc.json` | 5af79fbd… | 2026-10-23 | anthropic:claude-haiku-5-5 + anthropic:claude-sonnet-5-5 | Single provider, both backends passing |

**Artifact structure (both runs):**
- Public sanitized JSON (committed to `docs/validation/evidence/live-providers/`)
- Private raw exchanges (OHAND_PRIVATE_EVIDENCE_DIR outside repository, mode 0600)
- Reference in artifact: `run_id`, `bytes`, `sha256` of private raw file
- No endpoint addresses, credentials, or captured text in public artifact
- Model output preserved in private file for leak verification

**No-network proof evidence:**
- Test `no_network_proof_runs_the_whole_harness` in `core/tests/live_providers/main.rs`
- Runs against local TLS fixture server (no live credentials required)
- Exercises all failure scenarios and verifies malformed response handling
- Confirmed passing on commit `5e6a169`

⚠ **Partial status:** Evidence artifacts cited with build revision and collection time. Blocking: Every run must demonstrate two genuinely different backends both passing; current runs show either failed non-Anthropic backend (2026-10-19) or only one provider (2026-10-23).

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

**Blocking condition:** E02 cannot complete acceptance criteria until the coordinator runs the harness against two genuinely different backends (e.g., Anthropic + self-hosted or Anthropic + OpenAI) and commits the resulting artifact. Current evidence:
- **2026-10-19:** Cross-provider pair available but self-hosted leg failed extensively (invalid_output on 25/33 fixtures), collected before F23/F25 fixes, oracle match count used superseded oracle version
- **2026-10-23:** Current build and oracle version, but only single provider (Anthropic models)

Missing external input: A verified non-Anthropic backend run against the current harness and oracle at a current build revision. Fleet continues independent work; missing external input is not a reason to weaken checks or fabricate results.
