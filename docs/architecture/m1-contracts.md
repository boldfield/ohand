# M1 Architecture Contracts and Ownership Map

Status: executable specification, 2026-10-07. This document fixes the module boundaries, versioned interfaces, state vocabularies and file ownership for Milestone M1. It is a prose contract: it lists fields, invariants and error classes, and contains no implementation code. Downstream tasks fill in implementations; any valid implementation must satisfy this contract. A material change requires a focused follow-up task that updates this document before dependent work proceeds.

## Overview

M1 has three layers with separate ownership:

- **Rust core with SQLite** (`core/`): authoritative domain state, interpretation coordination, provider protocol adapters, job orchestration, reminder logic, retrieval, policy enforcement.
- **Native Swift** (`ios/`): protected storage, Keychain credentials, audio capture, on-device transcription, native HTTP transport, local notifications, permissions, app lifecycle and foreground job execution hosting.
- **Management shell** (`app/`): retrieval, settings and corrections. Whether it is Tauri or SwiftUI is decided by P10 from measured probes; the contract below is shell-independent.

Settled baseline: one installed iPhone app; no sync (state reports `not_configured`); no hosted tenancy (single-user owner scoping keeps future hosted seams inexpensive without building a multi-tenant service); no Mac client; foreground native capture surface; on-device transcription only; original-text full-text search only; one-shot reminders only.

Rust owns authoritative domain state. Swift owns platform effects and protected secrets and holds no domain decisions. A web-based shell never holds credentials, provider secrets, or private note content beyond what an authenticated read returns for display.

## Stable Identifiers and Versioning

### Capture ID

- A random UUID generated exactly once by the native entry point when a capture begins. It is never derived from content, so two intentional captures with identical text receive different IDs.
- Persisted durably in the ingress record before any acknowledgment is shown to the user.
- Every retry or replay of the same capture (cold relaunch, re-delivery, interrupted import) reuses the persisted ID; this makes the ID the idempotency key.
- Importing an ID that already exists returns the existing item and creates nothing. If the same ID arrives with different source content, the import is rejected with a conflicting-reuse error and the original record is retained untouched.

### Item ID

- A random UUID assigned by core at first import and stored, in the same transaction, with its capture ID under a uniqueness constraint. The mapping is one-to-one and immutable.
- Used in all downstream state: reminders, suggestions, corrections, exports, notification payload identifiers.

### Revision

- Each item has a monotonically increasing integer revision starting at 0 on import.
- Every user-initiated mutation increments the revision: text correction, type correction, session-topic assignment, scope change, completion, cancellation, deletion. The previous values remain in the correction history.
- Applying derived output (an interpretation proposal, a reminder time proposal) does not increment the revision. Derived output records the item revision it was computed from.
- Every write carries the expected revision (compare-and-set); a stale expectation is rejected with the current state returned.
- A proposal is applicable only if its source revision equals the item's current revision. A user mutation after the proposal was computed therefore invalidates it. Reprocessing can never overwrite a correction or lifecycle event.
- Only one proposal is applied per item revision at a time; applying a newer proposal for the same revision supersedes the older one atomically.

### Proposal, Request, Job and Profile Versions

- **Provider profile version**: immutable UUID. Any change to adapter, endpoint, model, capabilities, timeouts or authorized destinations creates a new profile version. The credential is referenced by a stable opaque credential reference that is not part of the profile version (see Credential Boundary).
- **Request version**: immutable identifier of one interpretation request, covering the versioned instruction set (I07), the request context version, the profile version and the source revision. Recorded for audit.
- **Proposal schema version**: a proposal carries a schema version number, validated before any semantic check. Unknown or unsupported schema versions are rejected without mutation.
- **Proposal identity**: each proposal has an immutable proposal ID and records capture ID, source revision, request version and proposal schema version.
- **Job version**: each job has an immutable job ID and records the job type (interpretation, transcription attachment, shadow review), the profile version and request version pinned at enqueue, the source revision, attempt count and creation time. Transcription-attachment jobs run on-device, so their profile version and request version are not applicable and are recorded as absent. A job keeps its pinned profile version for its whole life. Changing the default profile never reroutes queued jobs; if a pinned profile version is deleted or its destination is no longer authorized, the job stops with an explicit configuration failure and waits for the user, never falling back to another destination.

### Time Context

Every capture stores: capture instant (UTC), IANA timezone identifier, UTC offset at capture, locale, and calendar. Date and reminder resolution uses the capture instant and stored timezone as its reference, never the processing time. Core receives an injected clock and timezone provider; wall-clock and timezone changes are events that trigger reconciliation of pending reminders and prompts.

## State Model: Source, Corrections and Derived State

### Authoritative Source

- Original text, or an audio reference when no transcript exists yet; capture instant and time context; item scope and route; entry metadata.
- Immutable after import. Corrections coexist and never replace source.
- Always searchable (once text exists) and always recoverable.

### Explicit User Corrections

- Separate revisioned records: text corrections, type changes, session-topic assignments, scope changes.
- Timestamped and inspectable; the source and the latest correction are both available to the UI.
- Later reprocessing cannot overwrite them.

### Derived State (Non-Authoritative)

- Interpretation results: note, action or idea annotation, reminder time proposal, session-topic proposal, source spans. M1 has no embeddings and no generated summaries or answers.
- Replaceable on demand. An invalid or stale result is discarded and the previous state remains; it never demotes the item.
- Schema validation alone is never sufficient; semantic validation (source spans exist, times are plausible, scope and permission unchanged) precedes application. Rejected output mutates nothing.

### Independent State Dimensions

Each dimension below is stored and reported separately. A value in one dimension never implies a value in another.

**Save state** (durability of the source):
- `not_saved`: before the durable commit.
- `saved_local`: the source is durably committed on the device and acknowledged. The acknowledgment point for a native handoff is the committed ingress record; core import may follow later and is idempotent.

**Sync state** (independent of save): `not_configured` is the only M1 value. M2 may add pending and synced values. M1 never displays a synced claim.

**Processing state** (interpretation):
- `unprocessed`: not yet attempted, or waiting for an authorized destination or configuration.
- `processing`: a job is leased or in flight.
- `processed`: a valid proposal was applied.
- `abstained`: the interpreter explicitly abstained; source remains searchable and never becomes an action.
- `uninterpreted`: processing ended without a usable result (permanent failure, malformed or refused output); source remains searchable with visible status.

Transient failures (timeout, network, outage) keep the state `unprocessed` or `processing` with the job in retry wait; they never produce `uninterpreted`.

**Transcription state** (voice captures only):
- `audio_pending`: audio saved, on-device transcription not started.
- `transcribing`: in progress.
- `transcribed`: transcript text attached; the transcript is the searchable source text and the audio retention clock starts.
- `transcription_unsupported`: language or model not available on the device; audio retained with visible retry and delete options.
- `transcription_failed`: transcription failed (for example unreadable audio); audio retained with visible retry and delete options.

**Reminder request state** (what the user asked for):
- `not_requested`.
- `resolved`: a single absolute instant was resolved; waiting for scheduling.
- `not_scheduled_yet`: a reminder was requested but the time is ambiguous or incomplete. The original phrase is preserved and the UI offers a correction path (choose a time). Nothing is scheduled.
- `unsupported_recurrence`: a repeating reminder was requested. It is saved as not scheduled, clearly explained, and offers a manual one-shot alternative. It is never silently reduced to a one-shot.
- `unschedulable`: a resolved reminder cannot be installed. A reason is recorded: `time_in_past`, `permission_denied`, or `capacity_exceeded`. No automatic time adjustment. Permission recovery and rescheduling paths are offered.
- `cancelled`: cancelled by the user, or because the item was completed, cancelled or deleted.

**Reminder schedule state** (native installation):
- `not_scheduled`, `pending_schedule` (resolved, installation not yet confirmed, for example around boot or a permission change), `scheduled` (the OS reports the request installed), `schedule_failed` (installation error surfaced to the UI; the request stays `resolved` and retried).

**Reminder delivery state** (evidence only):
- `unknown`: no OS evidence. Passage of the due time never changes this; the UI may say the due time has passed and delivery is unknown.
- `delivered`: the OS reported the notification delivered or presented.
- `opened`: the user opened the app through the notification.

**Reminder acknowledgment** is independent of delivery and of item completion: `not_acknowledged` or `acknowledged` (the user explicitly acknowledged or dismissed the reminder). Acknowledging does not complete the item; completing is a separate item lifecycle event.

**Item lifecycle state**:
- `active`: normal; searchable and eligible for suggestions subject to policy.
- `completed`: marked done by the user; searchable, not suggested.
- `cancelled`: dropped by the user; searchable, not suggested.
- `deleted`: tombstone; excluded from every result while cleanup completes.

**Suggestion state**: `not_eligible` (excluded by policy, lifecycle, snooze, scope, uninterpreted/abstained status or speculative intent), `eligible`, `snoozed` (cooldown-gated), `pull_only` (user said stop suggesting; remains searchable).

The statement the UI may make about a captured reminder is derived from these dimensions. By default the save acknowledgment distinguishes "saved" from "reminder confirmed installed"; an interrupted transcript or unscheduled reminder never implies success.

## Privacy Model: Scopes, Routes, Permissions and Preview

Four concepts are distinct and are used with exactly these meanings in every section.

### Item scope

The stored privacy classification of an item: `personal` or `work`. Every item has exactly one. Personal items are the private material. Scope is explicit policy: it is assigned from the user's configured route at capture time, may be changed only by an explicit user action recorded as a revisioned correction, and is never created, upgraded or changed by a classifier, a provider response, or text found inside a capture.

### Session read scope

The runtime read-authorization state maintained by the native authentication service (C01) for the current app session; it is not an item attribute and is never an ingest input. Values: `capture_only` (locked, cancelled or failed authentication, or relocked: new captures can be saved, but no stored history, counts, snippets or errors derived from stored content may be returned) and `authenticated` (stored items may be read). Relock, backgrounding policy and failed unlock return the session to `capture_only`. A query additionally carries an item-scope filter (personal, work, or both), enforced before search and ranking, so a work-only query cannot reveal personal text through results, snippets, counts or error messages.

### Route and processing permission

A route is a user-configured policy object: route ID, item scope, per-capability destination authorizations (which destination, if any, may receive content for interpretation, optional review and so on), and a `preview_safe` flag. Fresh install has a single universal route with item scope `personal`, no authorized remote destinations (local-only), and `preview_safe` false. Further routes (for example a work route) exist only if the user creates them in setup. Captures use the user's configured default route; a per-capture choice appears only if the user configured more than one route, so there is no per-capture privacy prompt by default.

Processing permission is authorization of a destination for a capability on a route, configured explicitly by the user before any content leaves the device. Local processing (transcription, fast-path recognition, search, deterministic validation) needs no remote authorization. Remote destinations are the configured cloud provider, a private server endpoint (explicit setup and transport validation, one per profile, no automatic fallback) and the optional shadow review endpoint (sampled, diagnostic only, never mutates authoritative state). Every job is authorized against its stored route immediately before dispatch. A classifier, provider output or captured instruction can never grant, widen or change a permission. A queued job's pinned destination is never silently replaced.

### Preview-safe route permission

`preview_safe` is a per-route flag with these rules:
- Defaults to false on every route; only an explicit user action in preview settings may set it.
- A route whose item scope is `personal` (the private routes, including the universal default route) can never be preview-safe.
- Classification, item-level user exclusion and suggestion eligibility can only remove preview eligibility; nothing derived can grant it.
- An item is preview-eligible only if its route is preview-safe, the global preview opt-in is enabled, and the item is otherwise suggestion-eligible.
- Revoking the flag, reclassifying or deleting an item, or stop-suggesting cancels and reconciles pending preview notifications. Banners already delivered by the OS cannot be recalled and the UI says so.

### Session-topic facet

The session-topic (for example a private therapy-session note) is a user-correctable facet that is independent of item type (note, action, idea), of item scope, of session read scope and of every permission:
- Source-linked: carries evidence (source span or user assignment) and is revision-tracked.
- Set by a bounded local phrase recognizer, by an interpretation proposal, or by the user; the user's assignment always wins and survives reprocessing.
- Used as a retrieval filter, combinable with item scope filters; a local-only session note is retrievable without a configured provider.
- Setting it never changes item scope, upload permission, or preview eligibility.

### Enforcement summary

- Retrieval: session read scope is checked first, then item-scope filtering, before search, ranking, snippets, counts and errors. Query text is never uploaded for literal search.
- Notifications: see the Notification Effect Interface payload kinds. OS notification text is generic unless the preview rules above approve it.
- Export: includes only the user-requested scopes and only after authentication; no credentials, endpoint secrets or deleted content.

## Core Domain Contracts

### Capture Ingestion Contract

Input: capture ID; text, or an audio reference inside protected storage; item scope and route ID from the user's configured routing (personal or work, never a session value); optional session-topic assignment; capture instant and time context; optional entry metadata (permission state, locked or unlocked entry, signal quality).

Output: acknowledgment containing capture ID, item ID, save timestamp. The import transaction creates the item, indexes source text and records initial states. No remote request is made.

Errors: duplicate capture ID returns the existing item; same ID with different content is a conflicting-reuse rejection; missing or unknown route, null content or unsupported scope value is a specific validation rejection; storage failure leaves no partial write and the ingress record intact for retry.

### Interpretation Request Contract

Input: capture ID, source revision, authoritative text (original or latest correction), request version, profile ID and version, route (internal use only for authorization, not disclosed to the interpreter), time context.

Output (proposal): schema version; zero or one note, action or idea annotation; optional reminder time with resolution quality (explicit, inferred, ambiguous); optional session-topic proposal; source spans into the original text; or an explicit abstention.

Errors: timeout or outage keeps the job in retry wait with no mutation; invalid or schema-unsupported output is rejected and the source is retained as searchable; refusal or capability mismatch becomes an abstention or a visible unavailable status; profile or configuration mismatch stops the job with a visible explanation and no fallback.

### Reminder Scheduling Contract

- Supported: explicit one-shot reminders with a deterministically resolved absolute instant (stored UTC, with the timezone kept for display), from a bounded offline fast path or from a validated interpretation result.
- Unsupported: recurring reminders. They are saved as `unsupported_recurrence`, never reduced to a one-shot; recurring reminders are a candidate for early M2.
- Ambiguous times ("next week") become `not_scheduled_yet` with a correction path; they are never guessed.
- Durable desired state (item ID, reminder ID, resolved instant, schedule generation) is stored by core before any native call and reconciled with the OS; installation is not inferred from delivery.
- Core-derived notification identifier: a deterministic string built from the reminder ID and a schedule generation number that increases each time the reminder's time changes. It is used verbatim as the OS notification request identifier, so a retry or crash replay replaces rather than duplicates, and reconciliation compares the core's desired set to the OS pending set by this identifier. There is no separate OS-assigned identifier.
- Capacity: the OS pending-request limit and a documented horizon are tracked; overflow is `unschedulable` with reason `capacity_exceeded` plus a refill policy, never silent dropping.
- Errors: past time, denied permission, capacity and install failure map to the reminder states above and are surfaced.

### Suggestion Rotation Contract

- Eligible: active, interpreted items with clear actionable form. Excluded: completed, cancelled, deleted, snoozed, pull-only, uninterpreted, abstained and speculative items.
- Selection is deterministic under an injected clock; the reason for each selection is recorded; no fabricated urgency.
- Responses: not-now (cooldown), stop-suggesting (pull-only, still searchable), done (completion event). Non-response is not completion or a signal of importance.
- Default notification content is generic, with the item chosen on authenticated open. Previews follow the preview-safe route permission above.

### Deletion Contract

- Input: item ID and expected revision. Effects, atomic in core: mark the item `deleted` (tombstone), remove it from every index and result, cancel its jobs and reminders, enqueue cleanup of audio files, ingress records, caches and native notifications.
- A racing job result, correction or stale replay after deletion is rejected. Deleting an already deleted item is a no-op. Cleanup is best-effort and idempotent; an interruption never makes readable text reappear.
- The tombstone is durable on the device. Backups are covered by the honest lifecycle in Data Protection and Backup; deletion is logical and is not described as guaranteed physical flash erasure.

## Native-to-Core Ownership and Effect Interfaces

### Ownership

Swift owns: protected storage and file protection; Keychain storage and secret entry; audio capture; on-device transcription; native HTTP execution including credential attachment; local notification primitives; permissions; app lifecycle events and bounded background time; session read authentication.

Rust owns: identifiers and revisions; all state machines above; interpretation dispatch and provider protocol adapters (request building, response parsing, validation); job queue, retry and leases; reminder orchestration and reconciliation; destination authorization; retrieval; deletion coordination; cleanup ordering.

All native capabilities reach Rust through injected effect interfaces so Rust is testable without a device. Each effect call is cancellable, time-bounded and returns a normalized result.

### Normalized errors and lifetime rules

- Every effect returns success or an error of one of these classes: `transient` (retry with backoff), `permanent` (surface to the user; no retry), `unauthorized` (permission or credential problem; explicit, never a silent fallback), `cancelled`, `unsupported`.
- Errors never carry secrets or content; diagnostics are redacted.
- Rust orchestrates and decides; Swift executes. A native effect that outlives its owner is cancelled; cancelled work produces no mutation and state is reconciled at next launch.
- Native background time is best-effort and bounded. Lease expiry preserves honest pending state and never loses an acknowledged capture. Processing in M1 runs while the app is in the foreground, plus a bounded completion lease when it moves to the background; no guaranteed background wake.

### Notification Effect Interface

- Schedule: core identifier, due instant, payload kind, payload text (only for the preview kind), opaque item/action identifier, and, for the preview kind, the approval reference (route ID and preview policy version). Output: installed, or an error class.
- Cancel: core identifier; idempotent.
- List pending: returns the pending requests keyed by core identifier with due instants.
- Event ingestion: delivered, opened and action events keyed by core identifier, used only as delivery evidence.
- Payload kinds: `generic` (fixed app wording with an opaque identifier; the default for reminders and prompts) and `preview_approved` (item-specific text). Only core may produce `preview_approved`, and only for an item on a preview-safe route that passed all preview rules; the native bridge rejects a preview-kind request lacking a valid approval reference. Identifiers in payloads are opaque and convey no meaning. Credentials never enter payloads.
- Invariants: scheduling the same core identifier twice replaces, never duplicates; cancellation is idempotent; every scheduled payload is inspectable in tests.

### Credential Boundary and Storage Service

- Secret bytes live only in the Keychain and, transiently, in native memory during an outbound request. **Rust never holds secret bytes**, at any duration. Provider profiles hold an opaque credential reference only.
- The credential service is native-owned and is not an injected Rust effect for secrets. It offers: add, update and delete of a secret by credential reference, and a status query (present, absent, invalidated) that Rust may call for health display. Secret entry happens on a native secure input surface; the management shell, including a web-based shell, only triggers it and receives the reference and status. Entry bypasses Rust and the webview entirely.
- The native HTTP transport receives a credential reference and an attachment rule (which header, which scheme) from core, resolves the secret from the Keychain at the moment of dispatch, attaches it, and never returns it.
- Behavior on change is a single rule: a credential reference is stable and mutable. Update overwrites the secret behind the same reference. Jobs resolve the credential current at dispatch; a job queued before an update uses the new secret when it runs. If the secret was deleted or invalidated, the job fails explicitly with an unauthorized class error and waits for the user; it does not fall back to another profile or endpoint. The pinned profile version fixes destination and model, not secret bytes.
- Secrets never appear in job state, logs, exports, synced configuration, notification payloads or control extensions.

### Audio Capture Effect Interface

- Start (capture ID) returns a recording handle; stop returns an audio reference, duration and status; cancel is idempotent.
- Hardware interruption or backgrounding preserves partial audio with visible status. Start and stop are paired. Microphone permission denial is an `unauthorized` error. There is no background or always-listening recording.

### HTTP Transport Interface

- Request: destination URL, method, headers, optional body, timeout, response size bound, optional credential reference with attachment rule. Response: status, headers, body, or a normalized error.
- Invariants: authorization of the destination precedes the call; TLS validation is required with no silent cleartext or certificate bypass; cross-origin redirects never forward credentials; response size and time are bounded; no automatic retries (retry policy is Rust-owned); diagnostics are redacted.

### Transcription Interface

- Transcribe: audio reference, language code. Result: transcript text, confidence, detected language, or an error (`unsupported` for language or model, `permanent` for unreadable audio).
- Invariants: on-device only, with no cloud fallback. An unsupported result leaves audio pending with visible retry and delete.

### Lifecycle Coordination Interface

- Events to core: foreground, background, time or timezone change, protected data availability.
- Request bounded background time: returns a lease with an expiry or a refusal.
- Invariants: expiry never loses an acknowledged capture; background completion is not guaranteed.

### Ingress Handoff Contract

The native entry point (cold or warm launch, supported system control, shortcut) writes a durable ingress record and only then acknowledges:
- Fields: capture ID, raw text or an audio file reference in the protected staging area, entry context (instant, time context, permission state, locked or unlocked), route ID and item scope from the user's configured default (personal or work), optional session-topic assignment.
- Core imports the record in one transaction (create item, index text, record states) and then reports the import confirmed; the native side removes the staging record only after confirmation. Repeated delivery, interruption and restart yield neither duplicates nor lost acknowledged captures.
- A locked or cold handoff remains durable (see staging protection class). Capture never grants access to private history.

## Module Ownership Map

The following map lists every implementation area and its owning task. Each path is owned by one task, except the shared files listed with several tasks in the Build table; those are serialized by the edges in Shared File Ordering. Table task columns match the `file_scope` arrays in `docs/features/m1-tasks.json`.


### Build, Workspace, and CI Modules

| Module | Responsibility | Task(s) |
| --- | --- | --- |
| `core/` | Rust workspace root | F02 |
| `Cargo.toml` | Workspace manifest and dependency declarations | F02 |
| `Cargo.lock` | Locked dependencies | F02 |
| `Makefile` | Build, lint, test commands (F01 creates the contract-check targets, F02 extends, F05 later edits) | F01, F02, F05 |
| `tools/contracts/` | Contract and ownership-map validation behind `make check` and `make test` | F01 |
| `ios/` | Native iOS workspace root | F03 |
| `ios/project.yml` | Native Xcode project generation (F03 creates, F05 later edits) | F03, F05 |
| `.github/workflows/core.yml` | Linux CI for Rust checks | F04 |
| `.github/workflows/ios.yml` | macOS CI for native simulator | F05 |
| `.github/workflows/hygiene.yml` | Secret scanner and fixture policy | F06 |
| `AGENTS.md` | Canonical build and test commands (F04 first, F05 next, T07 last) | F04, F05, T07 |
| `tools/hygiene/` | Hygiene check implementation | F06 |
| `docs/contributing.md` | Contribution guidelines | F06 |
| `tools/apple-build/` | Reproducible signing and device build | P08 |
| `tools/apple-build/trial/` | Trial-distribution tooling (subdirectory of P08's directory; T06 is later) | T06 |
| `README.md` | Project overview | T07 |
| `docs/setup.md` | Install and setup guide | T07 |
| `docs/recovery.md` | Recovery guide | T07 |
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
| `core/bindings/` | Rust → Swift generated bindings | P01 |
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
| `docs/validation/m1-device-results.md` | Actual-device results | T05 |
| `docs/validation/m1-trial-results.md` | Two-week trial results | T08 |
| `docs/testing/m1-upgrade-evidence.md` | Upgrade evidence | T10 |
| `docs/releases/m1-trial.md` | Trial release record | T06 |



## Shared File Ordering

Every owned path in the map belongs to exactly one task except the shared files below. The manifest gives F01 only this document; F01 also reserves `Makefile` and `tools/contracts/` so that the contract has real checks before any application code exists. Each shared file has an existing dependency chain in `docs/features/m1-tasks.json` that serializes its editors; no two editors run concurrently. Anything else needing a shared-file edit must wait on one of these edges or get a separate serialized integration task.

- Makefile: F01 creates it with the contract-check targets, F02 extends it with the Rust targets, and F05 later adds the native targets. Edge: F01, then F02, then F04, then F05. Later editors keep the contract checks inside `make check` and `make test`.
- AGENTS.md: F04, then F05, then T07. Edge: F04, then F05, then T06, then T07.
- ios/project.yml: F03 creates it, F05 later edits it. Edge: F03, then F05.
- tools/apple-build/: P08 owns the directory; T06 owns only its trial/ subdirectory. P08 is an ancestor of T06 in the dependency graph, so T06 starts after P08 lands.
- Cargo.toml and Cargo.lock: F02 only. F02 reserves workspace and module registration so feature tasks add files inside their own directories without editing the manifest.
- core/bindings/: P01 only; B01 depends on P01 and uses the generated output without editing it.

Native target inclusion is reserved by F03 in the same way. Production service registration is B02's, lifecycle order is B03's, and shell wiring is U01's.

## Data Protection and Backup Lifecycle

### File protection per store

| Store | File protection class | Reason |
| --- | --- | --- |
| Ingress staging records and in-progress audio | `NSFileProtectionCompleteUnlessOpen` | New files can be created and an already-open file kept writing while the device is locked, so a locked or cold handoff stays durable. Not readable while locked. |
| SQLite database, write-ahead log and journal files | `NSFileProtectionCompleteUntilFirstUserAuthentication` | Core must import staged captures, reconcile reminders and run bounded background completion while locked after the first unlock. Reading private history is gated by the session read scope, not by the file class alone. |
| Finalized audio files | `NSFileProtectionCompleteUntilFirstUserAuthentication` | Same reason; retention sweeps and transcription retry may run after first unlock. |
| Full-text index and caches | `NSFileProtectionCompleteUntilFirstUserAuthentication` | Rebuildable derived data that must stay readable wherever the database is. |
| Configuration (profiles without secrets, route and preview settings, retention settings) | `NSFileProtectionCompleteUntilFirstUserAuthentication` | Route settings can reveal private structure, so there is no unprotected class. |

Before the first unlock after a device restart no store is readable, so capture cannot start; the native surface says so rather than losing input.

Provider secrets live in the Keychain with accessibility `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: bounded background completion may need them while locked, and the device-only variant keeps them out of device migration and backups. An encrypted backup restored to the same device may still bring such items back, so re-entry after a restore is the conservative assumption, not a guarantee. P11 verifies lock and relaunch behavior with synthetic secrets; a stricter class is used if the probe shows it is workable. No shared Keychain access group is used.

### Backup inclusion

| Store | Device/iCloud backup |
| --- | --- |
| SQLite database and configuration (without secrets) | Included, protected by the backup's encryption, so a replaced device can recover notes through the OS restore. Restoration behavior is not tested in M1. |
| Audio files (staged and finalized) | Excluded. |
| Ingress staging records | Excluded. |
| Full-text index and caches, temporary files | Excluded; the index is rebuilt from source after a restore. |
| Keychain secrets | Excluded (device-only); the user re-enters credentials after a restore. |

After a restore from backup, core re-reconciles reminders against the OS, rebuilds the index, drops jobs whose credential is missing into an explicit waiting state and marks audio-only captures whose files are absent as unavailable instead of failing silently.

### Deletion versus backup (honest lifecycle)

- Deleting an item tombstones it on the device immediately and removes it from indexes; audio, staging, cache and notification cleanup follows.
- Backups made after deletion contain only the tombstone state. A backup made before the deletion still contains the item, and restoring from it brings the item back. M1 cannot delete from existing backups, and does not claim to.
- After a restore, the user can delete the item again. Delete-all (L05) applies a generation fence on the device. Device restoration hardening is M3.
- Logical deletion is not guaranteed physical flash erasure.

### Audio retention

- Raw audio expires seven days after successful transcription (the clock starts at transcript success, never at capture). The period is configurable.
- Audio that is the sole source (transcription pending, unsupported or failed) is retained with a visible unresolved state until recoverable text exists or the user explicitly deletes it.
- Disk pressure is documented and never causes silent loss of a sole source.

### Export

- A consistent snapshot of the user-requested scopes, after authentication: source, corrections, metadata and history. No credentials, endpoint secrets or deleted content. Audio only on explicit request.
- Reminders export as history, not re-importable scheduled events. Export is not a tested full restore; device restoration and sync hardening are M3.

## Provider Capabilities

Each provider profile records adapter type (Anthropic, OpenAI, verified self-hosted), endpoint when applicable, model, credential reference, timeouts and retry policy, authorized destinations, capabilities, and its immutable profile version.

Capabilities are separate entries, each with a support state (`supported`, `unsupported`, `unverified`), the evidence that established it (adapter test or probe reference), and limits (input size, structured-output mode):
- **Text interpretation**: structured proposals. The only provider capability used in M1.
- **Transcription**: provider transcription is not used; M1 transcribes on device only.
- **Embeddings**: not supported in M1 (no embeddings).
- **Speech generation**: not supported in M1.

A profile claims only verified capabilities; a self-hosted endpoint is `unverified` until the actual probe (V08) passes. The settings UI shows every unsupported or unverified capability explicitly as unavailable with the reason. A job needing a capability the profile lacks stops with a visible message; it never falls back to another profile.

## Settled Architectural Choices

These are recorded, not reopened here:
- Rust plus SQLite owns domain state, provider adapters and orchestration; Swift owns protected storage, credentials, audio, notifications and native transport.
- One iPhone, no sync (state reports not configured), no hosted tenancy, no Mac client in M1.
- Foreground native capture surface; voice explicitly opens the recording surface; no always-listening and no background microphone.
- On-device transcription only; unsupported language or model keeps durable audio with explicit retry and delete; no transcription cloud fallback.
- Offline fast-path recognition of a bounded documented set of explicit reminder and session-topic phrases.
- Original-text full-text search with deterministic date and scope filters; no embeddings or generated answers.
- One-shot reminders only; recurrence is honestly unsupported. Clock passage is not delivery evidence. Acknowledge and complete stay distinct.
- Optional daily prompt is off until configured, bounded, and independent of explicit reminders; generic by default; previews need the preview-safe route permission plus explicit opt-in.
- No classifier is ever the sole disclosure gate (DESIGN.md): processing permission comes only from explicit user configuration, and classification can only narrow permission or preview eligibility.
- No automatic vendor fallback; profile versions pin queued work.
- Spark hardware alone establishes no protocol; a self-hosted claim requires the actual probe and adapter tests.
- Runtime review is optional, sampled, bounded and diagnostic; its failure cannot delay capture or mutate state.
- Native work is validated by standard macOS CI for the exact submitted revision. Linux checks cannot substitute for a successful native build.

## Validation and CI Requirements

- Linux core CI (F04): build, lint and test on hosted runners.
- Native simulator CI (F05): real build and test on macOS runners for the exact revision; F05 also establishes the result-collection path.
- Artifact hygiene (F06): secret scanning and fixture policy; fixtures are synthetic and no secrets, private data or signing material enter the public repository.
- No placeholder success is acceptable: a command is reported as passing only if it ran. Until F02 and F05 land there is no application build or test command.
- F01 creates `make check` and `make test` with real contract checks. `make check` reconciles this ownership map with every `file_scope` path and owner in `docs/features/m1-tasks.json`. It also confirms that the dependency graph is acyclic, that every shared path is listed in Shared File Ordering with its editors serialized by a dependency path, and that this contract has no fenced code. `make test` runs unit tests that feed synthetic drift into those checks and confirm each one fails. F02 and F05 add their real build, lint and test commands to the same targets and do not remove the contract checks.

## Next Steps

- F02 creates the Rust workspace and module skeletons; F03 creates the native project; F04 and F05 add CI and the shared Makefile, `AGENTS.md` and `ios/project.yml` edits in the order above.
- D01 implements the schema from the state and identifier definitions above; remaining tasks fill their owned modules and validate against these contracts.
