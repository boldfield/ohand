# Live interchangeable backends (E02)

This report covers task E02. Every figure below is read from a committed, sanitized artifact in
`docs/validation/evidence/live-providers/`. Each artifact records its build revision and collection time,
and points at raw request/response bodies kept privately (outside Git, mode 0600) by SHA-256. All captures are
synthetic (`fixtures/intent/contrastive-fixtures.json`). Cost figures are labelled estimates from a supplied
rates file, not billed amounts.

## Claimed backends

Claimed live backends, both from the qualifying run below:

- `anthropic:claude-haiku-5-5` (Anthropic protocol, adapter and endpoint)
- `openai:gpt-6-luna` (OpenAI protocol, adapter and endpoint)

`openai:gpt-5.5` ran in the same run and is reported but not separately claimed. Self-hosted backends
(gpt-oss, qwen, deepseek) from the 2026-10-24 run are **not claimed**: they ended with 7 to 28 `invalid_output`
results per 33 fixtures, and gpt-oss:20b, gpt-oss:120b and qwen3.5:122b were capability-replaced as unverified
models in that artifact. The two Claude and OpenAI backends are not certified by each other; each is judged on
its own row below.

## Qualifying run

Artifact: `live-providers-live-20261026T042529571Z-27d573faf269f83658229102fcc3f9dca864e9cc.json`
Build revision: `27d573faf269f83658229102fcc3f9dca864e9cc` (clean). Collected: 2026-10-26T04:25:29Z.
Fixtures: 35. Instruction version: `798036ed8db89475886471020bf1db1a45d9a15da10be6a73d12aa1fa80e520e`.
Private raw evidence SHA-256: `cc85d6bd…0aa5` (942,211 bytes).

| Backend | Profile (config) version | Applied | Abstained | invalid_output | Other failures | Matched oracle | Forbidden mutations |
|---|---|---|---|---|---|---|---|
| anthropic:claude-haiku-5-5 | `895b3de7-855c-4046-9759-a09efc481318` | 28 | 7 | 0 | 0 | 34/35 | 0 |
| openai:gpt-6-luna | `6c6ebf1a-7557-43e2-bdca-f6b3e8073c64` | 29 | 6 | 0 | 0 | 34/35 | 0 |
| openai:gpt-5.5 (reported) | `93793a40-2d9a-4a41-b9ec-adf14f33074e` | 29 | 6 | 0 | 0 | 34/35 | 0 |

Oracle misses: haiku and gpt-5.5 missed `question-reflection`; gpt-6-luna missed `split-clock-task-day`.

Latency, tokens and cost (summed over provider calls; 3 of the 35 fixtures per backend made no provider call):

| Backend | Provider calls | Latency sum (ms) | Reported tokens in / out | Cost estimate |
|---|---|---|---|---|
| anthropic:claude-haiku-5-5 | 32 | 83,118 | 107,725 / 15,993 | $0.0004711 per record shown; total $0.018769 over 32 records |
| openai:gpt-6-luna | 32 | 120,387 | 52,609 / 10,284 | not computed (no OpenAI rates supplied) |
| openai:gpt-5.5 | 33 | 123,080 | 52,460 / 9,264 | not computed (no OpenAI rates supplied) |

The cost of the OpenAI legs is unknown, not zero.

Switching: the artifact records 3 pairwise switches (haiku to gpt-6-luna, haiku to gpt-5.5, gpt-6-luna to
gpt-5.5). Each checked 35 captures and found 0 captures, 0 results and 0 attributions changed. The first two are
cross-protocol (Anthropic to OpenAI).

## Failure behavior (same artifact)

Each scenario ran on exactly one backend:

| Scenario | Backend | Outcome |
|---|---|---|
| unreachable_endpoint | `self-hosted:claude-haiku-5-5` (a loopback endpoint with no listener; the label reuses the model name, a harness labelling quirk) | backed_off, reason `unavailable`, retriable, capture durable, 1 call, 0 applied |
| invalid_hosted_credential | anthropic:claude-haiku-5-5 | failed, reason `unauthorized`, not retriable, capture durable, 1 call, 0 applied |
| revoked_profile_version | anthropic:claude-haiku-5-5 | not_run, reason `profile_revoked`, 0 provider calls, capture durable |
| malformed_response | n/a | **not exercised live** in this run |

Earlier OpenAI-only runs (`…20261025T054428997Z-71e0091b…json`, `…20261025T135141329Z-2e110bf7…json`) show the
same invalid-credential and profile-revocation outcomes on `openai:gpt-5.5` and `openai:gpt-6-luna`. The
`openai:gpt-5.5` leg of the first run failed 32 of 35 fixtures with HTTP 400 `rejected`, with zero forbidden
mutations (fail-safe, but not a working backend at that revision).

### Revoked credential

`revoked_profile_version` is application-side retirement before dispatch; it is **not** evidence of a provider
rejecting a revoked credential. The real credential-revocation observation is the E02b pair on Anthropic with a
throwaway key, at build `9931e61a12b1497a9b7c4e89ad85bd2db12ed993`:

- before: `live-providers-revocation-before-20261027T211914302Z-9931e61a….json`, collected 2026-10-27T21:19:14Z.
  Outcome `applied`, route `provider`, 1 provider call, 1 applied proposal.
- after the key was revoked: `live-providers-revocation-after-20261027T211947818Z-9931e61a….json`, collected
  2026-10-27T21:19:47Z. Outcome `failed`, reason `unauthorized`, not retriable, 1 provider call, 0 applied,
  capture durable, 0 forbidden mutations.

This covers Anthropic only. Revocation of an OpenAI key was not exercised. The older `invalid_hosted_credential`
scenario uses a constant credential that was never valid; it is not revocation evidence.

## Zero forbidden mutations

Every record in every committed live artifact reports 0 for `raw_capture_changed`, `correction_changed`,
`scope_changed` and `lifecycle_left_active`. For the qualifying run this is 105 records (35 per backend, 3
backends), 0 mutations. Zero forbidden mutations shows that failed output is contained. It does not make a
failing backend working; certification above rests on the backend's own applied/abstained/matched counts.

## Other committed runs

| Artifact (collected, revision) | Backends | Note |
|---|---|---|
| `…20261019T161442499Z-b28ead12….json` (2026-10-19, b28ead1) | self-hosted gpt-oss:20b, anthropic haiku | Before F23/F25; gpt-oss:20b 25 of 33 `invalid_output`; superseded oracle |
| `…20261023T135717890Z-5af79fbd….json` (2026-10-23, 5af79fb) | anthropic haiku, sonnet | One protocol only |
| `…20261024T084039065Z-ae7444c8….json` (2026-10-24, ae7444c) | 5 self-hosted plus 2 Anthropic | Best self-hosted deepseek: 24/33 matched, 7 invalid_output; others 4 to 16 of 33 matched |
| `…20261025T054428997Z-71e0091b….json` (2026-10-25, 71e0091) | gpt-5.5, gpt-5.4-mini, haiku | gpt-5.5 failed 32 fixtures (HTTP 400) |
| `…20261025T135141329Z-2e110bf7….json` (2026-10-25, 2e110bf) | 4 OpenAI models | 26 to 31 of 35 matched |

## Status against the acceptance criteria

1. Provider, model, config versions, outcomes, latency, tokens and cost are reported above from named artifacts.
   Switching left raw captures unchanged (0 changes in 3 pairs, 35 captures each). OpenAI cost is unknown.
2. Unavailable endpoint, invalid credential and a real credential revocation (Anthropic) were observed live.
   Malformed provider responses were not exercised live in these artifacts; that scenario is covered only by the
   no-network harness tests. Revocation on OpenAI is open.
3. The two claimed backends, Anthropic and OpenAI, have 0 forbidden mutations over the 35-fixture contrastive
   suite, with the model and instruction versions above. Their oracle match rates (34/35 each) are reported, not
   inferred from each other.
4. Each number cites an artifact, build revision and collection time. The no-network proof
   (`no_network_proof_runs_the_whole_harness`) runs in `make test`; this document does not claim a separate CI
   pass beyond that.
