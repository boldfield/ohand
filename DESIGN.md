# Oh And: product and delivery design

Status: initial design, 2026-10-07. Product name **Oh And**; repository and Odonian project identifier **ohand**. This document specifies intended behavior, not implemented capabilities.

## Problem and product contract

Capturing a thought and remembering to return to a tracker are separate problems. Oh And must solve both without requiring a maintenance habit. Normal use requires no project selection, tags, inbox clearing, streak, or daily planning ceremony. There may be a management view, but it is not the front door.

Three interactions define the product:

1. **Catch this:** voice or silent input, an honest durable-save acknowledgment, then return to the day. No unsolicited coaching during capture.
2. **Help me now:** retrieve a fragment, ask what is possible in volunteered context, or explore a thought. Suggestions remain proposals until adopted.
3. **Bring this back:** explicit reminders, bounded optional resurfacing, and later preparation for a session or event.

The durable memory belongs to the application, not a model provider's chat history. Source words and explicit user updates are authoritative. Categories, summaries, embeddings, and suggested steps are derived and replaceable.

## Platforms and architecture

Start with an installed iPhone app and native capture/system integrations. Evaluate Tauri 2 with Swift integrations against the actual phone flow before choosing it. If necessary, use a SwiftUI phone shell and consider Tauri for Mac. Keep the domain core independent of webview and framework runtime types.

Test the relevant entry points, such as a system control or Action button where supported, rather than promising a gesture before verifying it. Silent capture and accessibility are first-class. Capturing from a locked device and reading private stored content may have different authentication requirements. There is no always-listening requirement.

A Mac companion is required by M2. A hotkey should open directly to capture or retrieval. The Mac is another client, not a required always-on server. Web and messaging surfaces are optional and cannot own the data model.

Keep the architecture small: local durable storage, structured item state, a sync/outbox mechanism, asynchronous AI processing, deterministic reminder scheduling, and retrieval. These are responsibilities, not a mandate for separate services. Select storage/sync infrastructure for reliability and the processing policy rather than introducing routine maintenance.

Stable identifiers and idempotent operations prevent duplicates. Reprocessing must not overwrite corrections or resurrect completed or deleted items. Define conflict handling for concurrent edits; wall-clock last-write-wins is not sufficient justification for correctness. If a second client ships in M1, retry and minimum sync correctness ship with it.

Capture survives network and model failure. Distinguish **saved on this device**, **synchronized**, and **reminder scheduled**. A local save does not prove that a reminder exists on another device. Measure trigger-to-ready and end-of-input-to-save separately from speech duration and AI latency.

Use local scheduled notifications so delivery at a requested time does not require an AI call. Permission, Focus settings, power state, and human attention are separate limitations. Never claim that delivering a notification guarantees it was noticed.

## Intent without forms

Examples are synthetic:

| Input | Intended behavior |
| --- | --- |
| Maybe a roof garden would be nice | Preserve an idea, not an obligation. |
| I need to call the roofer | Record an undated action without another approval ceremony. |
| The roof quote expires Friday | Preserve dated information; do not invent a notification time. |
| Remind me Friday at 3 p.m. to call the roofer | Resolve and echo the date/time; schedule when unambiguous. |
| Bring this up in therapy | Preserve a private session topic, not a diagnosis or self-improvement task. |
| Done with the roofer call | M2 target: complete the clearly referenced item; clarify ambiguity. In M1 preserve this source without mutating another item; use the explicit completion control. |
| Maybe I should quit this project | Preserve exploratory thought; do not cancel a project. |

Broad intentions remain findable even without a well-formed next step. Clear natural-language intent remains usable without magic phrasing. Clarify only material ambiguity; approved defaults may reduce repeated questions.

An ambiguous reminder is saved with an honest **not scheduled yet** state and an optional correction path. It must not disappear or create a mandatory triage queue. The system cannot invent obligations, deadlines, completion, or cancellation.

## Resurfacing and return after absence

Explicit reminders and optional suggestions are separate mechanisms:

- Reminders follow the requested time and repeat policy. Silence never cancels, snoozes, or completes one.
- Suggestions have a separate, controllable budget. Begin with one small quiet daily prompt at a user-selected time as an experiment, not a compulsory morning ritual or fixed top-three list.

For undated actions, begin with transparent eligibility and rotation. Exclude completed, cancelled, snoozed, and pull-only items. Do not manufacture urgency. M2 can filter by volunteered context such as being at home with fifteen minutes available. Explain the selection rule on request. Model-estimated effort is only a suggestion.

“Not now” changes suggestion eligibility using an understandable cooldown. “Stop suggesting this” retains the record for retrieval. “Make it smaller” proposes an entry step without replacing the original intention. Nonresponse says nothing definitive about importance or completion.

After nonuse, the next capture works immediately. No catch-up wizard, overdue wall, accumulated digests, or backlog count. Optional prompting may back off under the configured policy; promised reminders remain separate. Prior reminder history is inspectable without flooding the welcome-back experience.

Define missed-reminder behavior explicitly. A one-shot reminder and a requested repeat-until-acknowledged reminder are different contracts. Do not invent escalation or treat delivery as completion.

## Privacy, ownership, and lifecycle

Configure processing boundaries before upload. A universal capture route must be safe for its most private permitted input. Neither a cloud classifier nor a fallible local classifier can serve as the sole disclosure gate. Avoid a privacy decision on every capture; configure policy during setup and make any separate routes obvious before use.

Specify the execution destination for transcription, embeddings, interpretation, generation, and review. Encryption at rest or in sync does not prevent an inference provider from seeing plaintext it processes. A personally controlled server is still remote from the phone; do not label it on-device.

No content leaves the device until a corresponding destination and policy are configured. An unavailable approved endpoint queues work or uses an explicitly approved fallback. Never silently switch private input to another vendor. Retrieval, summaries, and notification payloads must respect boundaries between work and private material. Lock-screen wording must not disclose private note contents.

Define source-text retention and bounded raw-audio retention. Basic export and deletion belong in M1. Deletion must cover records and derived indexes/caches, document the backup lifecycle, and survive restoration. Do not promise immediate erasure from immutable backups.

No autonomous outbound messages, appointments, or purchases in M1–M3. Conversation can prepare an action without executing it elsewhere. Development autonomy does not confer runtime authority.

## Pluggable AI

Domain behavior must be independent of provider protocols. Provider/model choice is configuration, not a rewrite. Initial targets are Anthropic, OpenAI, and a supported self-hosted endpoint, including a Spark-hosted deployment once its actual serving API is verified. Hardware or an “OpenAI-compatible” label alone does not establish compatibility.

A provider profile describes adapter/protocol, endpoint when needed, model, credential reference or authorized account connection, timeouts, capabilities, and permitted destinations. Export references rather than secrets. Define configuration-version behavior for queued and in-flight jobs so changes cannot silently reroute input.

Treat text interpretation, transcription, embeddings, and speech output as separate capabilities. A text-only endpoint remains useful with separate speech components. Real-time voice and elaborate routing are later options. Normalize errors, cancellation, streaming when supported, and validated structured output. Unsupported capabilities must be visible; invalid model output cannot corrupt authoritative state.

Changing providers must not migrate raw notes or rewrite intent. Record enough provider/model/version provenance to diagnose derived results. Embedding changes require an index rebuild or migration and a non-vector retrieval fallback; do not mix incompatible vectors.

Use supported provider authentication. A paid chat subscription does not automatically grant third-party API access. Verify any account-backed authorization flow against current provider documentation before promising it. Never embed a developer's service key in a public client or expose secrets to a webview, logs, exports, or normal synced configuration. Use native secure storage or an appropriate credential broker.

Contract tests must check semantics as well as protocol compatibility: preserved intent, no fabricated deadlines, source-grounded retrieval, unavailable endpoints, and invalid responses. Demonstrate configuration-based switching between at least two genuinely different backends in M1.

## Runtime adversarial review

Independent development review is required. A second runtime model is optional and must earn its complexity through evaluation.

The pipeline is durable raw save, proposed interpretation with source provenance, deterministic validation, any required review, then authoritative update. Saving never waits on review. Existing reminders operate independently of AI availability.

Deterministic checks cover schemas, identifiers, valid state transitions, time resolution, stale revisions, bounds, and routing. They do not prove meaning. Negated, quoted, hypothetical, or already-completed actions can contain valid evidence spans and parseable dates. Two models reading the same defective transcript cannot recover missing audio; test transcription failures separately and retain audio according to policy.

M1 uses contrastive offline evaluations and optional budgeted sampled shadow review inside approved processing boundaries. Shadow verdicts are diagnostic and cannot claim to have prevented an already-applied change. Timeout means unreviewed. Corrections remain authoritative.

M2 may introduce pre-commit review for specific classes only if measured benefit outweighs delay, cost, abstention, and reviewer-induced errors. Candidates include ambiguous changes to existing state and generated private summaries. Do not require two paid provider accounts or debate every tag.

Provide source, necessary context, policy, and the candidate change to the reviewer, without the interpreter's brand or rationale. Selected cases may use independent extraction before comparison. Different model families offer diversity but do not guarantee independence or truth.

Bound review to one round and at most one repair/recheck. On unresolved disagreement preserve the source and prior authoritative state, withholding only disputed mutations. Avoid a compulsory adjudication inbox. Blocking-review outage means pending and unapplied, never bypassed. Shadow annotations cannot unschedule reminders or create permissions.

Interpreter and reviewer destinations each require authorization. Review private summaries for source fidelity and coverage, not diagnosis or the validity of feelings. Extractive selections still risk omission and misleading context.

Evaluate negation, reported speech, hypothetical plans, completed work, corrections, timezones, retries, ambiguous references, unsupported summary claims/omissions, transcription errors, and captured instructions that try to override policy. Compare a single interpreter, same-family review, cross-family review, and where practical a stronger single model at similar cost. Measure actual errors prevented and introduced, false rejections, late/missed reminders, pending-state persistence, latency, and cost. Agreement rate and a second LLM's verdict are not sufficient ground truth. Use synthetic fixtures and explicitly authorized private examples.

## Three milestones

### M1 — Trust the capture and return loop

Scope: phone voice and silent capture, honest durable save, offline queue, a minimal reversible note/action/idea distinction, retrieval of original words and private notes since a requested date, correction/completion/stop-suggesting controls, explicit reminders, one optional proactive channel, privacy boundaries, basic export/deletion, and return after a lapse. A small Mac inlet is optional if it does not delay the phone trial. Full conversational coaching and additional context integrations wait.

Resolve the Tauri/native-shell feasibility decision using actual-device evidence: cold launch, locked/unlocked behavior, permissions, keyboard/accessibility, interrupted recording, offline save, local notifications with the UI closed, extension-to-app handoff where used, secure credentials, and reproducible signed builds. A webview test does not prove a native lock-screen flow.

Exit evidence:

- Actual-device trials covering offline save, interrupted processes, repeated transport, and a Mac-asleep scenario if applicable.
- No acknowledged capture lost or reminder duplicated in observed tests; no invented reminders.
- Measured trigger-to-ready and end-of-input-to-save latency, reported separately.
- Forced pipeline failure leaves save/sync/scheduling state honest and independently detectable.
- A two-week observation window with enough captures, retrievals, and resurfacing encounters to judge usefulness, allowing nonuse and a multi-day gap without cleanup.
- Configuration-based switching between two different supported AI backends without changing stored captures or domain rules. Verify the actual self-hosted API before claiming its support.
- Interpreter contracts, adversarial fixtures, standard-runner CI, and an open-source repository without private fixture data. Optional shadow-review instrumentation can land independently of the base trial; it remains disabled until its capability checks pass.

If people avoid capture or mute the only prompt, revise the loop before adding features. Cut Mac conveniences and sophisticated classification before weakening phone capture or the complete return/update loop. Temporary explicit markers are not proof of frictionless automatic classification. Two weeks proves initial feasibility, not sustained adoption; observation continues.

### M2 — Help remember and get started

Scope: source-linked private session preparation, Mac capture/retrieval, spoken conversational retrieval, exploratory conversation, useful grouping, volunteered context, and optional smaller-step suggestions. Add external context only for a demonstrated need. No mandatory tagging or end-of-session action review.

Exit evidence:

- Retrieve real past thoughts from vague wording with inspectable sources and honest not-found behavior.
- Use session preparation for an actual session and revise it from feedback.
- Mixed voice conversation does not convert speculation or assistant suggestions into obligations.
- Corrections and completions survive reprocessing and sync.
- Undated intentions resurface without repeated pressure; optional help can be muted while explicit reminders continue.
- Boundary tests prevent a private note from appearing in a work result or lock-screen payload.
- Provider authentication/capability limits and any selective blocking-review timeout, disagreement, stale-revision, and routing behavior are verified.

Judge usefulness and burden, not notification click rates. Extend provider speech/retrieval capabilities only as used, and complete the Mac surface using the framework evidence from M1.

### M3 — Keep working without becoming another project

Harden conflicts, device loss/replacement, backup/restore, export/deletion propagation, provider outages, changed notification permissions, upgrades, operational ownership, latency, and accessibility. Additional hardware support requires a demonstrated gap.

Exit evidence:

- A 30-day observation window without routine manual service maintenance.
- Device restoration preserves source records, corrections, completed items, and deletion policy.
- Interrupted sync and concurrent updates have tested, visible conflict behavior.
- Server/model failures neither lose captures nor silently disable installed reminders.
- A week away creates no maintenance backlog.
- Provider changes, credential rotation/revocation, index migrations, and portable configuration/export are verified.
- Another self-hosting user has reproducible setup instructions, and failure ownership/notification is defined without requiring a monitoring dashboard.

“Mostly baked” means a trusted daily loop across ordinary use, outages, constrained contexts, and lapses. It does not mean every integration exists or that reminders guarantee action.

## Hosted service: M4 or later

Keep M1–M3 single-user while preserving inexpensive boundaries: personal-space/owner identity, scoped storage/retrieval/jobs, per-owner provider profiles and credential references, and domain operations independent of transport. These seams are not proof of tenant isolation. Defer organizations, billing, invitations, marketplaces, and a multi-tenant control plane.

Record storage/sync, AI execution location, and whose credentials pay for inference are independent choices. BYOK does not prevent a host from seeing plaintext it processes. Encrypted sync and hosted inference have different access properties.

Endpoint reachability follows execution location. Do not require public exposure of a private inference server. Later options include device-executed processing, a secured private connection, or a user-controlled outbound worker. Offline capture can wait for an approved destination.

A hosted service needs verified isolation across records, indexes, jobs, notifications, and credentials; supported provider authentication; usage limits; consented routing; custom-endpoint network validation/isolation; and operational ownership.

## Delivery and unresolved implementation decisions

Apache-2.0 is the project license. Public source, publishable documentation, and synthetic fixtures belong here. Private design conversations and captured data do not.

Use Odonian for implementation: Haiku-sized build/design tasks, initial `haiku`, `escalate=true`, explicit independent Claude/Codex review, and `agent_merge=true` set at task creation. A separate merger lands approved work after required checks. There is no routine human design, task, milestone, merge, or release approval gate. Account-holder actions, credentials, and actual-device feedback remain necessary inputs. See [AGENTS.md](AGENTS.md).

Before dispatch, verify the live model allowlist, worker/reviewer/merger availability, forge access, and repository checks. Repair or decompose failures; do not weaken acceptance to pass review. Runtime review remains separate from development review.

Remaining implementation decisions include the concrete privacy/retention defaults, supported device/OS baseline and entry points, one initial optional prompting surface, storage/sync provider, self-hosted serving protocol, and the framework choice after feasibility tests. The coordinator resolves routine design choices through bounded investigation and records them; only genuinely missing user inputs need questions.

Use current primary platform documentation for implementation. Initial reference points: [Tauri architecture](https://v2.tauri.app/concept/architecture/), [Tauri mobile plugins](https://v2.tauri.app/develop/plugins/develop-mobile/), [Apple system controls](https://developer.apple.com/documentation/WidgetKit/Creating-controls-to-perform-actions-across-the-system), [Apple local notifications](https://developer.apple.com/documentation/usernotifications/scheduling-a-notification-locally-from-your-app), and [Odonian's execution contract](https://github.com/boldfield/odonian/blob/main/AGENT-API.md). Documentation establishes facilities; it does not replace device evidence.
