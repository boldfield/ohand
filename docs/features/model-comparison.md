# Paired model comparison

## Outcome and scope

Oh And can compare two configured interpretation profiles against the same immutable input. The first deliverable is an optional M1 developer benchmark. M2 adds opt-in sampled comparison of normal interpretations and a small, optional comparison interface. Neither feature is a prerequisite for the base M1 signed build, physical matrix or trial. A benchmark never mutates real items or reminders; a production challenger never replaces the chosen primary result.

This is independent interpretation, not adversarial review. Both arms receive equivalent source/context/instructions, without the other arm's output, explanation or identity. E03's reviewer sees a candidate for a different purpose; its verdicts must never be represented as independent challenger evidence. Model agreement is not accuracy. Automatic model judges, automated provider switching, user-level traffic experiments and multi-user experiment infrastructure are outside this feature.

The initial benchmark accepts synthetic fixture corpora and explicitly selected, authorized inputs only through the protected selection path described for M2. It does not read a production capture database or discover/export personal notes. Existing synthetic fixture expectations, semantic validation and E01 metrics are reused. Existing E02 remains responsible for its own live interchangeability evidence; passing fake transport tests does not certify a real model.

## Controlled comparisons and honest measurements

An experiment pins the corpus/case revision, instruction and prompt versions, normalized request context including original time/timezone/locale, requested settings, per-arm profile/model versions, adapter/build revision and run identity. Record known effective settings and unresolved provider defaults separately. A differing prompt, missing context or unsupported setting prevents a claim that model identity was the only variable. Historical private selections use the original relevant time context rather than reinterpret yesterday's “tomorrow” using the replay date.

The host runner reuses production protocol adapters and normalized provider dispatch. It supplies a host transport effect and credential resolver; it does not implement a second vendor prompt or response parser. The runner has no authoritative apply capability. Unsupported adapter/capability combinations fail explicitly; cloud comparison does not wait for a Spark adapter. Newly supported adapters can join through the same boundary.

Run arms in a bounded, recorded order. Default to sequential counterbalanced order so a shared endpoint is not automatically loaded with simultaneous competing inference. Every network attempt consumes a reserved request allowance, including retries. Partial, failed, cancelled, skipped, timed-out and unknown outcomes remain visible. Interrupted runs never silently rerun an attempt whose remote completion is unknown. Resume preserves completed results and requires an explicit, budgeted new attempt for ambiguous work. Pinning configuration does not guarantee bit-for-bit model determinism.

Reports separate objective rubric outcomes, disagreements and subjective preferences. Report false actions/deadlines/completions, missed supported intent, abstentions and unsupported claims independently. Include sample and matched-pair counts, missing-arm reasons, latency and failure distributions, token usage where supplied, and cost provenance. A timeout is not a correct abstention. Do not infer model quality from agreement, missing user corrections, a tiny sample or a fixture replay labeled as live inference. Missing usage/cost is unknown, not zero. Estimated cost identifies its supplied rate/version; no price lookup or hardcoded claim of current vendor prices is required.

## Authorization and bounded execution

Benchmark network execution is explicit. Dry runs and CI are credential-free with deterministic fakes/local fixture servers; live credentials are supplied out of band by reference. Secret values never enter run records, command arguments, reports, logs or the public repository. Headless HTTP uses verified TLS, destination-bound credentials, redirect rejection, bounded bodies, timeouts and cancellation. No ambient fallback to another endpoint or account is permitted.

Live comparison is off by default. The user chooses the primary/challenger profiles, eligible scope, sample rate and hard request-attempt limits. An enforceable request cap remains even when money cost is unknown. The experiment does not grant processing permission: each destination and capability must already be explicitly authorized and is checked again immediately before every dispatch. Known therapy/session-topic and unknown classifications are excluded unless explicitly included in the experiment and independently authorized for both destinations. Freeze the request before the primary outcome, then select after its facet is known and recheck at dispatch. Report this post-primary selection bias; classification is not a permission grant or a guarantee that mislabeled sensitive content has been identified. A classifier cannot grant either permission.

Only comparable AI interpretation requests enter the paired model population. Deterministic fast-path resolutions are identified separately, not silently attributed to a model or replayed remotely. The primary capture/apply path never waits for the challenger. Shared resource contention cannot be promised away; lower-priority comparison work yields to primary work and remains bounded. Pausing/revoking an experiment stops new dispatch and handles late results according to pinned source revision, policy and reset-generation rules.

## Protected live diagnostics and optional interaction

Reuse existing durable jobs/leases and platform lifecycle execution; add a diagnostic job kind and bounded comparison policy rather than a second background scheduler. Freeze/identify the actual primary request before output exists. Retain primary and challenger outcomes against that same revision; an unrelated rerun of A must not masquerade as the outcome that the user received.

Diagnostic state is local, protected and scoped from its first real use. Prefer references over copies of source text. Necessary source snapshots and model outputs receive the same authenticated read, scope, expiry, deletion and reset treatment as their source. Source deletion fences racing writes and immediately hides related diagnostics, preferences and selected-corpus material, with required cleanup tracked durably. Do not add an unprotected JSON sidecar or plaintext diagnostic database. Aggregate exports omit source/model-output text by default; exporting personal examples requires explicit authorized selection.

The UI has optional experiment controls, a bounded disagreement list, blinded A/B choices and a summary. It is not a new inbox to clear and does not generate a review reminder. Choices include A, B, both acceptable, neither and skip; stable randomized presentation avoids always putting the primary first. A choice records a preference for the pinned outputs and rubric, without changing the item, reminder or default provider. Ordinary later corrections are separate weak observational signals, not automatically ground-truth labels; the user saw the primary result, so these observations are biased toward primary mistakes and do not expose challenger-only regressions. No correction is not evidence of correctness. Stale/deleted/relocked content is not displayed.

## Delivery and ownership rules

The task manifest accompanying this document defines the small execution slices, owned paths and prerequisites. Every task includes its own meaningful behavior tests and stops at its stated boundary. Do not expand a task into the whole feature, split its safety checks into a later test-only ticket or invent a new backend framework. Inspect landed predecessor APIs before implementing; source pointers describe inspected locations, not permission to edit another task's module.

All new tasks start on Haiku with automatic escalation, priority 750, independent Opus and gpt-5.6-sol review, and the existing Oh And separate automatic merge worker. Promote every task to READY. M2 core-only tasks follow T06, the frozen signed trial build, so they can be implemented during the observation period without changing the tested binary. Native activation and UI tasks additionally wait for T08, the actual M1 exit evidence. No existing base M1 task receives a dependency on this optional feature. Existing E01/E03 ownership is respected or explicitly serialized, and overlapping shared registration edits are ordered. Native changes require exact-revision macOS CI and real-core boundary integration under AGENTS.md. External evidence is never fabricated to finish a task.

Inspected baseline: `aabfab150eba59ece79646edb78799a7dcba6aba`. Relevant sources: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/dispatch.rs:154`, `core/src/providers/contracts/request.rs:43`, `core/src/providers/contracts/profile.rs:197`, `core/src/store/schema/mod.rs:28`, `docs/architecture/m1-contracts.md:139`, and `docs/features/m1-task-refinement.md`. Production adapter work was in review at drafting; use its merged interfaces, not an unreviewed snapshot.

## Reviewed integration decisions

Fable reviewed the existing plan and both provider branches. The review exposed different prompts/temperatures, incompatible transport traits and discarded metadata. XB02–XB04 add backward-compatible diagnostic inputs and metadata separately for each provider; unsupported diagnostics fail rather than silently using legacy prompts. XB06 uses one secure host HTTP implementation with thin implementations of the landed transport traits. No breaking global transport migration is required to start this feature.

The secret-byte exception is deliberately narrow: `tools/bench` is a developer-only executable crate which can resolve out-of-band credentials for its own host HTTP effects. The application must not link it, and app-linked Rust retains the native credential-reference-only boundary. The exception does not authorize private capture exports into benchmark journals.

E02's existing live-backend evidence task had no transport/composition prerequisite. Add existing V05b, B02 and U05b as prerequisites so it can use the real native transport, assembled app and secure profile setup. Its original acceptance and scope remain intact: record actual synthetic live-backend observations using those existing facilities, and block precisely if credentials, host or evidence are unavailable. No XB task is added to the base M1 graph.

## Haiku-sized execution slices

Each row is a separately reviewed task, with two concrete acceptance clauses and its own behavior tests. The JSON manifest is the machine-readable ownership and dependency overlay; the original M1 ownership groups remain the historical baseline. New paths and specifically listed shared-file extensions below are authorized only in this serialized order.

| Key | Phase | Deliverable | Prerequisites |
| --- | --- | --- | --- |
| XB01 | M1 optional | Define synthetic benchmark records and recoverable run journals | V01 |
| XB02 | M1 optional | Add backward-compatible diagnostic request and call metadata contracts | I06, V05b, XB01 |
| XB03 | M1 optional | Use canonical diagnostic instructions and metadata in Anthropic | XB02, I07 |
| XB04 | M1 optional | Use canonical diagnostic instructions and metadata in OpenAI | XB02, I07 |
| XB05 | M1 optional | Derive independent authorized paired requests | XB02, V02, V03, I07 |
| XB06 | M1 optional | Implement bounded credential-safe host transport | XB01, XB03, XB04, XB05 |
| XB07 | M1 optional | Execute bounded resumable comparison pairs | XB05, XB06 |
| XB08 | M1 optional | Score paired synthetic outcomes with existing evaluation rubrics | XB07, E01, I05 |
| XB09 | M1 optional | Compose explicit synthetic benchmark CLI commands | XB08 |
| XB10 | M1 optional | Render readable paired benchmark reports | XB09 |
| XB11 | M1 optional | Register verified self-hosted profiles in the benchmark | XB10, V09 |
| XL01 | M2 core | Add protected comparison configuration and record storage | T06, XB05 |
| XL02 | M2 core | Bind comparison lifetime to source deletion reset and retention | XL01, L05b |
| XL03 | M2 core | Select eligible comparisons and reserve request budgets | XL02, E03a, J01 |
| XL04 | M2 core | Yield diagnostic jobs to primary processing | XL03, J02b |
| XL05 | M2 core | Run independent challenger jobs without apply authority | XL04, XB03, XB04 |
| XL06a | M2 core | Observe frozen primary requests without blocking interpretation | XL05, I06 |
| XL06b | M2 enablement | Register native comparison effects after the base trial | XL06a, T08, B02, V05b, C01b |
| XL07 | M2 UI | Build opt-in model comparison settings | XL06b, U05b |
| XL08 | M2 core | Query comparison summaries with explicit denominators | XL05 |
| XL09 | M2 core | Query bounded independent-result disagreements | XL08 |
| XL10 | M2 core | Record blinded preferences against immutable pairs | XL09 |
| XL11 | M2 UI | Show a bounded optional disagreement list | XL07, XL09 |
| XL12 | M2 UI | Add a blinded pair preference card | XL11, XL10 |
| XL14 | M2 core | Associate later corrections as weak observational signals | XL10, D03 |
| XL13 | M2 UI | Show comparison summaries and measurement limits | XL12, XL08, XL14 |
| XL15 | M2 core | Replay explicitly selected private examples inside protected storage | XL14, XL05 |
| XL16 | M2 UI | Select private examples for an explicit comparison replay | XL13, XL15 |

### XB01

Create the developer-only Rust bench crate and its versioned experiment/case/attempt records; reserve sibling modules and crate dependencies without implementing execution.

Owned paths: `tools/bench/`, `Cargo.toml`, `Cargo.lock`.

Acceptance:

1. Round-trip synthetic run records with pinned corpus, source/context, prompt/profile/build identities and explicit unknown metadata; reject missing identifiers and private/production database inputs. Journal records contain no credentials or private endpoint values.
2. Append/reopen detects a truncated tail, preserves completed records and records ambiguous started attempts as unknown. Tests cover corruption/schema mismatch and no silent overwrite. Register the crate in normal workspace checks while keeping it out of app dependencies.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`.

### XB02

Extend provider contracts additively for explicit rendered instructions/settings and optional observations, preserving existing production invocation APIs.

Owned paths: `core/src/providers/contracts/`, `core/tests/provider_contract/`.

Acceptance:

1. A diagnostic request carries canonical instructions, normalized context and requested settings without another model answer; outcome metadata distinguishes requested/effective/unknown settings, usage availability and safe provider provenance. Keep credentials and raw request IDs out of public reports.
2. Existing callers and fake adapters compile unchanged; unsupported diagnostic capability fails explicitly rather than invoking a legacy hardcoded prompt. Focused contract tests cover metadata absence, unsupported settings and cancellation/failure with no invented zero usage.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`.

### XB03

Implement the additive diagnostic call using the actual Anthropic adapter and I07 rendered instructions; preserve normal API compatibility.

Owned paths: `core/src/providers/anthropic/`, `core/tests/providers_anthropic/`.

Acceptance:

1. Wire fixtures inspect the exact sent instructions, source/context, model and supported requested settings; no hidden hardcoded prompt or temperature overrides a declared experiment setting. Unsupported settings are explicit.
2. Normalize available usage/effective metadata from the same response while absent values remain unknown; existing protocol, refusal, timeout and size-bound tests still pass. Do not build a second Anthropic parser in the CLI.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `core/src/interpretation/instructions/mod.rs:1`.

### XB04

Implement the corresponding diagnostic call through the existing OpenAI adapter.

Owned paths: `core/src/providers/openai/`, `core/tests/providers_openai/`.

Acceptance:

1. Wire fixtures prove rendered I07 instructions rather than a version label reach the provider with the frozen input/context and explicit supported settings. Unsupported options fail explicitly rather than disappear.
2. Available usage and actual configuration metadata are associated with this attempt; unknown usage remains unknown. Preserve existing error/refusal/cancellation bounds and production callers without introducing a separate CLI vendor implementation.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `core/src/interpretation/instructions/mod.rs:1`.

### XB05

Create pure request derivation and comparability checks usable by both benchmark and later live comparisons; register only this new module in its parent.

Owned paths: `core/src/review/compare/`, `core/src/review/mod.rs`.

Acceptance:

1. From one frozen source revision/time context/rendered instruction bundle derive two profile-pinned requests whose model-visible content is equivalent; no type accepts the opposing answer/rationale. Test mismatched revisions, instructions and unsupported settings.
2. Each arm requires an independent current destination/capability authorization. Reject revoked or unverified profiles; report known configuration differences/unknown defaults and never call agreement accuracy. No authoritative store/apply handle is available.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XB06

Supply one secure host HTTP effect with thin bindings to the landed provider transport traits; secret bytes exist only in this developer crate.

Owned paths: `tools/bench/src/transport/`, `tools/bench/src/lib.rs`, `tools/bench/Cargo.toml`, `Cargo.lock`.

Acceptance:

1. Local fixture servers exercise TLS verification, destination allowlisting, no redirects, body bounds, deadline/cancellation and per-dispatch credential resolution. Reject cross-origin credential use and unapproved destinations; never fall back to another profile.
2. Secret references resolve out of band and values never enter arguments, journals, stdout/stderr or error reports, including error bodies. Tests use synthetic canaries. App-linked Rust and native credential ownership stay unchanged.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `docs/architecture/m1-contracts.md:237`, `docs/architecture/m1-contracts.md:250`.

### XB07

Execute synthetic paired cases via normalized diagnostic adapters and the journal, with explicit attempt budgets and deterministic recorded arm order.

Owned paths: `tools/bench/src/executor/`, `tools/bench/src/lib.rs`.

Acceptance:

1. Counterbalanced sequential arms share the frozen case but not responses; authorize and reserve each attempt before dispatch. Bound retry counts and concurrency; fakes prove budget, cancellation and no real-item/reminder mutation.
2. Resume preserves completed sides and missing/failed/unknown sides with reasons. A crash after sending cannot silently issue another paid attempt; explicit rerun consumes a new allowance and preserves provenance. Never merge results from changed experiment configurations.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `docs/features/model-comparison.md:1`.

### XB08

Extend existing E01 evaluation behavior for matched pairs, retaining its semantic guard oracle and separate scratch state per arm.

Owned paths: `tools/evaluation/comparison/`.

Acceptance:

1. Score each arm against known fixture expectations using actual validator/guard behavior in isolated synthetic scratch stores. Separate false actions/deadlines/completions, missed supported intent, abstentions and invalid outputs; no real store is opened.
2. Report eligible/matched/missing/failed counts with reasons and denominators; unknown or failed arms cannot become passing abstentions. Disagreement, preference and objective correctness remain separate; CI uses fixtures, never paid live calls.

Source pointers: `docs/features/m1-plan.md:722`, `core/src/interpretation/apply/mod.rs:1`.

### XB09

Wire existing modules into a developer command for dry-run, explicit live execution and safe resume.

Owned paths: `tools/bench/src/cli/`, `tools/bench/src/main.rs`, `tools/bench/src/lib.rs`, `docs/validation/model-benchmark.md`.

Acceptance:

1. Command selects exactly two supported profiles and a validated synthetic corpus, displays planned attempts and requires explicit network execution. Dry-run and help require no credentials; neither mode discovers or opens production data.
2. Exercise the complete CLI against fixture servers and cancellation/missing credentials. Results identify fake/recorded/live mode and build/config revisions honestly. Publish minimal usage and secret-reference instructions, with no claim that fixture runs validate actual provider quality.

Source pointers: `docs/features/model-comparison.md:1`.

### XB10

Present a bounded local text/JSON report from existing scored records, without adding an application dashboard.

Owned paths: `tools/bench/src/report/`, `tools/bench/src/cli/`, `tools/bench/src/lib.rs`.

Acceptance:

1. Show per-arm rubric results, matched denominators, missing outcomes and comparable latency/usage summaries. Cost stays unknown without supplied versioned rates; estimates identify provenance and are never presented as billed cost.
2. Reports flag settings/prompt comparability limits and do not choose or activate a winner. Golden synthetic examples cover empty/incomplete/error runs and escaping/redaction, while tests assert substantive counts rather than only substrings.

Source pointers: `docs/features/model-comparison.md:1`.

### XB11

Add only the verified self-hosted adapter to the existing optional benchmark registry.

Owned paths: `tools/bench/src/adapters/`, `tools/bench/src/cli/`, `tools/bench/src/lib.rs`.

Acceptance:

1. Use the landed V09 adapter/capabilities and existing transport/security rules, preserving the independent cloud-only benchmark path. Unsupported diagnostic instructions/settings fail explicitly; a protocol label alone is not evidence.
2. Fixtures verify configured profile/model selection, missing credentials and network failure with no silent fallback. Live self-hosted quality claims require actual run provenance; do not require a reachable Spark to run existing cloud or fake comparisons.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `core/src/providers/self_hosted/mod.rs:1`.

### XL01

Add the minimal versioned experiment/sample schema within the protected application database, with source/revision links and authenticated scoped access.

Owned paths: `core/src/review/compare/store/`, `core/src/review/compare/mod.rs`, `core/src/store/schema/`.

Acceptance:

1. Persist default-off experiment settings and normalized proposal/span metadata using existing migrations and transactions. No API accepts duplicated capture text, raw provider response or secret values; private display joins source only through authorized reads.
2. Migration/reopen tests cover scope isolation, stale/deleted source guards, unknown metadata and failed writes. This module remains unreachable by live sampling until XL02 lifecycle integration and later native registration pass.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL02

Attach diagnostic lifetime rules before any real sample can be written.

Owned paths: `core/src/review/compare/lifecycle/`, `core/src/review/compare/mod.rs`, `core/src/lifecycle/delete_intent/`, `core/src/lifecycle/reset/`, `core/src/lifecycle/retention/`.

Acceptance:

1. Per-item delete/reset atomically fences comparison results and cancels associated jobs; authenticated queries immediately hide diagnostics and preferences. Retention expires samples and related labels, and default exports exclude them.
2. Race/restart tests cover late results, queued selected cases and recreated identifiers without resurrection or orphaned readable records. Reuse existing transaction/reset hooks; no independent tombstone authority or plaintext sidecar.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL03

Implement comparison-specific eligibility plus atomic reservation/enqueue using the existing job queue; reuse suitable pure E03 policy primitives without treating reviewer verdicts as challenger output.

Owned paths: `core/src/review/compare/selection/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Off/default, paused, unknown/session-topic, scope and route exclusions are checked after primary interpretation and again later at dispatch. Explicit session inclusion never grants destination permission. Fast-path cases and unavailable primary traces are reported separately.
2. Atomically reserve a bounded attempt allowance with an idempotent comparison job. Duplicate callbacks, crash/replay and budget exhaustion cannot exceed the cap or block capture. Report post-primary selection bias; unknown monetary cost cannot disable request limits.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`, `core/src/jobs/queue/mod.rs:1`.

### XL04

Add a bounded low-priority diagnostic job class to the existing runner, preserving its lease/retry implementation.

Owned paths: `core/src/jobs/queue/`, `core/src/jobs/runner/`, `core/tests/job_queue/`, `core/tests/job_runner/`.

Acceptance:

1. Eligible primary work wins over comparison work; diagnostic starvation is allowed and reported without catch-up bursts. No second scheduler, lease store or background service is introduced.
2. Deterministic queue/runner tests prove primary progress during queued diagnostics, bounded concurrent requests, lease recovery and pause/cancellation. Never claim that scheduler priority eliminates shared remote-server resource contention.

Source pointers: `core/src/jobs/queue/mod.rs:1`, `core/src/jobs/runner/mod.rs:1`.

### XL05

Execute the challenger from an immutable primary-request trace after eligibility, storing only guarded diagnostic outcome fields.

Owned paths: `core/src/review/compare/handler/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Re-authorize destination, source revision, experiment and reset generation immediately before dispatch and before accepting results. The handler receives diagnostic storage/effects but no authoritative apply/reminder write capability; forged proposals cannot obtain one.
2. Fakes exercise independent request bodies, revoked profiles, stale/deleted sources, timeout and retry budgets. Primary outcome is the actual recorded result for that request, never a relabeled rerun; insufficient trace metadata makes the pair explicitly ineligible.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL06a

Add the primary request/outcome observation seam used by comparison, with frozen input before the answer and eligibility only after its result.

Owned paths: `core/src/interpretation/dispatch/`, `core/tests/interpretation_dispatch/`.

Acceptance:

1. Record the actual normalized primary request/context/settings and actual outcome identity without duplicating source text or leaking credentials. Production processing/apply does not await comparison and preserves its chosen profile/settings; an insufficient or incompatible trace is explicitly excluded rather than substituted with a rerun.
2. Tests prove after-primary eligibility, unknown/session exclusions, no primary-output field in challenger requests, and primary progress during diagnostic storage or enqueue failure. Private writes require the existing XL02 lifecycle guard; feature remains off without later native registration.

Source pointers: `core/src/interpretation/dispatch/mod.rs:1`, `docs/features/model-comparison.md:1`.

### XL06b

Register the completed core comparison observer/handler with existing native effects after the base trial exit, keeping comparison default off.

Owned paths: `ios/Services/ModelComparison/`, `ios/Tests/ModelComparison/`, `ios/AppAssembly/`.

Acceptance:

1. Wire real transport, credentials, authentication and lifecycle to the existing core observer/runner without adding a second scheduler or wait on the capture path. Missing capability leaves comparison disabled.
2. Exact-revision real-core simulator tests inspect outgoing bodies for absence of the opposing answer and verify revocation/relock/deletion plus no diagnostic authoritative mutations. Identify the new candidate revision; do not claim an older signed trial binary has this feature.

Source pointers: `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`, `docs/architecture/m1-contracts.md:260`.

### XL07

Expose existing experiment configuration as a small optional settings surface and register the feature entry after the base app exists. Use the shell actually chosen by P10; do not assume TypeScript, Tauri or SwiftUI in advance.

Owned paths: `app/model-comparison/settings/`, `app/model-comparison/navigation/`, `ios/AppAssembly/`.

Acceptance:

1. Choose challenger, explicit eligible scopes, sample rate and hard attempt cap; default off with separate session-content inclusion and existing destination authorization. Pause and delete-results call durable operations, not local-only UI toggles.
2. Interaction tests cover cancellation, revoked/missing profiles, unknown cost and restart persistence with accessible labels. No setup wizard, badge or recurring grading obligation is introduced.

Source pointers: `docs/features/model-comparison.md:1`.

### XL08

Compute bounded scoped aggregate queries from stored diagnostic outcomes.

Owned paths: `core/src/review/compare/summary/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Expose eligible/selected/completed/matched/failed/skipped/unknown counts and per-arm validation, latency and available usage summaries with defined denominators; no unlabeled accuracy or inferred zero cost.
2. Queries respect authentication/scope/deletion and bound work on empty/large datasets. Fixture tests include missing arms, stale revisions and biased sampling without deriving correctness from agreement or lack of corrections.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL09

Identify meaningful normalized outcome differences without changing authoritative state or inventing a grading backlog.

Owned paths: `core/src/review/compare/disagreements/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Provide a bounded cursor query of current eligible pairs, differing fields and source references, resolving private text only via existing authenticated reads. Unknown/missing arms are not reported as substantive model agreement.
2. Tests cover deleted/stale pairs, paging, duplicate outcomes and meaningful reminder/type differences. Do not persist a second source-text copy or leak hidden model identity through blind-preview fields.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL10

Persist optional A/B/both/neither/skip judgments with stable counterbalanced display order and versioned pair linkage.

Owned paths: `core/src/review/compare/preferences/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Persist side assignment without exposing provider identity until a judgment is saved or explicitly revealed; retries and reopening preserve assignment. Each judgment references exact output revisions and rubric.
2. Preference/delete/relock race tests prove stale pairs cannot receive misleading labels; choices never modify captures, reminders, processing permissions or provider defaults. Deletion/reset removes preferences with the linked sample.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`.

### XL11

Present the existing disagreement query as an optional pull-only view.

Owned paths: `app/model-comparison/disagreements/`.

Acceptance:

1. Show meaningful differences and explicit missing/stale outcomes with authenticated source access; paging is bounded and empty state is calm. No badge, unsolicited notification or must-clear count is introduced.
2. UI tests cover relock, deletion during display, unavailable results and accessible navigation without leaking model identity in blinded summaries.

Source pointers: `docs/features/model-comparison.md:1`.

### XL12

Present one immutable comparison pair and record a voluntary preference via existing operations.

Owned paths: `app/model-comparison/preference/`.

Acceptance:

1. Offer A/B/both/neither/skip with stable randomized order and explicit identity reveal; display source/context through authenticated access. The card sends neither answer to an external judge.
2. Interaction tests verify labels, skip, retry, stale/deleted pairs and relock; saving a choice never changes the item or chosen provider and does not create another user task.

Source pointers: `docs/features/model-comparison.md:1`.

### XL14

Associate relevant existing correction events with comparison revisions without treating them as ground truth.

Owned paths: `core/src/review/compare/corrections/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Link only an observed correction lineage to its actual primary request/sample and preserve event timing; distinguish correction from completion/new intent or unrelated revision where evidence permits, otherwise mark unknown.
2. Query results explicitly identify one-sided primary-exposure bias and missing observations. No correction never counts as success; deletion/retention removes links and tests cover changed intent, duplicate events and stale revisions.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`, `core/src/store/events/mod.rs:1`.

### XL13

Render existing aggregate metrics and deliberate navigation between settings, comparisons and summaries.

Owned paths: `app/model-comparison/summary/`, `app/model-comparison/navigation/`.

Acceptance:

1. Display sample/matched/missing counts, failure and latency outcomes, available usage/cost and optional preferences with clear unknown states. Explain post-primary eligibility and one-sided correction bias.
2. UI tests cover empty/small/incomplete samples and accessibility. No automatic winner/default switch, model-agreement accuracy or claim that the sample represents all captures.

Source pointers: `docs/features/model-comparison.md:1`.

### XL15

Use an authenticated explicit source selection to enqueue independent diagnostic replay pairs through the existing protected queue/store.

Owned paths: `core/src/review/compare/selection_replay/`, `core/src/review/compare/mod.rs`.

Acceptance:

1. Pin source revision, original time context and both authorized profiles; enforce explicit session inclusion, per-arm permission and attempt budgets. Label replay separately from the historical primary outcome; do not export to developer run directories.
2. Fixtures prove deletion/revocation/reset races cancel selection and reject late writes, independent arms share no outputs and neither applies state. Historical user corrections remain optional weak context/labels, not silently leaked into the opposing request.

Source pointers: `docs/architecture/m1-contracts.md:139`, `docs/architecture/m1-contracts.md:205`, `core/src/store/schema/mod.rs:28`, `core/src/providers/contracts/mod.rs:16`, `core/src/providers/contracts/dispatch.rs:124`, `core/src/providers/contracts/request.rs:43`.

### XL16

Add a bounded authenticated selection and run-confirmation flow for optional personal-example comparisons.

Owned paths: `app/model-comparison/selection/`, `app/model-comparison/navigation/`.

Acceptance:

1. User deliberately chooses source revisions and approved profiles, sees planned attempts and explicitly includes session material when wanted. This flow stores references in protected diagnostics and never writes an unmanaged private export.
2. Interaction tests cover cancellation, stale/deleted selection, revoked destinations, budget failure and relock; show honest queued/partial/failure results and keep source capture usable throughout.

Source pointers: `docs/features/model-comparison.md:1`.
