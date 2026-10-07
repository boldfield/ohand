# M1 Architecture Contracts and Ownership Map

Status: executable specification, 2026-10-07. This document establishes the module boundaries, versioned interfaces, and ownership model for Milestone M1. The implementation details are filled by downstream tasks; this document defines the contract any valid implementation must satisfy.

## Overview

Oh And M1 consists of three primary layers with clear boundaries and ownership:

- **Rust Core + SQLite** (`core/`): domain state, interpretation coordination, provider adapters, reminder scheduling, data validation and policy enforcement.
- **Native Swift** (`ios/`): protected credentials and secrets management, native audio capture, system integration (notifications, controls, permissions), foreground job execution lifecycle.
- **Management UI** (`app/`, selected via P10): capture coordination, retrieval, settings and user interactions.

No shared state exists between layers. Rust owns authoritative domain state; Swift owns protected platform services and local effects only. Webview (if selected) never holds credentials, provider keys, or private note content.

## Stable Identifiers and Versioning

### Capture ID

- Generated once by the native capture entry point (cold/warm launch, control handoff, shortcut).
- **Idempotency key**: a random UUID4, persisted immediately before acknowledgment to the user. The same capture ID is reused on retry.
- One-to-one mapping: any retry with the same capture ID produces the same item ID and storage record.
- Unique per capture intention. A random UUID is NOT derived from content; it is generated once and persisted, ensuring no accidental deduplication of two identical captures from different moments.
- Passed to ingress, persisted in storage, returned in completion acknowledgment.

**Idempotency contract:** If ingress is called twice with the same capture ID, the second call returns the existing item without duplication.

### Item ID

- Derived from capture ID during ingestion; one-to-one mapping.
- Unique per stored intention (note, action, idea).
- Immutable; used in all downstream state, reminders, suggestions, and exports.

### Revision Semantics

- Each item maintains a monotonically increasing revision number (0-based).
- Source and explicit user corrections increment revision.
- Derived state (interpretations, reminders) DO NOT increment item revision; they reference the source revision they were computed from.
- Stale updates with outdated revision are rejected (compare-and-set).
- Reprocessing with an older source revision cannot overwrite corrections or user-applied lifecycle events.
- Applying a proposal validates source revision; proposals with source revision older than the current item revision are rejected.

### Profile and Configuration Versions

- Provider profiles have immutable version IDs (UUID).
- Queued jobs pin their profile version at dispatch time.
- Profile changes never silently reroute old queued work.
- Configuration-version mismatches during retry are explicit failures, not silent fallback.

### Request, Proposal, and Job Versions

- Interpretation requests include immutable request-version ID.
- Provider responses reference request-version for audit trail.
- Proposals (action/reminder/note suggestions) reference their source capture and request-version.
- Applying a proposal validates source revision; stale sources are rejected.
- Job version: each queued interpretation/transcription/processing task is tied to a specific profile version and request context version. A job created under profile v1 retains that version even if the user changes the default profile; job creation time is immutable.

## State Model: Source, Corrections, and Derived State

### Authoritative Source

- Original capture text or audio reference, immutable once ingested.
- Timezone, locale, capture timestamp, privacy route, and explicit capture metadata.
- Corrections never replace source; they coexist.
- Source is always searchable and always recoverable.

### Explicit User Corrections

- Separate revisioned records: text corrections, type changes, session-topic assignments.
- Corrections increment item revision and are timestamped.
- Later reprocessing cannot overwrite corrections.
- Correction history is inspectable; source and latest correction are both available to UI.

### Derived State (Non-Authoritative)

- Interpretation results, inferred notes/actions/ideas, reminder time proposals, embeddings, summaries.
- May be re-computed on demand; invalid/stale derived output reverts to prior state rather than demoting item.
- Proposal schema validation alone is never sufficient; semantic validation is required before application.
- Invalid interpretations (malformed JSON, missing required fields, stale source revisions) are rejected without mutation.

### Distinct Lifecycle States

**Save State:**
- `saved_local`: durable write to device SQLite, acknowledged to user.
- `synced` (M2+, default `not_configured` in M1): synchronized to configured remote (if any).

These are orthogonal to processing and reminder states. A save does not imply processing has started. A save does not imply a reminder has been scheduled.

**Processing State:**
- `unprocessed`: awaiting interpretation.
- `processing`: job in queue or in flight.
- `processed`: interpretation complete (success).
- `uninterpreted`: processing failed; source retained as searchable.

**Audio Transcription State** (for voice captures):
- `audio_pending`: raw audio recorded, awaiting on-device transcription.
- `transcribing`: on-device transcription in progress.
- `transcribed`: on-device transcription succeeded; text available.
- `transcription_unsupported`: requested language or model not available on device; audio retained with visible retry/delete options.
- `transcription_failed`: on-device transcription permanently failed (e.g., corrupted audio); audio retained with visible status.

**Reminder Requested/Desired State:**
- `not_requested`: no reminder asked.
- `desired`: reminder requested, time resolved, waiting for OS scheduling.
- `unsupported`: requested recurrence, ambiguous time, or time resolution failed; original phrase preserved with optional correction path.

**Reminder Scheduled State:**
- `scheduled`: OS native notification installed with identifier.
- `pending_schedule`: time resolved but not yet installed (transient state during boot or permission change).

**Reminder Delivery and Acknowledgment State (separate from Scheduled):**
- `unknown`: OS has no delivery evidence (app was closed, notification was not explicitly acknowledged by user).
- `delivered`: OS reports notification was delivered to user.
- `user_acted`: user tapped notification and took action (viewed, opened app).
- `acknowledged`: user explicitly acknowledged via UI (separate from delivery; marks acknowledgment if applicable).
- `cancelled`: user or system cancelled before delivery.

Note: save, processing state, schedule state, and delivery state are separate and independent facts. A reminder may be saved but not scheduled, scheduled but not delivered, and delivered but not acknowledged. Each state is tracked independently. Clock passage (elapsed time) is NOT delivery evidence.

**Item Lifecycle State:**
- `active`: normal state, available in searches and suggestions.
- `completed`: user marked done; removed from suggestions, remains searchable.
- `cancelled`: user explicitly cancelled; removed from suggestions, remains searchable.
- `deleted`: user deleted; marked as tombstone, removed from all results, cleanup in progress.

**Suggestion State:**
- `not_eligible`: excluded by policy, completion, deletion, snoozed, privacy scope, or speculative intent.
- `eligible`: candidate for rotation in optional prompts.
- `snoozed`: user deferred; cooldown-gated re-eligibility.
- `pull_only`: user requested stop-suggesting; remains searchable.

## Privacy Model: Routes, Scopes, and Policy Enforcement

### Privacy Routes and Scopes

Privacy is governed by two independent dimensions:

**Privacy Scopes** (data classification):
- **Personal**: default route; private thoughts, personal-context captures, therapy notes, exploratory ideas.
- **Work**: work-context captures and derivations. Explicitly opted-in by user.
- **Session**: a source-linked facet independent of personal/work scopes and processing permissions. Identifies a user-correctable session or event context (e.g., "therapy session", "project review") without affecting privacy classification or upload permission.

Route assignment is stored with capture metadata at ingestion time. No classifier creates or changes routes; only explicit user configuration or API authorization (at setup time) does. A single capture is assigned to exactly one personal/work scope. Session facet is independent and may be assigned to any personal/work item.

**Processing Permissions** (where data may be processed):
- **Local-Only Processing**: transcription (on-device only), fast-path reminders, retrieval, FTS indexing. No outbound requests; deterministic validation only.
- **Configured Cloud Provider**: authorized destinations for interpretation/embedding/summarization (if enabled for that route).
- **Private Server Endpoint**: requires explicit setup and TLS validation before dispatch. One endpoint per profile; no automatic fallback.
- **Shadow Review Endpoint**: optional, sampled, diagnostic-only. Cannot mutate authoritative state or redirect captured content.

A single capture routed personal uses the personal-route provider destination. A capture routed work uses the work-route destination. Changing a provider profile affects all future queued work; existing queued jobs retain their profile version.

### Session-Topic Facet

The session-topic is a user-correctable facet **independent of privacy scope and processing permission**:
- Derived fact independent of item type (note/action/idea).
- User-correctable via explicit UI or edit operations.
- Supports filtering in retrieval (e.g., "session notes since Thursday").
- Source-linked: correction evidence is captured and revision-tracked.
- Does **not** automatically affect privacy scope, processing permissions, or destination routing.
- User may explicitly opt into disclosure of session-topic items if desired, but session-topic assignment alone does not grant preview eligibility or change privacy route.

### Privacy Scope Enforcement

**Retrieval filtering:**
- Personal-route items are excluded from work-scoped queries.
- Private reads require authentication (device unlock or explicit local auth boundary).
- Retrieval filters enforce scope before search/ranking; no private text appears in work-scoped results via counts or error messages.
- Query text never uploaded for literal searches; local FTS only.

**Notification and preview safety:**
- Notification payloads contain only generic wording and opaque identifiers; no private text ever in OS notifications.
- Preview eligibility (for optional daily prompts) is determined by route and user configuration:
  - Default: all prompts show generic text only (no preview).
  - If user enables preview mode: only items designated as preview-safe by the user may be previewed; personal-route items are never previewed, regardless of configuration.
  - Session-topic assignment does not grant preview eligibility; user must explicitly enable preview mode, and personal route still excludes all items.
  - Classification can only remove preview eligibility, never grant it.

**Export and backup:**
- Exports include all routes; user selects whether to include work or personal or both.
- Backup exclusions and retention policy are per-storage class, not per-route (see Data Lifecycle).

### Privacy Scope Defaults

- Fresh install: all captures default to personal route unless user reconfigures.
- No universal default routes that auto-classify by content.
- Preview mode is off by default; user must explicitly enable.
- Session-topic correction is always user-controlled; no classifier infers it.

## Core Domain Contracts

### Capture Ingestion Contract

**Input:**
- Capture ID (UUID4, provided by native entry).
- Text or audio reference (file path or inline for small text).
- Privacy scope assignment (personal/work, not session).
- Session-topic facet assignment (optional, user-correctable).
- Capture timestamp and timezone.
- Optional metadata (microphone state, permissions, signal quality).

**Output:**
- Durable acknowledgment to native layer (capture ID, item ID, save timestamp).
- Transactional write: item record created, indexed, and queryable.
- No remote requests.

**Error Contract:**
- Duplicate capture ID: idempotent return of existing item.
- Invalid input (missing route, null text): rejection with specific error.
- Storage failure: error with persistent state, no partial writes.

### Interpretation Request Contract

**Input:**
- Capture ID and revision.
- Authoritative source (text or transcript).
- Request-version ID.
- Profile ID and version.
- Explicit privacy scope (internal routing, not disclosed to interpreter).
- Time context (capture timestamp, timezone, user's current time).

**Output (Proposal):**
- Note, action, or idea annotation (zero or one of each).
- Extracted reminder time (if present) with resolution state (explicit/inferred/ambiguous/unsupported).
- Session-topic proposal (if supported).
- Source span references (byte offsets in original text).
- Explicit abstention if interpretation is uncertain or unsupported.

**Error Contract:**
- Timeout: no mutation, job marked pending.
- Invalid JSON: rejected, source preserved as uninterpreted.
- Refusal/capability-mismatch: explicit abstention, source searchable.
- Configuration/profile mismatch: rejected, queued for retry with clarification.

### Reminder Scheduling Contract

**Intent:**
- User requests reminder: explicit phrase + time resolution.
- Fast-path offline handler: recognize bounded set of explicit commands.
- Asynchronous handler: interpretation result + validated time.

**Output:**
- Resolved reminder intent (absolute instant in UTC, timezone stored for display).
- Native notification ID (opaque identifier returned by OS scheduler; core-derived stable identifier for reconciliation).
- Durable desired state record (item ID + reminder ID + resolved time).

**Notification Identifiers:**
- Core-derived deterministic identifier (e.g., reminder ID + schedule generation number) allows reconciliation and prevents duplicate delivery on retry.
- Native ID is obtained from OS and stored separately.
- Reconciliation: if core restarts and does not find a native notification matching the core identifier, it re-schedules.

**Unsupported Repeats:**
- Explicitly marked as unsupported (not silently reduced to one-shot).
- Original phrase preserved.
- Stored as not-scheduled with recovery path (manual one-shot creation, correction opportunity).

**Error Contract:**
- Ambiguous time (e.g., "next week"): marked unsupported, UI offers clarification path.
- Expired opportunity (requested time in past): marked unschedulable, no automatic time adjustment.
- Permission denied: marked unschedulable, permission recovery path available.
- Capacity exceeded: explicit unschedulable state, documented horizon and refill strategy.
- Schedule failure: error is surfaced to UI; state remains desired pending retry.

### Suggestion Rotation Contract

**Eligibility:**
- Exclude completed, cancelled, deleted, snoozed, pull-only items.
- Exclude uninterpreted items and items with only speculative intent.
- Exclude personal-route items unless explicitly designated preview-safe and user enables preview.
- Exclude items without clear actionable form.

**Selection:**
- Deterministic clock-controlled rotation.
- Reason recorded for each selection (rotate count, age, other policy).
- No fabricated urgency or deadline.

**User Response:**
- Not-now: cooldown-gated re-eligibility.
- Stop-suggesting: pull-only flag, remains searchable.
- Done: completion event, removes eligibility.
- No response: nonresponse is not completion or importance signal.

### Deletion Contract

**Input:**
- Item ID and current revision.

**Atomic Effects:**
- Mark item deleted.
- Remove from all indexes and search results.
- Enqueue cleanup for audio files, ingress records, native notifications.
- Stop future processing (cancel in-flight jobs).

**Durability:**
- Deletion marker is durable (surviving relaunch and restore from backup predating deletion).
- Cleanup is best-effort; interruption must not restore readable text.
- Racing job result or correction during deletion is rejected.
- Stale source replay after deletion is ignored.

**Error Contract:**
- Stale revision: rejection with current state.
- Already deleted: idempotent, no error.
- Active dependent reminders: marked as cancelled.

## Native-to-Core Ownership and Effect Interfaces

### Effect Categories Owned by Swift

1. **Protected Storage**: file protection, keychain credential access, device lock integration.
2. **Audio Capture**: microphone recording, permission handling, hardware interruption recovery.
3. **Native Notifications**: OS scheduling/delivery/list/cancel primitives.
4. **System Integration**: permissions request/monitoring, control handoff (Action button, Siri Shortcuts).
5. **App Lifecycle**: foreground/background/suspend events, interrupted task checkpointing.
6. **Secure Credential Storage**: native keychain storage, credential add/update/delete, immutable reference keys.

### Effect Categories Owned by Rust

1. **Interpretation Dispatch**: routing, profile selection, provider adapter calls.
2. **Job Orchestration**: queue management, retry logic, lease management.
3. **Reminder Orchestration**: time resolution, state synchronization with OS.
4. **Cleanup Coordination**: ordering of deletion effects, fence generation.
5. **Policy Enforcement**: privacy scope validation, unauthorized route rejection.

### Injected Effect Interfaces (Rust ↔ Swift)

All native effects are trait-injected. Rust owns the pure orchestration logic; Swift owns the platform implementation. This preserves core independence and enables deterministic testing.

**Notification Effect Interface:**
- Schedule: input (core-derived identifier, due time as instant, generic text only); output (native identifier or error).
- Cancel: input (native identifier); output (success or error).
- List pending: output (all pending notifications with core and native identifiers).
- Fields: core identifier, native identifier, due time, text, delivery status.
- Invariants: generic text only (no private content). Stable identifiers prevent duplicate delivery on retry. Cancellation is idempotent.

**Credential Storage Interface:**
- Get: input (opaque key); output (raw secret bytes or error).
- Store: input (opaque key, secret bytes); output (success or error). M1 may use this for testing; unused in production M1 base.
- Delete: input (opaque key); output (success or error). M1 may use this for testing; unused in production M1 base.
- Fields: key (opaque reference), secret (raw bytes).
- Invariants: secrets never persisted into job state. Retrieval errors are explicit; no fallback. Keys are opaque to core; Swift interprets them as Keychain references. Writes happen only at credential setup time or testing; no secret is persisted into queued jobs.

**Audio Capture Effect Interface:**
- Start recording: input (capture ID); output (recording handle).
- Stop: input (handle); output (audio reference or error).
- Cancel: input (handle); output (success or error).
- Fields: capture ID, recording handle, audio file path, duration, error state.
- Invariants: start/stop are paired. Cancellation is idempotent. Partial audio from interruption is preserved with visible status.

**HTTP Transport Interface:**
- Request: input (URL, method, headers, optional body, timeout duration); output (response or error).
- Fields: URL, method, headers, body, timeout, response status, response body, response headers.
- Invariants: TLS certification required. Timeouts respected. Response size bounded. Errors logged with secrets redacted. No automatic retries (retry policy is Rust-owned).

**Transcription Interface:**
- Transcribe: input (audio file path, language code); output (transcript text, confidence, language detected, or error).
- Fields: audio path, language code, result text, confidence, detected language, error (not found, unsupported language, unsupported model).
- Invariants: on-device only; no cloud fallback. Unsupported language/model returns explicit error, not degradation. Audio remains pending; user has visible retry/delete options.

**Lifecycle Coordination Interface:**
- On foreground: signal that app entered foreground.
- On background: signal that app entered background.
- Request background execution: input (requested duration); output (execution lease or error).
- Fields: execution lease with expiry time.
- Invariants: background execution is best-effort and bounded. Lease expiry does not lose acknowledged captures. No background wake for processing (processing executes only on foreground entry or explicit scheduling).

### Ingress Handoff Contract

Native entry point (cold/warm launch, control activation) produces durable ingress record:

- Capture ID (a random UUID, persisted immediately before user acknowledgment).
- Raw input (text or audio reference).
- Entry context (timestamp, permissions state, locked/unlocked).
- Route assignment (personal/work, not session).
- Optional session facet assignment.

Core ingestion transactionally imports:
- Create item, assign item ID.
- Index source text.
- Acknowledge to native layer.

Native layer returns deterministic save acknowledgment (no remote calls required).

### Error and Lifetime Contracts

**Lifetime:**
- Rust orchestrates jobs and scheduling; Swift executes effects.
- Swift effects are best-effort for background work (bounded time).
- Timeout/cancellation in Swift is graceful; state remains honest.
- Relaunch reconciles incomplete work.

**Error Handling:**
- All native effect errors are normalized to Result<T, Error>.
- Invalid/permanent errors (bad credential, unsupported device) are propagated and surfaced.
- Transient errors (network, timeout) are retried with backoff.
- Authorization failures are explicit; silent fallback is forbidden.

**Cancellation:**
- Jobs track cancellation tokens.
- In-flight Swift effects are cancellable.
- Cancelled work produces no mutations; state reconciles on relaunch.

## Module Ownership Map

The following map lists every implementation area and its owning task. All file paths are owned exclusively by one task; shared manifests (Cargo.toml, Makefile, ios/project.yml, AGENTS.md) must have ordering dependencies documented, or rely on an existing edge-based serialization.

### Build, Workspace, and CI Modules

| Module | Responsibility | Task(s) |
| --- | --- | --- |
| `core/` | Rust workspace root | F02 |
| `Cargo.toml` | Workspace manifest and dependency declarations | F02 |
| `Cargo.lock` | Locked dependencies | F02 |
| `Makefile` | Build, lint, test commands (shared: F02 owns initial, F05 extends) | F02, F05 |
| `ios/` | Native iOS workspace root | F03 |
| `ios/project.yml` | Native Xcode project generation (shared: F03 owns initial, F05 reads) | F03, F05 |
| `.github/workflows/core.yml` | Linux CI for Rust checks | F04 |
| `.github/workflows/ios.yml` | macOS CI for native simulator | F05 |
| `.github/workflows/hygiene.yml` | Secret scanner and fixture policy | F06 |
| `AGENTS.md` | Worker capability documentation (shared: F04 owns initial, F05 extends) | F04, F05 |
| `tools/hygiene/` | Hygiene check implementation | F06 |
| `docs/contributing.md` | Contribution guidelines | F06 |
| `tools/apple-build/` | Reproducible signing and device build | P08 |
| `tools/bindings/` | Binding generation (reproducible output) | B01 |
| `tools/evaluation/` | Semantic evaluation reporting | E01 |
| `tools/provider-probe/` | Self-hosted endpoint validation | V08 |
| `tools/release/` | Trial version/build identifiers | T10 |

### Core Modules (Rust)

| Module | Responsibility | Task |
| --- | --- | --- |
| `core/src/store/schema/` | SQLite versioned schema, migrations | D01 |
| `core/tests/migrations/` | Schema migration tests | D01 |
| `core/src/store/captures/` | Durable raw capture storage (idempotent) | D02 |
| `core/tests/capture_storage/` | Capture storage tests | D02 |
| `core/src/store/events/` | User corrections, lifecycle events (revisions) | D03 |
| `core/tests/events/` | Event storage tests | D03 |
| `core/src/domain/items/` | Authoritative item state projection, write guards | D04 |
| `core/tests/item_state/` | Item state tests | D04 |
| `core/src/domain/status/` | Save/sync/processing/reminder/delivery states | D05 |
| `core/tests/status/` | Status model tests | D05 |
| `core/src/ingress/` | Native capture handoff, durable import contract | C02 |
| `core/src/time/` | Date/time resolution, timezone/locale handling | I02 |
| `core/tests/time/` | Time resolution tests | I02 |
| `core/src/interpretation/contracts/` | Proposal schemas, provenance validation | I01 |
| `core/tests/proposals/` | Proposal validation tests | I01 |
| `core/src/interpretation/fast_path/` | Offline explicit reminder/session-topic recognition | I03 |
| `core/tests/fast_path/` | Fast-path tests | I03 |
| `core/src/interpretation/instructions/` | Versioned interpretation instructions, request context | I07 |
| `core/tests/interpretation_instructions/` | Instruction tests | I07 |
| `core/src/interpretation/apply/` | Proposal validation and atomic application | I05 |
| `core/tests/proposal_application/` | Proposal application tests | I05 |
| `core/src/interpretation/dispatch/` | Adapter selection, routing, provider coordination | I06 |
| `core/tests/interpretation_dispatch/` | Dispatch tests | I06 |
| `core/src/providers/contracts/` | Provider protocol contracts, normalized I/O | V01 |
| `core/tests/provider_contract/` | Provider contract tests | V01 |
| `core/src/providers/anthropic/` | Anthropic API adapter | V06 |
| `core/tests/providers_anthropic/` | Anthropic adapter tests | V06 |
| `core/src/providers/openai/` | OpenAI API adapter | V07 |
| `core/tests/providers_openai/` | OpenAI adapter tests | V07 |
| `core/src/providers/self_hosted/` | Verified self-hosted endpoint adapter | V09 |
| `core/tests/providers_self_hosted/` | Self-hosted adapter tests | V09 |
| `core/src/privacy/routing/` | Route authorization, destination validation | V02 |
| `core/tests/routing/` | Routing tests | V02 |
| `core/src/jobs/queue/` | Durable job queue, lease management | J01 |
| `core/tests/job_queue/` | Job queue tests | J01 |
| `core/src/jobs/configuration/` | Profile versioning, configuration change handling | V03 |
| `core/tests/job_configuration/` | Job configuration tests | V03 |
| `core/src/jobs/runner/` | Foreground job execution orchestration | J02 |
| `core/tests/job_runner/` | Job runner tests | J02 |
| `core/src/jobs/health/` | Bounded processing health reporting | J03 |
| `core/src/retrieval/index/` | Transactional FTS indexing (original/corrected text) | R01 |
| `core/tests/search_index/` | FTS index tests | R01 |
| `core/src/retrieval/query/` | Scoped literal search, filters, pagination | R02 |
| `core/tests/retrieval/` | Query tests | R02 |
| `core/src/retrieval/phrases/` | Natural-language filter resolution (since, type, scope) | R03 |
| `core/tests/query_phrases/` | Phrase resolution tests | R03 |
| `core/src/reminders/state/` | Reminder desired state and operation records | N01 |
| `core/tests/reminder_state/` | Reminder state tests | N01 |
| `core/src/reminders/reconcile/` | Native notification reconciliation (desired ↔ OS) | N03 |
| `core/tests/reminder_reconcile/` | Reconciliation tests | N03 |
| `core/src/reminders/coordinator/` | Capture-to-scheduled orchestration | N05 |
| `core/tests/reminders_coordinator/` | Coordinator tests | N05 |
| `core/src/reminders/history/` | Reminder history (requested/installed/user events) | N06 |
| `core/tests/reminder_history/` | History tests | N06 |
| `core/src/reminders/capacity/` | OS pending-request capacity and permission tracking | N04 |
| `core/src/suggestions/eligibility/` | Eligibility scoring and rotation logic | S01 |
| `core/tests/suggestion_eligibility/` | Eligibility tests | S01 |
| `core/src/suggestions/schedule/` | Daily optional prompt scheduling | S02 |
| `core/tests/prompt_schedule/` | Prompt schedule tests | S02 |
| `core/src/suggestions/coordinator/` | Prompt activation and item selection | S03 |
| `core/tests/suggestions_coordinator/` | Suggestion coordinator tests | S03 |
| `core/src/suggestions/preview.rs` | Non-private item preview eligibility (route-based, not classifier) | S04 |
| `core/src/review/shadow/` | Optional budgeted shadow review (sampled, diagnostic) | E03 |
| `core/src/lifecycle/delete_intent/` | Deletion intent and processing tombstones | L01 |
| `core/tests/deletion_intent/` | Deletion tests | L01 |
| `core/src/lifecycle/retention/` | Audio retention policy and expiry | L03 |
| `core/src/export/` | Versioned export and source-safe serialization | L04 |
| `core/src/ffi/` | Typed bindings, ABI, thread safety, error conversion | B01 |
| `core/src/metrics/` | Content-free latency and reliability metrics | T01 |

### Native Modules (Swift)

| Module | Responsibility | Task(s) |
| --- | --- | --- |
| `ios/Services/ProtectedStorage/` | File protection, device lock integration, backup exclusion | C01 |
| `ios/Services/Authentication/` | Session-scoped read-auth boundary | C01 |
| `ios/Services/Credentials/` | Keychain storage, opaque reference keys, add/update/delete operations | V04 |
| `ios/Tests/Credentials/` | Credential storage tests | V04 |
| `ios/Services/ProviderTransport/` | Native HTTP, TLS validation, redacted diagnostics | V05 |
| `ios/Tests/ProviderTransport/` | Provider transport tests | V05 |
| `ios/Services/Notifications/` | Schedule/cancel/list, event ingestion bridge | N02 |
| `ios/Tests/Notifications/` | Notification service tests | N02 |
| `ios/Services/Ingress/` | Durable capture import, file ownership | C02 |
| `ios/Tests/Ingress/` | Ingress tests | C02 |
| `ios/Services/Transcription/` | On-device transcript attachment and queueing | C05 |
| `ios/Tests/Transcription/` | Transcription tests | C05 |
| `ios/Services/ShadowReview/` | Optional shadow-review feature, diagnostic only | E03 |
| `ios/Services/NotificationPermission/` | Notification permission request/monitoring | N04 |
| `ios/Services/Permissions/` | Microphone, speech, and notification permission UI and coordination | U11 |
| `ios/Services/Health/` | Processing health reporting native integration | J03 |
| `ios/Services/Metrics/` | Content-free metrics collection native integration | T01 |
| `ios/Services/JobRunner/` | Foreground lifecycle job execution | J02 |
| `ios/Tests/JobRunner/` | Job runner tests | J02 |
| `ios/Services/Deletion/` | Audio, ingress, cache, notification cleanup | L02 |
| `ios/Tests/Deletion/` | Deletion tests | L02 |
| `ios/Services/AudioRetention/` | Expiry sweep, retention status | L03 |
| `ios/Services/Export/` | Consistent snapshot export via user-selected destination | L04 |
| `ios/Services/Reset/` | Delete-all generation fencing and orchestration | L05 |
| `ios/Tests/Reset/` | Reset tests | L05 |
| `ios/Services/NotificationActions/ActionHandler.swift` | Notification tap/action handling, deep-link routing | N07 |
| `ios/Tests/NotificationActions/ActionHandlerTests.swift` | Notification action tests | N07 |
| `ios/Services/NotificationActions/PreviewPolicy.swift` | Preview-safe route filtering for notifications | S04 |
| `ios/Services/BackgroundCompletion/` | Bounded background execution lease and expiry | B04 |
| `ios/Tests/BackgroundCompletion/` | Background completion tests | B04 |
| `ios/Services/TimeChangeCoordinator/` | Wall-clock and timezone change reconciliation | B05 |
| `ios/Tests/TimeChangeCoordinator/` | Time change tests | B05 |
| `ios/Services/Assembly/CompositionRoot.swift` | Production service dependency injection | B02 |
| `ios/Tests/Services/CompositionRootTests.swift` | Composition root tests | B02 |
| `ios/Services/Assembly/LifecycleCoordinator.swift` | Launch/foreground/background reconciliation | B03 |
| `ios/Tests/Services/LifecycleCoordinatorTests.swift` | Lifecycle coordinator tests | B03 |
| `ios/Services/OptionalCapabilities/` | Verified self-hosted and shadow capability registration | T11 |
| `ios/Tests/OptionalCapabilities/` | Optional capability tests | T11 |
| `ios/Capture/Text/` | Text entry and save surface | C03 |
| `ios/Tests/TextCapture/` | Text capture tests | C03 |
| `ios/Capture/Voice/` | Recording control with recovery UI | C04 |
| `ios/Tests/VoiceCapture/` | Voice capture tests | C04 |
| `ios/Capture/Entry/` | System control entry and handoff integration | C06 |
| `ios/Capture/Acknowledgment/` | Save acknowledgment UI (save/transcript/reminder states) | C06 |
| `ios/Tests/CaptureEntry/` | Capture entry tests | C06 |
| `ios/OhAndCoreBridge/` | Swift → Rust bridge, error conversion, threading | B01 |
| `ios/AppAssembly/` | Production app composition and UI assembly | U01 |

### Management UI Modules (App, Tauri or SwiftUI)

| Module | Responsibility | Task |
| --- | --- | --- |
| `app/` | Root composition and navigation | U01 |
| `app/retrieval/` | Original-text search, filter UI, source detail | U02 |
| `app/source-detail/` | Source provenance, audio playback, corrections | U02 |
| `app/item-controls/` | Type correction, lifecycle controls (done/cancel) | U03 |
| `app/reminders/` | Reminder editing, history, status UI | U04 |
| `app/provider-settings/` | Profile add/edit/switch/remove, credential entry | U05 |
| `app/privacy-settings/` | Route preferences, destination configuration | U06 |
| `app/data-settings/` | Retention, export, item-delete, delete-all | U07 |
| `app/settings/permissions/` | Microphone, speech, notification permission UI | U11 |
| `app/prompt-settings/` | Optional prompt time, toggle, mute | U08 |
| `app/suggestions/` | Prompt activation UI, response actions, item selection | U08 |
| `app/settings/prompt-preview/` | Preview mode toggle and eligibility explanation | S04 |
| `app/status/` | Health reporting, recovery UI (no monitoring required) | U09 |
| `app/accessibility/` | Voice/text/keyboard navigation, dynamic type, VoiceOver | U10 |

### Probe and Validation Modules

| Module | Responsibility | Task |
| --- | --- | --- |
| `core/bindings/` | Rust → Swift generated bindings | B01 |
| `ios/BridgeProbe/` | Round-trip boundary validation | P01 |
| `ios/NotificationProbe/` | Native scheduling/list/cancel primitives | P05 |
| `ios/AudioProbe/` | Recording interruption and partial-audio recovery | P03 |
| `ios/TranscriptionProbe/` | On-device transcription availability probe | P04 |
| `ios/CredentialProbe/` | Keychain accessibility and lock behavior | P11 |
| `ios/CaptureProbe/` | System-control handoff and entry validation | P02 |
| `probes/tauri/` | Minimal Tauri 2 iOS management probe | P06 |
| `probes/tauri-handoff/` | Native → Tauri entry handoff validation | P07 |

### Test and Fixture Modules

| Module | Responsibility | Task(s) |
| --- | --- | --- |
| `fixtures/intent/` | Synthetic intent fixtures (expected/forbidden outcomes) | I04 |
| `tests/resilience/capture-processing/` | Fault injection and recovery validation (capture path) | T02 |
| `tests/resilience/notifications/` | Fault injection and recovery validation (reminder path) | T03 |
| `tests/privacy/` | Boundary and routing canaries | T04 |
| `ios/Tests/EndToEnd/` | Production wiring simulator test suite | T09 |
| `ios/Tests/FailureJourneys/` | Provider/permission/interruption failure paths | T12 |
| `ios/Tests/Accessibility/` | Accessibility validation (VoiceOver, Dynamic Type, navigation) | U10 |

### Documentation and Validation Modules

| Module | Responsibility | Task |
| --- | --- | --- |
| `docs/architecture/m1-contracts.md` | Architecture boundaries and contracts (this document) | F01 |
| `docs/architecture/m1-shell.md` | Management-shell decision record | P10 |
| `docs/features/m1-plan.md` | Executable M1 specification | (reference) |
| `docs/features/m1-tasks.json` | Machine-readable M1 task graph | (reference) |
| `docs/validation/m1-protocol.md` | Device and two-week trial evidence protocol | F07 |
| `docs/validation/core-binding.md` | Rust-Swift boundary validation | P01 |
| `docs/validation/notification-probe.md` | Notification scheduling and limits validation | P05 |
| `docs/validation/capture-entry.md` | Native control handoff and entry validation | P02 |
| `docs/validation/audio-probe.md` | Recording and partial-audio recovery validation | P03 |
| `docs/validation/transcription-probe.md` | On-device transcription capabilities validation | P04 |
| `docs/validation/credential-probe.md` | Credential protection and lock behavior validation | P11 |
| `docs/validation/signing.md` | Reproducible signing and device-build procedure | P08 |
| `docs/validation/tauri-build.md` | Tauri iOS management shell probe validation | P06 |
| `docs/validation/tauri-handoff.md` | Native-to-Tauri handoff validation | P07 |
| `docs/validation/intent-fixtures.md` | Synthetic intent fixture documentation and expected outcomes | I04 |
| `docs/validation/evaluation-format.md` | Semantic evaluation report format | E01 |
| `docs/validation/spark-protocol.md` | Self-hosted endpoint validation and compatibility | V08 |
| `docs/validation/live-providers.md` | Live provider interchangeability validation | E02 |
| `docs/validation/device-feasibility.md` | Actual-device feasibility evidence (P09) | P09 |
| `docs/validation/accessibility.md` | Native accessibility validation results | U10 |

## Data Protection and Backup Lifecycle

### File Protection Classes

All stored data is protected by device file protection class:
- **NSFileProtectionComplete**: captures (text/audio references), items, corrections, reminders, import records. Requires device unlock. Aligns with Keychain `kSecAttrAccessibleWhenUnlocked`.
- **NSFileProtectionCompleteUnlessOpen**: indexes, caches, FTS indexes. Accessible after first unlock within session. Aligns with Keychain `kSecAttrAccessibleAfterFirstUnlock`.
- **NSFileProtectionNone**: configuration, metadata (non-sensitive). Never locked. Aligns with Keychain `kSecAttrAccessibleAlways`.

Credentials and secrets are stored in native Keychain with appropriate accessibility class (at minimum `kSecAttrAccessibleAfterFirstUnlock` for supported cases; stricter where needed for sensitive credentials).

### Audio Retention and Deletion

- **Successful transcription**: original audio file expires after 7 days (default configurable).
- **Transcription pending**: audio retained indefinitely until user explicitly deletes or successful transcription occurs.
- **Sole source**: if audio is the only record and transcription is unsupported/failed, retained with visible status (no auto-delete).
- **User deletion**: explicit item deletion enqueues audio file removal atomically.
- **Backup exclusion**: audio files are excluded from iCloud backup and device backup, never encrypted to remote.

### Deletion Guarantee and Backup Lifecycle

- Deletion marks item as tombstone and removes from all indexes (immediate).
- Audio file removal and ingress-record cleanup are enqueued (best-effort).
- Interruption during cleanup does not restore readable text or reminders.
- Repeated deletion of same item ID is idempotent (no error).
- **Backup truth**: deleted items appear as tombstones in backups created after deletion. A device restore from a backup taken *before* the deletion will restore the item; a backup taken *after* deletion will not resurrect it. M1 does not promise that deletion is permanent in an immutable backup snapshot; it promises that the device's records accurately reflect deletion and that future captures after a factory reset are independent. Device restoration behavior is covered by M3; M1 documents the honest deletion-versus-backup lifecycle rather than guaranteeing rescue from immutable historical backups.

### Export Behavior

- Export captures a snapshot of all items, corrections, metadata, and audit trail.
- User selects which routes to include (personal, work, or both).
- Reminders are exported as historical record only, not as re-importable scheduled events.
- Audio files are included only if user explicitly requests; default excludes.
- Export is not a tested full backup/restore mechanism (M3 task).
- Device restoration is M3 scope; M1 supports deletion and local snapshot only.

### Credential Boundary

The credential boundary is **strictly compartmentalized**:
- **Swift (native) owns**: Keychain storage, credential retrieval, add/update/delete operations for setup and testing.
- **Rust owns**: credential dispatch (only at provider-request time, never persisted).
- **Never held in job state**: credentials are resolved at dispatch time, not stored in queue. If a job is retried after a credential is deleted, the retrieval will fail explicitly.
- **No credential flow to webview or exports**: credentials never serialized into profiles or configuration exports.
- **Keychain-resolved bytes are transient**: Rust receives raw secret bytes only for the active HTTP request; cleared immediately after send.
- **Credential write semantics**: Swift Keychain store/delete operations may be used at setup time for user-provided credentials (e.g., API key entry) or in testing. In M1 production, no credential is persisted into job state; it is resolved on demand at dispatch.

## Settled Architectural Choices

### Core Technology

- **Rust + SQLite**: domain state, interpretation, reminder scheduling, provider coordination.
- **Swift (native)**: protected storage, credentials, audio, system integration, effects execution.
- **No sync in M1**: state says sync is not configured; seams for M2 are inexpensive (owner-scoped storage).
- **Single phone only in M1**: no multi-device sync; Mac is deferred to M2.

### Capture and Processing

- **Foreground native capture surface**: no background microphone, no always-listening.
- **Voice explicitly opens recording surface**: cold/warm/locked handoff verified by P02.
- **Silent text entry**: direct field with save action, no classification ceremony.
- **On-device transcription only (M1)**: unsupported language/missing model preserves audio with explicit retry/delete.
- **No transcription cloud fallback**: if on-device fails, audio remains pending with visible options.
- **Offline fast-path reminders**: bounded set of explicit commands recognized locally without provider call.
- **Original-text FTS retrieval**: no embeddings or generated answers in M1.

### Reminders and Notifications

- **One-shot reminders only (M1)**: recurring requests explicitly marked unsupported, not silently reduced.
- **Deterministic time resolution**: ambiguity and past-time-requested preserved as-is, not auto-corrected.
- **Durable reconciliation with OS**: desired state tracked separately, not inferred from delivery.
- **Clock passage is not delivery evidence**: elapsed time does not imply user attention or action completion.
- **Notification payloads are generic**: no private text in OS notifications.
- **Optional daily prompt (off by default)**: one generic prompt at user-selected time, bounded horizon.
- **Explicit opt-in for previews**: default generic; preview-safe designation and user opt-in required.

### Privacy and Credentials

- **No automatic vendor fallback**: profile/destination changes never silently reroute in-flight work.
- **Credentials in native Keychain**: never sent to webview, logs, exports, or synced profiles.
- **Private read authentication**: lock-screen unlock or explicit local auth required for private-route access.
- **No disclosure gate from classifier alone**: no classifier controls processing permission; only explicit user configuration grants processing permission.
- **Universal route is not preview-safe by default**: classification can only remove eligibility, never grant preview permission. Preview requires explicit user opt-in and route designation.
- **Spark hardware alone establishes no protocol**: actual endpoint verification required before claim (V08).
- **No hosted tenancy in M1**: single-user owner scoping preserves inexpensive future hosted seams without building a multi-tenant service.

### Configuration and Extensibility

- **Versioned provider profiles**: immutable version IDs, configuration changes never silently reroute queued work.
- **Injected effects interfaces**: all native operations are trait-injected; Rust is framework-independent.
- **Sealed shared manifests**: F02/F03 reserve registrations; parallel tasks do not concurrently edit Cargo.toml or project.yml. Later additions use documented serialization edges (e.g., F02 → D01 → J01).
- **Extension seams for optional capabilities**: shadow review and self-hosted registration use extension registry without core edits.

### Data Lifecycle

- **Source immutable after ingestion**: corrections coexist; reprocessing cannot overwrite.
- **Derived state replaceable**: invalid/stale interpretations revert to prior state, not demotion.
- **Deletion is durable tombstone**: cleanup is best-effort; interruption must not restore readable text.
- **Audio retention bounded**: successful-transcription audio expires (default 7 days); sole-source audio retained with visible status.
- **Export is snapshot only**: M1 does not claim full backup/restore; sync hardening is M3.
- **Honest backup/restore behavior**: deleted items appear as tombstones in post-deletion backups; restore from pre-deletion backup may restore the item. Device restoration behavior and full sync are M3 scope.

## Provider Capabilities and Credentials

### Provider Profile Structure

Each provider profile records:
- Adapter type (Anthropic, OpenAI, self-hosted endpoint).
- Endpoint URL (when applicable).
- Model identifier.
- Credential reference (opaque key; secret resolved at dispatch).
- Timeout and retry policy.
- **Capabilities**: list of supported operations (text interpretation, transcription, embeddings, speech-generation).
- Profile version (immutable UUID; changing any setting creates a new version).

Queued jobs pin their profile version at dispatch time. Profile changes never silently reroute old queued work.

### Supported Provider Capabilities

M1 recognizes the following capabilities per profile:
- **Text interpretation**: receipt of text input, structured output proposal (note/action/idea detection, reminder time extraction).
- **Transcription**: M1 uses on-device only (no provider-based capability); listed for M2+ compatibility.
- **Embeddings**: listed for M2+ compatibility; not supported in M1.
- **Speech generation**: listed for M2+ compatibility; not supported in M1.

Unsupported capabilities are visible in provider settings. If a profile lacks a required capability, the system shows explicit unavailable status, not silent fallback to another provider.

### Credential Storage and Resolution

- Credentials are native Keychain-resident references, never included in exported profiles or job state.
- Swift resolves credentials on demand (only when provider request is about to dispatch).
- Rust receives raw secret bytes transiently; never stored in any queue, cache, or log.
- Credential retrieval errors are explicit and propagated; no automatic fallback to another profile or endpoint.
- Credential changes affect only new queued work; existing queued jobs retain their profile version and continue with old credentials (and fail explicitly if credentials are deleted).

### Clock and Timezone Handling

The Rust core receives injected clock and timezone providers:
- Current time (for comparing due dates and checking if reminders have elapsed).
- Local time with timezone (for display and user-facing time formatting).
- Wall-clock and timezone change events trigger reconciliation of reminders and prompts (e.g., moving across time zones re-evaluates all pending reminder times).

## Validation and CI Requirements

- **Linux core CI** (F04): build, lint, test on GitHub runners.
- **Native simulator CI** (F05): real build/test on macOS runners only. Linux workers may author native code but cannot substitute Linux checks for a successful native build; macOS CI is required.
- **Artifact hygiene** (F06): secret scanner + policy checks; no real secrets, private data, or signing material in public repo.
- **No placeholder success**: all checks must be real; failing tests block PRs.

## Next Steps

- F02 creates the Rust workspace, implements core module skeletons, and establishes build/test commands.
- F03 creates the native iOS project structure with reproducible generated builds.
- F04 wires core CI; F05 adds native simulator CI.
- D01 implements the versioned SQLite schema from this contract.
- Remaining tasks fill modules and validate against these contracts.

All implementation changes are bounded to task-owned paths. Shared manifests (Cargo.toml, Makefile, ios/project.yml, AGENTS.md) follow explicit ordering dependencies: F02 owns initial Cargo.toml/Makefile, F05 extends them; F03 owns ios/project.yml, F05 reads it; F04 owns initial AGENTS.md, F05 extends it.
