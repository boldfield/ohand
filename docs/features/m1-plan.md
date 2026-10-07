# M1: trust the capture and return loop

Status: executable task specification; implementation and device results are not yet complete.

This plan implements [DESIGN.md](../../DESIGN.md), with autonomous delivery governed by [AGENTS.md](../../AGENTS.md). Codex and Fable jointly reviewed the decomposition. The JSON manifest beside this document is the machine-readable task graph; the prose here and in each task is the implementation contract.

## Execution contract

Every task starts with Haiku, escalation enabled, priority **750**, independent **Opus and gpt-5.6-sol** reviews, and agent merge enabled through Odonian's separate merger. The live fleet supports this Codex reviewer; the design's preferred gpt-6.1-sol identifier was not deployed when this plan was prepared. Required checks and independent review remain mandatory. No routine human design, merge or milestone approval gate is introduced.

Register all tasks READY. Dependencies govern claimability; READY does not mean prerequisites are already complete. Odonian's creation API initially assigns backlog internally, so registration promotes every task before handoff. Do not intentionally leave tasks in backlog or add approval holds. Missing credentials, endpoint access or physical evidence can later create an honest operational block; never paper-complete those checks. Record the exact external input once and continue independent tasks rather than repeatedly retrying an unavailable input.

Task specs contain intent, owned paths, dependency keys and concrete acceptance criteria. Each implementation PR must run the checks established by F04/F05 relevant to its change and add focused behavioral verification for its acceptance criteria. Documentation/evidence tasks validate referenced artifacts and report actual observations. Initial source is documentation-only: no existing application pattern pointers can be supplied honestly. All paths below are proposed ownership boundaries; F01 finalizes them, and implementers resolve these pointers against that contract. Do not independently invent competing interfaces.

F02/F03 reserve core module registrations and native target inclusion so parallel feature tasks can add their own files without concurrently editing central manifests. Shared dependency/registry additions must follow an existing dependency edge or be assigned a separate serialized integration task. B01 owns production bindings; B02 owns production service registration; B03 owns lifecycle order; U01 owns shell wiring. If evidence requires a material contract change, make a focused follow-up task and update affected dependencies before dependent work proceeds. Do not disguise a larger feature inside a small task.

## M1 decisions and limits

- One installed iPhone app; Rust domain core, SQLite durable state, native Swift capture and system services. Tauri versus SwiftUI management shell is decided by P10 from measured probes. Mac and sync wait for M2; state says sync is not configured.
- Target physical evidence: iPhone 16 Pro, user-reported iOS 26.6.2. Record the actual OS build during testing. Do not infer platform API behavior from that version string.
- Production capture uses a foreground native surface. Independent extension writers are outside M1; feasibility probes may evaluate handoff restrictions. Voice explicitly opens the supported recording surface; no always-listening or background-microphone promise.
- Offline on-device transcription only. Unsupported language/model availability preserves durable audio with visible retry/delete options. Raw audio expires seven days after successful transcription; sole untranscribed source remains until explicit resolution. Source text and corrections remain authoritative.
- Provider protocol adapters and domain orchestration live in Rust behind native transport/effect interfaces. Requests execute on the phone with durable retry state. Bounded native background time is best-effort and expiry preserves honest pending state; background completion is not guaranteed. Anthropic, OpenAI and the actual verified self-hosted API use configuration profiles and native credential storage. Spark hardware alone establishes no protocol. No credential or endpoint is committed publicly.
- Each capability destination is authorized before dispatch, including optional review. No automatic vendor fallback. Profile versions prevent queued work from silently changing destination. Single-user owner scoping preserves inexpensive future hosted seams without building a multi-tenant service.
- Original-text FTS and deterministic date/scope filters provide retrieval. No embeddings or generated answers in M1. Query parsing itself never uploads private query text. Uninterpreted or abstaining records remain searchable and do not become suggestion-eligible actions.
- Explicit one-shot reminders use deterministic date resolution and durable reconciliation with native local notifications. Unsupported recurrence is saved as not scheduled and clearly explained; recurring reminders are a candidate for early M2. Clock passage is not delivery evidence. Acknowledge and complete remain distinct.
- Optional daily prompting is off until configured, bounded and independent of explicit reminders. Generic notification text is the default, with current eligible content selected on authenticated open. Explicit opt-in may show eligible item previews from a user-designated preview-safe route (never the universal route by default); private routes never qualify, and already delivered OS banners cannot be recalled. Measure whether either mode is actually useful.
- File protection, credential isolation, private read authentication, backup exclusions/lifecycle, deletion reconciliation and portable export are M1 requirements. Export is not a tested full restore mechanism; device restoration and sync hardening remain M3. Do not describe logical deletion as guaranteed physical flash erasure.
- Runtime review is optional, sampled, bounded and diagnostic. Its failure cannot delay durable capture or mutate authoritative state. Evaluate errors against fixtures and real corrections, not a second model's agreement.

## Completion evidence

The milestone is complete only when T05 records actual-device functionality, T06 provides a tested signed install, and T08 records a real two-week trial meeting the protocol's sample adequacy and lapse checks. A generated checklist, passing simulator or elapsed calendar alone cannot satisfy those tasks. No acknowledged capture loss or duplicate/invented reminder is acceptable in the observed exit matrix. Measure trigger-to-ready and end-of-input-to-save separately. Verify configuration switching between two genuinely different backends; claim self-hosted support only after the actual API probe and adapter tests.

The trial must assess capture avoidance, retrieval success and resurfacing usefulness, including nonuse and a multi-day gap. Muting the only useful return mechanism or consistently avoiding capture means revise the loop before declaring M1 successful. User testing and account-holder actions are evidence/inputs, not approval gates.

## Coverage map

| Requirement | Tasks |
| --- | --- |
| Build, CI, file ownership and privacy-safe fixtures | F01–F07 |
| Actual native/Tauri feasibility and shell choice | P01–P11 |
| Durable authoritative source, corrections and statuses | D01–D05, C01–C06 |
| Durable queue, configuration, providers and privacy routes | J01–J03, V01–V09 |
| Interpretation, abstention, deterministic time and retrieval | I01–I07, R01–R03 |
| Explicit reminders and bounded proactive return | N01–N07, S01–S04 |
| Export, deletion and source-safe retention | L01–L05 |
| Semantic evaluation and optional shadow review | E01–E03 |
| Production bridge, composition and lifecycle | B01–B05 |
| Management, permissions, privacy and accessibility | U01–U11 |
| Faults, live device, upgrades, distribution and trial | T01–T12 |

## Review resolutions

Versioned interpretation instructions are owned by I07. Session-topic is a source-linked, user-correctable facet; personal/work/session scopes and processing permissions are distinct. Spoken mutation of existing items is deferred in M1; source is retained and UI changes remain supported. U03 owns explicit transcript correction, U02 owns retained-audio playback.

The management shell and base composition do not wait for Spark access or optional shadow review. T11 integrates those capabilities afterward; S04 adds explicitly authorized previews afterward. The physical exit matrix verifies every capability claimed for the trial. Spark, shadow review and preview features may remain disabled without holding up the base signed build or trial; their own canaries must pass before enablement. Those tasks still start READY and run when dependencies and real inputs permit. Missing inputs never certify compatibility. Actual probe/evaluation/device results must link named sanitized artifacts and exact build identifiers, not just authored results prose.

Native work is validated by standard macOS CI for the exact submitted revision; Linux workers may author native code but cannot substitute Linux checks for a successful native build. F05 establishes and demonstrates that result-collection path. T10 produces a signed candidate, T05 tests it, and T06 distributes that same candidate. A two-week trial requires a signing lifetime or tested renewal plan covering its duration.

Native effect tasks require a simulator integration check against the real core. By default, the save acknowledgment explicitly distinguishes saved-but-unprocessed from a confirmed installed reminder; an interrupted transcript must never imply reminder success.

## Task specifications

### F01

**Specify M1 architecture boundaries and versioned interfaces**

Dependencies: none.

Owned paths: `docs/architecture/m1-contracts.md`.

Fix the initial Rust/SQLite core, native Swift capture/services, foreground job execution, single-phone/no-sync baseline, and replaceable management shell. Publish a module/file ownership map and prose contracts before implementing those interfaces.

Acceptance and verification:

- Define stable capture/item IDs, per-item revision, source versus correction/derived state, privacy routes, proposal/job/profile versions, time context and distinct save/sync/schedule/delivery states.
- Define native-to-core ownership and error/lifetime contracts, ingress handoff, provider capabilities, notification identifiers, data protection and backup policy; list fields and invariants without implementation snippets.
- Map every implementation area to an owned module; shared schema/registration edits must be ordered or reserved for an integration task. Record supported one-shot reminders and honest unsupported repeats.
- Keep this to a bounded prose contract and ownership map, not application implementation. Define privacy scopes (personal/work/session), a user-correctable session-topic facet independent of note/action/idea, and preview-safe route permission which classification can only narrow.
- Record these settled choices rather than redesign them: Rust + SQLite owns domain state, provider adapters and orchestration; Swift owns protected storage/credentials, audio, native transport, notification effects and lifecycle; phone-only M1; foreground capture; on-device transcription; original-text FTS; one-shot reminders; no AI disclosure gate; no sync or hosted tenancy. Define injectable transport/effect interfaces for these boundaries.

### F02

**Create the Rust core workspace and executable test harness**

Dependencies: F01.

Owned paths: `core/`, `Cargo.toml`, `Cargo.lock`, `Makefile`.

Create the minimal framework-independent Rust core workspace, module skeletons defined by F01, and real build/lint/test commands. No product features.

Acceptance and verification:

- Linux clean checkout builds, lints and runs a meaningful interface/serialization smoke test using pinned dependencies.
- Core has no Tauri/webview dependency; future modules can be filled without concurrent edits to one shared root. Document command behavior and fail on real test failures.
- Declare the initial M1 dependencies and module inclusion strategy up front, including SQLite/time/serialization; parallel tasks must not independently mutate Cargo.toml/Cargo.lock. Later additions require serialized dependency work.

### F03

**Create a reproducible native iOS probe project**

Dependencies: F01.

Owned paths: `ios/`, `ios/project.yml`.

Create a generated native Swift project with a minimal SwiftUI probe screen and native module skeletons; keep generated project files out of parallel leaf edits.

Acceptance and verification:

- Document a reproducible unsigned simulator build and selected provisional SDK/deployment baseline; a clean checkout generates the same targets.
- Use a unique project bundle identifier under the intended domain and configurable signing team. No certificates, provisioning profiles or credentials are committed.
- Reserve probe/control targets and relevant usage strings; use deterministic directory-based source inclusion so concurrent native tasks do not edit project.yml. Production capture uses a foreground native surface.

### F07

**Define the M1 device and two-week trial evidence protocol**

Dependencies: F01.

Owned paths: `docs/validation/m1-protocol.md`.

Define pass/fail evidence before collecting it, using the DESIGN M1 exit criteria and no daily streak requirement.

Acceptance and verification:

- Specify device/OS metadata, offline/interruption/permissions/Mac-asleep cases, latency measurement points, a multi-day lapse, sample adequacy and two-week elapsed observation window.
- Report only content-free metrics publicly; private examples stay local. Unmet evidence is explicitly unmet, not a completed trial; define when more observation or loop revision is required.

### F04

**Add Linux core CI and documented worker checks**

Dependencies: F02.

Owned paths: `.github/workflows/core.yml`, `AGENTS.md`.

Wire the actual Rust checks to standard Linux GitHub runners and document which checks Linux Odonian workers must run.

Acceptance and verification:

- PR checks run on a clean checkout with locked dependencies and fail on test/lint failure.
- No-op success placeholders are forbidden; document that simulator/device evidence is separate and cap artifact retention.

### D01

**Implement SQLite schema and migration transactions**

Dependencies: F02.

Owned paths: `core/src/store/schema/`, `core/tests/migrations/`.

Create the versioned schema for source records, item revisions, events, derived results, jobs, profiles and reminder/suggestion metadata from F01.

Acceptance and verification:

- Empty database migrates atomically; interrupted migration recovers or fails visibly without losing source records.
- Schema/version validation and injected clock tests run on Linux; forward-incompatible database versions cannot be silently opened writable.
- Own the initial table/migration envelope for all M1 modules from F01; subsequent feature tasks use those seams. New schema changes need ordered follow-up migration tasks, not concurrent edits here.

### I02

**Resolve supported date/time expressions deterministically**

Dependencies: F02.

Owned paths: `core/src/time/`, `core/tests/time/`.

Implement a deliberately bounded date/time resolver with captured locale/timezone/reference clock and an explicit ambiguity result.

Acceptance and verification:

- Table tests cover explicit dates, tomorrow/weekday/since phrases, daylight-saving gaps/folds, timezone changes, past times and unsupported repeats.
- Never invent a missing hour or silently normalize an ambiguous time; preserve the original phrase and resolved context for display.

### F05

**Add macOS simulator CI for native probes**

Dependencies: F03, F04.

Owned paths: `.github/workflows/ios.yml`, `Makefile`, `AGENTS.md`, `ios/project.yml`.

Add reproducible native simulator build/tests on standard macOS runners without requiring signing secrets.

Acceptance and verification:

- A real simulator launch/smoke test runs in CI and its logs identify toolchain/device versions.
- Fork PRs require no credentials; use least-privilege workflow permissions and bounded artifacts. Document meaningful native check commands and evidence consumption by Linux reviewers.
- Linux workers must trigger and await the standard macOS runner checks for the exact submitted commit, link its run and artifacts before review, and block if unavailable. Demonstrate this remote validation path with one probe PR; Linux-only validation cannot certify Swift.

### D02

**Implement idempotent durable raw capture storage**

Dependencies: D01.

Owned paths: `core/src/store/captures/`, `core/tests/capture_storage/`.

Persist source text/audio references and capture context under stable caller-generated IDs.

Acceptance and verification:

- Identical retries return the same record; conflicting reuse of an ID fails rather than replacing source words.
- Fault injection around transaction boundaries proves that acknowledgment happens only after durability; source provenance/timezone/privacy metadata persist across reopen.

### V01

**Define provider profiles and a fake-provider contract harness**

Dependencies: D01.

Owned paths: `core/src/providers/contracts/`, `core/tests/provider_contract/`.

Implement versioned non-secret provider profiles and normalized interpretation request/response/error contracts.

Acceptance and verification:

- Profiles declare protocol/model/capabilities/credential references/timeouts/permitted destinations; validation rejects incomplete or unsupported profiles.
- A deterministic fake tests timeout, cancellation, unavailable/invalid output and bounded response size, with no provider history as canonical memory.

### F06

**Add public-fixture and secret hygiene checks**

Dependencies: F04, F05.

Owned paths: `tools/hygiene/`, `.github/workflows/hygiene.yml`, `docs/contributing.md`.

Enforce public-repository fixture hygiene without pretending a filename scan proves privacy.

Acceptance and verification:

- Use a maintained secret scanner plus policy checks for private captures, recordings and signing material; synthetic audio is allowed only under explicitly documented fixture paths/provenance.
- Seeded test secrets/private-path fixtures fail the check, approved synthetic fixtures pass, and no real secret is printed in logs.

### P01

**Prove the Rust-to-Swift boundary on a simulator**

Dependencies: F02, F03, F05.

Owned paths: `core/bindings/`, `ios/BridgeProbe/`, `docs/validation/core-binding.md`.

Build the smallest real Rust/Swift call boundary before committing native product code to it. Prefer maintained binding generation; do not build a general RPC layer.

Acceptance and verification:

- Round-trip a capture-shaped value and normalized failure; verify Unicode, large input bounds, cancellation/lifetime and memory ownership.
- Build and run the probe under the simulator and core harness. If the boundary cannot be supported, document the blocker and require an architecture revision before dependent work.

### P05

**Probe local notification scheduling and limits**

Dependencies: F03, F05.

Owned paths: `ios/NotificationProbe/`, `docs/validation/notification-probe.md`.

Verify native scheduling/list/cancel primitives and the platform behavior relevant to the reminder contract.

Acceptance and verification:

- Test permission states, duplicate identifiers, due-time/timezone behavior and delivery with app UI closed; separate simulated evidence from physical testing.
- Determine pending-request limits from current primary documentation and observed behavior; explain what the OS can and cannot report about delivered/seen/missed notifications.

### P06

**Create the minimal Tauri 2 iOS management-shell probe**

Dependencies: F02, F03, F05.

Owned paths: `probes/tauri/`, `docs/validation/tauri-build.md`.

Build a minimal Tauri candidate for management/settings screens; native capture remains independent.

Acceptance and verification:

- Pinned clean-checkout build and simulator launch work, with setup/repro steps and one native round-trip.
- No product logic is duplicated in the web UI and no provider key is passed into JavaScript; incompatibilities are recorded, not hidden by a different untested target.

### P08

**Implement the signing and device-build procedure**

Dependencies: F03, F05.

Owned paths: `tools/apple-build/`, `docs/validation/signing.md`.

Provide reproducible signed-build automation using externally supplied Apple identity and credentials.

Acceptance and verification:

- Unsigned simulator path remains credential-free; signing inputs come only from secure local/CI injection, never command output or repository files.
- With supplied inputs, produce and install a signed probe build and record content-free build/device identifiers; absent inputs leave actual signing evidence blocked, not claimed.
- Document temporary keychain cleanup and account-holder enrollment steps without requiring per-build human approval.
- Choose a signing/distribution route whose actual install validity covers the two-week trial, or explicitly plan and test renewal without data loss. Do not promise an unverified enrollment or expiry duration.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### D03

**Implement revisioned user corrections and lifecycle events**

Dependencies: D02.

Owned paths: `core/src/store/events/`, `core/tests/events/`.

Add explicit user correction, completion/cancellation and suggestion-control events with compare-and-set revision semantics.

Acceptance and verification:

- Conflicting stale updates are rejected; event retries have one effect and clear error states.
- User corrections remain separate from original source, and tests distinguish broad intentions, notes, ideas and actions without generating obligations.

### J01

**Implement durable capability jobs with retry leases**

Dependencies: D02.

Owned paths: `core/src/jobs/queue/`, `core/tests/job_queue/`.

Queue asynchronous interpretation/transcription-related work without delaying capture acknowledgment.

Acceptance and verification:

- Jobs keyed by capture/capability/config version survive crash and retry; interrupted leases recover and duplicate delivery cannot apply twice.
- Backoff is bounded, offline jobs remain inspectable, cancellation is durable, and deleted/stale targets cannot be claimed for mutation.

### V02

**Implement destination and capability authorization**

Dependencies: V01.

Owned paths: `core/src/privacy/routing/`, `core/tests/routing/`.

Authorize every processing job before payload leaves the device, using stored capture policy and configured destinations.

Acceptance and verification:

- Fresh install is local-only; cloud, private-server and reviewer routes require explicit setup authorization per capability.
- Tests prove a classifier cannot upgrade disclosure permission, a provider outage cannot reroute payloads, and malicious stored instructions cannot change routing policy.

### V08

**Verify the actual Spark serving protocol and phone reachability**

Dependencies: V01, F07.

Owned paths: `docs/validation/spark-protocol.md`, `tools/provider-probe/`.

Inspect the actual user-controlled endpoint with authorized synthetic requests; identify serving software, protocol, model and supported capabilities.

Acceptance and verification:

- Record a sanitized compatibility matrix and reproducible probe with secrets/addresses supplied externally; no private address or credential is published.
- Verify reachability from the intended phone context, authentication/TLS and structured-response behavior. Missing endpoint access leaves this evidence task blocked, not generically declared OpenAI-compatible.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### P11

**Probe credential protection independently of the core**

Dependencies: F03, F05.

Owned paths: `ios/CredentialProbe/`, `docs/validation/credential-probe.md`.

Use synthetic Keychain entries to establish credential accessibility and native isolation before the shell decision.

Acceptance and verification:

- Probe can build without production schema/bridge dependencies; document accessibility choices and lock/relaunch tests for P09.
- Record simulator results honestly and leave hardware-only checks for P09; never put a credential into a webview or public evidence.

### P02

**Probe native control handoff and protected ingress**

Dependencies: P01.

Owned paths: `ios/CaptureProbe/`, `docs/validation/capture-entry.md`.

Measure supported system-control/shortcut handoff to a native capture screen and write a synthetic durable ingress record.

Acceptance and verification:

- Test cold and warm launch, unlocked/locked and before/after first unlock behavior where available; distinguish simulator results from physical device evidence still needed.
- Do not promise background microphone or bypass authentication. Capture entry cannot expose private history, and the handoff retains one stable capture ID.

### D04

**Implement authoritative projections and model-write guards**

Dependencies: D03.

Owned paths: `core/src/domain/items/`, `core/tests/item_state/`.

Derive current item state from sources, explicit updates and permitted model annotations under a documented precedence rule.

Acceptance and verification:

- Valid transitions are tested and impossible transitions rejected; model reprocessing cannot undo corrections, completion or cancellation.
- A rebuild from retained records/events equals current projections; invalid or stale derived output preserves the prior state rather than demoting an item.

### V03

**Pin jobs to profile versions and handle configuration changes**

Dependencies: V02, J01.

Owned paths: `core/src/jobs/configuration/`, `core/tests/job_configuration/`.

Bind queued work to its original approved profile/policy version and define safe retirement/retry after edits or revocation.

Acceptance and verification:

- Switching the default provider never silently dispatches old queued content to a new destination; explicit requeue is inspectable.
- Revocation stops future dispatch and makes late results ineligible where required; credentials are resolved at execution without being persisted into the job payload.

### V04

**Implement native credential storage and redaction**

Dependencies: P01, V01.

Owned paths: `ios/Services/Credentials/`, `ios/Tests/Credentials/`.

Store provider secrets in native Keychain behind opaque references and expose only approved operations across the core/UI boundary.

Acceptance and verification:

- Add/update/delete handles invalidated keys and storage errors; webview, control extensions, logs, exports and synced profile data never receive secret values.
- Use appropriate accessibility class and verify lock/relaunch behavior with synthetic secrets; do not grant broad shared credential access just to simplify a probe.

### P03

**Probe recording interruption and partial-audio recovery**

Dependencies: P02.

Owned paths: `ios/AudioProbe/`, `docs/validation/audio-probe.md`.

Exercise native recording and durable partial-file handling independently of classification or networking.

Acceptance and verification:

- Synthetic recording survives cancellation/interruption with a recoverable saved prefix or an honest unsaved failure; no success before durable finalization.
- Document permission denial, foreground/background and lock constraints, sample format, duration/size bounds and the measurements to repeat on a real phone.

### P07

**Probe native-to-Tauri handoff and management accessibility**

Dependencies: P02, P06.

Owned paths: `probes/tauri-handoff/`, `docs/validation/tauri-handoff.md`.

Connect the native entry probe to the candidate management UI using stable identifiers only.

Acceptance and verification:

- Cold/warm handoff preserves capture identity and does not require a running webview to save; unknown or malicious routes are rejected.
- Record keyboard, VoiceOver, large-text and secret-isolation behavior with a defined repeatable device procedure.

### D05

**Implement separate save, processing and reminder status values**

Dependencies: D04.

Owned paths: `core/src/domain/status/`, `core/tests/status/`.

Represent local save, optional sync, processing, schedule installation and delivery capability as distinct facts.

Acceptance and verification:

- M1 sync truthfully reads not configured; neither a source save nor a model response implies an installed reminder.
- Permission denial, pending ambiguity, provider outage and expired scheduling opportunity have distinguishable status and never claim user attention.

### I01

**Implement interpretation proposal schemas and provenance**

Dependencies: D04, V01.

Owned paths: `core/src/interpretation/contracts/`, `core/tests/proposals/`.

Represent note/action/idea annotations, reminder proposals and updates as validated, source-linked candidates rather than unrestricted state writes.

Acceptance and verification:

- Each candidate references capture ID/revision, source spans and processing version; reject unknown fields/IDs, invalid bounds and unsupported operations.
- Represent abstention and uncertain targets/times explicitly; schema validity alone is never treated as proof of semantic intent.
- Session-topic is an independent derived facet with source evidence and user correction. Privacy/read scope is explicit policy, never a classifier-created authorization. Unsupported spoken changes to existing items abstain in M1; UI corrections/completion remain supported.

### R01

**Implement transactional original-text indexing and rebuild**

Dependencies: D04.

Owned paths: `core/src/retrieval/index/`, `core/tests/search_index/`.

Add SQLite full-text indexing for original and explicitly corrected text, preserving source attribution and privacy scope.

Acceptance and verification:

- Insert/correction/deletion hooks maintain the index transactionally; crash/rebuild cannot invent or resurrect records.
- Index rebuild and direct-source fallback agree on accessible records, and derived model summaries are not presented as original quotations.

### N01

**Implement reminder desired-state and operation records**

Dependencies: D04, I02.

Owned paths: `core/src/reminders/state/`, `core/tests/reminder_state/`.

Store one-shot reminder intent, stable native identifiers and durable schedule/cancel operations separately from delivered/seen facts.

Acceptance and verification:

- Creating/editing/cancelling/completing an action produces idempotent desired operations; pending ambiguity and expired opportunity remain inspectable.
- Unsupported repeating requests are not silently reduced to one shot; tests reject fabricated deadlines and preserve explicit user corrections.

### S01

**Implement transparent suggestion eligibility and rotation**

Dependencies: D04.

Owned paths: `core/src/suggestions/eligibility/`, `core/tests/suggestion_eligibility/`.

Select from eligible undated actions without fabricated urgency, with stored reasons and deterministic clock-controlled rotation.

Acceptance and verification:

- Exclude completed/cancelled/deleted/snoozed/pull-only and ineligible private items; record why the selected item was eligible.
- Not-now uses a stated cooldown, stop-suggesting is durable pull-only, and nonresponse never completes/cancels an intention or manufactures importance.
- Uninterpreted records and unsupported/ambiguous intent are searchable but never eligible as inferred actions.

### P04

**Probe offline on-device transcription capabilities**

Dependencies: P03.

Owned paths: `ios/TranscriptionProbe/`, `docs/validation/transcription-probe.md`.

Determine an actually supported on-device transcription path and its model/language/download constraints.

Acceptance and verification:

- Recognize synthetic audio with network unavailable; verify the API is required to stay on device rather than silently use cloud fallback.
- Unsupported device/language, missing model and revoked permission preserve audio with a visible pending/unavailable state. Document setup requirements and evidence limits.
- Build and API availability may be checked in a simulator, but actual offline recognition and language/model availability are certified only by P09. Measure transcription duration and lock/background interruption on-device before making timing claims.

### I03

**Implement narrow offline reminder and session-topic recognition**

Dependencies: I01, I02.

Owned paths: `core/src/interpretation/fast_path/`, `core/tests/fast_path/`.

Recognize a documented limited set of explicit reminder commands and abstain on ambiguous or unsupported utterances.

Acceptance and verification:

- Minimal pairs test negation, quotations, hypothetical/reported speech, already-completed work, missing times and repeats; starts-with-remind-me is not sufficient.
- A fully supported command produces a sourced candidate offline; other clear language remains available to the interpreter instead of being permanently discarded or silently scheduled.
- Recognize a bounded documented set of explicit session-topic phrases locally, such as Bring this up in therapy, with negated/quoted minimal pairs and abstention. This sets a derived session-topic facet only, never privacy scope or upload permission.

### I04

**Author synthetic intent fixtures with expected and forbidden outcomes**

Dependencies: I01, I02.

Owned paths: `fixtures/intent/`, `docs/validation/intent-fixtures.md`.

Create privacy-safe contrastive fixtures for the whole M1 interpretation contract before live model testing.

Acceptance and verification:

- Cover DESIGN examples, mixed note/task captures, corrections, ambiguity, broad intentions, quotes/negation, prompt injection, dates/timezones and dropped-ASR-word scenarios.
- Each fixture states expected and forbidden mutations and its provenance; a second LLM is not the sole oracle. Label cases where the transcript lacks enough information to recover truth.
- Spoken existing-item updates such as Done with the roofer call are explicitly unsupported in M1: preserve the source and expect no target mutation. Label every recorded-provider fixture synthetic; do not imply it came from live account traffic.

### I05

**Implement proposal validation and atomic application**

Dependencies: I01, D04, J01, V03.

Owned paths: `core/src/interpretation/apply/`, `core/tests/proposal_application/`.

Validate candidate schema, source, revision, policy and lifecycle before applying replaceable derived state.

Acceptance and verification:

- Invalid/stale/unauthorized results preserve prior state; atomic application and job completion prevent duplicate effects after crash.
- Reminder candidates require explicit intent and deterministic resolution consistent with source/context; unsupported grammar stays unscheduled with original intention intact.
- Failed or abstaining first interpretation leaves an explicit uninterpreted, searchable source record, excluded from suggestions; later failure retains prior authoritative state.

### R02

**Implement scoped retrieval and literal query results**

Dependencies: R01.

Owned paths: `core/src/retrieval/query/`, `core/tests/retrieval/`.

Return source-linked text matches under type, date, status and processing/privacy-route filters.

Acceptance and verification:

- Enforce scope before searching/ranking/results; work-only requests cannot leak private text through snippets, counts or errors.
- Stable pagination/order and honest no-match behavior are tested using synthetic mixed-domain records; no generated answer or embedding dependency.
- Support session-topic and explicit privacy/read scope filters together; a local-only session note remains retrievable without a configured provider.

### L01

**Implement idempotent deletion intent and processing tombstones**

Dependencies: D04, J01, R01, N01.

Owned paths: `core/src/lifecycle/delete_intent/`, `core/tests/deletion_intent/`.

Atomically mark user-requested deletion, remove readable/indexed content and stop future processing while retaining only minimal non-content recovery metadata.

Acceptance and verification:

- Racing job result, correction, duplicate delete and stale source replay cannot restore readable text.
- Enqueue durable cleanup for audio/ingress/native notifications and define deletion progress; a crash cannot report deletion complete before required effects finish.

### B01

**Maintain the production Rust-Swift bridge**

Dependencies: P01, D05, V01.

Owned paths: `core/src/ffi/`, `ios/OhAndCoreBridge/`, `tools/bindings/`.

Turn the proven boundary into a reproducible, maintained typed bridge used by production native services.

Acceptance and verification:

- Generate and verify simulator/device bindings in documented builds; keep ABI ownership, cancellation, threading and error conversion explicit.
- Exercise actual persistence and status calls through Swift; test repeated initialization, invalid input and background-thread callbacks without UI-thread violations.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.
- Each owned module declares its exports through the reserved interface mechanism; generated bindings are reproducible build output, not shared committed files. Native consumers depend on this bridge before adding effects.

### V05

**Implement bounded native HTTP transport for providers**

Dependencies: V02, V04, B01.

Owned paths: `ios/Services/ProviderTransport/`, `ios/Tests/ProviderTransport/`.

Create the native request transport used by adapters, with authorization enforcement before any request and Keychain resolution at dispatch.

Acceptance and verification:

- Timeout, cancellation, response bounds, error normalization, redacted diagnostics and endpoint/TLS validation are tested through a controlled fixture server.
- Reject cross-origin redirect credential forwarding and silent cleartext/certificate bypass; user-configured private endpoints work only under explicitly documented supported transport.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### C01

**Implement native data protection and private-read authentication**

Dependencies: P02, V04, D02, B01.

Owned paths: `ios/Services/ProtectedStorage/`, `ios/Services/Authentication/`.

Apply native file/keychain protection and a session-scoped read-auth boundary across capture storage and private views.

Acceptance and verification:

- Document accessibility before/after first unlock, app-switcher redaction and OS backup inclusion/exclusion; locked capture never grants private-history access.
- Tests cover denied/cancelled authentication and relock/foreground transitions. Failed unlock does not erase or disclose stored content.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### R03

**Implement supported natural-language retrieval filters**

Dependencies: R02, I02.

Owned paths: `core/src/retrieval/phrases/`, `core/tests/query_phrases/`.

Translate documented query phrases such as private session notes since a date into local query filters while preserving literal search fallback.

Acceptance and verification:

- Common since/date/type expressions resolve with explicit timezone/context; uncertain phrases expose optional clarification instead of silently choosing a date.
- Tests retrieve original words without routing private queries to a model, and unsupported phrasing still has useful literal search.

### N02

**Implement native notification bridge with generic payloads**

Dependencies: P05, N01, B01.

Owned paths: `ios/Services/Notifications/`, `ios/Tests/Notifications/`.

Implement schedule/cancel/list and event ingestion behind the core notification contract.

Acceptance and verification:

- Stable identifiers round-trip, errors are normalized, and notification content contains only generic wording plus an opaque item/action identifier.
- Bridge transports opaque validated identifiers only; authenticated reads and user mutations are owned by N07. Credentials never enter payloads.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### E01

**Implement semantic evaluation reports for deterministic and recorded runs**

Dependencies: I04, I05, V01.

Owned paths: `tools/evaluation/`, `docs/validation/evaluation-format.md`.

Run fixture expectations through the actual fast path/application guard and recorded provider responses with reproducible reports.

Acceptance and verification:

- Report false actions/deadlines/completions, abstentions, unsupported claims and class-level failures separately; no agreement-as-accuracy score.
- CI requires zero forbidden authoritative mutations in the synthetic safety corpus; record corpus version and avoid scoring unavailable live backends as passed.
- Recorded response fixtures are synthetic authored scenarios unless a specific authorized live-run provenance is attached; reports distinguish fake/recorded/live execution and never infer live quality from synthetic passing.

### P09

**Collect actual-device feasibility evidence**

Dependencies: P03, P04, P05, P07, P08, F07, P11.

Owned paths: `docs/validation/device-feasibility.md`.

Execute the documented native and Tauri probe matrix on the actual target phone, using synthetic content.

Acceptance and verification:

- Record measured cold/warm/locked/unlocked capture, permission states, interruptions, offline ASR, notifications with UI closed, handoff, accessibility and secure credential behavior.
- Measure trigger-to-ready and end-of-input-to-durable-save separately. Physical-device evidence cannot be substituted by simulator or a blank checklist.
- If access or capabilities are missing, record the precise unmet check and block rather than certify the matrix.
- Baseline target is iPhone 16 Pro, user-reported iOS 26.6.2; record the actually observed OS build, signing and credential-lock behavior in sanitized evidence.
- Offline speech availability and time-to-transcript are physical-device checks, including immediate screen lock; logs must distinguish saved audio from successful transcription and installed reminders.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### I07

**Define versioned interpretation instructions and provider-neutral mapping**

Dependencies: I01, I04.

Owned paths: `core/src/interpretation/instructions/`, `core/tests/interpretation_instructions/`.

Own the actual instructions, request context and response mapping used by interpretation adapters.

Acceptance and verification:

- Instructions and output contract are versioned, source text is treated as untrusted data, and capture/profile/time context is explicit.
- Synthetic golden request/mapping fixtures cover negation, mixed intents, session topics and abstention; instructions cannot confer disclosure permissions or authorize unsupported existing-item updates.

### V06

**Implement the Anthropic interpretation adapter**

Dependencies: V01, V05.

Owned paths: `core/src/providers/anthropic/`, `core/tests/providers_anthropic/`.

Implement the documented Anthropic protocol for the M1 structured interpretation capability.

Acceptance and verification:

- Synthetic protocol fixtures cover valid/invalid structured output, auth failure, rate limit, timeout and cancellation through shared contracts.
- Model and account come from the profile; no embedded service keys, protocol-specific types in domain state, or unsupported speech capability claims.

### V07

**Implement the OpenAI interpretation adapter**

Dependencies: V01, V05.

Owned paths: `core/src/providers/openai/`, `core/tests/providers_openai/`.

Implement the documented OpenAI protocol for the same provider-independent M1 interpretation capability.

Acceptance and verification:

- Pass the same semantic/protocol contract fixtures, including malformed output, refusal, rate limit, auth failure and cancellation.
- Use configured supported API credentials; do not assume a chat subscription is an API credential. Source capture storage and domain contracts remain unchanged.

### V09

**Implement the verified self-hosted provider adapter**

Dependencies: V05, V08.

Owned paths: `core/src/providers/self_hosted/`, `core/tests/providers_self_hosted/`.

Support precisely the protocol established by V08, reusing transport without requiring public exposure of the server.

Acceptance and verification:

- Configured model/endpoint/auth passes recorded contract fixtures plus the verified endpoint smoke test; unsupported capabilities are explicit.
- Away-from-network failure keeps captures queued and never falls back to a cloud provider; report the exact verified compatibility boundary.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### C02

**Implement durable native ingress and core import**

Dependencies: P01, C01, D02, B01.

Owned paths: `ios/Services/Ingress/`, `core/src/ingress/`, `ios/Tests/Ingress/`.

Implement durable native foreground save and core ingestion for the verified entry-point handoff. Production M1 opens the foreground native capture surface; do not add a separate extension writer.

Acceptance and verification:

- Durable acknowledgement follows committed source persistence. Repeated delivery, interruption and restart produce neither duplicate items nor missing acknowledged captures.
- Test atomic file/DB ownership, malformed/partial ingress, recovery and cleanup after confirmed import inside protected storage.
- Any later independent extension writer needs a separate task and proven concurrency/protection contract; it is not part of this task.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### N03

**Implement idempotent reminder reconciliation**

Dependencies: N01, N02.

Owned paths: `core/src/reminders/reconcile/`, `core/tests/reminder_reconcile/`.

Reconcile durable desired reminder state with OS pending requests after write, retry and relaunch.

Acceptance and verification:

- Crash between OS success and local acknowledgment does not duplicate requests; changed/cancelled/completed/deleted items remove stale requests.
- Late OS callbacks cannot resurrect a cancelled reminder; use explicit revision/effect identities and tests for duplicate/out-of-order completion.

### E03

**Add optional budgeted shadow-review instrumentation**

Dependencies: V03, E01, V04, B01.

Owned paths: `core/src/review/shadow/`, `ios/Services/ShadowReview/`.

Implement opt-in sampled diagnostic review behind an independent approved profile and bounded request budget.

Acceptance and verification:

- Default off and never required for capture; timeout records unreviewed, and no verdict can mutate items, reminders or routing permissions.
- Capture only privacy-approved evaluation data and compare against known expectations/corrections; enforce sample/budget limits and expose configuration and reviewer-induced disagreements.

### P10

**Record the management-shell decision from feasibility evidence**

Dependencies: P09.

Owned paths: `docs/architecture/m1-shell.md`.

Choose Tauri or SwiftUI management UI based on the completed feasibility evidence; preserve Rust core and native capture boundary.

Acceptance and verification:

- State measured reasons, source paths to retain, integration/build implications and any remediation; choose one production shell, not two maintained implementations.
- Do not pass a failed capture/accessibility/privacy requirement by weakening it. Required unresolved blockers prevent this decision task from completing.

### I06

**Implement the interpretation dispatcher across provider adapters**

Dependencies: V06, V07, I03, I05, I07.

Owned paths: `core/src/interpretation/dispatch/`, `core/tests/interpretation_dispatch/`.

Wire raw saved captures through fast-path handling or approved-provider interpretation into the existing validation/apply boundary.

Acceptance and verification:

- A capture actually produces the supported annotations/action/reminder candidate using a selected adapter; no provider call bypasses routing or blocks raw save.
- Model failure preserves source/prior state; clear free-form intent remains usable, and processing unavailability is surfaced honestly. Add integration fixtures covering the complete dispatcher path.

### C03

**Implement the silent capture surface**

Dependencies: P02, C02, D05.

Owned paths: `ios/Capture/Text/`, `ios/Tests/TextCapture/`.

Provide a direct native text field and save action reachable without opening a task dashboard.

Acceptance and verification:

- Offline capture uses ingress and returns an honest save result; repeated save/interrupt/relaunch retains the same intended record.
- No obligatory tags/project selection, classification wait, coaching or inbox cleanup; keyboard and large-text behavior are tested.

### C04

**Implement native voice capture and recovery UI**

Dependencies: P03, C02, D05.

Owned paths: `ios/Capture/Voice/`, `ios/Tests/VoiceCapture/`.

Ship the recording control with durable stop/interruption recovery and understandable recording/save states.

Acceptance and verification:

- Microphone denied/interrupted/locked/terminated paths preserve recoverable audio or an honest failure; re-entry shows resumable/recoverable source without a maintenance queue.
- Recording duration/size limits and cancellation are explicit, no always-listening behavior, and no network request occurs for raw recording.

### N04

**Implement notification capacity and permission reconciliation**

Dependencies: N03, D05.

Owned paths: `core/src/reminders/capacity/`, `ios/Services/NotificationPermission/`.

Respect actual pending-request capacity and changed permission states, with explicit reminders taking precedence over optional prompts.

Acceptance and verification:

- Boundary tests prove no silent loss when the cap is reached; expose unscheduled/undeliverable state and a documented horizon/refill strategy.
- Permission changes, Focus-related uncertainty and elapsed due times do not falsely mark delivery or user attention; reconcile again on supported foreground events.

### L02

**Implement native deletion cleanup reconciliation**

Dependencies: L01, C02, N03.

Owned paths: `ios/Services/Deletion/`, `ios/Tests/Deletion/`.

Execute and reconcile deletion effects across audio, ingress, caches, jobs and notifications.

Acceptance and verification:

- Reopen after each injected cleanup interruption converges without private remnants becoming searchable or notifications remaining active.
- This task covers per-item cleanup only; delete-all generation fencing and optional credential reset belong to L05.

### J02

**Implement the foreground lifecycle job runner**

Dependencies: J01, V03, I06, B01.

Owned paths: `core/src/jobs/runner/`, `core/tests/job_runner/`, `ios/Services/JobRunner/`, `ios/Tests/JobRunner/`.

Drain eligible jobs when the native app has supported execution time; background execution is opportunistic, not promised.

Acceptance and verification:

- Launch/foreground/network-return resume work without duplicate application; suspend/termination cancels or checkpoints safely.
- An interpretation completed after its requested reminder time cannot silently backdate or substitute a new deadline; captured and scheduled status remain separate.
- Rust owns dispatch/retry orchestration against injected effects; Swift owns only platform lifecycle/execution hooks. Transcription is an injected capability registered by C05, not an assumed handler.

### E02

**Verify live interchangeable backends end to end**

Dependencies: V06, V07, I06, E01.

Owned paths: `docs/validation/live-providers.md`.

Run authorized synthetic captures through two genuinely different live backends selected by configuration, including adapter and apply behavior.

Acceptance and verification:

- Report provider/model/config versions, semantic outcomes, latency and cost; raw captures remain unchanged when switching.
- Exercise unavailable/invalid response and revoked-credential behavior. Absent API credentials leaves live proof blocked, never replaced by fixtures alone.
- For each claimed live backend require zero forbidden mutations over the named contrastive suite, with observed counts and model/prompt versions. A failed backend is not certified by another model agreeing with it.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### S02

**Implement the bounded optional daily prompt schedule**

Dependencies: N04, S01.

Owned paths: `core/src/suggestions/schedule/`, `core/tests/prompt_schedule/`.

Schedule one generic daily local prompt only after a time is chosen, with a short documented horizon and no endless backlog.

Acceptance and verification:

- Off by default, bounded horizon and capacity below explicit reminders; disabling prompting cancels only prompts.
- Refill the bounded daily prompt horizon only on app open; re-entry/time changes do not replay missed prompts, and default generic payloads contain no private content.

### L04

**Implement portable export with consistent source state**

Dependencies: D04, L02, V04.

Owned paths: `core/src/export/`, `ios/Services/Export/`.

Export a consistent versioned snapshot of source/correction/current state and minimal provenance through a native user-selected destination.

Acceptance and verification:

- Export contains no credentials, internal endpoint secrets, deleted content or private data outside the authenticated requested scope; optional audio inclusion is explicit.
- A test-only reader reconstructs equivalent item state and detects malformed/truncated export; native share/temp files are protected and cleaned. M1 does not claim full backup/restore sync.

### C05

**Integrate offline transcription into saved captures**

Dependencies: P04, C04, J01, J02.

Owned paths: `ios/Services/Transcription/`, `ios/Tests/Transcription/`.

Attach on-device transcript revisions and provenance to durable audio captures and queue interpretation when permitted.

Acceptance and verification:

- Available on-device model yields text offline; unsupported/missing-model/failed jobs retain source and remain intelligibly pending.
- Late transcripts cannot overwrite a user correction; save, transcript and interpretation completion are distinct and tested with interruption/retry.
- Register the real native transcription handler with the job runner through the reserved capability seam and verify a queued saved recording actually executes through that path.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### J03

**Expose bounded processing health and recoverable errors**

Dependencies: J02, D05, B01.

Owned paths: `core/src/jobs/health/`, `ios/Services/Health/`.

Provide content-free last-success/pending-age/error facts without making users operate a queue.

Acceptance and verification:

- A forced stall is detectable independently of model success; retry and unavailable-destination states are honest.
- Health reporting does not leak capture content or create a nagging notification stream; queue maintenance is never required to make a new capture.

### N05

**Connect interpretation and user edits to actual scheduling**

Dependencies: I06, J02, N04.

Owned paths: `core/src/reminders/coordinator/`, `core/tests/reminders_coordinator/`.

Complete the durable capture-to-installed-notification path and the cancellation/edit path.

Acceptance and verification:

- Supported offline commands schedule without a remote AI call; approved asynchronous results schedule only within a valid opportunity and captured time context.
- Acknowledge scheduled only after OS installation success; edit/completion retries and app restart preserve one desired reminder and truthful status.

### S03

**Connect prompt activation to current eligible content**

Dependencies: S02, N02.

Owned paths: `core/src/suggestions/coordinator/`, `core/tests/suggestions_coordinator/`.

On authenticated prompt activation, resolve current eligibility and offer one item with simple response actions.

Acceptance and verification:

- Deleted/completed/snoozed items preselected earlier are never shown as current; no eligible item produces a calm empty result.
- Not-now/stop-suggesting/done use existing revisioned operations; muting optional help leaves explicit reminders unchanged.

### L03

**Implement source-safe bounded audio retention**

Dependencies: C05, L02.

Owned paths: `core/src/lifecycle/retention/`, `ios/Services/AudioRetention/`.

Expire transcribed source audio under a documented bounded policy without automatically discarding the only untranscribed source.

Acceptance and verification:

- Clock-injected sweep tests cover success/pending/failure/user-corrected/deleted records and interrupted file removal.
- Default successful-transcription audio retention is seven days; sole-source audio is retained with visible storage/retention status until recoverable text exists or explicit user deletion. Document disk-pressure behavior without silent loss.
- Untranscribed source-only recordings have a visible unresolved state and explicit retry/delete resolution; the successful-transcription retention clock starts at transcript success, never at capture time.

### T02

**Test durable capture and processing fault recovery**

Dependencies: C05, J02, I05, L02.

Owned paths: `tests/resilience/capture-processing/`.

Build deterministic failure injection at write/import/job/apply/delete boundaries using production interfaces.

Acceptance and verification:

- Kill/reopen or equivalent process-level crash tests show no loss of acknowledged capture and no duplicate authoritative mutations.
- Provider timeout/invalid output/config changes/deletion races preserve source and permission boundaries; test real integration paths, not copies of implementation logic.

### N06

**Implement reminder history without inferring unseen events**

Dependencies: N05.

Owned paths: `core/src/reminders/history/`, `core/tests/reminder_history/`.

Expose requested time, installation/cancellation and observed OS/user events as inspectable history.

Acceptance and verification:

- Elapsed due time is reported as elapsed/unknown where the OS supplies no delivery evidence; no automatic completion or missed/seen fiction.
- User acknowledgment is explicit and repeat intent remains distinct; returning after a lapse does not replay old notification history as a digest.
- Use unacknowledged for elapsed reminders without observed delivery evidence; never infer fired, missed or noticed from the clock alone.

### B04

**Bound native work when capture backgrounds**

Dependencies: J02, C05, N04.

Owned paths: `ios/Services/BackgroundCompletion/`, `ios/Tests/BackgroundCompletion/`.

Request supported finite native background execution at capture end and handle expiry without losing acknowledged source.

Acceptance and verification:

- Verify current Apple-supported mechanism and record its constraints; expiration cancels/checkpoints work safely and never relies on perpetual processing.
- A pending transcript or interpretation remains saved-but-unprocessed, with any reminder explicitly not scheduled. Optional configured generic pending-reminder alerts are bounded, separately identified, and never represented as the requested reminder.
- Exercise lock immediately after recording and expire the execution window; no lost source, invented schedule success or endless retry alerts. Actual-device timing belongs in T05.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### C06

**Wire production system entry points and save acknowledgment**

Dependencies: C03, C05, N05, B04.

Owned paths: `ios/Capture/Entry/`, `ios/Capture/Acknowledgment/`, `ios/Tests/CaptureEntry/`.

Connect the validated system control/shortcut entry to production native voice or silent capture and return promptly after saving.

Acceptance and verification:

- Cold/warm/locked handoff honors the measured authentication limits and never waits for a webview to persist input.
- Acknowledgment distinguishes saved audio/text, pending transcription, not-scheduled and installed reminder with resolved time; no unsolicited suggestions appear during capture.
- Default save acknowledgment distinguishes saved-but-not-yet-processed and explicitly says no reminder has been confirmed while transcription/interpretation is pending. Background interruption must never leave a bare success signal implying a reminder exists.

### N07

**Handle notification actions and authenticated deep links**

Dependencies: N06, C01.

Owned paths: `ios/Services/NotificationActions/ActionHandler.swift`, `ios/Tests/NotificationActions/ActionHandlerTests.swift`.

Route notification taps and allowed actions into existing revisioned user events.

Acceptance and verification:

- Acknowledge and complete are distinct operations; stale identifiers, deleted items and duplicate callbacks never resurrect or double-mutate records.
- Private reads and mutations requiring authentication are deferred until unlock; action callback ingestion persists or reports failure and subsequent launch reconciles it.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### L05

**Implement delete-all reset orchestration**

Dependencies: L02, L03, L04.

Owned paths: `ios/Services/Reset/`, `ios/Tests/Reset/`.

Coordinate explicit delete-all across existing per-item cleanup and optional credential/profile reset.

Acceptance and verification:

- A durable reset generation fences in-flight processing and new effects; retries after crashes cannot restore deleted content or leave reminders scheduled.
- Test populated mixed-route data, indexes, audio, export/share temporaries, pending jobs and optional credential removal. New captures after completed reset belong to a fresh generation.

### T01

**Add content-free local latency and reliability metrics**

Dependencies: C06, J03.

Owned paths: `core/src/metrics/`, `ios/Services/Metrics/`.

Record local counts and timing needed for the M1 trial without collecting personal source text, prompts, secrets or addresses.

Acceptance and verification:

- Measure trigger-to-ready separately from end-of-input-to-durable-save, and record processing/scheduling failures without claiming human attention.
- Metrics are bounded, can be disabled/exported through an explicit authenticated action, and have no automatic remote telemetry destination.

### B02

**Assemble the production service composition root**

Dependencies: B01, C06, S03, L03, L04, J03.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Tests/Services/CompositionRootTests.swift`.

Connect the existing core, providers, authorized native transport, jobs, credentials and reminder services through one injectable composition root.

Acceptance and verification:

- Production construction selects real adapters from versioned configuration; fake dependencies require an explicit test build path.
- A composition integration test saves, dispatches a fake response, reads the resulting item, and observes a scheduler request through actual service interfaces.
- Provide typed extension seams for optional capabilities so later integration can add owned modules without concurrent edits to this composition root.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### B03

**Reconcile services on launch and foreground activation**

Dependencies: B02.

Owned paths: `ios/Services/Assembly/LifecycleCoordinator.swift`, `ios/Tests/Services/LifecycleCoordinatorTests.swift`.

Order recovery and foreground work without making capture wait for network or cleanup.

Acceptance and verification:

- Reconcile ingress, expired job leases, deletion, reminder desired state, retention, prompt eligibility and health with bounded idempotent work; serialize overlapping launches.
- Test process interruption at each recovery boundary and repeated activation. Provide an injectable trigger for separately owned system clock/zone-change handling.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### T11

**Integrate optional provider and shadow capabilities**

Dependencies: B02, V09, E03.

Owned paths: `ios/Services/OptionalCapabilities/`, `ios/Tests/OptionalCapabilities/`.

Register verified self-hosted and opt-in shadow capabilities through the production composition extension seam.

Acceptance and verification:

- The base app constructs and captures when neither optional feature is configured. Feature registration uses the same routing/credential rules and adds no hidden default endpoint.
- Synthetic end-to-end checks exercise both registrations; real self-hosted claims link V08/V09 evidence. No edits to concurrent shell or root files are needed.
- Own routing/credential-isolation canaries for both optional integrations, including denied routes, revoked profiles and shadow verdicts unable to mutate authoritative state; absent or unverified capability stays disabled.

### U01

**Assemble the selected production management shell**

Dependencies: P10, B03, N07.

Owned paths: `app/`, `ios/AppAssembly/`.

Connect the selected SwiftUI or Tauri management shell to the already assembled production services, authenticated navigation and stable view models. Do not reimplement the composition root or lifecycle scheduler.

Acceptance and verification:

- App launches to capture; retrieval/settings are secondary and no backlog count or catch-up wizard is introduced.
- Dependency injection resolves real services rather than probe stubs, including lifecycle job runner and reminder/suggestion/deletion reconciliation; remove or isolate probes from the release target.

### B05

**Reconcile wall-clock and timezone changes**

Dependencies: B03, N06, S03.

Owned paths: `ios/Services/TimeChangeCoordinator/`, `ios/Tests/TimeChangeCoordinator/`.

Handle system time/zone changes through existing reminder and prompt reconciliation.

Acceptance and verification:

- Preserve explicitly resolved absolute reminder instants; implement documented local-time daily prompt semantics through DST gaps/folds and zone travel.
- Test repeated callbacks, clock jumps forward/backward and overdue pending work without duplicate delivery requests or burst catch-up.
- For the native effect boundary, run at least one simulator integration check against the real Rust core on the exact submitted revision; an isolated Swift mock alone is insufficient.

### U02

**Build original-text search and source detail views**

Dependencies: U01, R03, C01.

Owned paths: `app/retrieval/`, `app/source-detail/`.

Expose local original-text retrieval, supported conversational filters and source provenance in the selected UI.

Acceptance and verification:

- Search returns original/corrected text with date/type/source and honest no-result state; private reads require authentication.
- Literal fallback and private topics since a date work offline; no generated answer is shown as quotation.
- Offer playback only for retained source audio with the same private-read authentication; expired/deleted audio is honestly unavailable and cannot leave a broken playback control.
- Expose session-topic filtering and since-date retrieval with original source text in the private authenticated view.

### U03

**Add item type correction and lifecycle controls**

Dependencies: U02, D04.

Owned paths: `app/item-controls/`.

Wire type correction, done/cancel and suggestion controls to revisioned domain operations.

Acceptance and verification:

- Each action updates actual stored state and survives relaunch/reprocessing; stale revision conflicts are visible and do not silently overwrite.
- Not-now and stop-suggesting behavior are explained; actions do not turn an idea into an obligation without explicit user intent.
- Allow source/transcript correction as an explicit authoritative revision while preserving original provenance; later transcription/model output cannot overwrite the correction.
- Provide an explicit session-topic facet toggle independently of note/action/idea and privacy scope; correction survives later reprocessing.

### U04

**Add reminder editing and inspectable history**

Dependencies: U03, N06.

Owned paths: `app/reminders/`.

Connect reminder edit/cancel and history UI to real scheduling and status facts.

Acceptance and verification:

- Resolved time/timezone, unsupported repeats, capacity/permission errors and passed scheduling opportunities are understandable.
- Edits cancel stale requests, generic notification links reopen the correct authenticated record, and history does not invent delivery/attention.

### U05

**Add provider profile and credential setup UI**

Dependencies: U04, V06, V07, V04.

Owned paths: `app/provider-settings/`.

Wire add/edit/switch/remove provider configuration and secure key entry to the actual adapters.

Acceptance and verification:

- Profile capability/endpoint/model validation and redacted connection test work; secrets go directly to native secure storage and are not re-rendered.
- Changing default provider preserves captures and follows queued-job config pinning; show local-only and unavailable private-server modes honestly.

### U06

**Add processing-policy onboarding and privacy-route controls**

Dependencies: U05, V02, V03.

Owned paths: `app/privacy-settings/`.

Configure destinations before first upload without imposing a privacy decision on every capture.

Acceptance and verification:

- Default local-only capture works before setup; separate private/general route preferences are clear and sticky, never inferred as permission by a classifier.
- Profile destination/capability changes require deliberate authorization and reflect queued-work behavior; revocation and no-approved-provider cases remain usable.

### U07

**Add retention, export and deletion controls**

Dependencies: U06, L03, L04, L05.

Owned paths: `app/data-settings/`.

Expose the actual audio-retention/export/item-delete/delete-all operations through authenticated native services.

Acceptance and verification:

- Actions report real progress/error and sole-source retention exceptions; no completed status while required cleanup remains pending.
- Explain external export/OS backup limits, clean temporary artifacts and keep new capture usable during non-destructive maintenance.

### U11

**Implement first-run permissions without blocking capture**

Dependencies: U06, C06.

Owned paths: `app/settings/permissions/`, `ios/Services/Permissions/`.

Request microphone, speech and notification access only at the corresponding feature boundary and explain denial recovery.

Acceptance and verification:

- Fresh install with no provider and denied notifications supports local silent capture and retrieval.
- Each denial, restricted status, later revocation and Settings return is tested; unavailable speech preserves audio and does not trigger remote transcription.

### U08

**Add optional prompt settings and response UI**

Dependencies: U07, S03.

Owned paths: `app/prompt-settings/`, `app/suggestions/`.

Expose one optional daily prompt, time selection, off switch and current-item responses.

Acceptance and verification:

- Opt-in is required, prompt horizon/cooldown are understandable, and no private item is revealed before authenticated open.
- Off/not-now/stop/done affect correct stored policy without disabling explicit reminders; empty eligible set is calm and does not solicit inbox cleanup.

### T04

**Test processing-boundary and private-read isolation**

Dependencies: U07.

Owned paths: `tests/privacy/`.

Verify the complete UI-to-routing/transport/export/search/notification boundary with synthetic canary content.

Acceptance and verification:

- Unapproved routes, cross-origin redirects, stale jobs and reviewer profiles cannot disclose canaries; private-source queries/snippets and lock-screen payloads respect read scope.
- Assert no raw credentials/content in logs or exports and verify revocation while a job is in flight; record authorized remote processing accurately.
- This base privacy suite does not depend on optional Spark, shadow or preview features. T11/S04 own their capability canaries, which must pass before those capabilities are enabled or claimed in a candidate.

### U09

**Add honest status and minimal recovery views**

Dependencies: U08, J03, D05, N04, U11.

Owned paths: `app/status/`.

Expose saved/pending/unavailable/unscheduled facts without a monitoring dashboard becoming required.

Acceptance and verification:

- Forced worker/provider/notification faults are visible using source-preserving retry where appropriate; no routine manual queue triage is needed.
- Fresh capture remains immediate after nonuse and error states; M1 sync is described as not configured and no on-device-only promise covers remote inference.

### S04

**Add opt-in previews of eligible non-private suggestions**

Dependencies: S03, U08.

Owned paths: `core/src/suggestions/preview.rs`, `app/settings/prompt-preview/`, `ios/Services/NotificationActions/PreviewPolicy.swift`.

Offer a useful item-specific prompt as an explicit alternative to the generic default, without disclosing private routes.

Acceptance and verification:

- Default remains generic. Preview opt-in states lock-screen exposure clearly and applies only to explicitly non-private, suggestion-eligible items.
- Reclassify/delete/stop-suggesting cancels and reconciles pending previews; never promise recall of already delivered OS banners. Private content and identifiers conveying meaning never enter payloads.
- Preview content requires an explicitly preview-safe route configured by the user. The universal/default route is not preview-safe; classification can only remove eligibility, never grant preview permission. End-to-end tests inspect every scheduled payload.

### U10

**Complete native accessibility and interaction validation**

Dependencies: U09, C06.

Owned paths: `app/accessibility/`, `ios/Tests/Accessibility/`, `docs/validation/accessibility.md`.

Validate voice/silent capture, retrieval, item actions and settings with VoiceOver, Dynamic Type and keyboard/accessibility navigation.

Acceptance and verification:

- Automated audits plus a documented actual-device pass cover focus, labels, error announcements, large text and no inaccessible authentication dead ends.
- Record measured issues and fixes; simulator-only results cannot certify physical assistive interaction. Keep fixes limited to named UI accessibility behavior.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### T03

**Test reminder and suggestion resilience after a lapse**

Dependencies: N06, S03, U09.

Owned paths: `tests/resilience/notifications/`.

Exercise restart, permission/capacity changes, timezones and multi-day nonuse through the actual reminder/prompt services.

Acceptance and verification:

- No duplicate or fabricated reminder, no stale deleted-item prompt, no backlog burst or automatic completion after silence.
- Already-installed explicit reminders are independent of AI/Mac/network availability; separate simulator/mock evidence from physical delivery proof.

### T09

**Exercise the full simulator loop with fake providers**

Dependencies: U11, U09, B03, N07.

Owned paths: `ios/Tests/EndToEnd/`.

Add one maintained simulator test suite across actual production wiring, rather than isolated module fakes.

Acceptance and verification:

- Exercise silent capture, saved-source retrieval, classification, correction, completion, explicit reminder scheduling and notification action with deterministic provider/clock/OS boundaries.
- Run the happy-path suite in macOS CI on the submitted revision; provider and OS boundaries are deterministic while app/service wiring is real. Failure journeys belong to T12.
- With no provider configured, capture an explicit session topic, correct its facet, and retrieve original words using session-topic plus since-date filters without any network request.

### T10

**Version trial builds and verify upgrade persistence**

Dependencies: T09, P08.

Owned paths: `tools/release/`, `docs/testing/m1-upgrade-evidence.md`.

Produce reproducible trial version/build identifiers and an upgrade procedure before the physical exit matrix.

Acceptance and verification:

- Upgrade a prior seeded install without uninstalling; sources, corrections, tombstones, profile references and scheduled reminder identity survive. Verify simulator automatically and collect a real-device upgrade result.
- Build metadata and exported content-free metrics identify the tested revision; signing material and personal records never enter artifacts or repository.
- Produce the actual signed candidate artifact for T05, recording revision, version/build, checksum and signing route/validity metadata. T06 distributes this same tested candidate; any rebuilt binary requires repeated affected device checks.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### T12

**Exercise simulator failure journeys through the app**

Dependencies: T09, L05, B04, B05.

Owned paths: `ios/Tests/FailureJourneys/`.

Verify failures through production UI and service wiring, complementing the happy-path simulator suite.

Acceptance and verification:

- Exercise provider outage, denied/revoked permissions, interrupted transcription, expired background execution, restart and deletion during processing.
- Assert source durability, recovery without mandatory triage, no stale scheduled effects, and independently honest save/processing/scheduling state.

### T05

**Run the actual-device M1 functional exit matrix**

Dependencies: U10, T01, T02, T03, T04, E02, P08, T09, T10, T12, B05.

Owned paths: `docs/validation/m1-device-results.md`.

Install the production M1 build and execute the full device matrix with synthetic/authorized local data.

Acceptance and verification:

- Verify cold/locked/permission paths, offline recording/text, process interruption, local reminders UI closed, pipeline faults and Mac asleep with actual build/device identifiers.
- Attach sanitized results and exact reproduction of failures; every functional criterion must pass or the task remains blocked for remediation. Do not certify a probe build as the production app.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.
- Test every capability claimed by the exact candidate. Spark, shadow and preview capabilities may be absent/disabled for the base trial; enabling one requires its owning task canaries and affected device checks first.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### T06

**Produce the signed M1 trial build and installation guide**

Dependencies: T05.

Owned paths: `docs/releases/m1-trial.md`, `tools/apple-build/trial/`.

Package the verified build for the selected supported Apple distribution route and document a reproducible installation/update path.

Acceptance and verification:

- Artifact corresponds to reviewed source and passed checks, with secrets injected securely and no private data bundled.
- Verify installation and launch on the target phone; public README makes no broader distribution claim than the actual signed route. Account-holder restrictions/inputs are explicit.
- Use the exact candidate identifier/checksum certified by T10 and T05; do not rebuild and claim the new binary has inherited device validation.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

### T07

**Write M1 setup and operational recovery documentation**

Dependencies: T06.

Owned paths: `README.md`, `docs/setup.md`, `docs/recovery.md`, `AGENTS.md`.

Document the implemented app, actual supported models/devices/capture controls, provider setup and honest limitations.

Acceptance and verification:

- A clean-checkout build and synthetic first capture/search/reminder procedure is reproducible; do not advertise unverified Spark, offline ASR or lock-screen behavior.
- Explain local-only/no-sync M1, export/deletion/backup boundaries, expired signing and failure ownership without requiring routine service maintenance.

### T08

**Evaluate the two-week M1 trial and record exit evidence**

Dependencies: T06, T07, F07.

Owned paths: `docs/validation/m1-trial-results.md`.

Assess at least two elapsed weeks of actual use/authorized observations, including retrieval/resurfacing and a multi-day gap, against the preregistered protocol.

Acceptance and verification:

- Evidence is sufficient to judge useful return and burden, not merely launch counts or a streak. Include separate latency distributions, errors and no-cleanup lapse recovery.
- No source/private session contents are committed. If capture is avoided or the only prompt is muted, define a concrete loop revision; do not declare M1 complete based on code alone.
- Missing time/device observations or failed exit criteria leave this task blocked with precise evidence needs; continue independent work and create bounded corrective follow-ups through the coordinator.
- Record whether generic versus explicitly enabled non-private previews actually help retrieval and whether prompts are muted or ignored; usefulness and capture burden outweigh notification open rates. If the loop fails, record M1 as unmet and propose focused repair tasks.
- Every observed result cites a named sanitized evidence artifact, build/revision and collection time; preserve raw private evidence locally with an auditable reference. Missing evidence blocks the check; prose assertions alone cannot pass review.

External evidence/input is required. Missing access is a concrete execution block, not a reason to fabricate completion.

