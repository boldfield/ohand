# M1 task refinement — 2026-10-07

This execution overlay replaces 24 task groups with 50 smaller implementation tasks. The other 55 never-started tasks retain their scope. Nineteen already completed, attempted, active or externally blocked groups were excluded. The effective M1 graph contains 124 tasks. This changes implementation granularity, not product scope or milestone exit criteria.

Addendum 2026-10-08: V08 was originally excluded as an externally blocked evidence task. After two rejected review rounds on its PR #60, it is now split into V08a (worker-executable server-side probe) and V08b (maintainer-supplied phone-context evidence). The original task `939fe62e` is retired; see the replacement map and the V08a/V08b slices below.

## Execution authority

Read this document with `m1-task-refinement.json`, `m1-plan.md` and the architecture contract. The original `m1-tasks.json` remains the immutable baseline and ownership-group catalog used by the existing contract checker. For current execution, apply this overlay: each replaced group delegates its listed paths to the child tasks below, and consumers depend on its last child. An archived parent is an audit record, not completed implementation.

Initial model is Haiku with automatic escalation, priority 750, independent Opus and gpt-5.6-sol reviewers, and the separate automatic merge worker. Every new task is promoted to READY; dependency gates remain enforced. No routine human gate is introduced.

Sibling tasks are serialized where paths overlap. Each slice includes the tests for its own behavior and failure boundary. Review a slice against its stated stopping point, not against still-pending sibling implementation; the union must satisfy every original acceptance criterion. Security and consistency invariants remain required from the first exposed implementation. Do not ship a credential-leaking transport or a partially fenced destructive operation while waiting for another task.

Existing public function/module contracts and landed predecessor implementations are authoritative. Source pointers below were checked at `ea8b74a`; line numbers may move. Branch from current main, inspect actual APIs, and reuse the database transaction, capture, provider and time-resolution implementations. Do not invent a parallel storage or provider system from an old planning document.

Run current `make check` and `make test` as applicable, plus focused behavior tests. Native edits require exact-submitted-revision macOS CI evidence per AGENTS.md. A Linux check or a Swift-only mock cannot certify real-core native integration. Device/live-provider/trial acceptance still requires real evidence.

## Corrections discovered during sizing

- C05 now owns `core/src/ingress/transcription/` and the declaration/export for that module in `core/src/ingress/mod.rs`, after C02. Transcript mutation remains Rust-owned; Swift supplies recognition effects.

- L05 now owns `core/src/lifecycle/reset/` and its declaration/export in `core/src/lifecycle/mod.rs`. Its first child atomically owns the durable reset fence, composing existing guard/deletion APIs; its second child performs native orchestration. If a prerequisite has failed to expose its promised guard/cleanup seam, record that concrete prerequisite defect rather than implementing a second authority or weakening fencing.

- These are explicit additions to the baseline ownership map. Parent-module edits are limited to the named module declaration/export. Module-local FFI exports follow B01’s reserved mechanism; generated bindings remain build output. Existing transaction hooks are reused.

- B01a ownership allowance (explicit, one-time, serialized by B01a's place after P01): besides its owned paths it may (1) remove the committed `core/bindings/include/ohand_bindings.h` and the `core/bindings/tests/header_drift.rs` test that compared against it, together with the `cbindgen` dev-dependency and its `Cargo.lock` entries; (2) edit `core/bindings/build-ios.sh` and the two `HEADER_SEARCH_PATHS` lines in `ios/project.yml` so Xcode builds use the generated header; (3) add `bindings-check`/`bindings-test` to `Makefile` and wire them into `check` and `test`; (4) update stale comments and `docs/validation/core-binding.md`. `tools/bindings` is a separate Cargo workspace, so the root `Cargo.toml` is unchanged. B01b and B01c add exports only under `core/src/ffi/` (a file and its `pub mod` line in `core/src/ffi/mod.rs`); they do not edit `core/bindings/src/lib.rs`, the generator or these shared files.

- B01b ownership allowance (explicit, one-time, serialized after B01a): criterion 2 requires `OhAndCoreBridge` to link the real Rust core and run simulator tests against it, which cannot be done inside `core/src/ffi/` and `ios/OhAndCoreBridge/` alone. B01b may therefore additionally edit (1) `ios/project.yml` for the `OhAndCoreBridge` link/search-path/pre-build settings and the `OhAndTests` header settings; (2) `core/bindings/build-ios.sh` to serialize parallel Xcode invocations (rustup and the shared cargo directory race otherwise); (3) `docs/validation/core-binding.md` for the handle/callback contract; and (4) add `ios/Tests/CoreBridge/` to hold its simulator tests. Recorded by the B01b worker on PR #87 because reviewers requested a recorded allowance; the coordinator may amend it.

- C02a additionally waits for D04 and R01 because import must atomically create the item projection and index. B01a can build the binding pipeline after P01; runtime and production exports still wait for D05/V01.

- V08 conflated two evidence sources with different owners. Its server-side probe runs from the Odonian worker, which now has verified read access to the provider configuration file. Its phone-context reachability evidence can only come from the maintainer's iPhone, which reaches the Spark endpoint over Tailscale rather than a public address or the worker's network. V08a owns the probe tool, its tests and the Linux-context findings; V08b owns the phone-context section and the final verified status. V09 now depends on V08b, the last child, so the adapter still cannot start before both evidence sources exist. V08a must not be approved on the strength of a prose status: every published value comes from a committed sanitized artifact produced by the probe as submitted.

- V06 and V07 now depend on V01 alone: they implement Rust adapters against its transport interface using fixtures. V05 is the native effect implementation and remains required by app composition/integration, not by independent protocol adapter implementation. This is an explicit dependency override for those retained tasks.

## Why some tasks stay together

D04 keeps projection precedence and model-write guards together; I05 keeps semantic validation and atomic application together; V03 keeps pinning and revocation together. N01/N03, L01 and the L05a reset fence keep durable state changes and their retry/race guarantees together. Splitting those guarantees into separate, independently exposed changes would create misleading intermediate correctness. B02 is composition-root wiring; a test-only follow-up would make its review weaker. B03 owns ordered lifecycle assembly. Device and live-trial tasks retain their evidence boundaries because splitting a calendar wait does not shorten it.

Smaller changes reduce the amount of code and context per review/repair cycle. They do not eliminate wrong API usage, missing CI evidence or genuine platform failures. This audit does not claim task size was the only source of delays.

## Replacement map

| Original | Replacement tasks | Reason |
| --- | --- | --- |
| B01 | B01a, B01b, B01c | Separate repeatable binding builds from runtime handle safety and production persistence exports. |
| C01 | C01a, C01b | Filesystem policy and authenticated read sessions have separate platform APIs and tests. |
| C02 | C02a, C02b | Core ingestion transaction and native ownership handoff are independently implementable boundaries. |
| J02 | J02a, J02b | Keep retry/dispatch policy in Rust and isolate Swift lifecycle effects. |
| L04 | L04a, L04b | Portable serialization and the native authenticated share lifecycle are distinct deliverables. |
| L03 | L03a, L03b | Eligibility/time policy belongs in the core; native file deletion has a separate retry lifecycle. |
| E03 | E03a, E03b | Sampling/budget policy and authorized remote diagnostic execution can be reviewed separately. |
| C04 | C04a, C04b | Recorder ownership/state machine and the recovery presentation are separate native surfaces. |
| C05 | C05a, C05b | Platform speech capability and durable queue integration involve different failure boundaries. |
| N04 | N04a, N04b | Capacity allocation is deterministic core logic; notification authorization is a native state source. |
| I03 | I03a, I03b | Reminder grammar and session-topic recognition are independent bounded offline interpretations. |
| V05 | V05a, V05b | Separate an independently secure transport effect from its core request adapter, without postponing transport security. |
| U02 | U02a, U02b | Search navigation and detailed source/audio presentation are separate screens. |
| U03 | U03a, U03b | Authoritative content correction and item lifecycle actions have different forms and conflict behavior. |
| U04 | U04a, U04b | Reminder editing and evidence-only history are independently reviewable UI components. |
| U05 | U05a, U05b | Non-secret provider configuration can be built separately from secure credential entry and connection checks. |
| U06 | U06a, U06b | Initial policy setup and later revocation/requeue controls are separate workflows. |
| U07 | U07a, U07b, U07c | Three unrelated data-management flows should not be one implementation ticket. |
| U08 | U08a, U08b | Prompt configuration and the suggestion response card are different UI surfaces. |
| T02 | T02a, T02b | Process-level capture durability and provider/application races need different fault harnesses. |
| T04 | T04a, T04b | Outbound processing authorization and local read/output privacy are different attack surfaces. |
| T12 | T12a, T12b | Provider/permission failures and destructive lifecycle races form two bounded UI journeys. |
| L05 | L05a, L05b | The original Swift-only scope omitted Rust-owned reset fencing. Separate the atomic core reset operation from native cleanup orchestration. |
| V08 | V08a, V08b | The worker can only produce server-side protocol evidence; phone-context reachability over Tailscale is a maintainer input. Separate them so the probe can be reviewed and landed while the device evidence stays an honest block. |

## Retained unstarted tasks

Each was reviewed for sizing. The following remain one bounded module, integration boundary or evidence deliverable:

| Task | Retained scope |
| --- | --- |
| P02 | Probe native control handoff and protected ingress |
| D04 | Implement authoritative projections and model-write guards |
| V03 | Pin jobs to profile versions and handle configuration changes |
| V04 | Implement native credential storage and redaction |
| P03 | Probe recording interruption and partial-audio recovery |
| P07 | Probe native-to-Tauri handoff and management accessibility |
| D05 | Implement separate save, processing and reminder status values |
| I01 | Implement interpretation proposal schemas and provenance |
| R01 | Implement transactional original-text indexing and rebuild |
| N01 | Implement reminder desired-state and operation records |
| S01 | Implement transparent suggestion eligibility and rotation |
| P04 | Probe offline on-device transcription capabilities |
| I04 | Author synthetic intent fixtures with expected and forbidden outcomes |
| I05 | Implement proposal validation and atomic application |
| R02 | Implement scoped retrieval and literal query results |
| L01 | Implement idempotent deletion intent and processing tombstones |
| R03 | Implement supported natural-language retrieval filters |
| N02 | Implement native notification bridge with generic payloads |
| E01 | Implement semantic evaluation reports for deterministic and recorded runs |
| P09 | Collect actual-device feasibility evidence |
| I07 | Define versioned interpretation instructions and provider-neutral mapping |
| V06 | Implement the Anthropic interpretation adapter |
| V07 | Implement the OpenAI interpretation adapter |
| V09 | Implement the verified self-hosted provider adapter |
| N03 | Implement idempotent reminder reconciliation |
| P10 | Record the management-shell decision from feasibility evidence |
| I06 | Implement the interpretation dispatcher across provider adapters |
| C03 | Implement the silent capture surface |
| L02 | Implement native deletion cleanup reconciliation |
| E02 | Verify live interchangeable backends end to end |
| S02 | Implement the bounded optional daily prompt schedule |
| J03 | Expose bounded processing health and recoverable errors |
| N05 | Connect interpretation and user edits to actual scheduling |
| S03 | Connect prompt activation to current eligible content |
| N06 | Implement reminder history without inferring unseen events |
| B04 | Bound native work when capture backgrounds |
| C06 | Wire production system entry points and save acknowledgment |
| N07 | Handle notification actions and authenticated deep links |
| T01 | Add content-free local latency and reliability metrics |
| B02 | Assemble the production service composition root |
| B03 | Reconcile services on launch and foreground activation |
| T11 | Integrate optional provider and shadow capabilities |
| U01 | Assemble the selected production management shell |
| B05 | Reconcile wall-clock and timezone changes |
| U11 | Implement first-run permissions without blocking capture |
| U09 | Add honest status and minimal recovery views |
| S04 | Add opt-in previews of eligible non-private suggestions |
| U10 | Complete native accessibility and interaction validation |
| T03 | Test reminder and suggestion resilience after a lapse |
| T09 | Exercise the full simulator loop with fake providers |
| T10 | Version trial builds and verify upgrade persistence |
| T05 | Run the actual-device M1 functional exit matrix |
| T06 | Produce the signed M1 trial build and installation guide |
| T07 | Write M1 setup and operational recovery documentation |
| T08 | Evaluate the two-week M1 trial and record exit evidence |

## Slice specifications

### B01a

Make production bindings reproducible

Reuse the P01 proof to establish deterministic simulator/device generation and module-local export discovery; do not add product service operations yet.

Owned paths: `tools/bindings/`, `core/src/ffi/`, `ios/OhAndCoreBridge/`.

Dependencies: P01.

Acceptance:

1. Clean build produces matching Rust/Swift bindings for supported targets without modifying tracked files; generated files remain build output.
2. A minimal real-core value/error round trip builds and runs under simulator CI; document how later owned modules declare exports without concurrent central-file edits.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:211`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original B01 criteria: 1, 3, 4.

### B01b

Implement safe native core handles and callbacks

Implement the shared handle lifetime, error conversion, cancellation and callback-threading boundary used by production exports.

Owned paths: `core/src/ffi/`, `ios/OhAndCoreBridge/`.

Dependencies: B01a, D05, V01.

Acceptance:

1. Repeat initialization/destruction and cancellation, reject invalid handles/input, and convert normalized errors without panic/exception or invalid string ownership crossing the ABI.
2. Exercise background callbacks against the real core in simulator tests, delivering UI-facing effects on the documented thread and preventing callbacks after cancellation/teardown.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:211`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original B01 criteria: 1, 2, 3.

### B01c

Expose durable capture and status through production bindings

Connect the established bridge to existing capture persistence and status operations, with no duplicate store or new service registry.

Owned paths: `core/src/ffi/`, `ios/OhAndCoreBridge/`.

Dependencies: B01b.

Acceptance:

1. Swift saves and reads a real persisted capture and independent processing/scheduling statuses through core operations; failed save produces no durable-success acknowledgment.
2. Test restart, duplicate capture ID and normalized failure on simulator; verify the preceding lifetime and generated-binding mechanisms remain the only production path.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:211`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original B01 criteria: 2, 3, 4.

### V05a

Implement secure bounded native provider transport

Implement the native HTTP effect with TLS/destination checks, opaque credential lookup, redaction and limits from its first usable version.

Owned paths: `ios/Services/ProviderTransport/`, `ios/Tests/ProviderTransport/`.

Dependencies: V02, V04, B01c.

Acceptance:

1. Controlled fixture-server tests cover timeout, cancellation, bounded responses and normalized errors; no cleartext/certificate bypass or credential forwarding across origins is possible.
2. Require the established authorized destination/capability contract before dispatch and keep secrets inside the native operation; unapproved requests produce no network effect.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:250`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original V05 criteria: 1, 2.

### V05b

Connect core provider requests to native transport

Map the existing provider transport/effect interface through production bindings to the secure native implementation.

Owned paths: `ios/Services/ProviderTransport/`, `ios/Tests/ProviderTransport/`.

Dependencies: V05a.

Acceptance:

1. Real-core simulator tests exercise valid requests, denied/revoked routes and cancellation through the complete boundary; response/error mapping preserves the existing provider contract.
2. Version/credential references resolve at dispatch, redirects and error diagnostics remain protected, and no protocol adapter acquires raw secrets.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:250`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original V05 criteria: 1, 2, 3.

### V08a

Verify the Spark serving protocol from the worker context

Build a fail-closed, tested probe that inspects the user-controlled endpoint with authorized synthetic requests from the Odonian worker, and record only values the probe actually observed.

Owned paths: `tools/provider-probe/`, `docs/validation/spark-protocol.md`, `docs/validation/evidence/spark-probe/`.

Dependencies: V01, F07.

Context: PR #60 on branch `mr/939fe62e` holds the retired V08 attempt. Its code may be reused, but every finding from both review rounds recorded on task `939fe62e` must be resolved; the reviewers' findings are the defect list for this slice. The worker reads the provider configuration from `/etc/ohand-provider/models.json` or the non-secret `OHAND_PROVIDER_CONFIG_PATH`, per `docs/features/m1-external-prerequisites.md`. The endpoint may be reachable over cleartext HTTP on a private network; record the scheme that is actually configured and never report TLS that was not negotiated and verified.

Acceptance:

1. The probe fails closed. An unauthenticated request that succeeds is reported as "authentication not required", not as reachability success. An unknown or cleartext scheme is reported as such, and TLS is only reported when the certificate was verified by the default trust store. A models or chat response is only classified OpenAI-compatible when the body parses and has the expected structure. A structured-response probe passes only when the returned message content parses as JSON matching the requested shape. Any failed probe makes the tool exit non-zero. The provider key is explicit, a missing or empty base URL or credential is an explicit error, and no model name is hardcoded: chat probes use a model the endpoint listed.
2. Focused tests against a local stub HTTP server cover each classification above, the failure paths, the exit codes, and the sanitization guarantee that neither the endpoint address nor the credential appears in stdout, stderr or the sanitized artifact. The tests run under the existing `make test` entry point or a documented Python test command executed by CI.
3. The probe was run as submitted from the worker. It writes a named sanitized evidence artifact, committed under `docs/validation/evidence/spark-probe/`, that records the evidence identifier, the probe revision (git commit, with a dirty-tree flag), the collection time, each request's method, path, whether a credential was sent, the HTTP status, an allowlisted subset of response headers (at least `server`, `content-type`, `date`), the parsed model identifiers returned by the endpoint, and the structured-response result. The endpoint address and credential are never written to any artifact; the Kubernetes Secret and its mount revision are the auditable private reference.
4. `docs/validation/spark-protocol.md` contains only observed values, each citing the evidence identifier, probe revision and collection time, and a compatibility matrix derived from the endpoint's own `/models` response. It states explicitly that phone-context reachability is unverified and owned by V08b, and the task result repeats that. Approval of this slice does not complete V08.

Source pointers (baseline above): `docs/features/m1-external-prerequisites.md`, `docs/validation/m1-protocol.md:122`, `AGENTS.md:45`, PR #60 files `tools/provider-probe/spark_probe.py` and `docs/validation/spark-protocol.md`.

Contributes to original V08 criteria: 1, 3, and the authentication/TLS/structured-response part of 2.

### V08b

Record phone-context reachability of the Spark endpoint

Fold the maintainer-collected iPhone-over-Tailscale evidence into the protocol document and set the final verified status.

Owned paths: `docs/validation/spark-protocol.md`, `docs/validation/evidence/spark-phone/`.

Dependencies: V08a.

Context: this task starts blocked. The maintainer collects the evidence from the iPhone, with Tailscale connected, on both Wi-Fi and cellular, following the procedure in `docs/features/m1-external-prerequisites.md`, commits the sanitized record under `docs/validation/evidence/spark-phone/`, and then unblocks this task naming that file. A worker that is claimed without such a file in the repository must re-block with that exact missing prerequisite and must not substitute worker, simulator or Mac reachability.

Acceptance:

1. `docs/validation/spark-protocol.md` gains a phone-context section whose every value cites the maintainer's named artifact, its collection time and the network context (Wi-Fi or cellular, Tailscale connected) for both an unauthenticated and an authenticated request, including HTTP status, scheme, and certificate outcome.
2. The document's overall status becomes verified only if both the V08a server-side artifact and the phone-context artifact exist and agree on protocol and authentication behavior; any disagreement is recorded as the exact verified compatibility boundary for V09 rather than resolved by prose.

Source pointers (baseline above): `docs/features/m1-external-prerequisites.md`, `docs/validation/m1-protocol.md:122`.

Contributes to original V08 criteria: 2, 3.

### I03a

Recognize supported offline reminder commands

Recognize only the documented explicit reminder grammar using the existing date resolver and sourced proposal contract.

Owned paths: `core/src/interpretation/fast_path/`, `core/tests/fast_path/`.

Dependencies: I01, I02.

Acceptance:

1. Minimal pairs cover negation, quotation, hypothetical/reported speech, completed work, absent times and unsupported recurrence; ambiguous input abstains.
2. Supported commands produce sourced proposals offline; unmatched language remains available to the approved interpreter and never disappears or silently schedules.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:178`, `AGENTS.md:45`, `core/src/time/mod.rs:1`.

Contributes to original I03 criteria: 1, 2.

### I03b

Recognize explicit offline session-topic phrases

Add the bounded local session-topic grammar and compose its result with the existing reminder recognizer.

Owned paths: `core/src/interpretation/fast_path/`, `core/tests/fast_path/`.

Dependencies: I03a.

Acceptance:

1. Explicit session phrases produce only the derived topic facet; negative/quoted/ambiguous phrases abstain and neither scope nor disclosure permissions can change.
2. Test provider-free session-note retrieval handoff and mixed reminder/topic input while preserving source; the reminder recognizer remains behaviorally unchanged.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:178`, `AGENTS.md:45`, `core/src/time/mod.rs:1`.

Contributes to original I03 criteria: 1, 2, 3.

### C01a

Apply native file protection and backup policy

Apply the settled protection and backup attributes to each existing M1 store and temporary-file location.

Owned paths: `ios/Services/ProtectedStorage/`.

Dependencies: P02, V04, D02, B01c.

Acceptance:

1. Record and test per-store attributes and before/after-first-unlock expectations using the F01 contract; do not claim a simulator proves physical lock behavior.
2. A simulator integration persists via the real core in protected locations, verifies backup exclusions, and leaves data intact on a protection/setup failure.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:520`, `AGENTS.md:45`.

Contributes to original C01 criteria: 1, 3.

### C01b

Enforce authenticated private-read sessions

Gate private reads, relock transitions and app-switcher presentation using the native authentication session.

Owned paths: `ios/Services/Authentication/`.

Dependencies: C01a.

Acceptance:

1. Denied/cancelled authentication and foreground/relock invalidate access without deleting source; capture permission alone cannot read history.
2. A real-core simulator integration proves private reads are denied before authentication and after relock; redact the app switcher and link physical checks to the existing device evidence task.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:520`, `AGENTS.md:45`.

Contributes to original C01 criteria: 1, 2, 3.

### C02a

Implement transactional foreground ingress import

Import a foreground ingress record using existing capture storage inside a single durable, idempotent core transaction.

Owned paths: `core/src/ingress/`.

Dependencies: P01, C01b, D02, B01c, D04, R01.

Acceptance:

1. Identical ingress retries converge; conflicting ID reuse, malformed metadata and partial input fail without acknowledged source loss.
2. Inject failures before and after commit and reopen the database; return explicit committed/not-committed status and never delete an uncommitted sole source. Create the projection and text index within the same import transaction using D04/R01 APIs.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:266`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original C02 criteria: 1, 2, 3.

### C02b

Connect protected native ingress to durable core import

Connect native text/audio ownership to the core importer and acknowledge only after the durable boundary.

Owned paths: `ios/Services/Ingress/`, `ios/Tests/Ingress/`.

Dependencies: C02a.

Acceptance:

1. Real-core simulator tests cover process interruption, repeated handoff and cleanup only after import confirmation; audio references remain protected and valid.
2. Use one foreground app writer. Recovery exposes recoverable input or honest failure and never creates an extension writer, duplicate item or false saved acknowledgment.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:266`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original C02 criteria: 1, 2, 3, 4.

### N04a

Allocate bounded native-notification capacity

Allocate the verified OS pending-request budget to explicit reminders before optional prompts using the documented horizon/refill rule.

Owned paths: `core/src/reminders/capacity/`.

Dependencies: N03, D05.

Acceptance:

1. Below/at/above-limit tests return every overflow as explicit unscheduled work; nothing is silently dropped or falsely installed.
2. Desired-state reconciliation preserves stable IDs and priority on retries/refill; elapsed time never becomes delivery evidence.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:188`, `AGENTS.md:45`.

Contributes to original N04 criteria: 1, 2.

### N04b

Reconcile notification authorization and delivery uncertainty

Feed native authorization/revocation changes into the existing capacity/reconciliation/status contracts.

Owned paths: `ios/Services/NotificationPermission/`.

Dependencies: N04a.

Acceptance:

1. Revocation, denial and foreground refresh report usable unscheduled/undeliverable state without treating Focus or clock passage as delivery/attention.
2. Real-core simulator tests verify permission changes and capacity interactions with explicit reminders and optional prompts, without duplicate effects.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:188`, `AGENTS.md:45`.

Contributes to original N04 criteria: 1, 2.

### J02a

Implement the core job dispatch loop

Drain existing leased jobs through injected capabilities with bounded work and the established configuration/version guards.

Owned paths: `core/src/jobs/runner/`, `core/tests/job_runner/`.

Dependencies: J01, V03, I06, B01c.

Acceptance:

1. Fake-clock tests cover lease recovery, retry limits, cancellation/checkpoint and duplicate results against real job/apply operations.
2. A result received after a reminder opportunity preserves source and honest unscheduled state; the runner never invents a replacement deadline or destination.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:260`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original J02 criteria: 1, 2, 3.

### J02b

Drive the job loop from native lifecycle events

Register launch, foreground, reachability and suspension hooks that drive the existing core loop without blocking capture.

Owned paths: `ios/Services/JobRunner/`, `ios/Tests/JobRunner/`.

Dependencies: J02a.

Acceptance:

1. Repeated/overlapping activation cannot start duplicate drainers; suspend/terminate cancels or checkpoints through the core contract.
2. Real-core simulator tests resume durable pending jobs after restart/network return. Expose the capability registration seam for C05; do not claim transcription works before its handler is supplied.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:260`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original J02 criteria: 1, 2, 3.

### C04a

Implement bounded native recording sessions

Implement explicit user-started recording, protected incremental source persistence and bounded stop/cancel/interruption handling.

Owned paths: `ios/Capture/Voice/`, `ios/Tests/VoiceCapture/`.

Dependencies: P03, C02b, D05.

Acceptance:

1. Denied microphone, audio interruption, lock and duration/size limit produce recoverable saved audio or an honest failure; there is no always-listening or network recording path.
2. Simulator tests inject recorder failures and verify file ownership/acknowledgment; platform-only lock/termination behavior remains in the physical matrix.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:245`, `AGENTS.md:45`.

Contributes to original C04 criteria: 1, 2.

### C04b

Present recoverable voice captures on re-entry

Add the source-preserving recovery controls and presentation around the recording state machine.

Owned paths: `ios/Capture/Voice/`, `ios/Tests/VoiceCapture/`.

Dependencies: C04a.

Acceptance:

1. Restart discovers partial/saved recordings and offers bounded continue/finish/delete behavior without a mandatory cleanup queue; cancellation does not silently erase acknowledged source.
2. Tests cover re-entry after interruption and user cancellation, keeping duration/size/error messages honest and normal capture immediately usable.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:245`, `AGENTS.md:45`.

Contributes to original C04 criteria: 1, 2.

### C05a

Implement guarded core transcript attachment

Apply native transcription outcomes to durable audio captures in Rust; keep revision, lifecycle, reset-generation and routing authority in the core.

Owned paths: `core/src/ingress/transcription/`, `core/src/ingress/mod.rs`.

Dependencies: P04, C04b, J01, J02b.

Acceptance:

1. Apply a successful transcript with provenance, original-audio identity and successful-transcription time in one retry-safe transaction; use existing event/projection/job APIs and enqueue interpretation only when authorized. Failure or abstention retains the sole source and explicit pending state.
2. Late results after correction, deletion or reset cannot replace authoritative content; crash/retry cannot duplicate attachment or work. Tests distinguish saved audio, transcript completion and interpretation completion.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:255`, `docs/architecture/m1-contracts.md:555`, `core/src/ingress/mod.rs:1`, `core/src/store/captures/mod.rs:106`.

Contributes to original C05 criteria: 1, 2.

### C05b

Connect offline transcription to queued core attachment

Implement the proven on-device recognizer and register it with the runner, handing every outcome to the guarded core transcript operation.

Owned paths: `ios/Services/Transcription/`, `ios/Tests/Transcription/`.

Dependencies: C05a.

Acceptance:

1. A queued durable recording executes the real native handler and applies its result through C05a; interruption/retry/cancellation converge. Available on-device models produce text offline; unsupported or missing models stay pending and never fall back to a remote service.
2. Exact-revision real-core simulator integration covers handler registration, bounded result/error transport, stale results and preservation of source. Report actual offline recognition device evidence separately when the simulator cannot establish it; do not fabricate certification.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:255`, `AGENTS.md:45`.

Contributes to original C05 criteria: 1, 2, 3, 4.

### L03a

Compute source-safe audio retention eligibility

Determine retention state and cleanup intent using the successful-transcription timestamp and existing authoritative source state.

Owned paths: `core/src/lifecycle/retention/`.

Dependencies: C05b, L02.

Acceptance:

1. Default expiry is seven days after transcription success; pending/unsupported/failing sole-source recordings do not expire automatically.
2. Injected-clock cases cover user correction, deletion and absent timestamps. Return visible unresolved/storage-pressure states and explicit retry/delete options without silent loss.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:555`, `AGENTS.md:45`.

Contributes to original L03 criteria: 1, 2, 3.

### L03b

Execute resumable native audio retention cleanup

Remove only audio approved by the core retention policy and acknowledge effect completion durably.

Owned paths: `ios/Services/AudioRetention/`.

Dependencies: L03a.

Acceptance:

1. Interruption before/after file removal converges on next sweep; stale retention work cannot remove newly protected or sole-source audio.
2. Integration tests cover policy decisions against real files and completion state; failure is visible, bounded and never reported as successful cleanup.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:555`, `AGENTS.md:45`.

Contributes to original L03 criteria: 1, 2, 3.

### L04a

Export a consistent scoped core snapshot

Serialize source, corrections and authoritative state from a consistent read snapshot into the documented portable format.

Owned paths: `core/src/export/`.

Dependencies: D04, L02, V04.

Acceptance:

1. Explicit scope/authentication context excludes deleted/out-of-scope content, credentials and sensitive endpoint configuration; concurrent edits yield one coherent snapshot.
2. A test-only reader reconstructs equivalent item state and rejects malformed/truncated output; optional audio references are explicit and this is not a restore feature.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:561`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original L04 criteria: 1, 2.

### L04b

Share exports through protected native temporaries

Connect authenticated export requests and explicit optional audio inclusion to the platform share lifecycle.

Owned paths: `ios/Services/Export/`.

Dependencies: L04a.

Acceptance:

1. Share completion, cancellation, interruption and next launch clean protected temporaries without deleting source or reporting success prematurely.
2. Exercise real-core export in simulator and verify exported contents stay within selected scope, no secrets appear, and unavailable/deleted audio is reported honestly.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:561`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original L04 criteria: 1, 2.

### E03a

Implement opt-in shadow selection and budget accounting

Select diagnostic cases only when a separately approved review profile/policy and bounded sampling budget allow them.

Owned paths: `core/src/review/shadow/`.

Dependencies: V03, E01, V04, B01c.

Acceptance:

1. Default-off, revoked/unapproved route and exhausted-budget cases dispatch nothing; reserve/account budget consistently across retries.
2. Shadow records carry provenance and unreviewed/error outcomes but have no authority to mutate item, reminder or permission state. Test against actual domain guards.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original E03 criteria: 1, 2.

### E03b

Run authorized shadow diagnostics through native services

Execute selected review requests using existing native transport/credentials and store bounded diagnostic outcomes through the core contract.

Owned paths: `ios/Services/ShadowReview/`.

Dependencies: E03a.

Acceptance:

1. Timeout/cancellation records unreviewed; disagreement records diagnostics only, with no extra round or authoritative mutation.
2. Real-core simulator tests verify route denial, redacted diagnostics and budgeted dispatch; configuration exposes the separate approved profile and instrumentation remains optional.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original E03 criteria: 1, 2.

### U02a

Build scoped original-text search UI

Connect literal search, date and session-topic filters to existing scoped retrieval operations.

Owned paths: `app/retrieval/`.

Dependencies: U01, R03, C01b.

Acceptance:

1. Offline/provider-free search returns original/corrected words with date/type/source and an honest empty state; authenticated scope is enforced before counts/snippets appear.
2. Session topics since a date work without generated answers or network parsing, and selecting a result passes an opaque validated identifier to detail navigation.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`.

Contributes to original U02 criteria: 1, 2, 4.

### U02b

Build authenticated source detail and retained-audio playback

Show source/correction provenance and offer playback only for retained authorized source audio.

Owned paths: `app/source-detail/`.

Dependencies: U02a.

Acceptance:

1. Authentication/relock is enforced for text and playback; deleted/expired/unavailable audio has an honest disabled state and no stale file access.
2. Tests open search results, compare original and corrected text, play synthetic retained audio and handle deletion/expiration without invented quotations.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`.

Contributes to original U02 criteria: 1, 3, 4.

### U03a

Add source, type and session-topic correction controls

Add explicit source/transcript editing, type correction and the independent session-topic toggle using existing revisioned operations.

Owned paths: `app/item-controls/`.

Dependencies: U02b, D04.

Acceptance:

1. Corrections preserve original provenance and survive relaunch/reprocessing; stale revision conflicts are visible rather than silently overwritten.
2. Session-topic changes do not change privacy scope, and changing a label never silently converts an idea into an obligation.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:53`, `AGENTS.md:45`.

Contributes to original U03 criteria: 1, 3, 4.

### U03b

Add item completion and suggestion lifecycle controls

Add completion/cancellation, not-now and stop-suggesting actions using existing domain lifecycle operations.

Owned paths: `app/item-controls/`.

Dependencies: U03a.

Acceptance:

1. Each action updates actual durable state and handles stale/deleted targets; retries/reprocessing cannot undo the user action.
2. Explain cooldown and pull-only behavior; stopping suggestions preserves retrieval and does not cancel an explicit reminder unless the domain operation explicitly calls for it.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:53`, `AGENTS.md:45`.

Contributes to original U03 criteria: 1, 2.

### U04a

Add explicit reminder editing UI

Connect requested/resolved date/time edits and cancellation to existing reminder commands.

Owned paths: `app/reminders/`.

Dependencies: U03b, N06.

Acceptance:

1. Show resolved timezone and honest unsupported-repeat, permission, capacity and expired-opportunity states; saving an edit is not falsely presented as OS installation.
2. Tests prove edits cancel stale requests and retain one desired reminder across retry/restart.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:188`, `AGENTS.md:45`.

Contributes to original U04 criteria: 1, 2.

### U04b

Present reminder history and authenticated navigation

Display observed installation/cancellation/user events and route generic notification links to the correct protected record.

Owned paths: `app/reminders/`.

Dependencies: U04a.

Acceptance:

1. No clock-derived fired/seen/missed fiction appears; acknowledge is distinct from complete and unknown delivery stays unknown.
2. Tests cover stale/deleted link IDs, failed authentication and re-entry after a lapse without replaying old history as a digest.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:188`, `AGENTS.md:45`.

Contributes to original U04 criteria: 1, 2.

### U05a

Build provider-profile configuration controls

Add protocol/endpoint/model/capability selection and default-profile changes against the existing versioned profile service.

Owned paths: `app/provider-settings/`.

Dependencies: U04b, V06, V07, V04.

Acceptance:

1. Validation rejects unsupported/incomplete combinations and clearly represents local-only or unreachable private-server operation.
2. Changing defaults preserves captures and queued-job destination pinning; UI never rewrites old jobs to another vendor implicitly.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:566`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original U05 criteria: 1, 2.

### U05b

Add native credential setup and redacted connection checks

Connect profile credential references to native secure entry/update/removal and an explicitly authorized connection test.

Owned paths: `app/provider-settings/`.

Dependencies: U05a.

Acceptance:

1. Secret input goes directly to the native credential service, is cleared promptly, and is never rendered back into a webview, persisted UI state or diagnostic output.
2. Tests cover invalid/removed credentials, network failure and capability mismatch with redacted feedback; no background test uploads private capture text.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:566`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original U05 criteria: 1, 2.

### U06a

Build local-first processing-policy onboarding

Offer deliberate destination/capability authorization while preserving a functional local-only initial state.

Owned paths: `app/privacy-settings/`.

Dependencies: U05b, V02, V03.

Acceptance:

1. Fresh install captures and retrieves before any provider setup; route labels are clear and sticky, with no classifier-created authorization.
2. Tests show accepting one capability/destination does not authorize another, and cancelling setup leaves local behavior usable.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:139`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original U06 criteria: 1, 2.

### U06b

Add processing-policy change and revocation controls

Expose existing versioned policy updates and explicit queued-work choices without silently changing destinations.

Owned paths: `app/privacy-settings/`.

Dependencies: U06a.

Acceptance:

1. Revoke or change a profile/capability while work is queued or in-flight and display the actual resulting state; late results cannot bypass revocation.
2. Explicit retry/requeue is inspectable, no-approved-provider state remains usable, and route preferences survive relaunch.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:139`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original U06 criteria: 1, 2.

### L05a

Implement durable core reset generation and fencing

Persist the global reset boundary in Rust and compose existing per-item deletion/cleanup intents under a durable generation fence.

Owned paths: `core/src/lifecycle/reset/`, `core/src/lifecycle/mod.rs`.

Dependencies: L02, L03b, L04b.

Acceptance:

1. Start/retry reset atomically records its generation and invalidates old in-flight work; every core result/apply path uses the existing fence hook so reset cannot revive captures or issue new effects. Reuse the established schema/transaction and guard interfaces.
2. Process-restart tests cover populated mixed-route records, jobs and indexes. Reset remains pending until required cleanup acknowledgments arrive; capture after completed reset belongs to a fresh generation.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:205`, `docs/architecture/m1-contracts.md:548`, `core/src/lifecycle/mod.rs:1`, `core/src/store/schema/mod.rs:165`.

Contributes to original L05 criteria: 1, 2.

### L05b

Orchestrate native reset cleanup and credential choice

Coordinate native deletion effects and explicit optional credential/profile removal around the durable Rust reset operation.

Owned paths: `ios/Services/Reset/`, `ios/Tests/Reset/`.

Dependencies: L05a.

Acceptance:

1. Exercise mixed-route captures, retained audio, ingress, export/share temporaries and native notifications; reuse per-item executors and acknowledge only actual completed cleanup. Required failures remain durably pending and retries after crash cannot restore private content or scheduled reminders.
2. Real-core simulator integration verifies old callbacks are rejected by the reset fence, optional credential removal follows the explicit choice, and post-completion capture uses a fresh generation. No Swift-owned competing generation or premature reset-success state.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:205`, `docs/architecture/m1-contracts.md:548`, `core/src/lifecycle/mod.rs:1`.

Contributes to original L05 criteria: 1, 2.

### U07a

Add retention status and settings UI

Show and configure the existing source-safe retention policy and unresolved sole-audio cases.

Owned paths: `app/data-settings/`.

Dependencies: U06b, L03b, L04b, L05b.

Acceptance:

1. Seven-day successful-transcription default, sole-source exception and storage pressure are clear; retry/delete choices use existing operations.
2. Tests verify real policy/progress/error state and keep new capture available during non-destructive sweeps.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:548`, `AGENTS.md:45`.

Contributes to original U07 criteria: 1, 2.

### U07b

Add authenticated export controls

Connect scope selection, explicit audio inclusion and export/share feedback to the existing export service.

Owned paths: `app/data-settings/`.

Dependencies: U07a.

Acceptance:

1. UI reports actual snapshot/share failure or completion, explains external export/backup limits and does not expose secrets or out-of-scope records.
2. Tests cover cancelled share and cleanup of protected temporaries, with no false full-backup/restore claim.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:548`, `AGENTS.md:45`.

Contributes to original U07 criteria: 1, 2.

### U07c

Add deletion and reset progress controls

Connect explicit per-item deletion and delete-all/reset choices to the existing durable cleanup services.

Owned paths: `app/data-settings/`.

Dependencies: U07b.

Acceptance:

1. Deletion remains pending until required effects finish; retry/restart show actual progress/error and never redisplay deleted source.
2. Tests cover optional credential reset and clearly explain backup/export limits without promising recall of external copies; capture resumes under the new reset generation.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:548`, `AGENTS.md:45`.

Contributes to original U07 criteria: 1, 2.

### U08a

Build bounded optional-prompt settings

Expose opt-in, selected local time, bounded horizon/cooldown and disable controls for the existing prompt service.

Owned paths: `app/prompt-settings/`.

Dependencies: U07c, S03.

Acceptance:

1. Default is off; configuration and cancellation persist and never disable explicit reminders.
2. Explain horizon/refill behavior and test permission/capacity failure without a catch-up burst or disclosure of private content.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:198`, `AGENTS.md:45`.

Contributes to original U08 criteria: 1, 2.

### U08b

Build authenticated suggestion response UI

Present the current eligible suggestion and route not-now/stop/done through existing revisioned operations.

Owned paths: `app/suggestions/`.

Dependencies: U08a.

Acceptance:

1. Authenticate before private content; stale/completed/deleted suggestions are resolved again and an empty set is calm.
2. Tests verify actual stored policy/lifecycle changes and relaunch persistence without inbox cleanup or accidental changes to explicit reminders.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:198`, `AGENTS.md:45`.

Contributes to original U08 criteria: 1, 2.

### T02a

Verify capture durability under process failure

Build a process-level durability harness around actual capture/ingress persistence.

Owned paths: `tests/resilience/capture-processing/`.

Dependencies: C05b, J02b, I05, L02.

Acceptance:

1. Kill/reopen around each durable acknowledgment boundary and prove every acknowledged capture survives with stable identity.
2. Repeated ingress and partial file handoff produce neither duplicate captures nor deletion of the sole uncommitted source; fixture state is synthetic.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:170`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original T02 criteria: 1.

### T02b

Verify processing retry and deletion-race safety

Inject provider/configuration/application failures through existing runner and guard integration paths.

Owned paths: `tests/resilience/capture-processing/`.

Dependencies: T02a.

Acceptance:

1. Timeout, invalid output and changed policy preserve source/permission boundaries; crash/retry cannot duplicate an authoritative mutation.
2. A deletion or explicit correction racing a provider result cannot resurrect or overwrite source; tests invoke real integration paths rather than copied algorithms.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:170`, `AGENTS.md:45`, `core/src/store/captures/mod.rs:106`, `core/src/store/schema/mod.rs:165`.

Contributes to original T02 criteria: 1, 2.

### T04a

Verify outbound destination and credential isolation

Exercise base-provider authorization, transport and queued/in-flight configuration handling using synthetic canaries.

Owned paths: `tests/privacy/`.

Dependencies: U07c.

Acceptance:

1. Unapproved capability/destination, redirect and stale/revoked profile tests emit no unauthorized canary or credential; approved remote processing is accurately identified.
2. No secret/content leaks into transport logs. This suite excludes optional Spark/shadow/preview requirements; their owners retain dedicated canaries.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original T04 criteria: 1, 2, 3.

### T04b

Verify private reads, notifications and export isolation

Exercise local authenticated reads and outward presentations using mixed-scope synthetic records.

Owned paths: `tests/privacy/`.

Dependencies: T04a.

Acceptance:

1. Queries, counts/snippets, private source detail, generic notification payloads and exports cannot reveal out-of-scope or locked content.
2. Deleted/relocked records and cancelled exports leave no readable application temporaries; test log redaction and preserve the separate optional-capability canaries.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:127`, `AGENTS.md:45`, `core/src/providers/contracts/mod.rs:1`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:197`.

Contributes to original T04 criteria: 1, 2, 3.

### T12a

Exercise provider and permission failure journeys

Drive the real app through provider outage and denied/revoked permission cases with deterministic boundary fakes.

Owned paths: `ios/Tests/FailureJourneys/`.

Dependencies: T09, L05b, B04, B05.

Acceptance:

1. Saved source remains retrievable, retry is source-preserving, and capture remains available without mandatory triage.
2. Assert independently honest save/processing/scheduling state through UI plus persisted state, including failed notification permission.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:260`, `AGENTS.md:45`.

Contributes to original T12 criteria: 1, 2.

### T12b

Exercise restart, transcription and deletion failure journeys

Drive interruption, expired background execution, restart and deletion-during-processing through production app wiring.

Owned paths: `ios/Tests/FailureJourneys/`.

Dependencies: T12a.

Acceptance:

1. Acknowledged audio/text survive interrupted transcription and restart; late jobs do not restore deleted items or stale notification effects.
2. Recovery avoids a cleanup gate and keeps all status dimensions honest; test actual native/core composition with synthetic source and deterministic effects.

Source pointers (baseline above): `docs/architecture/m1-contracts.md:260`, `AGENTS.md:45`.

Contributes to original T12 criteria: 1, 2.

## Original acceptance coverage

The following original criteria remain required collectively across the replacement children. Child task acceptance above determines each individual stopping point.

### B01 coverage

1. Generate and verify simulator/device bindings in documented builds; keep ABI ownership, cancellation, threading and error conversion explicit. Covered by: B01a, B01b.
2. Exercise actual persistence and status calls through Swift; test repeated initialization, invalid input and background-thread callbacks without UI-thread violations. Covered by: B01b, B01c.
3. For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient. Covered by: B01a, B01b, B01c.
4. Each owned module declares its exports through the reserved interface mechanism; generated bindings are reproducible build output, not shared committed files. Native consumers depend on this bridge before adding effects. Covered by: B01a, B01c.

### C01 coverage

1. Document accessibility before/after first unlock, app-switcher redaction and OS backup inclusion/exclusion; locked capture never grants private-history access. Covered by: C01a, C01b.
2. Tests cover denied/cancelled authentication and relock/foreground transitions. Failed unlock does not erase or disclose stored content. Covered by: C01b.
3. For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient. Covered by: C01a, C01b.

### C02 coverage

1. Durable acknowledgement follows committed source persistence. Repeated delivery, interruption and restart produce neither duplicate items nor missing acknowledged captures. Covered by: C02a, C02b.
2. Test atomic file/DB ownership, malformed/partial ingress, recovery and cleanup after confirmed import inside protected storage. Covered by: C02a, C02b.
3. Any later independent extension writer needs a separate task and proven concurrency/protection contract; it is not part of this task. Covered by: C02a, C02b.
4. For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient. Covered by: C02b.

### J02 coverage

1. Launch/foreground/network-return resume work without duplicate application; suspend/termination cancels or checkpoints safely. Covered by: J02a, J02b.
2. An interpretation completed after its requested reminder time cannot silently backdate or substitute a new deadline; captured and scheduled status remain separate. Covered by: J02a, J02b.
3. Rust owns dispatch/retry orchestration against injected effects; Swift owns only platform lifecycle/execution hooks. Transcription is an injected capability registered by C05, not an assumed handler. Covered by: J02a, J02b.

### L04 coverage

1. Export contains no credentials, internal endpoint secrets, deleted content or private data outside the authenticated requested scope; optional audio inclusion is explicit. Covered by: L04a, L04b.
2. A test-only reader reconstructs equivalent item state and detects malformed/truncated export; native share/temp files are protected and cleaned. M1 does not claim full backup/restore sync. Covered by: L04a, L04b.

### L03 coverage

1. Clock-injected sweep tests cover success/pending/failure/user-corrected/deleted records and interrupted file removal. Covered by: L03a, L03b.
2. Default successful-transcription audio retention is seven days; sole-source audio is retained with visible storage/retention status until recoverable text exists or explicit user deletion. Document disk-pressure behavior without silent loss. Covered by: L03a, L03b.
3. Untranscribed source-only recordings have a visible unresolved state and explicit retry/delete resolution; the successful-transcription retention clock starts at transcript success, never at capture time. Covered by: L03a, L03b.

### E03 coverage

1. Default off and never required for capture; timeout records unreviewed, and no verdict can mutate items, reminders or routing permissions. Covered by: E03a, E03b.
2. Capture only privacy-approved evaluation data and compare against known expectations/corrections; enforce sample/budget limits and expose configuration and reviewer-induced disagreements. Covered by: E03a, E03b.

### C04 coverage

1. Microphone denied/interrupted/locked/terminated paths preserve recoverable audio or an honest failure; re-entry shows resumable/recoverable source without a maintenance queue. Covered by: C04a, C04b.
2. Recording duration/size limits and cancellation are explicit, no always-listening behavior, and no network request occurs for raw recording. Covered by: C04a, C04b.

### C05 coverage

1. Available on-device model yields text offline; unsupported/missing-model/failed jobs retain source and remain intelligibly pending. Covered by: C05a, C05b.
2. Late transcripts cannot overwrite a user correction; save, transcript and interpretation completion are distinct and tested with interruption/retry. Covered by: C05a, C05b.
3. Register the real native transcription handler with the job runner through the reserved capability seam and verify a queued saved recording actually executes through that path. Covered by: C05b.
4. For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient. Covered by: C05b.

### N04 coverage

1. Boundary tests prove no silent loss when the cap is reached; expose unscheduled/undeliverable state and a documented horizon/refill strategy. Covered by: N04a, N04b.
2. Permission changes, Focus-related uncertainty and elapsed due times do not falsely mark delivery or user attention; reconcile again on supported foreground events. Covered by: N04a, N04b.

### I03 coverage

1. Minimal pairs test negation, quotations, hypothetical/reported speech, already-completed work, missing times and repeats; starts-with-remind-me is not sufficient. Covered by: I03a, I03b.
2. A fully supported command produces a sourced candidate offline; other clear language remains available to the interpreter instead of being permanently discarded or silently scheduled. Covered by: I03a, I03b.
3. Recognize a bounded documented set of explicit session-topic phrases locally, such as Bring this up in therapy, with negated/quoted minimal pairs and abstention. This sets a derived session-topic facet only, never privacy scope or upload permission. Covered by: I03b.

### V05 coverage

1. Timeout, cancellation, response bounds, error normalization, redacted diagnostics and endpoint/TLS validation are tested through a controlled fixture server. Covered by: V05a, V05b.
2. Reject cross-origin redirect credential forwarding and silent cleartext/certificate bypass; user-configured private endpoints work only under explicitly documented supported transport. Covered by: V05a, V05b.
3. For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient. Covered by: V05b.

### U02 coverage

1. Search returns original/corrected text with date/type/source and honest no-result state; private reads require authentication. Covered by: U02a, U02b.
2. Literal fallback and private topics since a date work offline; no generated answer is shown as quotation. Covered by: U02a.
3. Offer playback only for retained source audio with the same private-read authentication; expired/deleted audio is honestly unavailable and cannot leave a broken playback control. Covered by: U02b.
4. Expose session-topic filtering and since-date retrieval with original source text in the private authenticated view. Covered by: U02a, U02b.

### U03 coverage

1. Each action updates actual stored state and survives relaunch/reprocessing; stale revision conflicts are visible and do not silently overwrite. Covered by: U03a, U03b.
2. Not-now and stop-suggesting behavior are explained; actions do not turn an idea into an obligation without explicit user intent. Covered by: U03b.
3. Allow source/transcript correction as an explicit authoritative revision while preserving original provenance; later transcription/model output cannot overwrite the correction. Covered by: U03a.
4. Provide an explicit session-topic facet toggle independently of note/action/idea and privacy scope; correction survives later reprocessing. Covered by: U03a.

### U04 coverage

1. Resolved time/timezone, unsupported repeats, capacity/permission errors and passed scheduling opportunities are understandable. Covered by: U04a, U04b.
2. Edits cancel stale requests, generic notification links reopen the correct authenticated record, and history does not invent delivery/attention. Covered by: U04a, U04b.

### U05 coverage

1. Profile capability/endpoint/model validation and redacted connection test work; secrets go directly to native secure storage and are not re-rendered. Covered by: U05a, U05b.
2. Changing default provider preserves captures and follows queued-job config pinning; show local-only and unavailable private-server modes honestly. Covered by: U05a, U05b.

### U06 coverage

1. Default local-only capture works before setup; separate private/general route preferences are clear and sticky, never inferred as permission by a classifier. Covered by: U06a, U06b.
2. Profile destination/capability changes require deliberate authorization and reflect queued-work behavior; revocation and no-approved-provider cases remain usable. Covered by: U06a, U06b.

### U07 coverage

1. Actions report real progress/error and sole-source retention exceptions; no completed status while required cleanup remains pending. Covered by: U07a, U07b, U07c.
2. Explain external export/OS backup limits, clean temporary artifacts and keep new capture usable during non-destructive maintenance. Covered by: U07a, U07b, U07c.

### U08 coverage

1. Opt-in is required, prompt horizon/cooldown are understandable, and no private item is revealed before authenticated open. Covered by: U08a, U08b.
2. Off/not-now/stop/done affect correct stored policy without disabling explicit reminders; empty eligible set is calm and does not solicit inbox cleanup. Covered by: U08a, U08b.

### T02 coverage

1. Kill/reopen or equivalent process-level crash tests show no loss of acknowledged capture and no duplicate authoritative mutations. Covered by: T02a, T02b.
2. Provider timeout/invalid output/config changes/deletion races preserve source and permission boundaries; test real integration paths, not copies of implementation logic. Covered by: T02b.

### T04 coverage

1. Unapproved routes, cross-origin redirects, stale jobs and reviewer profiles cannot disclose canaries; private-source queries/snippets and lock-screen payloads respect read scope. Covered by: T04a, T04b.
2. Assert no raw credentials/content in logs or exports and verify revocation while a job is in flight; record authorized remote processing accurately. Covered by: T04a, T04b.
3. This base privacy suite does not depend on optional Spark, shadow or preview features. T11/S04 own their capability canaries, which must pass before those capabilities are enabled or claimed in a candidate. Covered by: T04a, T04b.

### T12 coverage

1. Exercise provider outage, denied/revoked permissions, interrupted transcription, expired background execution, restart and deletion during processing. Covered by: T12a, T12b.
2. Assert source durability, recovery without mandatory triage, no stale scheduled effects, and independently honest save/processing/scheduling state. Covered by: T12a, T12b.

### L05 coverage

1. A durable reset generation fences in-flight processing and new effects; retries after crashes cannot restore deleted content or leave reminders scheduled. Covered by: L05a, L05b.
2. Test populated mixed-route data, indexes, audio, export/share temporaries, pending jobs and optional credential removal. New captures after completed reset belong to a fresh generation. Covered by: L05a, L05b.

