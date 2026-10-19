# M1 task refinement — 2026-10-07

This execution overlay replaces 25 task groups with 53 smaller implementation tasks. The other 55 never-started tasks retain their scope. Eighteen already completed, attempted, active or externally blocked groups were excluded. The effective M1 graph contains 126 tasks. This changes implementation granularity, not product scope or milestone exit criteria.

Addendum 2026-10-08: V08 was originally excluded as an externally blocked evidence task. After two rejected review rounds on its PR #60, it is now split into V08a (worker-executable server-side probe) and V08b (maintainer-supplied phone-context evidence). The original task `939fe62e` is retired; see the replacement map and the V08a/V08b slices below. Addendum 2026-10-08, later: P08 is split the same way into P08a (signing tooling, Linux-testable with stubbed Apple tools) and P08b (maintainer-produced signed build and device-install evidence on a Mac with Xcode). The original task `7d4836ee` is retired; P09, T05 and T10 now depend on P08b.

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

- C02a additionally waits for D04 and R01 because import must atomically create the item projection and index. B01a can build the binding pipeline after P01; runtime and production exports still wait for D05/V01.

- V08 conflated two evidence sources with different owners. Its server-side probe runs from the Odonian worker, which now has verified read access to the provider configuration file. Its phone-context reachability evidence can only come from the maintainer's iPhone, which reaches the Spark endpoint over Tailscale rather than a public address or the worker's network. V08a owns the probe tool, its tests and the Linux-context findings; V08b owns the phone-context section and the final verified status. V09 now depends on V08b, the last child, so the adapter still cannot start before both evidence sources exist. V08a must not be approved on the strength of a prose status: every published value comes from a committed sanitized artifact produced by the probe as submitted.

- B01b holds an ordered ownership allowance, granted by the coordinator on 2026-10-08 after PR #87 showed that criterion 2 cannot be met inside `core/src/ffi/` and `ios/OhAndCoreBridge/` alone: (1) in `ios/project.yml`, only the OhAndCoreBridge target's link, search-path, `SWIFT_INCLUDE_PATHS` and pre-build settings, and the OhAndTests target's `OHAND_RUST_OUTPUT_DIR`, `HEADER_SEARCH_PATHS` and `SWIFT_INCLUDE_PATHS`; (2) in `core/bindings/build-ios.sh`, a lock that serializes parallel Xcode invocations; (3) a handle/callback section in `docs/validation/core-binding.md`; (4) the new `ios/Tests/CoreBridge/` directory. Because P07 edits `ios/project.yml` concurrently, B01b now also waits for P07. The generator and `core/bindings/src/lib.rs` remain off-limits to B01b, and an allowance written inside an implementation PR is still not an allowance.

- B01c holds an ordered ownership allowance, granted by the coordinator on 2026-10-08 after PR #110 showed that criterion 2 (restart, duplicate capture ID and normalized failure on simulator) needs a simulator test next to B01b's: it may add and later maintain the single file `ios/Tests/CoreBridge/CaptureStatusTests.swift`. B01b has landed (PR #108), so no ordering conflict remains in that directory. Nothing else under `ios/Tests/`, `ios/project.yml` or `core/bindings/` is granted, and an allowance written inside an implementation PR is still not an allowance.

- I05's criterion 2 ("reminder candidates require explicit intent") was reviewed on PR #96 by probing one new withdrawal phrasing per round. After four rounds the breaker blocked the task with Opus approving and Codex producing a further phrasing each time. On 2026-10-08 the coordinator bounded the withdrawal grammar I05 must recognize, without weakening the criterion: a reminder request followed in the same capture by an unquoted, non-negated, non-infinitive withdrawal stays unscheduled, with the source record and non-reminder facets intact. Required constructions: (a) retraction verbs and idioms: forget, skip, cancel, delete, stop, never mind, scratch that, take back, change mind, remember on my own, don't bother, and bare "actually no" / "actually don't"; (b) a negated form of exactly these verbs: want, need, desire, wish (any inflection), when it either governs the reminder itself ("I don't want that reminder", "I don't need the reminder", "I don't desire that reminder") or carries a wish-ended marker (no longer, anymore, any more); no other verb is required under this clause; (c) needless predicates: not needed, not necessary, not required. Positive controls that must still schedule: complement infinitives ("don't want to miss it"), negated verbs governing a task object ("don't cancel the ladder order"), and unrelated negations ("it's not urgent"). The PR carries every listed construction as a durable regression. Reviewers verify criterion 2 against this list. A phrasing outside it, and the known over-rejections ("about cancelling the gym", "call the roofer and cancel the order", "take the ladder back", "so I don't need to worry"), are findings for I05b, not blockers for I05.

- Native-boundary exports are a standing ownership allowance, granted by the coordinator on 2026-10-09 after V05b blocked itself: the B01 slices built the binding mechanism and are done, but every later native task whose acceptance says "through production bindings" needs core exports that no task owns. Any implementation task whose acceptance requires a core operation not yet exported may, in the same PR and only when its owned paths cannot satisfy the criterion: (1) add one task-named module under `core/src/ffi/` (for V05b, `core/src/ffi/provider_transport.rs` with its tests) plus its single `pub mod` line in `core/src/ffi/mod.rs`; (2) add one task-named extension file under `ios/OhAndCoreBridge/` (for V05b, `CoreHandle+ProviderTransport.swift`) that exposes what the `OhAndServices` code needs; (3) nothing else under those paths: no edits to existing export files, other tasks' modules, `core/bindings/src/lib.rs` or the generator. Exports follow the B01 conventions (opaque handles, registered callbacks, normalized failures, generated header). A `pub mod` conflict with a concurrent task is resolved by rebasing; the merger bounces for rework on conflict. V05b additionally, once, may give the `OhAndServices` target in `ios/project.yml` the same `OHAND_RUST_OUTPUT_DIR`, `HEADER_SEARCH_PATHS`, `SWIFT_INCLUDE_PATHS` and link settings the `OhAndCoreBridge` target carries, and the `OhAndServices` dependency on `OhAndCoreBridge`; later native tasks inherit that. Design constraints for the V05b export, recorded from the block note: destination and `credential_ref` come from `resolve_execution_target` (`core/src/jobs/configuration/mod.rs`) and `authorize_job` (`core/src/privacy/routing/mod.rs`) at dispatch, never from the caller; the exchange runs off the core worker thread so provider calls cannot stall capture saves; cancellation is observable through the existing `CancelToken` (`core/src/providers/contracts/dispatch.rs`). Seeding: until U05a/U05b expose profile and route creation through the app, a real-core simulator test may seed `routes`, `route_authorizations`, `provider_profiles` and `jobs` by SQL against the migrated schema, the same way `core/tests/routing` and `core/tests/job_configuration` do in Rust, confined to one helper file under the task's test directory and run only after the core handle has applied migrations. That is a test fixture, not a production path.

- B02 additionally waits for N02 (recorded 2026-10-09 after B02 blocked itself): its composition integration test observes a scheduler request through actual service interfaces, so the native notification bridge must exist first. The board's `depends_on` for B02 had also been emptied and was restored from this graph.

- P08 conflated signing automation, which the Linux fleet can build and test against stubbed `security`, `xcodebuild` and `devicectl`, with a signed build installed on a physical iPhone, which only the maintainer can produce on a Mac with Xcode, an Apple Development identity and a device profile. Apple Developer Program enrollment completed on 2026-10-08; this Mac still has no Xcode, identity or profile. P08a owns the tooling, its behaviour tests and the signing document's procedure sections; P08b owns the evidence record and the validity/renewal statement. P09, T05 and T10 depend on P08b, the last child.

- The Xcode 16.4 pin in `ios/project.yml` and `.github/workflows/ios.yml` was the macos-15 runner image default when F03 and F05 ran; no rationale was recorded. It ships the iOS 18.5 SDK and cannot install on the maintainer's iPhone, which runs iOS 26.6.2, so it blocks P08b outright. New task F08 moves native CI to the `macos-26` image and Xcode 26.6, the newest version on GitHub-hosted runners as of 2026-10-08 (Xcode 27 is not yet on hosted images), and re-proves every probe under that SDK. F08 waits for P07, which edits `ios/project.yml`; B01b and P08a wait for F08. Bump again to Xcode 27 when the hosted image carries it.

- P09 scope amendment (coordinator, 2026-10-09, at the maintainer's direction): hardware keyboard navigation of the Tauri probe, step 4 of the device procedure in `docs/validation/tauri-handoff.md`, is deferred out of P09 to U10, which already owns keyboard and accessibility navigation of the real management UI with a documented actual-device pass. The maintainer has no Bluetooth or Smart Keyboard for the trial phone, and the probe is a feasibility shell rather than the UI that ships. The checks that were run are recorded in `docs/validation/evidence/device-feasibility/2026-10-09-device-matrix.json` and `2026-10-10-latency-accessibility.json`; each record lists its own not-tested and not-observable items, which remain unmet checks for P09 unless a later amendment defers them. P09's other acceptance criteria are unchanged. P09 is graded against its own pinned criteria (`docs/features/m1-plan.md#p09`); `docs/validation/m1-protocol.md` is T08's two-week trial rubric and lists app-level scenarios no probe can exercise, so it is not P09's coverage checklist.
- P09 scope amendment 2 (coordinator, 2026-10-10, at the maintainer's direction): the remaining not-tested and not-observable items in the two P09 records are deferred out of P09 to T05, the actual-device M1 functional exit matrix, which installs the production build on the same phone and already owns cold/locked/permission paths, offline recording/text, process interruption and reminder behaviour. Deferred: offline speech recognition with an immediate screen lock (the TranscriptionProbe fixture transcribes in about 0.3 s, so a lock cannot land inside the window; T05 runs the real app on real-length captures); capture before first unlock after reboot and system termination between control handoff and commit (P02); refusal under low free space; notification survival of Focus and summaries, Low Power Mode, reboot, a time-zone change after scheduling, provisional and ephemeral authorization, and reinstall or OS upgrade (P05); keychain behaviour after passcode removal (P11); and survival of the previous audio fixture after a background permission revoke (P04). These are added to T05's device matrix by name and remain unmet until T05 observes them; T05's own criteria are not lowered. P09 keeps every other criterion: the locked-capture check is now closed by the Face-ID-covered lock-screen press recorded in `2026-10-09-device-matrix.json` rev 3, and the P03 permission-denied screenshot is still a required artifact. P09's pinned criteria remain `docs/features/m1-plan.md#p09`.
- P09 scope amendment 3 (coordinator, 2026-10-10, under the maintainer's standing decision to defer remaining probe-matrix checks to the tasks that run the real app): two documented checks that PR #151 round 2 found on neither earlier list are deferred by name. (a) Shortcuts or Action-button entry latency on cold and warm launch (`docs/validation/capture-entry.md`, 'Needs a physical device', first item) goes to T05, which measures the production build on the same phone with the screen-recording method already used for Control Center entry; the P07 shortcut-URL records already show that entry path opening the app cold and warm, only its latency is unmeasured. (b) The VoiceOver live-status announcement of the Tauri probe summary when the count changes (`docs/validation/tauri-handoff.md` device procedure step 5) goes to U10, which owns VoiceOver behaviour of the real management UI and already holds the keyboard deferral from amendment 1; the summary's static reading is recorded in `2026-10-10-latency-accessibility.json`. Untimed voice Start and text-field trigger-to-ready are likewise T05's. Both items remain unmet until observed there; T05 and U10 are not lowered. P09 has no other undeferred unmet item: every remaining entry in its unmet-checks list is named in amendments 1, 2 or 3.
- P09 scope amendment 4 (coordinator, 2026-10-10, after PR #151 round 3; same standing decision): the complete enumeration of the device cells the six probe documents assign to P09 is now `docs/validation/evidence/device-feasibility/p09-probe-matrix-sweep.json`, 54 cells, each mapped to the record field that observes it, to the amendment that defers it, or marked not observable with the reason. It replaces the closing sentence of amendment 3, which was wrong: four documented cells were on no list. Deferred by this amendment: the on-device speech model install path, size and network need for a missing locale (`transcription-probe.md` line 68) to T07, which documents the install path in the setup documents, and to T05, which observes it on the phone (the M1 trial is en-US and that model was present); whether delivered notifications or repeating triggers consume pending slots (`notification-probe.md`) to T05 (the probe has no such mode, and N04a keeps fewer than 64 pending by design); the authorization decision changing in Settings while requests are pending, and clock, date-change and DST behaviour, to T05, naming what amendment 2 covered only in part; and the interruption duration measured against the moment the interruption began (`audio-probe.md` step 4) to T05. Recorded as not observable, not deferred: the P11 keychain cell before the first unlock after reboot (the probe cannot run then; V04's lock claim keeps depending on it) and a capture-entry file write after first unlock while locked (the control does not run while locked). Every other cell in the sweep is observed in a record field. T05, T07 and U10 are not lowered. P09 is complete when `docs/validation/device-feasibility.md` cites the sweep and its unmet list matches the sweep's deferred and not-observable cells.

- C05a holds an ordered ownership allowance, granted by the coordinator on 2026-10-10 after PR #150 rounds 1 and 2 showed that criterion 1 ("with provenance, original-audio identity") cannot be met inside `core/src/ingress/transcription/`: the item model has only `TextState::Original` and `TextState::Corrected`, so a machine transcript written as a text correction is projected as a user edit, and there is nowhere authorized to keep the audio digest, recognizer, language and confidence. C05a may, in this one PR: (1) in `core/src/domain/items/`, add a transcript event type distinct from `correction`, a `TextState::Transcribed { text, transcribed_at }` variant and the matching `StateTransition`, so that projection and provenance report the transcript as machine text, never as `text_corrected`; a later user correction still supersedes it and a transcript never overrides a correction; (2) in `core/src/store/schema/` and `core/tests/migrations/`, add schema version 7 with one table for transcript provenance keyed by item and source job (audio reference, audio SHA-256, recognizer, detected language, confidence, transcribed-at), with the migration tests this repository requires; (3) in `core/src/lifecycle/delete_intent/`, redact the transcript event text and delete that table's rows for the item, with a deletion test; (4) bind the attachment identity to item and source job (collision-safe event identity, verified on redelivery together with the audio identity), which is inside its owned paths. No other paths are added. L03a, L05a and U02b consume the new state and table through the existing read APIs and must not re-define them; L05a's reset fence must cover the new table. C05b is unchanged and still serializes after C05a.
- C05a allowance extension (coordinator, 2026-10-10, after PR #150 round 3): the grant above placed the transcript event type in `core/src/domain/items/`, but `EventType` lives in `core/src/store/events/`, and every reader of an item's effective text treats any `text` correction as the user's correction: `core/src/interpretation/dispatch/source.rs` and `core/src/ffi/provider_transport.rs` emit `TextBasis::Correction` with the correction record id, which the provider and interpretation contracts require to be a UUID, so the authorized interpretation that C05a enqueues cannot build a valid request; `core/src/retrieval/index/` reports the transcript as `corrected`. Both round-3 reviewers asked for the allowance to be extended rather than for machine text to keep masquerading as a correction. C05a may therefore also, in the same PR: (5) in `core/src/store/events/`, add `EventType::Transcript` with its own payload, persisted as an event and never as a `corrections` row; the transcript text and its record identity live in the v7 provenance table (the unlanded migration may gain a text column) or in the event payload, and the record identity is a UUID (a deterministic UUIDv5 over item id and source job id is acceptable and keeps redelivery idempotent and collision-safe; the workspace `uuid` crate may enable `v5`); (6) in `core/src/providers/contracts/request.rs` and `core/src/interpretation/contracts/`, add `TextBasis::Transcript { transcript_record_id, item_revision }` additively, with the same UUID validation and wire mirror as `Correction`, existing adapters, fakes, fixtures and callers compiling unchanged, and `fixtures/intent` and `docs/features/fixture-schema.md` untouched; (7) in `core/src/interpretation/dispatch/source.rs`, `core/src/ffi/provider_transport.rs` and their tests, use the transcript with the `Transcript` basis when the item has no user text correction; a user text correction still wins and a transcript never produces `TextBasis::Correction`; (8) in `core/src/retrieval/index/`, add a `Transcribed` basis and project correction, then transcript, then captured text. Required tests: an end-to-end attachment that loads the interpretation source, builds and validates the request without a UUID failure; the provider-transport basis; the retrieval basis and search over transcribed text; and the existing transcript-never-overrides-correction regressions. `ios/` is not granted: no Swift code switches on the basis kind. No other paths are added. XL06a (M2, `core/src/interpretation/dispatch/`) is gated on T08 and serializes after this.
- C05a allowance extension 2 (coordinator, 2026-10-10, after the Opus worker's block on extension 1): a new `TextBasis` variant is a compile-time change at every exhaustive match in the core, and extension 1 listed only the readers the reviewers had named. The complete list of non-test sites that match on or construct `TextBasis`, from `grep -rn 'TextBasis::' core/src` at main f67c4d1, is: `providers/contracts/request.rs`, `interpretation/contracts/mod.rs`, `interpretation/dispatch/source.rs`, `interpretation/apply/mod.rs` (basis resolution at `resolve_basis_text` and the `text_basis_kind` persistence match), `retrieval/index/mod.rs`, `ffi/provider_transport.rs`, `ffi/provider_transport/shadow_review.rs`, and `review/compare/source.rs`. C05a may therefore also, in the same PR: (9) in `core/src/interpretation/apply/`, resolve a `Transcript` basis against the item's current transcript record (stale if a user text correction exists or the record id differs) and persist `text_basis_kind = 'transcript'` with the record id, with apply tests that run an interpretation on a transcribed item end to end; (10) in `core/src/ffi/provider_transport/shadow_review.rs`, the same transcript fallback as the primary transport; (11) in `core/src/review/compare/source.rs`, the additive match arm only (M2 code, its tasks are gated on T08); (12) in `core/src/store/events/`, the effective-text query used for a text correction's old value may prefer the transcript over the captured text when no correction exists, so a user correcting a transcript records the transcript as the old value. Any further site that the new variant makes non-exhaustive may gain an additive arm, listed by path in the PR description; no behavioural change outside (5)-(12) is granted and `ios/` remains ungranted. The prior work is preserved at `origin/mr/7128e775`.

- N03 bounded interpretation and allowance (coordinator, 2026-10-11, after PR #166 was auto-blocked at round 2). Across PRs #162, #164 and #166, seven rejections each found a real AC1/AC2 defect next to the previous one: a lost acknowledgment expired as `TimeInPast`, two snapshot races, a past-due mismatched request, permanent failures retried, reinstalls bypassing the failure disposition, and now a pre-due mismatched request left live when its replacement fails. The criterion stands. This entry states it as invariants so that review measures the pass against a finite surface rather than one case at a time.
  - (1) Ownership allowance: in `core/src/reminders/state/mod.rs` (owned by N01, which has landed), only the additive `OperationState::Rejected` variant, its `as_str`/`FromStr` arms and the enum's doc comment. The only non-test match on `OperationState` outside `core/src/reminders/` is `core/src/lifecycle/delete_intent/mod.rs` (`unfinished_notification_operations`). It has a wildcard arm, and treating a rejected schedule as not blocking cleanup is correct, because `retire_generation` records a cancel for any generation that has a schedule operation, whatever its state. No edit is granted there. `retire_generation` stays untouched. A `Failed` or `Rejected` schedule left on a retired generation is accepted, because every N03 reader checks the current generation. N04 recovery and any diagnostics must scope by current generation the same way.
  - (2) AC1 "stale requests are removed" means **identifier agreement**. For every identifier the pass saw in the OS pending set, the pass ends having either left the request because it is still desired at exactly the desired instant, re-read after any effect as `roll_back_if_stale` does, or issued a cancel for it. A same-identifier request at a different or unreported instant is cancelled **before** any schedule decision for that entry. This holds before and after the due instant, for every operation state including `Rejected` (withheld), and whether the schedule that follows succeeds, fails or is withheld. Same-identifier `schedule` replacement must not be relied on to remove a mismatched request. A crash between that cancel and the reinstall leaves nothing pending, and the next pass reinstalls it. That gap is the accepted cost. A failed cancel goes in `failures` and is retried by the next pass, because the request is still pending and still mismatched.
  - (3) **Honest durable state** (already implemented, restated as the review surface): the disposition (acknowledged, attempted with unknown outcome, rejected, unconfirmed, expired) comes only from recorded attempts and effect outcomes. Absence from the pending set is never evidence of installation or non-installation.
  - (4) Bounds. A transient failure is retried by the next pass. Pass frequency (writes, launch and foreground through B03) is the backoff the contract's "retry with backoff" requires of N03; no timer-based backoff is required. A desired-state write committed on another connection after the pass's last per-entry re-read is closed by the next pass, not by N03. Triggering a pass after desired-state writes, and serializing overlapping passes, belong to composition (B02/B03). Explicit payload kinds beyond `generic` are N02/preview work.
  - (5) Round-3 regressions required in `core/tests/reminder_reconcile/`, each over a same-identifier request pending at a mismatched instant before the due time: a definitive replacement failure (the request is removed; the next pass withholds with no OS call and nothing is pending); a transient replacement failure (removed, `schedule_failed`, then retried); a never-attempted first install refused (removed); and an already `Rejected` entry (removed, with no schedule call). Also guard the `Acknowledged`→`Failed` update in `begin_schedule_attempt` with the same current-generation check as the reminder update. No other paths are added.

- P11a and P11b added (coordinator, 2026-10-11): the production app cannot yet be signed. `tools/apple-build/sign_probe.py` supports only application-only probe schemes (`SUPPORTED_SCHEME_BUNDLE_IDENTIFIERS`) and maps a single bundle identifier in `provisioningProfiles`. `OhAndApp` embeds the `OhAndCaptureControl` app extension (`com.boldfield.ohand.app.capture-control`), so its export needs a profile per bundle identifier (`docs/architecture/m1-shell.md`, Remediation 3; `docs/validation/signing.md`, Route). T10 must produce the signed candidate for T05, and T10 sits about twenty serial steps away, so the gap would surface only immediately before the trial. P11a extends the tooling now, and P11b records the maintainer's signed install of `OhAndApp` with the embedded extension. T10 now also depends on P11b. `OhAndApp` requests only `keychain-access-groups` under its own prefix, with no App Groups or restricted capabilities, so the maintainer's existing wildcard Development profile is expected to cover both bundle identifiers. P11a must still support a per-bundle profile mapping, because a later capability can require explicit App IDs. P11a co-owns `tools/apple-build/` with P08a, which is done, and does not touch `tools/apple-build/trial/`, which belongs to T06.

- UI chain restructured (coordinator, 2026-10-12, at the maintainer's direction). The original plan chained U02 through U08 in strict order, each waiting on its predecessor, with no shared-file reason recorded: each slice owns a distinct `app/` directory, and the contract assigns shell wiring to U01. Every link was checked against the slice specs. Kept as real couplings: U02a→U02b (search passes an opaque identifier to detail navigation); U02b→U03a (corrections are presented on source detail); U03a→U03b and U04a→U04b (same directory); U05a→U05b→U06a→U06b (onboarding selects configured provider profiles); U06b→U11; U08a→U08b. Removed as ordering-only links: U03b→U04a (U04a now depends on U02b and N06); U04b→U05a (now U01, V04, V06, V07); U06b→U07a (now U01, L03b, L04b, L05b); U07c→U08a (now U01, S03). U09 no longer serves as the "all UI done" gate and depends on U01, J03, D05, N04b and U11. The tasks that needed the whole UI now name every branch end: T09 depends on U03b, U04b, U07c, U08b, U09, U11, B03 and N07; U10 on U03b, U04b, U07c, U08b, U09 and C06; T03 on N06, S03, U09, U04b and U08b. The critical path drops from 30 to 20 tasks.
  - Shared host allowance (standing, for the tasks above and S04): the settings root and navigation in `app/` belong to U01, and the source-detail screen in `app/source-detail/` belongs to U02b. Each settings slice (U05a, U07a, U08a, U09, U11, S04) may add exactly one entry to U01's settings or navigation host for its own section. Each of U03a and U04a may add exactly one section entry to U02b's source-detail host. An entry is a single reference to a view in the task's own directory, in whatever form the host defines. Behaviour, layout and other entries in the host are not granted. A conflict with a concurrent slice's entry is resolved by rebasing, and the merger bounces the PR for rework on conflict. Host owners should keep their section lists one entry per line so that concurrent additions merge cleanly.
  - **Source-detail section models (coordinator, 2026-10-19, after U04a's PR #300 was blocked in round 2).** A section on source detail needs its model supplied by the host, and nothing in `app/` lets a section get one without touching `SourceDetailModel`. No environment key carries the composition root, and `ios/AppAssembly/` is not owned by either task. So each of U03a and U04a may also make, in `app/source-detail/SourceDetailModel.swift`:
    - one optional stored property holding its own section model (U03a: `itemControls: ItemControlsModel?`; U04a: `reminderEditor: ReminderEditModel?`);
    - its construction in `assembled(itemID:root:)`. The construction may pass a closure that calls the host's existing `load()` after the section commits a change, so the detail refreshes.

    Nothing else in the host is granted: no new loading, state, reads or behaviour, and no change to other properties. Both tasks reshape `assembled` the same way (`let model = …; model.<section> = …; return model`), so whichever lands second rebases and keeps both lines.

- Test-target registration allowance (coordinator, 2026-10-12, after PR #178 round 2 blocked T02a on ownership alone). The verification tasks own top-level test directories that Cargo does not discover: it finds integration tests only in a package's own `tests/`, the root workspace lists its members explicitly, and a separate workspace would not run under `make test`. So a Rust suite in an owned directory can only run in `make test` and CI through a `[[test]]` entry in `core/Cargo.toml`. Standing allowance: each of T02a, T02b, T03, T04a and T04b may add exactly one `[[test]]` entry to `core/Cargo.toml`, containing only `name` and a `path` inside its own owned directory (for example, `name = "capture_processing_resilience"`, `path = "../tests/resilience/capture-processing/main.rs"`). Tasks that share a directory share one entry: T02b reuses T02a's entry and T04b reuses T04a's, and the later sibling only adds modules under the shared directory. If the earlier sibling has not landed, the first to land adds the entry and the other rebases. No other change to `core/Cargo.toml`, the root `Cargo.toml` or `Cargo.lock` is covered, including new dependencies, dev-dependencies, features or `harness = false`. A suite that needs one of those must block and name it. A suite that is not Rust (Swift or simulator tests) is not covered by this allowance.

- P11a criterion 2 amended (coordinator, 2026-10-12, after PR #177 rounds 1–2). The P11a slice contradicted itself: criterion 1 takes the signed bundle set from the unsigned archive, while criterion 2 required refusing an uncovered bundle "before any build". Meeting both meant predicting the archive's bundles from `ios/project.yml` before building. Every round then found another project-spec construct (dependency kinds, build phases, copy phases, Info.plist literals, transitive targets) through which that prediction could differ from the archive. Bundle coverage is now decided from the archive itself, so criterion 2 reads "before any keychain, profile-installation, signing or export work". The unsigned archive build may precede the refusal, because it uses no identity or profile and signs nothing. The set of refusals is unchanged: uncovered, expired, Xcode-managed and target-device-missing profiles still refuse with a distinct exit, naming only the bundle identifier. Only their position in the run moves, so acceptance is not lowered. Review surface:
  - (1) **Coverage source.** The coverage decision uses the bundles enumerated from the unsigned archive: the application plus every nested `.app`, `.appex` and `.xpc` at any depth, including inside frameworks. A bundle without a readable `CFBundleIdentifier`, a duplicate identifier, or any `.xpc` (the tool does not sign XPC services) is refused. Enumeration fails closed: an unreadable `Info.plist` refuses and is never skipped. `.framework` and resource bundles take no profile.
  - (2) **Ordering.** No identity import, temporary keychain creation, keychain search-list change, profile installation, export, signing or device step happens before that decision. Checks that depend only on the profile (expired, Xcode-managed, device not listed) may also run before the build. Keychain cleanup still runs on every exit path.
  - (3) **No spec parser on the review surface.** A project-spec prediction of the bundle set is no longer required. Any such prediction kept in the tool is advisory. It may produce an earlier, friendlier refusal, but it never allows a bundle the archive check would refuse. Its completeness against XcodeGen constructs is not a review criterion. Removing it is preferred.
  - (4) The behaviour tests in criterion 4 then drive the uncovered-extension, expired and Xcode-managed cases through a stub archive containing the offending bundle. They assert that no `security` keychain or import call and no `-exportArchive` call was made.

- F09 added (coordinator, 2026-10-12, from the first P11b device run). The signed `OhAndApp` build exported, installed and launched on the maintainer's iPhone with the extension embedded, then dyld killed it at launch: `Library not loaded: @rpath/OhAndCoreBridge.framework/OhAndCoreBridge`, referenced from `OhAnd.app/Frameworks/OhAndServices.framework`. In `ios/project.yml`, `OhAndApp` depends on `OhAndServices`, and `OhAndServices` depends on `OhAndCoreBridge`. XcodeGen embeds only an application's direct framework dependencies, so `OhAndCoreBridge` is linked but never copied into the app. CI never saw this: `OhAndTests` runs hosted in `OhAnd.app` under `xcodebuild test`, which puts the build-products directory on the framework search path, and no CI job launches `OhAndApp` on its own. F09 embeds the framework and adds the missing launch smoke test. P11b now waits for F09. The maintainer reruns the P11b procedure after F09 lands.

- S02 bounded interpretation (coordinator, 2026-10-12, after PR #197 round 1). Across PRs #188, #193 and #197, fourteen rejections each found a real path to the same AC2 defect: a prompt that already fired was replayed after a backward clock change. The paths were a refresh rebuild, app open as the first look, a multi-day jump, capacity full at the first look, a time change before any reconcile, and now disable/re-enable. They are all one root cause. Evidence that a date's prompt may have been delivered lives on mutable plan entries (`PlannedPrompt.installed`), and every operation that rebuilds or empties the plan before settling that evidence destroys it. Patching each path separately does not converge. The criterion is unchanged and is now stated as one invariant with a fixed review surface:
  - (1) **Spent dates are a ledger, not plan state.** `PromptSchedule` keeps an append-only set of spent local dates and a persisted wall-clock high-water mark (the latest `now` any entry point has observed). It keeps them separately from the plan. No operation removes a date from the ledger: configure (including Off), time or zone change, refresh, rebuild, a capacity pass or re-enable. The only removal is age pruning of dates more than 14 days before the high-water date. `fill` never plans a spent date, and never plans an instant at or before `max(now, high-water)`.
  - (2) **What spends a date.** A date is spent when core has installed or attempted its prompt (outcome known or unknown) and any of these holds: (a) the high-water mark has reached the prompt's instant; (b) the request is absent from an OS pending listing although core did not cancel it; (c) core cancels it, or empties the plan containing it, while it has not been seen still pending in that same pass. Case (c) includes disable. **Ambiguity fails toward suppression.** Skipping a date's prompt when delivery evidence is ambiguous is correct and is not a defect. Ambiguity arises after a rollback, on disable, or after unverified absence. Replay is the only AC2 violation.
  - (3) **What does not spend a date.** Core cancelled the request for capacity or reconfiguration after seeing it still pending in the OS listing of the same pass. A request the OS still holds has not fired. A displaced prompt therefore comes back when capacity frees, which keeps the PR #188 findings fixed. The high-water mark alone is not proof of non-delivery, because a prompt can fire while core is not observing and the clock can then be rolled back (PR #193, finding 1). In the ordinary path (no clock rollback, no disable, no capacity pressure), every chosen day gets its prompt. A skip there is a defect.
  - (4) **One chokepoint.** Every public entry point that takes `now` (`configure`, `refresh`, `record_capacity_pass`, `reconcile_prompts`, `cancel_prompts`) first raises the high-water mark and settles spent evidence from the current plan into the ledger. Only then does it rebuild, cancel or discard anything. A review finding that names a path into the plan that bypasses this settle step is in scope. A finding that needs a rollback larger than the 14-day retention is out of scope.
  - (5) **Review surface.** A property-style test drives random interleavings of configure (time change, Off, On), refresh triggers, clock jumps forward and back (within 14 days), zone changes, capacity passes and OS fire or vanish events. It asserts that no local date's prompt identifier is installed after that date was spent, and that the ordinary path installs every day. It can be a seeded deterministic generator; no new dependency is needed. The existing named regressions stay. Further rejections must show a concrete interleaving that breaks (1)–(4), not a new phrasing of rollback.
  - (6) **Error-path skips** (added 2026-10-13, after PR #197 round 4). When an entry point returns an error after some native effects have already succeeded, the evidence for those effects can be lost. A date can then be skipped on the next pass, for example a successful cancel whose release is never recorded. A skip that follows a returned error is suppression, not a defect, because AC2 forbids only replay. "Ordinary path" in (3) means a path on which no native call or store operation fails. S02 must still record provenance it already holds before its next fallible call. Concretely, `capacity_pass` releases the successful `cancel_where` cancels before it calls `reconcile_with_capacity`. S02 does not change `reconcile_with_capacity` (owned by the reminder capacity code) to return partial results on error. A cancel that function made before failing may skip its date. A finding that an error path causes a skip is non-blocking. A finding that an error path causes a replay is blocking.

- L05a fence interpretation and allowance (coordinator, 2026-10-12, after L05a blocked itself on main b52dc35). Criterion 1 says every core result/apply path "uses the existing fence hook", but no reset fence hook exists. What does exist is the per-item deletion guard. `mark_deletion_intent` tombstones an item, cancels its jobs and reminders and enqueues cleanup. Every apply path already rejects work for a `deleted` item. That covers job results, corrections, transcript attachments (C05a), interpretation applies and reminder effects. Ingress import of an already-imported capture returns `ItemDeleted` (`core/src/ingress/import.rs`, `find_existing_item`). **That per-item guard is the fence hook criterion 1 refers to.** L05a composes it under a durable generation. It adds no generation check to the jobs, interpretation, transcription, captures, reminders or ingress paths.
  - (1) **Allowance.** In `core/src/store/schema/mod.rs`, only one appended migration step (v8 → v9) creating a reset table, with its shape registered for `validate_schema_shape`. It has a migration test in `core/tests/migrations/`. In `core/src/lifecycle/delete_intent/` (owned by L01, which has landed), only an additive crate-visible helper that reset uses to tombstone an item at its current revision, with the same effects as `mark_deletion_intent`. Existing behaviour and signatures stay unchanged. Nothing else outside L05a's owned paths is covered.
  - (2) **Generation boundary.** Starting reset records, in one transaction, a new generation, its `pending` phase and the boundary `max(items.rowid)`. Reset, and any retry after a crash, tombstones only live items at or below that boundary, resumably and idempotently. An item created after reset started is above the boundary, so it belongs to the fresh generation and no retry deletes it. The boundary uses no clock comparison. Reset moves to `completed` only when every tombstoned item's required cleanup work is acknowledged (criterion 2). A second reset while one is pending resumes the pending generation and does not start another.
  - (3) **Review surface for criterion 1.** Process-restart tests over populated mixed-route records, jobs, transcripts, reminders and indexes show that each late result arriving after reset has started is rejected by the existing guard and revives nothing: a job result, a correction, a transcript attachment, an interpretation apply, a reminder effect, and a re-import of a pre-reset capture ID. If such a test finds an apply path that does not reject for a tombstoned item, that is a defect in the path's owner. L05a blocks and names the path. It does not patch the path.
  - (4) **Native staging belongs to L05b.** A capture staged natively before the reset tap but not yet imported has no item, so no tombstone can fence it. L05b must import the native ingress staging before it calls core's reset start. It must also purge any staged record written before the tap that is still unimported after that drain. Both are required cleanup acknowledgments. A capture made after the tap is fresh-generation content and must not be purged.

- F10 added (coordinator, 2026-10-12, at the maintainer's direction). On 2026-10-12 the `simulator` job in `ios.yml` failed five times on UI tests and passed on rerun with no code change. These failures blocked P11b once and cost F09 and S02 a rerun each. All of them are XCTest waits timing out on a simulator that `OhAndTests` had already used. F10 finds the cause of each failure mode and fixes it, without lowering what the tests check.

- F11 added (coordinator, 2026-10-13). On F10's head `6ab5caa`, attempt 3 of run 38361190686 failed in `OhAndTests`: `NotificationActionCoreIntegrationTests.testCompleteStoredBeforeRelaunchRunsAfterTheNextUnlock`, where `sqlite3_prepare_v2` returned 5 (`SQLITE_BUSY`) at line 71. The test reads the store through its own SQLite connection while the core still holds a write lock. The core sets a 5 s busy timeout on its own connections (`core/src/store/schema/mod.rs:115`), but 12 native test files open direct connections without one. F11 gives every one of those direct connections a busy timeout. It depends on B02, whose PR edits one of those files.

- F12 added (coordinator, 2026-10-13). On F10's head `1b04f01` (run 38462101769, attempt 1, job 115519616876), the `OhAndTests` host process stopped in `NotificationBridgeBoundednessTests.testAScheduleThatFailsWhileAnEarlierRestoreIsSuspendedKeepsTheRecordedRequest`. The test started at 20:32:26 and printed nothing until "Restarting after unexpected exit, crash, or test timeout" at 20:33:04. The test uses a 50 ms effect timeout and test gates. Whether this is a test race or a bridge defect is unknown. F12 diagnoses it and fixes the cause.
- F10 probe-source allowance (coordinator, 2026-10-14). In round 2, Codex rejected PR #201 head `1b04f01` because it changed `ios/NotificationProbe/Sources/ProbeScenarios.swift`, which F10 did not own. That finding was correct under the overlay as written. The worker then reverted the change in `b638a56`. But the change is F10's fix for the fourth failure mode it found, "foreground delivery never reached the delegate". A 2 s one-shot interval trigger can be in the past by the time a slow `add()` returns on a loaded host, and the system then drops it silently. The cause is in the probe app's scenario, not in the UI test, so F10 cannot fix it inside its original paths. The revert puts a known failure mode back into the streak that is supposed to prove it gone. The coordinator therefore grants `ios/NotificationProbe/Sources/ProbeScenarios.swift`, limited to the foreground-delivery scenario's trigger interval, its early-exit wait window and the fields it reports (`addMilliseconds`, `intervalSeconds`). The grant also covers the `docs/validation/notification-probe.md` rows that describe steps F10 changes. Nothing else in the probe app is granted. The streak in criterion 3 is counted on the head that includes the restored change.

- L05b credential-claim fence and allowance (coordinator, 2026-10-13, after PR #206 round 2). Both reviewers found the same race. `removeCredentialsIfChosen` takes one `profileCredentials` snapshot, awaits the profile revocations, and only then deletes Keychain items. A profile version created in that window that keeps a bound reference without re-entering the secret is missing from the snapshot, so it loses its credential. No production profile-write path exists yet: U05a and U05b add it. Criterion 2 must still hold by construction, so the fence lives in storage, not in a future caller's discipline.
  - (1) **Allowance.** In `core/src/store/schema/mod.rs`, only one appended migration step (v9 → v10). It creates `reset_credential_claims(credential_ref TEXT PRIMARY KEY, generation INTEGER NOT NULL REFERENCES reset_generations(generation))` and two triggers, `BEFORE INSERT ON provider_profiles` and `BEFORE UPDATE OF credential_ref ON provider_profiles`. Each does `RAISE(ABORT, 'credential_pending_removal')` when the new row's `credential_ref` is claimed. `validate_schema_shape` already covers triggers. The step gets a migration test in `core/tests/migrations/`. Core functions to claim, list and release claims go in `core/src/ffi/reset.rs` (already in the PR), with their Swift bridge. Nothing else outside L05b's owned paths is covered.
  - (2) **Protocol.**
    - (a) **Claim.** One core transaction reads the current profiles and inserts a claim for each bound secret reference that no unbound profile holds. References already held by a later profile are left unclaimed and are never deleted.
    - (b) Revoke the bound profiles.
    - (c) Delete the Keychain item for each **claimed** reference, conditional on its tap identity as now.
    - (d) Release the claims in one transaction, together with clearing the removal request, only after every deletion is acknowledged.
    - Resume after a crash or a failure uses the persisted claim set as the authority, not a new snapshot. A profile write that reuses a claimed reference is refused by storage with `credential_pending_removal` until (d).
  - (3) **Review surface.**
    - A real-core test inserts a later profile that reuses a reference after the claim returns and before `deleteCredential`, and asserts the insert is refused and nothing else changes.
    - A real-core test shows that a profile inserted before the claim keeps its reference unclaimed and its secret.
    - The migration test covers both triggers.
    - Out of scope: how U05a and U05b present the refusal (re-enter the secret under a new reference, or retry after reset), and profile-write UX.
  - (4) **Binding precedes the tap boundary** (added 2026-10-14, after PR #206 round 3, where both reviewers found it independently). `reset()` used to call `staging.snapshot()`, which sets the tap and ingress boundary, and only after further awaits bind the removal with `core.profileCredentials()` and the `secretIdentity` reads. A profile written in that window became "bound" and was revoked, and its reference was claimed and its secret deleted. A secret re-entered in that window was bound under its new identity and deleted. The fix:
    - Bind the removal, with both the profile read and the identity reads, before `staging.snapshot()`.
    - A profile or secret written between binding and the boundary is then later state. The claim step already leaves it alone: its reference is held by an unbound profile, or its identity no longer matches. That errs toward keeping data.
    - A bound profile or secret that disappears in that window needs nothing special, because revoke and the identity-conditional delete are already no-ops for it.
    - A real-core regression has a deterministic hook between binding and the snapshot. The hook inserts a same-reference profile and re-enters the secret. The test asserts that both survive and that the tap-time profile is still revoked.
    - No other change to the boundary or to ingress staging is authorized.
- T11 prerequisites (coordinator, 2026-10-13, after T11 blocked itself on main 67f85fd). The core FFI never dispatches the self-hosted protocol. `core/src/ffi/provider_transport.rs:692` and `:761` and `shadow_review.rs:535` return `unsupported_protocol`, and `core/src/ffi/job_runner.rs:776` registers only the Anthropic and OpenAI adapters. Also, `ServiceCompositionRoot` never calls `ProviderExchangeCoordinator.attach`, so a module cannot dispatch a shadow review. Both gaps are outside T11's owned paths. T11a (core) and T11b (root seam) close them, and T11 now depends on both.

- U01 prerequisites and allowance (coordinator, 2026-10-14, after PR #212 round 1). Both reviewers accepted U01's criterion 1, the capture-first shell. Both rejected criterion 2: the release app does not resolve real services. U01 blocked itself, correctly, because none of the three gaps can be closed inside `app/` and `ios/AppAssembly/`.
  - (1) **Notification actions.** No production code constructs `ActionHandler` or `NotificationEventReceiver`. `ActionHandler` is internal to `OhAndServices`, the root's `CoreHandle` is not exposed (B02 rule), and `CompositionEnvironment.notificationEvents` defaults to `{ _ in }`. So reminder Acknowledge and Complete, delivered and opened facts, and taps never reach the core, and journaled callbacks are never reconciled.
  - (2) **Fresh-install route.** Nothing in production writes the `routes` table (`core/src/privacy/routing/mod.rs:11`: "A fresh install has no routes or grants"). `require_route` (`core/src/ingress/import.rs:320`) rejects the configured `universal` route as unknown, so every fresh-install save stays staged. Every earlier test seeded routes by SQL under the native-boundary export allowance. This gap belongs to the plan, not to U01.
  - (3) **Voice.** The root exposes only `saveText`. `IngressAudioHandoff` and `recoverStagedIngress` are internal, so the release app cannot give `VoiceRecordingController` a durable handoff or recovery receiver, and injects `VoiceRecorderUnavailable`.
  - **New tasks.** U01a (core: provision the local-only capture route), U01b (root: route at open plus notification-action composition) and U01c (root: voice ingress seam). U01b and U01c serialize on `CompositionRoot.swift` after T11b. U01 now depends on all three.
  - **U01 allowance.**
    - (a) `ios/Tests/AppAssembly/` for U01's own tests.
    - (b) In `ios/Tests/ProjectSmoke/ProjectSmokeTests.swift`, only the `ContentView` assertion that U01's shell replaces. Any other assertion is out.
    - (c) In `ios/project.yml`, exactly one `OhAndApp` sources entry, for the repository's `app/` directory: `optional: true`, `createIntermediateGroups: true`, placeholders excluded. Without it no target compiles the `app/<slice>/` directories the UI slices own. If `ios/scripts/check_project_config.py` rejects the entry, U01 blocks and names the check. It does not edit the check.
  - **Still owed in U01.** Retry a failed launch from `sceneWillEnterForeground` and `willConnectTo` when no services exist. `launch()` is idempotent. A test covers it (Opus round 1, finding 2).
- B05 prerequisites: the native daily-prompt pipeline (coordinator, 2026-10-14, after PR #213 round 1). Both reviewers rejected B05's Swift `DailyPromptSchedule`. It is an in-memory copy of S02's `PromptSchedule` that diverges from it:
  - Opus: it replays a prompt delivered while the app was not observing, after the clock is set back.
  - Opus: it replans the same local date after a relaunch and westward travel.
  - GPT: it spends a date that core never attempted.
  - GPT: no production path installs a prompt at all.

  B05 blocked itself, correctly. S02 and S03 built the core schedule and coordinator, but nothing exposes `PromptSchedule` through the FFI, nothing persists it (`reconcile_prompts` says "persist it after every pass"), and nothing native installs prompts (`NotificationPermissionModels.swift:163`: "Installing them belongs to the prompt scheduler"; `LifecycleCoordinator.swift:103`: "the core's daily prompt schedule is not exposed through the bridge"). No task owned this. Without it, optional daily prompts never fire in M1, whatever U08a configures.
  - **New tasks.**
    - **S05a (core):** the core persists the schedule and owns it end to end. Native code supplies only the time zone, the permission state and the triggers.
    - **S05b (native):** a prompt service driven by the lifecycle, connected through the root.
    - **B05** then depends on S05b and drops its Swift copy. Its prompt half becomes "forward clock and zone changes to the core schedule and run a pass", tested against the real core. Its reminder half (PR #213: debouncing, absolute instants, `TimeChangeRealCoreTests`) was sound and is kept.
  - **S05a allowance.**
    - (1) One appended migration step in `core/src/store/schema/mod.rs`, the next version after v10. It creates a single-row `prompt_schedule` table holding the serialized `PromptSchedule` and `updated_at`, and it gets a migration test in `core/tests/migrations/`.
    - (2) In `core/src/ffi/notification_reconciliation.rs`, only what makes the reminder pass take its prompt demands from the persisted schedule through S02's `reconcile_prompts`, and persist the schedule in the same call. Native-supplied prompt candidates stop being a source of installs.

    - (3) **Native effect path**, added 2026-10-14 after PR #219 round 1, where both reviewers found that the native effect handler refuses every prompt command the core now issues.
      - The files: `NotificationIdentifier`/`NotificationModels.swift`, `NotificationBridge.swift`, `NotificationEventIngestor.swift`, `ReminderNativeEffects.swift` and `CoreReminderReconciler.swift`, with their tests under `ios/Tests/Notifications/` and `ios/Tests/NotificationPermission/`.
      - What it covers: accepting core-issued `prompt:<local date>` identifiers as a separate prompt identifier kind. That kind must be schedulable and cancellable, and it must appear in the pending and delivered listings the core reads. Today `NotificationBridge.swift:173` drops non-reminder identifiers from the pending listing, so the core would treat an installed prompt as gone and spend its date.
      - Reminder identifier validation stays exactly as strict.
      - A delivered or tapped prompt is recognized as a prompt event and is neither journaled as a reminder action nor reported as invalid. Routing a prompt tap to the S03 activation belongs to U08b.
      - The real-core test must assert that the granted prompts are in the fake centre, that the pass converges and that no scheduling failure is recorded.

    Nothing else outside S05a's paths is covered.
- T11 prerequisites, round 2 (coordinator, 2026-10-14, after PR #216 round 1). Both reviewers found that T11's self-hosted "registration" is inert. `OptionalCapabilitiesModule.selfHosted` has no consumer. Because T11a registered `SelfHosted` unconditionally in the job runner, a root without the module still dispatches a stored `supported` self-hosted profile (the PR's own test shows it). So "absent capability stays disabled" fails. T11 blocked itself, correctly: no module seam can switch a provider protocol on or off.
  - The coordinator chose a core-enforced gate over redefining the stored profile as the registration. T05 requires a capability to be absent or disabled for the base trial until its own canaries pass. That has to be a build-level decision the core enforces, not a property of user data that U05 may later write.
  - **New tasks.**
    - **T11c (core):** the host declares which provider protocols may dispatch. Self-hosted is off by default. T11c also adds a typed provider-profile status read.
    - **T11d (root):** modules declare enabled protocols, the root passes the union to the core at assembly, and modules get the status read as a typed client.
  - **Serialization.** The root edits are chained on `CompositionRoot.swift`: U01b → U01c → S05b → T11d. U01 is the critical path, B05 comes next, and T11 is off the M1 critical path. T11 now depends on T11c and T11d.
- C04c added (coordinator, 2026-10-14, after U01c PR #218 round 1). Opus approved U01c. GPT rejected it, correctly in substance. The root's `CompositionRecordingOutcome.audioUnavailable` (a record is staged but the audio is missing or unreadable) has no honest mapping into C04's voice contract. `VoiceHandoffResult` has only `saved`, `keptForRetry` (asserts the audio is durable), `notStaged` (asserts the audio remains) and `itemDeleted`, and `VoiceRecordingController.outcome(for:)` turns the first two into `keptForRetry`. The root seam itself is correct and meets U01c's acceptance. The gap is in the consumer contract, so C04c owns it. U01c lands as is, and U01 depends on C04c so its adapter can map honestly. `VoiceCaptureOutcome.unrecoverable(captureID:end:fileRetained:)` already exists.
- F13 and F14 added (coordinator, 2026-10-14). These are intermittent failures that reviewers saw and that no task owns. F13: `ProviderExchangeBoundaryTests.testUntrustedCertificateIsRefusedAndDiagnosticsStayProtected` got no final event within 10 s (run 38534729949, attempt 1, job 115754735518, `:357`). F14: the `tools/bench` test `fake_run_with_an_unverified_self_hosted_arm_skips_every_case_and_sends_nothing` failed once in `make test` (seen by the Opus reviewer on L05b, 2026-10-13).
- V06 and V07 now depend on V01 alone: they implement Rust adapters against its transport interface using fixtures. V05 is the native effect implementation and remains required by app composition/integration, not by independent protocol adapter implementation. This is an explicit dependency override for those retained tasks.
- Tier-2 prerequisites added (coordinator, 2026-10-14, at the maintainer's direction, before U02a, U05a, U05b and U06a were claimed). Checking those four against main at 164f722 found five gaps that no task owns:
  - **Nothing queues interpretation for a saved capture.** `import_foreground_ingress` (`core/src/ingress/import.rs:147`) stores the capture, item and index row and queues no job. The only production enqueue of an `interpret` job is the transcription attachment (`core/src/ingress/transcription/mod.rs:952`), and only when the native caller supplies an `InterpretationRequest`; the root never does (`TranscriptionCapability` defaults to `{ _ in nil }`). So no capture from the app is interpreted, and even the offline fast path (which runs only inside an interpret job) never sees it. I08 owns this.
  - **No production code writes a provider profile, a default or a route grant.** `INSERT INTO provider_profiles` and `INSERT INTO route_authorizations` appear only in tests. No default-profile concept exists. U05a's "existing versioned profile service" does not exist. V01b owns profiles and defaults; V02b owns route grants.
  - **No connection check exists** for U05b's "explicitly authorized connection test". V01c owns it.
  - **Retrieval is not exported.** `scoped_query`/`scoped_list` (`core/src/retrieval/query/mod.rs:76`, `:97`) have no FFI export, and `PrivateReadGate` admits only store check, get capture and item status. R04 owns the export.
  - **The root offers none of these to `app/` modules** (B02 rule: modules get typed clients only through `CompositionCoreClients`/`CompositionPrivateReads`). U01d adds the gated retrieval reads; U01e adds the configuration clients and the credential service. Both join the root-file chain after T11d (U01b → U01c → S05b → T11d → U01d → U01e), so S05b and T11d are not disturbed.
  - I08 amended after round 1 (PR #225): criteria 2 and 4 as first written were coordinator errors. The request version must be a UUID, and a no-profile non-match waits for configuration (`unprocessed`, `AwaitingConfiguration`) rather than ending `uninterpreted`. Both now name the contract they follow, criterion 4 asserts more than before, and test-fixture adaptations for the auto-queued job are granted.
  - Dependency edits: U02a also depends on U01d; U05a on U01e; U06a on U01e and U02a (its criterion 1 retrieves before provider setup, which needs U02a's search).
- F15 and F16 added (coordinator, 2026-10-15). Two intermittent failures that no task owns. Both rejected otherwise-approved work.
  - F15: memory-safety failures in the `notification_reconciliation` FFI tests on Linux reviewer pods. (a) F14 round 2, merged with `d3dafb3`: `answers_that_do_not_fit_the_command_are_refused_and_the_command_waits_for_a_valid_one` failed, then the `ohand_core` test process died with SIGSEGV. (b) S05b round 1, merged with `fefb034`: the lib test binary aborted with glibc `corrupted size vs. prev_size in fastbins` during `optional_prompts_use_the_slots_left_and_are_withheld_without_permission`, with no Rust change in the PR. The coordinator ran 8 consecutive full `cargo test --lib` runs on `d3dafb3` on macOS, all clean, so the failure is load- or platform-dependent. One confirmed hazard: the tests' `Session` has no `Drop`, so a panicking assertion skips `close()` and leaves the core handle open with the native-effects callback registered against a freed context. That explains (a)'s SIGSEGV after a failure, but not (b), which had no reported failure first.
  - F16: `NotificationProbeAuthorizedUITests.testAuthorizedSchedulingBehavior`, foreground-delivery activity (`:71`): "add took 99 ms for a 15 s trigger; delivered=false pending=false" (run 38666074347, job 116198215493, PR #223 head `49f0f8f`, 2026-10-15). F10's fix for a trigger that is already in the past by the time `add()` returns does not cover this. The request was added in good time and then vanished.
- App-facing API layer added (coordinator, 2026-10-15, at the maintainer's direction). U07a and U08a blocked themselves on main `fefb034` because the operations they configure are not reachable from `app/`. A read-only audit of the 12 unclaimed UI slices then found that the M1 plan never scheduled the layer between core and UI. Many operations exist as core functions (for example `mark_deletion_intent`, `get_reminder`, `apply_user_time_correction`, `end_item_with_event`, `record_not_now`, `requeue_job_to_new_profile`) but have no user-facing FFI export or root client. `ExportService` and `ResetService` are never constructed in production. Prompt taps are dropped because the root's ingestor has no `promptHandler`. A notification tap records the item but presents nothing. Only U09 was buildable as specified. The coordinator added A01–A13 and the two allowances below, deferred S04, and recorded each slice's new dependencies in `effective_dependencies`:
  - **Capability client allowance (standing).** This replaces root-file serialization for typed capability clients. A task whose slice cites this allowance may, in addition to its owned paths:
    1. Add its client and service files under the new `ios/Services/<Area>/` directory its spec names.
    2. In `ios/Services/Assembly/CompositionRoot.swift`, add one registration for that client: a client-name constant, one entry in `reservedOperationRanges`, its construction in the root assembly with `router.register` and undo, one public property or accessor on `ServiceCompositionRoot`, and one shutdown call. Each is a single line or one contiguous block at the end of its list.
    3. In `ios/Services/Authentication/PrivateReadGate.swift`, add one gated verb per private read the capability exports, using the existing `issue`/`admit` pattern.
    4. In `ios/Services/Assembly/LifecycleCoordinator.swift`, add one public accessor exposing an existing internal service, where the spec names it.

    Nothing else in those files changes. Every read of stored history or state derived from it goes through the read gate. Every write that changes stored history, reminders, deletion or what leaves the device requires an authenticated read session, and a capture-only session is refused. Conflicts are resolved by rebasing, and the merger bounces the PR for rework on conflict. Reviewers reject edits beyond these registrations. Root behaviour changes (the ingress path, launch order, notification handling) are not covered; they stay on the serialized root chain S05b → T11d → A10 → A11 → A12. U01d and U01e now register under this allowance, so they no longer wait on T11d or on each other.
  - **Management context allowance (standing).** Each UI slice may add exactly one field to `ManagementContext` (`ios/AppAssembly/ManagementCatalog.swift`) and one line that builds it in `AppServices` (`ios/AppAssembly/AppServices.swift`). The field carries only seams that already exist on main or in the slice's dependencies. This supplements the shared host allowance. U07c may additionally add exactly one section entry to U02b's source-detail host, for its per-item delete control.
  - **E02 restructured (coordinator, 2026-10-18).** E02 blocked on missing credentials. The maintainer supplied an Anthropic key and the Spark configuration, but no harness can run E02's acceptance; see E02a. E02a builds the harness. The coordinator then runs it on the maintainer's machine against every model Spark serves and one Anthropic model, and commits the sanitized evidence under `docs/validation/evidence/live-providers/`. E02 then writes `docs/validation/live-providers.md` from that evidence and depends on E02a. E02's acceptance is unchanged. Spark's endpoint lists six models, but `ollama ls` on spark01 shows five; `glm-5.3-flash` is served from elsewhere, and the evidence records it separately.
  - **F22 added (coordinator, 2026-10-18).** The first install of current main on the maintainer's iPhone crashed at every launch. The crash report's termination namespace is TCC: "attempted to access privacy-sensitive data without a usage description … must contain an NSFaceIDUsageDescription key". DS03 (#278) made launch call `LAContext.evaluatePolicy` (`ios/Services/Authentication/ReadAuthenticationBoundary.swift:61`). The app's `Info.plist` has never declared the key, and `ios/scripts/check_project_config.py:44` does not require it. The simulator does not enforce the key, so CI passed. See F22.
  - **F23 added (coordinator, 2026-10-19).** A paired benchmark of Claude Sonnet 5.5 and Haiku 5.5 on the synthetic corpus, run through the production request and mapping after XB12, found that Claude JSON-encodes string abstentions in 8 of 10 cases: `"abstention": "\"Negated\""` instead of `"Negated"`. The mapping rejects them, so "don't remind me" and noise captures fail as invalid output even though the model chose correctly. The cause is the wire schema's `anyOf` of a string enum and an `{"Other": …}` object (`core/src/interpretation/instructions/text.rs:102`). The maintainer chose to replace it with one object shape rather than unwrap the extra encoding. See F23.
  - **F24, F25 and F26 added (coordinator, 2026-10-19).** The first live E02a run (evidence `fe58be5`) and a paired Anthropic benchmark found three more defects. The Anthropic adapter's 1024-token output cap truncates long captures (F24). The live harness's oracle counts a capture whose interpretation failed as a match (F25). The evaluation scorer marks a correct, stored reminder as a missed intent (F26). F25 and F26 edit test directories that F23 also edits, so both depend on F23.
  - **F21 added (coordinator, 2026-10-18).** A live replay on Spark found that the self-hosted adapter's strict schema suppresses every optional proposal, reminders included, on all five Ollama models. E02's evidence run also waits for F21, so it measures the fixed request. See F21.
  - **Walkthrough allowance (standing, coordinator, 2026-10-18).** DS08's walkthrough (`ios/UITests/Walkthrough/WalkthroughUITests.swift`) names each screen's controls and titles, so any UI task that changes a screen it visits breaks it. On 2026-10-18 this blocked DS04a, DS07 and U02b. Any UI task may update the walkthrough for the screens it builds or changes:
    - their entries in the screen tables (for example `settingsScreens`);
    - their accessibility identifiers, expected titles and waits;
    - how the keyboard is dismissed on them.

    A task that builds a new screen may also add steps that capture it in both appearances, at compact width and, when available, at regular width.

    It may not:
    - remove a screen or a capture;
    - skip or soften a step, including making a step tolerate a missing screen;
    - lengthen `screenTimeout`;
    - turn a "did not appear" failure into anything weaker;
    - change shared helpers.

    The PR links the exact-head walkthrough job and lists the screenshots that changed. Conflicts between concurrent tasks are resolved by rebasing.
  - **U06a settings entry (coordinator, 2026-10-18, after PR #292 round 3).** U06a may add exactly one entry to U01's settings host, `ManagementCatalog.settingsSections` (`ios/AppAssembly/ManagementCatalog.swift:91`). The entry has the id `privacy-settings` and builds `PrivacySettingsView` from `context.privacySettings`. This is in addition to its management-context field and `AppServices` line. U06a may also add one app-shell reachability assertion under `ios/Tests/AppAssembly/`. These are the same terms as the shared host allowance; U06a was missing from that list.
  - **U02b regular-width Search pane (coordinator, 2026-10-18, after PR #290 round 1).** The design puts the item detail beside the results at regular width (`docs/features/m1-ui-design.md`, Search). DS05 left a placeholder (`RetrievalSearchView.itemPanePlaceholder` is `Color.clear`), and `RetrievalSearchModel.onSelect` always pushes. U02b may add a pane seam in `app/retrieval/` (`RetrievalSearchView`, `RetrievalSearchModel`, `RetrievalSearchModule`):
    - at regular width, keep the selected reference, outline the selected result in `accent`, and render the injected source-detail pane in place of the placeholder instead of pushing;
    - compact width is unchanged.

    U02b may also add a hosted test under `ios/Tests/Retrieval/` and at most a one-line pass-through in `ios/AppAssembly/ManagementCatalog.swift`. The walkthrough allowance covers capturing source detail.
  - **U06a Privacy row (coordinator, 2026-10-18, after PR #295 round 1).** The settings host draws only the ids listed in its two groups, so an entry in `settingsSections` alone cannot be reached. The design now has a Privacy row in the second group: Processing, Privacy, Audio, Export (`docs/features/m1-ui-design.md`, Settings). U06a may therefore:
    - add `privacy-settings` after `provider-settings` in `SettingsRootView.secondGroupEntryIDs` (`ios/AppAssembly/ManagementCatalog.swift:163`);
    - update the design-order assertion in `ios/Tests/AppAssembly/SettingsRootViewTests.swift` to match.

    Privacy is a separate row from Processing. Processing chooses who sorts captures and holds provider setup. Privacy holds the per-route grants: which purpose may reach which destination. The row's value is left for U06b. Nothing else in the host changes.
  - **F21 per-model evidence (coordinator, 2026-10-18, after PR #297 round 1).** F21 replaced the single verified model with `SMOKE_EVIDENCE`, three models each citing its own semantic smoke artifact. Two consumers mirror the old constants and must follow it. F21 may edit:
    - `ios/Services/OptionalCapabilities/SelfHostedEvidence.swift`, `ios/Services/OptionalCapabilities/OptionalCapabilitiesConfiguration.swift` and `ios/Tests/OptionalCapabilities/`;
    - `tools/bench/src/adapters/self_hosted.rs` and `tools/bench/src/cli/tests.rs`, limited to adapting to the three `Supported` models.

    The iOS mirror represents the evidence as follows. `SelfHostedVerifiedBoundary` keeps one shared worker-probe reference and one shared phone-reachability reference. Next to them it holds `models`, an ordered list mirroring `SMOKE_EVIDENCE`, in which each element pairs a model with its own adapter-smoke reference. The gate enables self-hosted only when all three hold:
    - the registration's model is one of `models`;
    - the registration cites both shared references;
    - the registration cites that model's own smoke reference. Another model's smoke reference does not count.

    `testTheVerifiedBoundaryMatchesTheCoreSource` checks each element's model, id, artifact path and collection time against the core source. It also checks that the number of elements equals the number of `SmokeEvidence` entries in the core source, so a model added only in core fails, and that every artifact exists. Integration tests cover three cases:
    - a second verified model (`gpt-oss:20b`) enables;
    - an unlisted model stays disabled with `modelNotVerified`;
    - a verified model that cites another model's smoke artifact stays disabled.

    These are native edits, so the exact-head macOS run is required. F21's remaining in-scope rework, as its block note lists it, still applies. XB12, which also owns `tools/bench/src/cli/`, now depends on F21.
  - **DS04b home seam (coordinator, 2026-10-18, after DS04a landed as #288).** Owning only `app/home/` cannot fill the home. DS04b may edit these files, for these purposes only:
    - `ios/Capture/Entry/CaptureEntryView.swift`: replace the `Color.clear` body with an injected home-body slot. The composer, header and capture behaviour are unchanged.
    - `ios/AppAssembly/AppRootView.swift` and `ios/AppAssembly/AppServices.swift`: pass `RecentItemsService`, the current read scope, the quiet-attempt state and an unlock closure that calls the existing explicit authentication into the home.
    - `ios/Services/Authentication/ReadAuthenticationSession.swift`: publish a read-only `isQuietAttemptInFlight`, derived from the existing `attemptInFlight`. If relock is not already observable, add a relock observation.

    The read gate, relock policy, attempt rules and every gated read stay unchanged. A test under `ios/Tests/Authentication/` shows the flag rising and falling around a quiet attempt, and that observing it changes no scope. DS04b now also depends on U06a and U02b, which edit `AppServices.swift` in open PRs, so these edits are serialized rather than raced.

    **Widened 2026-10-19, after both reviewers of PR #303 found two files missing.** DS04b may also edit:
    - `ios/Services/Assembly/CompositionRoot.swift`: expose the quiet-attempt flag and the read-access observation through the public composition surface, because `ReadAuthenticationSession` is internal to `OhAndServices`. Only read-only accessors may be added. No existing signature, read gate or scope rule changes.
    - `ios/AppAssembly/AppLauncherView.swift`: pass the home's recents into `AppRootView`. Nothing else in the launcher changes.

    No open task owns either file.
  - **S04 deferred.** As scoped it cannot be built:
    - `PayloadKind` has only `Generic` (`core/src/reminders/reconcile/effects.rs:48`), and prompts hard-code it (`core/src/suggestions/schedule/apply.rs:178`).
    - The preview opt-in has no store, and `routes.preview_safe` is only ever written as 0 (`core/src/ffi/route_provisioning.rs:99`).
    - Prompts choose their item at open time (S03), so a preview needs an S02/S03 design change.

    S04 is an opt-in feature and no M1 task depends on it. It now depends on T08 and will be re-decomposed after the trial verdict.
  - **Missing seams found by blocked tasks (coordinator, 2026-10-15).** Four tasks blocked with accurate reports. Each was checked against main `c33b978`.
    - F15's fix is approved by both reviewers, but its 50-run soak cannot pass while two `tools/bench` transport tests fail under load (3-second polling window). F17 fixes those tests; F15 depends on F17.
    - V01c's owned file was a sibling of `provider_transport` and could not reach its private transport. Its file moves under `core/src/ffi/provider_transport/`, as `shadow_review` already is.
    - U02a must show each result's type, but no layer of the retrieval answer carries it. R05 adds it; U02a depends on R05.
    - A08's "Delete audio" cannot pass the fence, because `authorize_audio_removal` approves only expired audio, and its "Retry transcription" has no job path to reuse. While tracing the second, the coordinator found that **no production code enqueues a `transcription_attachment` job at all**: voice captures stay `audio_pending` and are never transcribed. C07 queues the job at import, backfills stored voice captures at drain start and provides the requeue function; A14 adds the user-initiated removal decision. A08 keeps all its criteria and depends on both.
  - **Missing seams, round 2 (coordinator, 2026-10-16).** Two tasks blocked with accurate reports. Each was checked against main `f2594f2`.
    - **A09** needs a deletion hook in A05's `ItemDeletionService` and a playback fence in the services that `LifecycleCoordinator` builds. Both reviewers of PR #252 rejected those edits as outside A09's paths. GPT also found a race: the PR ended playback *before* the core accepted the deletion. A stale or refused deletion then stopped playback for nothing, and an open between the callback and the commit survived the deletion. A05 has landed, so A09 gets a narrow grant instead of a predecessor task (see A09). The observer fires only after the intent is recorded.
    - **T11**'s round-2 rejection is correct. Self-hosted enablement is decided once, synchronously, before modules attach (`CompositionRoot.swift:1755-1760`). The profile-status read answers later and has no consumer. So a registration naming a missing, revoked, wrong-protocol or wrong-model profile still enables self-hosted dispatch for *every* stored self-hosted profile. The coordinator chose a core gate over a root hook that switches the protocol on after the async read. A root hook would still enable every self-hosted profile once the registered one validated, and it opens a window between attach and the read. A core gate checks at each dispatch and needs no ordering. T11e restricts self-hosted dispatch to registered (profile version, model) pairs and lets modules declare them. T11 depends on T11e and needs no root edit.
    - **U05b** (2026-10-17, after PR #267 round 1) cannot replace, check or remove the secret of a profile that is already saved. The listing deliberately returns only `credential_bound`, never `credential_ref` (`core/src/providers/profiles/mod.rs:588`). Once a draft is saved, the settings screen has no way to name the stored version's Keychain item. So the "enter or replace the secret" guidance for `credential_missing`/`credential_rejected` cannot repair the profile that pinned jobs are waiting on. The reference must stay out of `app/`. But the native services layer already receives references: the transport at dispatch, and reset through `profile_credentials` (`core/src/ffi/reset.rs:175`). U01f adds credential operations keyed by profile version. They resolve the reference inside the services layer and never return it. U05b depends on U01f.
  - **UI design rework (coordinator, 2026-10-17).** The coordinator ran the app in a simulator and found that it had no visual design. It ran letterboxed (F18). The keyboard covered navigation. Settings rendered acceptance criteria as paragraphs. The maintainer set an identity and a design system, and chose a composer-style home with recent captures. Both are recorded in [the M1 UI design](m1-ui-design.md).
    - **New tasks:**

      | Task | Scope | Depends on |
      | --- | --- | --- |
      | DS01 | Design-system foundations: tokens, fonts, components | |
      | DS02 | Icon and launch artwork | |
      | DS03 | Quiet Face ID at launch and on return to the foreground | |
      | R06 | Gated "recent captures" read | |
      | DS04a | Composer home | |
      | DS04b | Recents and lock states on the home | DS03, R06 |
      | DS05 | Restyle Search | |
      | DS06 | Restyle Settings, Audio and Daily prompt | after U07b |
      | DS07 | Restyle Processing | after U05b |
      | DS08 | Screenshot walkthrough in CI | |
      | DS09 | Restyle Export (filed after U07b landed) | after U07c |

    - **Standing rule:** every UI task that has not started depends on DS01. It builds on `app/design-system/` and is reviewed against the design document's checklist as well as its own criteria. Behavioural acceptance is unchanged.
    - **Unlock behaviour:** DS03 changes when authentication is attempted, not the read gate. A failed, cancelled or unavailable attempt leaves `capture_only` and capture is never blocked.

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
| P08 | P08a, P08b | Signing automation is Linux-testable with stubbed Apple tools; a signed device install is a maintainer input on a Mac with Xcode. Separate them so the tooling can land while the device evidence stays an honest block. |

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

Recorded decision (coordinator, 2026-10-08, PR #110 round 2): the production export `ohand_core_start_save_capture` and its Swift acknowledgment are a pre-import durable capture write, not the Capture Ingestion Contract acknowledgment in `docs/architecture/m1-contracts.md` (capture ID, item ID, save timestamp). B01c accepts any non-empty `route_id` without validating it, because no route store exists before C02a. C02a owns unknown-route rejection, the import transaction and the contracted acknowledgment; native callers, including the C01b ingress path, must treat B01c's success as "the capture bytes are durable" and nothing more. The acknowledgment type must be named or documented so it cannot be mistaken for the ingestion acknowledgment.

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

### P08a

Build and test the signing and device-build tooling

Make the signing automation correct and provably safe on Linux with stubbed Apple tools, so that a maintainer can run it unchanged on a Mac.

Owned paths: `tools/apple-build/`, `docs/validation/signing.md` (procedure sections only; the evidence and validity sections belong to P08b).

Dependencies: F03, F05.

Context: PR #27 on branch `mr/7d4836ee` holds the retired P08 attempt. Its code may be reused, but every finding from both review rounds recorded on task `7d4836ee` must be resolved; those findings are the defect list for this slice. The target toolchain is the Xcode version pinned in `ios/project.yml`, which F08 moves to 26.6; P08a waits for F08. The intended route for the two-week trial is Development signing with the paid account's one-year profile; keep ad-hoc and App Store routes only if they are implemented correctly or remove them.

Acceptance:

1. The unsigned simulator path stays credential-free. Signing inputs come only from environment or injected files, never from the repository or command output, and no secret, raw device identifier or profile content reaches stdout, logs or evidence.
2. Each supported route uses the correct identity class and, where distribution requires it, an archive and export step with an explicit export options file; the provisioning profile is referenced by its name or UUID read from the profile, not by its file name. A requested install that fails or has no device identifier exits non-zero. Keychains are per-run temporary files and are cleaned up on every exit path.
3. Behaviour tests run the scripts against stub `security`, `xcodebuild` and `devicectl` on PATH and assert exit codes, messages, keychain cleanup on failure, redaction and the evidence schema; they are wired into `make test` and have no vacuous assertions.
4. `docs/validation/signing.md` documents the maintainer procedure for P08b and makes no validity or expiry claim that was not observed.

Source pointers (baseline above): `AGENTS.md:45`, `ios/project.yml:6`, `docs/validation/m1-protocol.md:122`, PR #27 files `tools/apple-build/sign-probe.sh`, `tools/apple-build/manage-keychain.sh`, `tools/apple-build/test_signing.py`.

Contributes to original P08 criteria: 1, 3, and the automation part of 2.

### P08b

Record the signed build and device-install evidence

Fold the maintainer-produced signed-build and device-install evidence into the signing document and state the verified install validity for the trial.

Owned paths: `docs/validation/signing.md` (evidence and validity sections), `docs/validation/evidence/apple-signing/`.

Dependencies: P08a.

Context: this task starts blocked. The maintainer runs the P08a tooling on a Mac with Xcode 16.4, an Apple Development identity and a profile covering the registered iPhone, following `docs/features/m1-external-prerequisites.md`, commits the sanitized record under `docs/validation/evidence/apple-signing/`, and unblocks this task naming that file. A worker claimed without such a file must re-block naming that exact missing prerequisite and must not substitute simulator results.

Acceptance:

1. `docs/validation/signing.md` cites the maintainer's named artifact with build revision, collection time, the content-free build and device identifiers the tooling emits, and the observed profile expiry.
2. The install validity statement for the two-week trial derives from the observed profile expiry, and the renewal-without-data-loss plan is either tested and recorded or explicitly marked untested.

Source pointers (baseline above): `docs/features/m1-external-prerequisites.md`, `docs/validation/m1-protocol.md:122`.

Contributes to original P08 criteria: 2, 4, 5.

### P11a

Sign applications that embed app extensions

Extend the signing and device-build tooling from application-only probes to the production `OhAndApp` scheme with its embedded `OhAndCaptureControl` extension, provably on Linux with stubbed Apple tools, so the maintainer can run it unchanged on a Mac.

Owned paths: `tools/apple-build/` (except `tools/apple-build/trial/`, which belongs to T06), `docs/validation/signing.md` (Route, Prerequisites, Inputs and procedure sections; the evidence and validity sections belong to P11b).

Dependencies: P08b.

Context: P08a built `tools/apple-build/sign_probe.py`. It archives unsigned and signs at export with manual signing and an explicit export options file, maps one bundle identifier to the profile UUID read from the profile, and refuses Xcode-managed profiles. The four application-only probes are its only supported schemes. `OhAndApp` (`ios/project.yml`, bundle `com.boldfield.ohand.app`) embeds `OhAndCaptureControl` (`com.boldfield.ohand.app.capture-control`) plus the `OhAndCoreBridge` and `OhAndServices` frameworks, which take no profile. Its entitlements are only `keychain-access-groups` under its own prefix. Keep the existing probe behaviour, exit codes and evidence fields unchanged. Renaming the script is not required.

Acceptance:

1. `OhAndApp` is a supported scheme. The tool reads every embedded application and app-extension bundle identifier from the unsigned archive, not from a hard-coded list, and writes `provisioningProfiles` with one entry per signed bundle (application and extensions, never frameworks), each mapped to the UUID read from its profile. One supplied profile may cover several bundles when its application identifier covers each of them (wildcard or exact). A per-bundle mapping of several profiles is also accepted, through environment or injected-file inputs only.
2. (Amended 2026-10-12; see "P11a criterion 2 amended" above.) Before any keychain, profile-installation, signing or export work, deciding bundle coverage from the unsigned archive, the tool refuses, with a distinct non-zero exit and a message naming the bundle identifier (never profile contents or the team identifier), when a bundle is not covered by any supplied profile or when a covering profile is expired, Xcode-managed, or does not list the target device. Keychain cleanup still runs on every exit path.
3. Install with `devicectl` installs the exported app, then launches it by bundle identifier and confirms it is running. A failed install, launch or running check exits non-zero. The sanitized evidence record gains the embedded bundle identifiers, the expiry of each profile used and the launch result. Its schema version is bumped, and the no-team-ID, no-UUID, no-raw-device-ID rules still hold.
4. Behaviour tests against `stub_apple_tools.py` cover: an archive with one extension under one wildcard profile; two bundles under a per-bundle mapping; an uncovered extension; an expired and an Xcode-managed covering profile; launch failure; unchanged results for every existing probe scheme; and redaction of the new fields. They run under `make apple-build-test` with no vacuous assertions.
5. `docs/validation/signing.md` replaces the "only application-only probes" limitation with the procedure for `OhAndApp`, including the maintainer steps P11b follows, and makes no validity claim that was not observed.

Source pointers (baseline above): `tools/apple-build/sign_probe.py:55`, `tools/apple-build/sign_probe.py:129`, `tools/apple-build/sign_probe.py:445`, `tools/apple-build/sign_probe.py:476`, `tools/apple-build/sign_probe.py:495`, `ios/project.yml:60`, `ios/project.yml:108`, `ios/Config/OhAndApp.entitlements`, `docs/validation/signing.md:19`, `docs/architecture/m1-shell.md` (Remediation 3).

Contributes to T10's signed-candidate criterion and T06's signed route. Neither is satisfied here.

### P11b

Record the signed production-app install with its embedded extension

Fold the maintainer's signed `OhAndApp` build, install and launch evidence into the signing document, so T10 and T06 start from a proven signing route for the production bundle layout.

Owned paths: `docs/validation/signing.md` (evidence and validity sections), `docs/validation/evidence/apple-signing/`.

Dependencies: P11a.

Context: this task starts blocked on an external input. The maintainer runs the P11a tooling for the `OhAndApp` scheme on the Mac with the pinned Xcode, the Apple Development identity and a Development profile covering both bundle identifiers and the registered iPhone, following the procedure P11a writes in `docs/validation/signing.md`. The maintainer commits the sanitized record under `docs/validation/evidence/apple-signing/` on main and unblocks this task with a note naming that file. `OhAndApp` may still be a pre-U01 shell. This task proves the signing route and the embedded-extension layout, not app features. A worker claimed without such a file must re-block naming that exact missing prerequisite and must not substitute simulator results.

Acceptance:

1. `docs/validation/signing.md` cites the named artifact with build revision, collection time, the embedded bundle identifiers, each profile's observed expiry, and the install and launch result.
2. The record shows the control extension is embedded and the app launches on the device. Whether the control appears in the system control gallery is recorded as observed or explicitly as not checked, never inferred.
3. The trial validity statement is updated from the shortest observed profile expiry across the signed bundles.

Source pointers (baseline above): `docs/validation/signing.md`, `docs/features/m1-external-prerequisites.md`.

Contributes to T10's signed-candidate criterion. Records evidence only and does not modify the tooling.

### F09

Embed every framework the production app loads, and smoke-launch it in CI

Make `OhAndApp` launch outside a test host by embedding `OhAndCoreBridge.framework` in `OhAnd.app/Frameworks`, and add a CI launch check so a missing embedded framework cannot pass CI again.

Owned paths: `ios/project.yml` (the `OhAndApp` target's `dependencies` list only), `.github/workflows/ios.yml` (a new `OhAndApp` build and launch-smoke step in the `simulator` job), `ios/scripts/check_project_config.py` and its unit test (criterion 4), `ios/scripts/build-simulator.sh` and `ios/scripts/smoke-simulator.sh` (only a small addition if `OhAndApp` needs one; existing callers and outputs unchanged).

Dependencies: none (F08 and P11a have landed).

Context: P11b's first signed device run (2026-10-12) installed `OhAnd.app` and launched it, and dyld then terminated it with `Library not loaded: @rpath/OhAndCoreBridge.framework/OhAndCoreBridge` (referenced from `OhAndServices.framework`; searched `OhAnd.app/Frameworks/`, no such file). The cause is in `ios/project.yml`: `OhAndApp.dependencies` lists `OhAndServices` and `OhAndCaptureControl` but not `OhAndCoreBridge`. That is a dependency of `OhAndServices` only, and XcodeGen does not embed a framework's dependencies into the application. `OhAndTests` passes because `xcodebuild test` runs it hosted with `DYLD_FRAMEWORK_PATH` pointing at build products. The existing launch smoke tests cover only `BridgeProbe`, `CredentialProbe` and `TranscriptionProbe`, never `OhAndApp`. Expected fix: one `- target: OhAndCoreBridge` entry, embedded, under `OhAndApp.dependencies`. Do not use `transitivelyLinkDependencies` or other project-wide settings. Do not change `OhAndServices`, the test targets or any probe target.

Acceptance:

1. After `ios/scripts/generate.sh`, a simulator build of `OhAndApp` produces `OhAnd.app/Frameworks/` containing both `OhAndServices.framework` and `OhAndCoreBridge.framework`, each with its binary. `OhAndCaptureControl.appex` is still embedded under `PlugIns/`. `OhAndCaptureControl` does not embed a second copy of either framework.
2. The `simulator` job in `ios.yml` builds `OhAndApp` for the simulator and runs `smoke-simulator.sh` against it with bundle ID `com.boldfield.ohand.app`, outside any test host. The step fails the job if the app is not still running after launch. Its log and screenshot go into the existing `ios-evidence` artifact (`retention-days: 7`). Show that the check works: on a commit with the fix temporarily reverted, the new step fails with the dyld `Library not loaded` termination, or `simctl` reports the process exited. Link that failing run, then show the run on the fix passing. Do not leave the revert in the final branch.
3. The existing `OhAndTests`, probe builds and probe smoke tests are unchanged and pass, and `make check`/`make test` pass. Required macOS CI evidence per AGENTS.md is linked for the exact submitted commit.
4. `ios/scripts/check_project_config.py` fails when a framework target that `OhAndApp` reaches through another framework's dependencies is not also a direct embedded dependency of `OhAndApp`, with a unit test. This keeps the bug from coming back through a future framework.

Source pointers: `ios/project.yml:41` (`OhAndServices`, depends on `OhAndCoreBridge`), `ios/project.yml:60` (`OhAndApp.dependencies`), `ios/project.yml:101` (`OhAndTests` test host), `.github/workflows/ios.yml:124` (existing launch smoke), `ios/scripts/smoke-simulator.sh`, `ios/scripts/check_project_config.py`.

Unblocks P11b, whose maintainer device run needs a launchable app. T06 and T10 also inherit this fix.

### F08

Move native CI to macOS 26 and Xcode 26.6

Replace the unexplained Xcode 16.4 pin with the newest toolchain on GitHub-hosted runners so that simulator evidence and the maintainer's device signing use the same SDK generation as the trial iPhone.

Owned paths: `ios/project.yml` (`options.xcodeVersion` only), `.github/workflows/ios.yml`, `ios/scripts/select_simulator.py`, `ios/scripts/test_select_simulator.py`, `ios/scripts/check_project_config.py`, the "Required checks for macOS workers" section of `AGENTS.md`.

Dependencies: P07.

Acceptance:

1. Both jobs in `ios.yml` run on `macos-26` with Xcode 26.6 selected, the simulator ceiling follows the selected SDK, and the config check enforces that the workflow pin and `options.xcodeVersion` agree.
2. An exact-head `ios.yml` run on the submitted commit is green: OhAndTests pass, every probe app builds, the launch smoke test passes, and the Tauri probe round-trip passes under the new SDK. Any probe that breaks under iOS 26 is fixed in this task only if the fix is confined to the owned paths; otherwise record the exact failure and block.
3. `AGENTS.md` names the new runner, Xcode and SDK, and the evidence requirements are unchanged.

Source pointers (baseline above): `.github/workflows/ios.yml:18`, `.github/workflows/ios.yml:26`, `ios/project.yml:6`, `ios/scripts/select_simulator.py:71`, `AGENTS.md:65`.

### F10

Make the simulator UI tests deterministic on hosted runners

Find the cause of each intermittent UI-test failure in the `simulator` job and fix it, so a red `ios.yml` run means a real defect.

Owned paths: `ios/NotificationProbe/Tests/`, `ios/CaptureProbe/UITests/`, `.github/workflows/ios.yml` (the `simulator` job's UI-test steps and the simulator preparation before them), `ios/scripts/` (only for a new simulator reset or preparation helper; existing scripts and their callers unchanged); `ios/NotificationProbe/Sources/ProbeScenarios.swift` (only the foreground-delivery scenario: its trigger interval, its wait window and the fields it reports); `docs/validation/notification-probe.md` (only the rows that describe the steps F10 changes). The last two were added by the coordinator on 2026-10-14; see the F10 probe-source allowance above.

Dependencies: none.

Observed failures (2026-10-12; each passed on rerun without a code change; job logs in runs 38302508836, 38308773981, 38273173711, 38271046668 and 38265038613):

- `NotificationProbeAuthorizedUITests.testAuthorizedSchedulingBehavior`: "capacity never published", an `XCTWaiter` timeout at `NotificationProbeUITestCase.swift:31` with a 90 s wait. The test took 227 s.
- `NotificationProbeDeniedUITests.testDeniedPermissionBehavior`: "authorization prompt never appeared", at `NotificationProbeUITestCase.swift:49` with a 30 s wait.
- `NotificationProbeAuthorizedUITests.testAuthorizedSchedulingBehavior`: "Failed to launch com.boldfield.ohand.probes.notification", at `NotificationProbeUITestCase.swift:10`.
- `CaptureProbeHandoffUITests.testEachShortcutHandoffRendersOneNewEntryAndPlainLaunchesRenderNone`: "app never left the foreground", at `CaptureProbeHandoffUITests.swift:61` with a 15 s wait after `press(.home)`. This happened twice.

Every failure came after `OhAndTests` had run on the same simulator, which leaves `OhAnd` processes, a share sheet and a microphone permission request behind.

Acceptance:

1. **Diagnosis before change.** For each of the four failure modes, the PR states the cause it found, with evidence: an `xcresult` screenshot or hierarchy at the failure, a log line, or a reproduction. Candidate causes include leftover system alerts or sheets from earlier steps, notification authorization persisting across tests or attempts, a simulator that is not quiescent, and a wait that does not observe the state it claims to. A cause it cannot confirm is labelled as a hypothesis.
2. **Fix the cause; don't mask it.**
   - Allowed: give each UI suite a known simulator state (for example, uninstall the probe app or reset permissions, terminate leftover apps, dismiss or prevent stray alerts, or use a freshly erased or separate simulator per UI suite); replace waits that don't observe the right state; and raise a wait only when the PR shows the operation legitimately takes that long on a hosted runner.
   - Not allowed: skipping or deleting tests or assertions; test retries (`-retry-tests-on-failure`, retry loops or rerun-until-green wrappers); treating a timeout as success; and moving any check out of the required job.
3. **Proof of stability.** On the final submitted head, the `simulator` job passes in at least 5 consecutive executions: the original run plus reruns with `gh run rerun <id>`, using `gh run watch --interval 60`. All attempt URLs are linked. A failed attempt resets the count and needs its cause stated.
   - **Amended 2026-10-13 (coordinator).** The job also runs `OhAndTests` before the UI steps, and a failure there skips them. Two flakes F10 does not own also fail that step: `SQLITE_BUSY` in `NotificationActionCoreIntegrationTests` (F11) and a hang in `NotificationBridgeBoundednessTests` (F12). The count is therefore defined over F10's targeted steps, the NotificationProbe UI tests and the CaptureProbe handoff UI test.
   - An execution counts when both targeted steps ran and passed.
   - A failure of either targeted step resets the count and needs its cause stated.
   - An execution where a step outside F10's scope failed and the targeted steps did not run is **neutral**. It neither counts nor resets. It is listed with its failing test and routed to its owner, not fixed in F10.
   - An execution where an out-of-scope step failed but both targeted steps still ran and passed counts.
   - The streak is five counting executions on the final head with no targeted failure between them. A streak does not count past an unexplained failure of a targeted step on that head.
4. Everything else stays unchanged and passing: the `OhAndTests`, `OhAndApp` and probe smoke tests, the artifact contents, `retention-days: 7` and the 60-minute job timeout. `make check` and `make test` pass. Required macOS CI evidence per AGENTS.md is linked.

Source pointers: `ios/NotificationProbe/Tests/NotificationProbeUITestCase.swift:10`, `:31`, `:49`; `ios/CaptureProbe/UITests/CaptureProbeHandoffUITests.swift:61`; `.github/workflows/ios.yml:90` (NotificationProbe UI step), `:242` (CaptureProbe handoff step).

### F11

Give direct test connections to the core store a busy timeout

Make the native integration tests that inspect the core's SQLite store wait for the core's write lock instead of failing on `SQLITE_BUSY`.

Owned paths: `ios/Tests/Deletion/RealCoreDeletionFixture.swift`, `ios/Tests/Ingress/IngressCoreImportIntegrationTests.swift`, `ios/Tests/ShadowReview/ShadowReviewFixture.swift`, `ios/Tests/Health/ProcessingHealthTests.swift`, `ios/Tests/NotificationPermission/NotificationPermissionCoreIntegrationTests.swift`, `ios/Tests/NotificationActions/NotificationActionCoreIntegrationTests.swift`, `ios/Tests/ProviderTransport/ProviderExchangeFixture.swift`, `ios/Tests/CoreBridge/CaptureStatusTests.swift`, `ios/Tests/CaptureEntry/CaptureEntryCoreIntegrationTests.swift`, `ios/Tests/Export/ExportCoreIntegrationTests.swift`, `ios/Tests/VoiceCapture/VoiceRecordingCoreIntegrationTests.swift`, `ios/Tests/Notifications/NotificationCoreIntegrationTests.swift`. Only the direct-connection helpers in these files change.

Dependencies: B02 (its PR edits `NotificationActionCoreIntegrationTests.swift`).

Observed failure (2026-10-13, F10 run 38361190686 attempt 3, job 115209045404): `NotificationActionCoreIntegrationTests.swift:71`, `XCTAssertEqual failed: ("5") is not equal to ("0")` in `testCompleteStoredBeforeRelaunchRunsAfterTheNextUnlock`. The test's `withDatabase` helper opens the store with `sqlite3_open` and no busy timeout while a core launch is still writing. Four other test helpers already set one (`TranscriptionTestSupport.swift:258`, `RealCoreRetentionFixture.swift:117`, `JobRunnerTestSupport.swift:21`).

Acceptance:

1. Every direct `sqlite3_open` connection in the owned files sets `sqlite3_busy_timeout` before its first statement. The value is at least the core's own 5 s (`core/src/store/schema/mod.rs:115`) and is named once per file. Connections that deliberately provoke contention keep their intended behaviour, and the PR lists any such connection.
2. No assertion is removed or relaxed. No test is skipped, retried or rewritten to tolerate `SQLITE_BUSY`. Production code, the core and the Rust tests are unchanged.
3. The PR states, for each owned file, whether its reads can overlap a running core (as in `NotificationActionCoreIntegrationTests`) or only happen after shutdown. It does not claim that an unchanged file was flaky without evidence.
4. Required macOS CI evidence per AGENTS.md is linked for the exact head, with `OhAndTests` passing. `make check` and `make test` pass.

Source pointers: `ios/Tests/NotificationActions/NotificationActionCoreIntegrationTests.swift:49` (`withDatabase`), `:71` (`scalar`); `core/src/store/schema/mod.rs:115`.

### F12

Diagnose and fix the NotificationBridgeBoundednessTests host restart

Find out why the `OhAndTests` host stopped (crash, hang or test-time allowance) in `testAScheduleThatFailsWhileAnEarlierRestoreIsSuspendedKeepsTheRecordedRequest`, and fix the cause.

Owned paths: `ios/Tests/Notifications/NotificationBridgeBoundednessTests.swift` and the test support it uses. Also `ios/Services/Notifications/NotificationBridge.swift`, but only if the diagnosis shows a bridge defect. In that case the PR adds a regression test that fails without the fix.

Dependencies: none.

Observed failure (2026-10-13, run 38462101769 attempt 1, job 115519616876): the test started at 20:32:26.46 and xcodebuild reported "Restarting after unexpected exit, crash, or test timeout" at 20:33:04.22. The other tests in the class each take under 0.3 s, apart from one 4 s test. The test sets `effectTimeout: 0.05` and coordinates with gates (`addGate`, `restoreGate`), `eventually` and `letLateWorkRun`.

Acceptance:

1. **Diagnosis with evidence.** Take it from the failing attempt's `OhAndTests.xcresult` (crash report, test-host diagnostics, activity log) or from a reproduction, for example running the test repeatedly with `-test-iterations` locally under CPU load. State whether the host crashed (with the stack), deadlocked (which wait never returns), or exceeded an execution allowance. A cause that cannot be confirmed is labelled as a hypothesis, with the evidence for and against.
2. **Fix the cause.** A test race is fixed by making the test wait on the state it needs. No skip, no retry, no removed or relaxed assertion, and no larger timeout unless it is shown to be the cause. If the cause is a bridge defect (a crash, deadlock or continuation misuse in production code), fix it in `NotificationBridge.swift` with a regression test. The existing boundedness behaviour stays unchanged.
3. **Repetition evidence.** The test passes 200 consecutive iterations (`-only-testing` with `-test-iterations 200`), either locally on macOS or in a CI log linked from the PR. The required `ios.yml` run on the exact head passes.
4. `make check` and `make test` pass. Required macOS CI evidence per AGENTS.md is linked.

Source pointers: `ios/Tests/Notifications/NotificationBridgeBoundednessTests.swift:449`; `ios/Services/Notifications/NotificationBridge.swift`.

### T11a

Dispatch the verified self-hosted protocol through the core FFI

Make self-hosted profiles reachable from the native job runner, provider exchange and shadow review, under the same routing and credential rules as the other protocols.

Owned paths: `core/src/ffi/provider_transport.rs`, `core/src/ffi/provider_transport/` (including `shadow_review.rs` and tests), `core/src/ffi/job_runner.rs`, `core/src/ffi/job_runner/`.

Dependencies: V09.

Acceptance:

1. `prepare` and `run_exchange` in `provider_transport.rs`, shadow-review dispatch, and the job runner's `AdapterRegistry` dispatch `ProviderProtocol::SelfHosted` through `SelfHostedAdapter`. The `unsupported_protocol` refusals for self-hosted are removed.
2. The checks are exactly those of the other protocols: execution resolution, authorized destinations, the credential reference matching the profile, stale source, revocation, and redaction. A self-hosted profile without its own valid https endpoint is rejected before anything is sent. There is no default or fallback endpoint.
3. Tests with a synthetic transport cover a successful self-hosted interpretation exchange, a job-runner dispatch and a shadow-review dispatch. They also cover a missing endpoint, an unauthorized destination and a revoked profile, each refused with nothing sent. Existing tests for the other protocols are unchanged and pass. `make check` and `make test` pass.

Source pointers: `core/src/ffi/provider_transport.rs:692`, `:761`; `core/src/ffi/provider_transport/shadow_review.rs:535`; `core/src/ffi/job_runner.rs:776`; `core/src/providers/self_hosted/mod.rs:200`.

### T11b

Attach core-mediated provider transport at the composition root

Let the production root attach the provider exchange transport to its core handle, so shadow review and other core-mediated exchanges can dispatch. Modules still get no raw `CoreHandle`.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: B03 (its PR edits `CompositionRoot.swift`).

Acceptance:

1. The root calls `ProviderExchangeCoordinator.attach(to:sender:)` once, on its own `CoreHandle` with the production `ProviderRequestSender`, during assembly. This runs under the root's existing undo-on-failure. The DEBUG test path injects a fake sender.
2. Modules get no raw `CoreHandle` and no `ProviderRequestSender`. A module that needs shadow review gets it through the existing typed `CompositionCoreClients` surface (B02 round-2 rule). Private reads stay behind `PrivateReadGate`.
3. A root test shows that a module calling `shadowReview()` can dispatch a review through the attached transport to a fake sender. A test shows a dispatch before attach, or with attach failing, is refused (`transport_not_registered`) and leaves the root assembled or cleanly undone. Existing `CompositionRootTests` pass. Required macOS CI evidence per AGENTS.md is linked.

Source pointers: `ios/Services/ProviderTransport/ProviderExchangeCoordinator.swift:33`; `ios/Services/Assembly/CompositionRoot.swift`.

### U01a

Provision the fresh-install local-only capture route

Give the core an idempotent operation that makes sure the configured capture route exists, so a fresh install can save before any processing policy is set up. Create the route with no processing destinations, so creating it never authorizes upload.

Owned paths: `core/src/ffi/route_provisioning.rs`, `core/src/ffi/route_provisioning/`, its single `pub mod` line in `core/src/ffi/mod.rs`, `ios/OhAndCoreBridge/CoreHandle+RouteProvisioning.swift`. These follow the native-boundary export conventions.

Dependencies: none.

Acceptance:

1. `ensure_local_capture_route(route_id, route_name, scope)` runs in one transaction.
   - When the route is absent, it inserts it with an empty destination set (the representation `authorize_job` parses as no destinations), `preview_safe = 0` and `created_at` from the core clock. It writes no `route_authorizations`.
   - When the route is present with the same scope, it changes nothing: destinations, name and `preview_safe` stay as the user configured them.
   - When the route is present with a different scope, it fails with a normalized `route_scope_mismatch` and changes nothing.
   - Repeated and concurrent calls are safe.
2. Rust tests cover all of these on a freshly migrated store:
   - after provisioning, a capture on the route imports and creates its item;
   - `authorize_job` for any remote capability on the route is denied;
   - an existing route with configured destinations and `preview_safe` is untouched;
   - a scope mismatch is refused;
   - a second call is a no-op.
3. The Swift bridge exposes the operation with its normalized failure. `make check` and `make test` pass. The macOS CI run for the exact head is linked: the bridge must compile.

Source pointers: `core/src/ingress/import.rs:320`; `core/src/privacy/routing/mod.rs:11`, `:267`; `core/src/store/schema/mod.rs:420`.

### U01b

Provision the route at open and compose notification actions at the root

Connect notification actions and the configured capture route through the production root, so U01 can install them without a raw `CoreHandle`.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: T11b (shared file), U01a.

Acceptance:

1. During assembly, under the existing undo-on-failure and before ingress accepts or recovers a save, the root calls `ensure_local_capture_route` with the configuration's `captureRouteID` and `captureItemScope`. A failure fails assembly with a distinct `CompositionError`. No save is ever reported as saved on an unprovisioned route.
2. The root builds the production `ActionHandler` over its own core (`CoreNotificationActionClient`), a durable `NotificationActionJournal` under the storage root, the root's read-authentication session as `ActionAuthorizing`, and a `NotificationEventReceiver` whose ingestor feeds the handler's `eventHandler`. It also forwards each event to any `CompositionEnvironment.notificationEvents` the caller supplied. It attaches the handler to the read session.
3. The root's public surface gives the app exactly:
   - the receiver, as a `UNUserNotificationCenterDelegate` to install;
   - an async reconcile for launch;
   - a way to set the authenticated open-item callback, which receives the item identifier the core resolved.

   No `CoreHandle`, journal or handler type becomes public.
4. Real-core root tests:
   - on a fresh store, `saveText` through a production-configured root returns saved and the item exists;
   - a simulated Complete response reaches the core once, and a duplicate does not mutate twice;
   - a response received while unauthenticated waits in the journal, and runs at reconcile after authentication;
   - a tap calls the open-item callback with the core-resolved identifier.

   Existing `CompositionRootTests` pass. The macOS CI run for the exact head is linked.

Source pointers: `ios/Services/NotificationActions/ActionHandler.swift:72`; `ios/Services/Notifications/NotificationEventIngestor.swift:76`; `ios/Services/Assembly/CompositionRoot.swift:111`, `:139`.

### U01c

Expose the voice ingress handoff and recovery at the root

Give the app a public, durable path for finished recordings and for restart recovery, so U01 can wire `VoiceRecordingController` and `VoiceRecoveryCoordinator` in place of `VoiceRecorderUnavailable`.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: U01b (shared file).

Acceptance:

1. The root publicly exposes three things:
   - **Saving a recording.** It saves a finished recording from the in-progress audio store through the existing `ForegroundIngressService` audio path: stage, finalize by move, import. It reports saved, kept for retry or failed with the same honesty rules as `saveText`.
   - **Restart recovery.** It runs restart recovery and reports the staged captures it re-imported plus the unclaimed in-progress audio file names. The names are nil whenever ownership is not fully known.
   - **The store location.** It provides the in-progress audio location the recorder must write to.

   No ingress, staging or core type becomes public beyond the value types these need.
2. Real-core root tests:
   - a synthetic recording handed off is saved as an audio capture with its item;
   - a handoff interrupted before import is recovered on the next assembly;
   - an unclaimed in-progress file is reported, not imported;
   - nothing is uploaded.

   Existing tests pass. The macOS CI run for the exact head is linked.

Source pointers: `ios/Services/Ingress/ForegroundIngressService.swift`; `ios/Services/Ingress/IngressRecord.swift:20`; `ios/Services/Assembly/CompositionRoot.swift:779`, `:826`; `ios/Capture/Voice/VoiceRecordingTypes.swift:105`; `ios/Capture/Voice/VoiceRecoveryTypes.swift:32`.

### S05a

Persist the daily prompt schedule in the core and expose it through the FFI

Make the core the only holder of the S02 prompt schedule, durable across relaunches, and run prompt installs from it inside the reminder pass.

Owned paths: `core/src/ffi/prompt_schedule.rs`, `core/src/ffi/prompt_schedule/`, its single `pub mod` line in `core/src/ffi/mod.rs`, `ios/OhAndCoreBridge/CoreHandle+PromptSchedule.swift`. It also holds the allowance in "B05 prerequisites": one migration step, and the prompt-source change in `core/src/ffi/notification_reconciliation.rs`.

Dependencies: none (L05b's v10 has landed).

Acceptance:

1. The core loads and saves `PromptSchedule` from the new table. Every exported entry point that takes `now` reads the core clock, takes the IANA zone from the caller and persists the schedule before it returns.
2. Exports:
   - configure (off, or a daily local time);
   - refresh (S02's `RefreshTrigger`: launch, foreground, time or zone change, settings change);
   - read the current settings and next planned occurrence, without content.
3. With optional prompts allowed and the schedule enabled, the reminder pass takes its prompt demands only from the persisted schedule, through `reconcile_prompts`, and records the capacity pass in the ledger. Prompt candidates supplied by the caller no longer cause installs. Reminders keep priority over prompts.
4. Rust tests on a migrated store cover:
   - the schedule survives reopening the store;
   - a prompt the OS delivered while nothing was observing is not reinstalled after the clock is set back;
   - after a reopen and westward zone travel, no second prompt for an already-held local date;
   - a planned occurrence that permission or capacity kept from being attempted is not spent, and a later time the same day schedules;
   - DST gap and fold use S02's local-time rules;
   - repeated refreshes do not duplicate an install;
   - the migration test.

   `make check` and `make test` pass. The exact-head macOS CI run is linked: the bridge must compile.

Source pointers: `core/src/suggestions/schedule/plan.rs:167`, `:213`, `:222`, `:299`; `core/src/suggestions/schedule/apply.rs:119`; `core/src/ffi/notification_reconciliation.rs:21`, `:249`.

### S05b

Drive the daily prompt schedule from the app lifecycle

Connect the core prompt schedule to launch, foreground, permission changes and settings, so a configured daily prompt is actually installed and kept current.

Owned paths: `ios/Services/Prompts/`, `ios/Tests/Prompts/`, `ios/Services/Assembly/CompositionRoot.swift` (only the prompt service's construction and its typed client), `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: S05a, U01c (shared file).

Acceptance:

1. A prompt service over a root-provided typed client exposes configure, read, and a time-change entry that refreshes with the current zone. The root installs it as the `LifecycleCoordinator` `.promptEligibility` action, so every launch, foreground and time-change pass refreshes the schedule and the same pass installs or cancels prompts through the core. U08a's settings UI calls configure; that UI is not built here.
2. Prompts stay off until configured. Turning them off cancels any installed prompt. Permission denied or provisional follows N04b's existing rules. No prompt content appears in any payload beyond S03's rules.
3. Real-core simulator tests with the fake notification centre:
   - configure, then launch, installs exactly one prompt for the next eligible local date;
   - a foreground pass does not duplicate it;
   - turning prompts off cancels it;
   - a relaunch keeps the ledger: no second prompt for a held date.

   Existing tests pass. The exact-head macOS CI run is linked.

Source pointers: `ios/Services/Assembly/LifecycleCoordinator.swift:101`-`:106`, `:592`; `ios/Services/NotificationPermission/NotificationPermissionReconciler.swift:38`; `ios/Services/Assembly/CompositionRoot.swift:1051`.

### T11c

Let the host decide which provider protocols the core may dispatch

Gate provider-protocol dispatch in the core on an explicit host declaration, so an optional protocol is disabled unless the build registers it. Add a typed provider-profile status read.

Owned paths: `core/src/ffi/provider_transport.rs`, `core/src/ffi/provider_transport/`, `core/src/ffi/job_runner.rs`, `core/src/ffi/job_runner/`, `ios/OhAndCoreBridge/CoreHandle+JobRunner.swift`, `ios/OhAndCoreBridge/CoreHandle+ProviderTransport.swift`.

Dependencies: none.

Acceptance:

1. A core-handle call sets the enabled provider protocols. The job-runner drain, `prepare` and `run_exchange`, and shadow-review dispatch refuse a protocol that is not enabled, with a normalized `protocol_not_enabled` and nothing sent.
2. The default is Anthropic and OpenAI enabled and self-hosted disabled. That keeps today's behaviour for the base protocols. Enabling self-hosted changes no other check (T11a rules).
3. A status read for a provider-profile version returns only whether it exists, its protocol, whether it is revoked, and whether it serves a given model. It returns no endpoint, credential or content.
4. Tests:
   - a stored `supported` self-hosted profile and job, with self-hosted not enabled, sends nothing in all three paths;
   - enabled, it dispatches;
   - disabling it again stops new dispatches;
   - the status read covers revoked, missing and wrong-protocol profiles;
   - existing tests pass, with fixtures enabling self-hosted where they rely on it.

   `make check` and `make test` pass. The exact-head macOS CI run is linked.

Source pointers: `core/src/ffi/job_runner.rs:776`; `core/src/ffi/provider_transport.rs:692`; `core/src/ffi/provider_transport/shadow_review.rs`.

### T11d

Let modules enable provider protocols at the composition root

Give `CompositionModule` a typed way to declare the provider protocols it enables, and give modules the provider-profile status read as a typed client.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: T11c, S05b (shared file).

Acceptance:

1. `CompositionModule` gains `enabledProviderProtocols`, which defaults to empty. During assembly, under undo-on-failure, the root passes the base protocols plus the union of the modules' declarations to T11c's call. A root with no module declaring self-hosted leaves it disabled.
2. `CompositionCoreClients` gains a typed provider-profile status client. No `CoreHandle` becomes public.
3. Root tests:
   - with no declaring module, a stored self-hosted job sends nothing;
   - a module declaring self-hosted enables dispatch;
   - the status client reports revoked and missing profiles.

   Existing tests pass. The exact-head macOS CI run is linked.

Source pointers: `ios/Services/Assembly/CompositionRoot.swift:156`, `:261`.

### T11e

Restrict self-hosted dispatch to the registered profile and model

Make the core's self-hosted gate name a stored profile version and model, not only the protocol, so that a build's self-hosted registration decides what can be sent: only the registered profile serving the registered model dispatches. Let modules declare those pairs at the composition root.

Owned paths: `core/src/ffi/provider_transport.rs`, `core/src/ffi/provider_transport/` (including `protocols.rs` and `connection_check.rs`), `core/src/ffi/job_runner.rs`, `core/src/ffi/job_runner/`, `ios/OhAndCoreBridge/CoreHandle+ProviderTransport.swift`, `ios/OhAndCoreBridge/CoreHandle+JobRunner.swift`. In `ios/Services/Assembly/CompositionRoot.swift`, only the `CompositionModule` declaration (`:390-409`) and the protocol-enable block in assembly (`:1755-1760`). Also `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: A10 (root chain; shared `CompositionRoot.swift`).

Acceptance:

1. A core-handle call replaces the handle's self-hosted registrations, a list of (profile version, model) pairs. The default is none.
   - Enabling the self-hosted protocol (T11c) stays necessary but is no longer sufficient.
   - Every path that sends to a self-hosted endpoint sends only when the stored profile version and model match a registered pair. Those paths are: the job-runner drain, `prepare`/`run_exchange`, shadow-review dispatch and V01c's connection check.
   - Otherwise the request is refused with a distinct normalized code (for example `self_hosted_not_registered`) and nothing is sent. The drain puts the job back without spending retry budget, as it does for `protocol_not_enabled`.
   - Malformed input (an empty profile version or model) is `invalid_request`.
2. The Anthropic and OpenAI protocols are unaffected. A registration changes no other check: execution resolution, destinations, credentials, staleness and revocation apply as before, so a registered profile that is revoked is still refused. Setting registrations reads no profile content and reports none.
3. `CompositionModule` gains a declaration of self-hosted registrations, defaulting to empty. During assembly, under undo-on-failure, the root passes the union of the modules' declarations to the core next to the protocol set. Undo restores no registrations. No `CoreHandle` becomes public.
4. Tests:
   - **Rust, for each sending path:**
     - a registered pair dispatches;
     - a second stored `supported` self-hosted profile sends nothing, and so does the registered profile under a different model, while the registered pair still dispatches;
     - self-hosted enabled with no registrations sends nothing;
     - replacing the registrations with none stops new dispatches;
     - a registered but revoked profile is refused.
   - **Root:**
     - a module declaring self-hosted with no registration sends nothing;
     - a module declaring self-hosted and a registration dispatches only the registered profile's job, and a different valid supported self-hosted job stays queued.

   Existing tests pass. Fixtures that rely on self-hosted dispatch register their profile, including T11d's root test. `make check` and `make test` pass. The exact-head macOS CI run is linked.

Source pointers: `core/src/ffi/provider_transport/protocols.rs:41-56` (`is_enabled`/`ensure_enabled`), `:65`; `ios/Services/Assembly/CompositionRoot.swift:398`, `:1755`.

### C04c

Give the voice handoff an honest result for unavailable audio

Extend the voice handoff contract so a recording whose audio could not be made durable is reported as unrecoverable, never as kept.

Owned paths: `ios/Capture/Voice/VoiceRecordingTypes.swift`, `ios/Capture/Voice/VoiceRecordingController.swift`, `ios/Tests/VoiceCapture/`.

Dependencies: none.

Acceptance:

1. `VoiceHandoffResult` gains an explicit case for "staged but the audio is unavailable".
2. `VoiceRecordingController` maps that case to `VoiceCaptureOutcome.unrecoverable`, with `fileRetained` reflecting whether the in-progress file still exists on disk. Nothing in that path claims the audio is kept, saved or retryable. Every exhaustive switch is updated. The existing outcomes are unchanged.
3. The test handoff adapter (`VoiceTestSupport.swift`) maps an ingress `sourceUnavailable` to the new case, not to `keptForRetry`.
4. Controller tests cover the new case with the file present and absent, and check the user-facing recovery state for each. Existing voice tests pass. The exact-head macOS CI run is linked.

Source pointers: `ios/Capture/Voice/VoiceRecordingTypes.swift:93`; `ios/Capture/Voice/VoiceRecordingController.swift:398`; `ios/Capture/Voice/VoiceRecordingTypes.swift:132`; `ios/Tests/VoiceCapture/VoiceTestSupport.swift`.

### F13

Make the untrusted-certificate exchange test deterministic

Find why `testUntrustedCertificateIsRefusedAndDiagnosticsStayProtected` sometimes gets no final event, and fix the cause.

Owned paths: `ios/Tests/ProviderTransport/`. If the cause is in the transport (`ios/Services/ProviderTransport/`) and not in the test, block and name it. Do not mask it.

Dependencies: none.

Acceptance:

1. **Diagnosis.** The PR states the cause with evidence from job 115754735518 (log lines around 08:34:45–08:35:10) or from a reproduction: the handshake failure was not delivered, delivered late, or lost by the fixture's event pump. A cause the evidence cannot confirm is labelled as a hypothesis.
2. **The fix addresses the cause.** No retries, no skipped or weakened assertions. A wait may be raised only if the PR shows the operation legitimately takes that long on a hosted runner. If the transport can fail to report a refused handshake, that is a product defect. Block and name it.
3. The F10 counting rule applies to this test. On the final head it passes in 5 consecutive executions of the `simulator` job's `OhAndTests` step. Executions where an earlier unrelated step failed are neutral. The run URLs are linked. `make check` and `make test` pass.

Source pointers: `ios/Tests/ProviderTransport/ProviderExchangeBoundaryTests.swift:347`, `:357`; `ios/Tests/ProviderTransport/ProviderExchangeFixture.swift:213`; `ios/Tests/ProviderTransport/FixtureTLSServer.swift:86`.

### F14

Make the bench unverified-self-hosted CLI test deterministic

Find why `fake_run_with_an_unverified_self_hosted_arm_skips_every_case_and_sends_nothing` failed intermittently in `make test`, and fix the cause.

Owned paths: `tools/bench/src/cli/tests.rs`. A cause outside it (for example shared process state in `tools/bench/src/cli/`) is in scope only for that fix, and the PR names it.

Dependencies: none.

Acceptance:

1. **Diagnosis.** The PR states the cause with evidence: a reproduction, for example `cargo test` looped or run under parallel load, plus the failing assertion. Candidates include shared environment variables or working directories across parallel tests, temp-path reuse, and time-dependent journal content. A cause the evidence cannot confirm is labelled as a hypothesis.
2. **The fix addresses the cause.** No retries, no `#[ignore]`, no weakened assertion, and no forcing of `--test-threads=1` across the suite.
3. The test passes 200 consecutive times under parallel `cargo test` on the final head, run with the loop command written in the PR. `make check` and `make test` pass.

Source pointers: `tools/bench/src/cli/tests.rs:1849`.

### I08

Queue interpretation for every saved capture

Make the core queue an interpretation job when a capture with text is imported and when a transcript attaches, with the profile pin chosen by the core, so the offline fast path and any authorized provider see app captures.

Owned paths: `core/src/interpretation/enqueue/` (new; its `pub mod` line in `core/src/interpretation/mod.rs`), `core/src/ingress/import.rs`, the interpretation-queueing code in `core/src/ingress/transcription/mod.rs` and the request wire in `core/src/ffi/transcription_attachment.rs`, their existing tests, and `core/tests/interpretation_enqueue/` (new). Amended 2026-10-14 after round 1: existing tests elsewhere (`core/src/export/tests.rs`, `core/src/lifecycle/reset/tests.rs`, `core/src/lifecycle/retention/tests.rs`, `core/tests/interpretation_dispatch/main.rs`, `tests/resilience/capture-processing/processing.rs`, and any other existing test that counts jobs after an import) may be adapted only to account for the job every text import now queues; no assertion is removed or weakened.

Dependencies: none.

Acceptance:

1. **One resolver decides the pin.** `core/src/interpretation/enqueue/` exposes one function that, inside the caller's transaction, returns the profile version a new `interpret` job for an item is pinned to, or none. Until V01b lands it always returns none (an unpinned job runs the fast path only and is `Local` under `authorize_job`). V01b replaces its body; no other code chooses a pin.
2. **Text imports queue a job in the import transaction.** A foreground import whose capture has text enqueues one `interpret` job at the item's initial revision, with a request version that is a deterministic UUID derived from `M1_INSTRUCTION_VERSION`, item and revision (the provider contract requires a UUID, `core/src/providers/contracts/request.rs:135`; the instruction digest itself is bound in `build_request`), and the resolver's pin, in the same IMMEDIATE transaction, so the acknowledgment still means "saved". Job identity is deterministic over item and revision, so a retried import of the same capture (already imported, or `ItemDeleted`) never adds a second job. An audio-only capture with no text queues nothing here.
3. **Transcripts queue a job without a native request.** When a transcript attaches and the caller supplied no interpretation request, the core queues one with the resolver's pin, through the existing authorize-then-keep savepoint. A native-supplied request whose pin differs from the resolver's is refused with a fixed failure and nothing is queued; native code cannot choose a destination. Existing attachment tests are updated only where they relied on the caller choosing the pin.
4. **End-to-end through the existing dispatcher.** Rust tests: a text capture "remind me tomorrow at 9am"-style command imported with no profiles is applied by the fast path and produces its reminder; an unrecognized capture with no profile stays `unprocessed` with its job reporting `AwaitingConfiguration` (the landed status contract, `core/src/domain/status/mod.rs:103`: waiting for configuration never produces `uninterpreted`), the test asserts both and that the capture is found by search; import retry does not duplicate jobs; a voice capture's transcript queues exactly one job. Capture save never waits on interpretation. `make check` and `make test` pass.

Out of scope: retroactively interpreting captures saved before a provider is configured (U06b's requeue controls), and any `ios/` edit.

Source pointers: `core/src/ingress/import.rs:147`; `core/src/ingress/transcription/mod.rs:99`, `:944`; `core/src/interpretation/dispatch/mod.rs:221`, `:247`; `core/src/privacy/routing/mod.rs:226`; `core/src/interpretation/instructions/text.rs:12`.

### V01b

Store versioned provider profiles and the default per capability

Give the core a production write path for provider profiles: create immutable versions, choose the default profile per processing capability, and list them, with an FFI export.

Owned paths: `core/src/providers/profiles/` (new; its `pub mod` line in `core/src/providers/mod.rs`), one migration step appended to `MIGRATIONS` in `core/src/store/schema/mod.rs` (the next version after S05a's), the body of I08's pin resolver in `core/src/interpretation/enqueue/`, `core/tests/provider_profiles/` (new), `core/tests/migrations/main.rs` (only the schema-version expectations the appended step forces and its own migration test; added 2026-10-15 after PR #228 round 1), and under the native-boundary allowance `core/src/ffi/provider_profiles.rs` (with its `pub mod` line) and `ios/OhAndCoreBridge/CoreHandle+ProviderProfiles.swift`.

Dependencies: S05a (shared schema file), I08 (resolver).

Acceptance:

1. **Versioned create.** Creating or editing a profile writes a new immutable `provider_profiles` row (same `profile_id`, new `profile_version`); existing rows are never updated except by the existing revocation. Validation rejects, with fixed failure codes that never echo input: unknown protocol; missing or malformed endpoint where the protocol needs one (self-hosted) or a caller-supplied endpoint where it does not; empty model; capabilities the protocol's adapter does not support; a cloud profile with no credential reference; timeout or retry values outside the contract's bounds. A credential reference claimed by a pending reset is refused through the existing L05b trigger as `credential_pending_removal`.
2. **Defaults.** A durable default per processing capability points at one profile version; setting it to a revoked or missing version is refused; clearing it is allowed. Changing the default never touches existing jobs (test: a queued job keeps its pin).
3. **The resolver uses the default only when it would dispatch.** I08's resolver returns the default version for text interpretation when one is set, unrevoked and `authorize_job` would authorize it on the item's route; otherwise none, so an unconfigured route still gets the offline fast path instead of a job that only waits.
4. **Listing** returns each profile's versions, protocol, endpoint, model, capabilities, whether a credential reference is bound (never the reference's secret), revocation and default flags. The FFI export covers create, set/clear default and list, following the B01 conventions. Rust tests cover each validation failure, versioning, defaults and resolver cases; `make check` and `make test` pass. Native tests are U01e's.

Source pointers: `core/src/store/schema/mod.rs:508`, `:879`; `core/src/jobs/configuration/mod.rs:29`, `:136`; `core/src/privacy/routing/mod.rs:183`; `core/src/providers/contracts/mod.rs:1`.

### V01c

Check a provider profile's connection on explicit request

Add a core operation that, only when called, sends one fixed synthetic request to a stored profile's own destination and reports a redacted classification.

Owned paths: `core/src/ffi/provider_transport/connection_check.rs` (new; its single `pub mod connection_check;` line in `core/src/ffi/provider_transport.rs`, mirroring `shadow_review`) and `ios/OhAndCoreBridge/CoreHandle+ProviderConnectionCheck.swift` under the native-boundary allowance, and their tests. As a child module it can use the transport's private registration, exchange and profile-loading items directly; nothing else in `provider_transport.rs` changes. (Amended 2026-10-15: the original sibling path could not reach the existing transport.)

Dependencies: V01b.

Acceptance:

1. The request is built by the profile's own protocol adapter from a fixed synthetic payload that contains no capture, query or user text; it goes only to the destinations `contacted_destinations` derives from that profile version, through the existing provider transport, off the core worker thread, cancellable through `CancelToken`. It does not require a route grant (the user is testing before granting) and it never runs in the background or on a schedule.
2. The outcome is one of a fixed set, at least: `ok`, `credential_missing`, `credential_rejected`, `unreachable`, `tls_failure`, `timeout`, `capability_mismatch` (for example an unknown model), `protocol_not_enabled` (T11c gate). No secret, response body, header or endpoint is echoed in the outcome, logs or failure strings.
3. Rust tests over the fake transport cover each outcome and show the payload contains no text from a seeded capture. `make check` and `make test` pass.

Source pointers: `core/src/ffi/provider_transport.rs`; `core/src/ffi/provider_transport/protocols.rs:65`; `core/src/providers/contracts/dispatch.rs:154`.

### V02b

Grant and revoke route processing permissions

Give the core a production write path for route processing policy: a deliberate, per-capability grant whose destinations come from a stored profile, its revocation, and a readable policy listing, with an FFI export.

Owned paths: `core/src/privacy/routing/grants.rs` (new; its `mod`/`pub use` lines in `core/src/privacy/routing/mod.rs`), `core/tests/route_grants/` (new), and under the native-boundary allowance `core/src/ffi/route_policy.rs` (with its `pub mod` line) and `ios/OhAndCoreBridge/CoreHandle+RoutePolicy.swift`.

Dependencies: none.

Acceptance:

1. **Grant.** Granting capability C on route R for profile version P derives the destinations from P in the core (the caller cannot name destinations), adds them to R's `processing_destinations` ceiling and to C's `route_authorizations` row, in one transaction. A revoked or missing P, an unknown route or a transcription grant (on-device only) is refused with a fixed failure. Repeating a grant is idempotent.
2. **Isolation.** Tests through `authorize_job` show that granting one capability does not authorize another, and granting one route does not authorize another route.
3. **Revoke.** Revoking C on R removes C's grant; queued jobs then fail authorization at dispatch with the existing denial, never falling back to another destination. Destinations no remaining grant on R uses leave the ceiling.
4. **Listing** returns each route's name, scope, granted capabilities and destination classes. Nothing here runs from a classifier or any inferred signal; only the explicit operation grants. Rust tests cover each case; `make check` and `make test` pass.

Source pointers: `core/src/privacy/routing/mod.rs:183`, `:266`; `core/src/store/schema/mod.rs:424`, `:438`; `core/src/ffi/route_provisioning.rs:1`.

### R04

Export scoped retrieval over the core boundary

Expose the existing scoped search and list operations to native code.

Owned paths: `core/src/ffi/retrieval.rs` (with its `pub mod` line) and `ios/OhAndCoreBridge/CoreHandle+Retrieval.swift` under the native-boundary allowance, and their tests.

Dependencies: none.

Acceptance:

1. Exports run `scoped_query` and `scoped_list` with the existing `QueryFilter` (scopes, date bounds, session topics) and bounded pagination on the core worker, returning per hit the item and capture identifiers, scope, current text, text basis, original text where corrected, matched span and capture date, plus the accessible total. Query text never leaves the device and is never echoed in failures.
2. The caller states the read scopes it is authorized for; the export cannot widen them. Authentication itself is enforced natively by U01d's gate.
3. Rust tests cover text search, date bounds, session-topic filtering, scope exclusion (no count or snippet from an excluded scope), pagination limits and invalid filters. `make check` and `make test` pass.

Source pointers: `core/src/retrieval/query/mod.rs:22`, `:65`, `:76`, `:97`; `core/src/retrieval/index/mod.rs:112`.

### R05

Carry each search hit's item type through the gated retrieval answer

Add the item's type to every retrieval hit so search results can show date, type and source without echoing the request filter.

Owned paths: `core/src/retrieval/index/mod.rs` (only `SearchHit` and the queries that build it), `core/src/retrieval/query/` (only where hits are built), `core/src/ffi/retrieval.rs` and its tests, `ios/OhAndCoreBridge/CoreHandle+Retrieval.swift`, and in `ios/Services/Authentication/PrivateReadGate.swift` only the `PrivateRetrievalHit` field and its decoding, plus the matching tests under `ios/Tests/Authentication/`.

Dependencies: none.

Acceptance:

1. `SearchHit` carries the item's current `item_type` from the item projection (`items.item_type`, `core/src/store/schema/mod.rs:372`), which is absent for an item that has not been classified. Absent is reported as absent, never as a guessed type.
2. The R04 hit JSON and `PrivateRetrievalHit` carry the same value (an optional string). Scope enforcement is unchanged: an excluded scope still yields no hit, count or type.
3. Rust tests: a typed item's hit carries its type, an unclassified item's hit carries none, and a type correction (D04) is reflected on the next search. A Swift test decodes both forms. `make check` and `make test` pass. The exact-head macOS CI run is linked.

Source pointers: `core/src/retrieval/index/mod.rs:112`; `core/src/retrieval/query/mod.rs:277`; `ios/Services/Authentication/PrivateReadGate.swift:73`.

### U01d

Offer gated retrieval reads at the composition root

Add search and list to the root's private read gate so `app/` modules can retrieve without a core handle.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Services/Authentication/PrivateReadGate.swift`, `ios/Tests/Services/CompositionRootTests.swift`, `ios/Tests/Authentication/`.

Dependencies: T11d (shared root file), R04.

Acceptance:

1. `PrivateReadGate` and `CompositionPrivateReads` gain search and list verbs over R04's exports, refused with `unauthorized` while capture-only, with answers admitted through the existing `admit` so a result arriving after a relock is discarded. The verbs pass the session's authorized scopes; a module cannot pass wider ones.
2. Typed request and result models live in `OhAndServices`; no `CoreHandle` becomes public.
3. Real-core root tests: search before authentication is refused; after authentication returns a seeded capture's text; a relock between request and answer discards the answer; scope exclusion holds. Existing tests pass. The exact-head macOS CI run is linked.

Source pointers: `ios/Services/Assembly/CompositionRoot.swift:145`, `:203`; `ios/Services/Authentication/PrivateReadGate.swift:18`, `:63`.

### U01e

Offer provider and route configuration clients at the composition root

Give `app/` modules typed clients for provider profiles, connection checks, route policy and credential entry.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, new client files under `ios/Services/ProviderConfiguration/`, `ios/Tests/Services/CompositionRootTests.swift`, `ios/Tests/ProviderConfiguration/` (new).

Dependencies: U01d (shared root file), V01b, V01c, V02b.

Acceptance:

1. `CompositionCoreClients` (or `CompositionContext`) gains typed clients over V01b (create, default, list), V01c (connection check) and V02b (grant, revoke, list), each taken once per root with its own reserved operation range, and the root's `CredentialService` behind a narrow interface that can add, update, delete and report status of a credential but never return a secret. No `CoreHandle` becomes public.
2. Profile writes and grants that change what leaves the device require an authenticated read session; capture-only callers are refused.
3. Real-core root tests: create a profile with a credential, set it default, grant interpretation on the capture route, and see a newly imported capture's job pinned to it (I08 + V01b resolver); the connection check against the fixture server reports `credential_rejected` for a bad key without the key appearing in any output; revoking the grant makes the job's dispatch fail authorization. Existing tests pass. The exact-head macOS CI run is linked.

Source pointers: `ios/Services/Assembly/CompositionRoot.swift:162`, `:349`; `ios/Services/Credentials/CredentialService.swift:58`.

### U01f

Manage a stored profile version's credential without exposing its reference

Let the provider-configuration services replace, check and remove the secret bound to a stored profile version, by profile version, so that settings can repair a saved profile without ever holding its credential reference.

Owned paths:
- `core/src/ffi/provider_profiles.rs`, `core/src/ffi/provider_profiles/`;
- in `core/src/providers/profiles/mod.rs`, only one read of a version's credential reference;
- `ios/OhAndCoreBridge/CoreHandle+ProviderProfiles.swift`;
- `ios/Services/ProviderConfiguration/`;
- `ios/Tests/ProviderConfiguration/`, `ios/Tests/Credentials/` (only new cases);
- in `ios/Services/Assembly/CompositionRoot.swift`, only the `ProviderCredentialEntry(...)` construction (`:1811-1812`), to pass it the profile client.

Dependencies: none.

Acceptance:

1. A provider-profiles command returns the credential reference of one stored profile version, or reports that the version is unknown or has no credential bound. It is not part of the listing. It is reachable only from `ios/Services/`: no `app/` client, view model or diagnostic receives a reference, and the listing still never returns one.
2. `ProviderCredentialEntry` gains three operations keyed by profile version: replace the secret, report the credential status, and remove the secret.
   - Each requires an authenticated read session at call time, and a capture-only session gets `notAuthenticated` without touching the core or the Keychain.
   - Each resolves the reference internally and calls the existing `CredentialService` operation. Replacing writes in place under the same reference, so the profile, its version and jobs pinned to it are unchanged and become dispatchable again.
   - An unknown version and a version with no credential bound give distinct errors. No answer or error carries a reference or a secret.
3. Removing a secret leaves the version `credentialMissing`. If other stored versions name the same reference, it affects them too; the entry documents this and a test shows it.
4. Tests:
   - **Rust:** the reference read returns the right reference, and handles unknown and unbound versions; the listing still omits references.
   - **Real-core Swift:**
     - save a profile with a stored secret, replace it by version, and show that the status is `present` and the reference is unchanged;
     - remove it by version, and show that a connection check reports `credentialMissing`;
     - show that an unknown or unbound version is refused, and that a capture-only session is refused.
   - No test output contains a reference or secret.

   `make check` and `make test` pass. The exact-head macOS CI run is linked.

Source pointers: `core/src/ffi/provider_profiles.rs:42` (`Command`), `:95`; `core/src/ffi/reset.rs:175` (existing reference read); `ios/Services/ProviderConfiguration/ProviderCredentialEntry.swift:23`; `ios/Services/Assembly/CompositionRoot.swift:1811`.

### F15

Find and fix the memory corruption in the notification reconciliation FFI tests

Find why the `notification_reconciliation` FFI tests intermittently crash the test process (SIGSEGV, heap corruption), and fix the cause, in the product code if it is there.

Owned paths: `core/src/ffi/notification_reconciliation.rs` and `core/src/ffi/notification_reconciliation/` (including its tests). If the cause is in the shared core-handle lifecycle (`core/src/ffi/core_handle/`), block and name it with evidence instead of working around it.

Dependencies: none.

Acceptance:

1. **Diagnosis.** The PR names the cause with evidence: a reproduction on Linux under parallel load (for example a CPU-saturating background load while looping `cargo test --locked --lib ffi::notification_reconciliation`), or a sanitizer or Miri finding where the toolchain on the worker supports it. A cause the evidence cannot confirm is labelled as a hypothesis. It must account for both recorded failures, including the heap corruption that had no failing assertion first.
2. **The fix addresses the cause.** If the core can call the native-effects callback, or read or write native buffers, after unregistration, after `ohand_core_close`, or after a command is abandoned, that is a product defect. Fix it here with a regression test. The test `Session` also tears down safely when a test panics: close the handle and unregister before the context is freed. That second fix alone does not satisfy this criterion. No retries, no `#[ignore]`, no weakened assertion, and no forcing of `--test-threads=1`.
3. 50 consecutive full `cargo test --locked --lib` runs under the same parallel load pass on the final head, using the loop command written in the PR. `make check` and `make test` pass.

Source pointers: `core/src/ffi/notification_reconciliation.rs:83`, `:92`, `:127`, `:173`, `:699`, `:792`; `core/src/ffi/notification_reconciliation/tests.rs:365`, `:401`, `:840`.

### F16

Make the notification probe's foreground delivery deterministic

Find why the foreground-delivery request in `testAuthorizedSchedulingBehavior` is sometimes neither delivered nor pending after a timely `add()`, and fix the cause.

Owned paths: `ios/NotificationProbe/Tests/`; in `ios/NotificationProbe/Sources/ProbeScenarios.swift`, the foreground-delivery scenario and any cleanup between the capacity scenario and it; the matching rows of `docs/validation/notification-probe.md`.

Dependencies: none.

Acceptance:

1. **Diagnosis.** The PR states the cause with evidence from run 38666074347 or a reproduction. One hypothesis to check: the capacity scenario adds 100 requests against the 64-request OS limit, and an add that completes late evicts the foreground request. A cause the evidence cannot confirm is labelled as a hypothesis.
2. **The fix addresses the cause.** No retries, no skipped or weakened assertions. Recorded OS behaviour in `docs/validation/notification-probe.md` stays accurate. If the fix changes a step, the matching row is updated.
3. The F10 counting rule applies. On the final head the test passes in 5 consecutive executions of its step. Executions where an earlier unrelated step failed are neutral. The run URLs are linked. `make check` and `make test` pass.

Source pointers: `ios/NotificationProbe/Tests/NotificationProbeAuthorizedUITests.swift:56`, `:66`, `:71`; `ios/NotificationProbe/Sources/ProbeScenarios.swift`.

### F17

Make the bench transport teardown tests deterministic under load

Make `tools/bench` transport tests that wait for an exchange to be torn down pass under CPU load, without weakening what they assert.

Owned paths: `tools/bench/src/transport/tests.rs`; in the other files under `tools/bench/src/transport/` (for example `fixture.rs`), only a test-observable teardown signal if the tests need one.

Dependencies: none.

Acceptance:

1. **Diagnosis.** The PR shows the failure under parallel load: `transport::tests::cancellation_returns_promptly_while_the_request_is_in_flight` and `transport::tests::the_dispatch_clock_deadline_ends_a_call_even_with_a_long_relative_budget` fail in `assert_exchange_torn_down`, which polls `closed_connections()` against a 3-second wall-clock window (`tools/bench/src/transport/tests.rs:679`). It states why that window is exceeded under load.
2. **The fix addresses the cause.** The tests wait for the teardown event itself, for example a channel or condition signalled when the fixture closes the connection, with a generous upper bound that only catches a hang. Both tests still assert that the call returned promptly and that the connection was closed. No retries, no `#[ignore]`, no removed assertion, no forcing of `--test-threads=1`.
3. 50 consecutive `cargo test --locked -p bench --lib` runs pass under a CPU-saturating background load on the final head, using the loop command written in the PR. `make check` and `make test` pass.

Source pointers: `tools/bench/src/transport/tests.rs:679`.

### F18

Run the app full screen instead of in legacy compatibility mode

The app's Info.plist declares neither `UILaunchScreen` nor a launch storyboard. Without one, iOS runs the app at a legacy fixed screen size, scaled up and letterboxed, on every device. `check_project_config.py` bans `UILaunchStoryboardName`, so the storyboard was removed at some point and nothing replaced it. Found by the coordinator on 2026-10-17 in a simulator walkthrough (iPhone 17, iOS 26.5).

Owned paths: `ios/AppAssembly/Info.plist` (only the new key), `ios/scripts/check_project_config.py` and `ios/scripts/test_check_project_config.py` (only the new rule and its tests), one new test file under `ios/Tests/AppAssembly/`.

Dependencies: none.

Acceptance:

1. `ios/AppAssembly/Info.plist` declares `UILaunchScreen` as a dictionary. An empty dictionary is enough; a background colour from the asset catalogue is allowed if one already exists. No storyboard is added.
2. `check_project_config.py` fails when the app's Info.plist lacks a `UILaunchScreen` dictionary. A unit test covers both the passing and the failing case. Probe Info.plists are not required to change.
3. A hosted test in `OhAndTests` asserts that the app runs at the device's native size, not a legacy one: the key window's scene bounds equal `UIScreen.main.bounds`, and the screen height is greater than 667 points on the CI simulator. Today, before the fix, this test must fail. Show the failing run on the parent commit, or explain why it cannot be shown.
4. The exact-head macOS CI run is linked, and its smoke screenshot shows the app filling the screen. `make check` passes.

Source pointers: `ios/AppAssembly/Info.plist`; `ios/scripts/check_project_config.py:245` (`check_plists`), `:268` (banned storyboard key).

### A01

Read one item's detail through a gated root client

Export a read of one item by opaque identifier, and give `app/` a typed, read-gated client for it.

Owned paths: `core/src/ffi/item_detail.rs` and `ios/OhAndCoreBridge/CoreHandle+ItemDetail.swift` (native-boundary allowance); `ios/Services/Items/Detail/` (new); `ios/Tests/ItemDetail/` (new); registrations under the capability client allowance (root and read gate).

Dependencies: none.

Acceptance:

1. The export returns, for one item ID:
   - current text with its basis, the original capture text, and the corrected text where one exists;
   - item revision, type, scope, session topic, lifecycle state and capture time;
   - the original audio reference, and transcript provenance where present;
   - the item's event and correction history, newest first and bounded.

   It reuses `load_item_state` (`core/src/domain/items/mod.rs:233`), `get_events_for_item` (`core/src/store/events/mod.rs:916`) and the transcript provenance read (`core/src/ingress/transcription/mod.rs:639`). An unknown or deleted item returns a fixed `not_found`. The caller's read scopes are enforced, and an item outside them is indistinguishable from not found.
2. The client is a public typed service on the root. It is refused while the session is capture-only and admitted through the read gate, so an answer that arrives after a relock is discarded. The item ID is validated as opaque, and nothing parses meaning out of it.
3. Tests:
   - Rust tests for each field, including a corrected item, a transcribed voice item, an excluded scope and a deleted item.
   - Real-core root tests for refusal before authentication, a relock discard, and a seeded item read end to end.
   - `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A02

Correct an item's text, type and session topic

Export the user's corrections as compare-and-set commands, and give `app/` a typed client for them.

Owned paths: `core/src/ffi/item_corrections.rs` and `ios/OhAndCoreBridge/CoreHandle+ItemCorrections.swift`; `core/tests/item_corrections/` (new); `ios/Services/Items/Corrections/` (new); `ios/Tests/ItemCorrections/` (new); registrations under the capability client allowance.

Dependencies: none.

Acceptance:

1. Text, type and session-topic corrections each take the item ID, the expected revision and an idempotent command ID.
   - Each is stored through the existing event path (`save_event`, `core/src/store/events/mod.rs:440`, which re-syncs the search index), never as a direct row edit.
   - A stale revision is refused with a fixed `stale_revision` failure that reports the current revision.
   - A repeated command ID reports the first outcome and writes nothing new.
   - The original capture is never altered.
2. Correcting the type away from an actionable type is decided in the core, with a test. If the item has a live reminder, it is cancelled through the existing reminder state path, or the correction is refused with a fixed reason. The choice and its contract basis are written in the PR.
3. A user correction always beats later model output and transcripts. The existing regressions still hold, and one new test covers a correction made while an interpretation job is in flight.
4. The client requires an authenticated session.
5. Tests: Rust and real-core root tests. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A03

Complete, cancel and quiet an item

Export item completion, cancellation, not-now and stop-suggesting as user commands keyed by item, and give `app/` a typed client for them.

Owned paths: `core/src/ffi/item_lifecycle_controls.rs` and `ios/OhAndCoreBridge/CoreHandle+ItemLifecycleControls.swift`; `core/tests/item_lifecycle_controls/` (new); `ios/Services/Items/LifecycleControls/` (new); `ios/Tests/ItemLifecycleControls/` (new); registrations under the capability client allowance.

Dependencies: none.

Acceptance:

1. Complete and cancel reuse `end_item_with_event` (`core/src/reminders/state/mod.rs:817`) with the same outcome as the notification action path (`core/src/ffi/notification_actions.rs:314`), but keyed by item ID and expected revision, not by a notification identifier.
2. Not-now reuses `record_not_now` (`core/src/suggestions/eligibility/mod.rs:381`), and stop-suggesting reuses the control event and `set_pull_only` (`:414`).
3. Every command is idempotent by command ID and refuses stale revisions. The resulting reminder and notification changes go through the next reminder pass (`root.reconcileReminders`), never through a direct native effect.
4. The client requires an authenticated session.
5. Tests: Rust tests for each command, including on a deleted item. Real-core root tests showing that completing an item with a live reminder leaves no pending notification after the pass. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A04

Read, edit, cancel and review an item's reminder

Export the reminder read, user time correction, cancellation and history, and give `app/` typed clients for them. Reads are gated.

Owned paths: `core/src/ffi/reminder_controls.rs` and `ios/OhAndCoreBridge/CoreHandle+ReminderControls.swift`; `core/tests/reminder_controls/` (new); `ios/Services/Reminders/Controls/` (new); `ios/Tests/ReminderControls/` (new); registrations under the capability client allowance.

Dependencies: none.

Acceptance:

1. **Read.** Returns the item's reminder: requested and resolved instant, time zone, the states the item status already reports, and any unsupported or ambiguity reason with its state version. It reuses `get_reminder` (`core/src/reminders/state/mod.rs:859`).
2. **Edit.** Applies `apply_user_time_correction` (`:595`) with the item revision and reminder version (`:384`). A stale version is refused. An edit can never backdate a reminder or silently substitute a different deadline.
3. **Cancel.** Applies `cancel_reminder` (`:686`).
4. **History.** Returns the bounded reminder history (`core/src/reminders/history/view.rs:149`).
5. Reads go through the read gate. Writes require an authenticated session, and their native effects go through the next reminder pass.
6. Tests: Rust tests, plus real-core root tests for edit and cancel reaching the notification centre after a pass. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A05

Delete an item and report deletion progress

Export per-item deletion intent and progress, and run the cleanup promptly through the lifecycle coordinator's existing deletion service.

Owned paths: `core/src/ffi/item_deletion.rs` and `ios/OhAndCoreBridge/CoreHandle+ItemDeletion.swift`; `core/tests/item_deletion/` (new); `ios/Services/Deletion/ItemDeletion/` (new); `ios/Tests/ItemDeletion/` (new); registrations under the capability client allowance, including one `LifecycleCoordinator` accessor for `deletionCleanup` (`ios/Services/Assembly/LifecycleCoordinator.swift:270`).

Dependencies: none.

Acceptance:

1. Marking an item deleted reuses `mark_deletion_intent` (`core/src/lifecycle/delete_intent/mod.rs:211`). It tombstones the item, cancels its jobs and reminders, and enqueues cleanup in one transaction, idempotently. Progress reuses `deletion_progress` (`:370`).
2. After the intent commits, the client triggers the existing deletion cleanup service immediately. It does not wait for the next launch, it never deletes native files itself, and a failure leaves the intent to be resumed by the normal lifecycle pass.
3. Requires an authenticated session. A deleted item disappears from search and detail reads, and a late job result for it is refused, per the existing tests.
4. Tests: Rust tests, plus a real-core root test in which deletion removes the item's pending notification and retained audio and reports progress through to completion. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A06

Activate and answer a suggestion

Export prompt activation and response as user commands, and give `app/` a typed client for them.

Owned paths: `core/src/ffi/suggestion_responses.rs` and `ios/OhAndCoreBridge/CoreHandle+SuggestionResponses.swift`; `core/tests/suggestion_responses/` (new); `ios/Services/Suggestions/Responses/` (new); `ios/Tests/SuggestionResponses/` (new); registrations under the capability client allowance.

Dependencies: none.

Acceptance:

1. Activation reuses `PromptCoordinator::activate` (`core/src/suggestions/coordinator/mod.rs:157`) for a delivered or opened prompt, and returns the chosen item's identifier and the text that may be displayed. It discloses nothing before authentication. A prompt that no longer has an eligible item reports that honestly.
2. Responses (not now, stop, done) reuse `respond` (`:201`) and are idempotent by command ID.
3. The client requires an authenticated session.
4. Tests: Rust tests for each response and for a stale or replaced prompt, plus a real-core root test. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A07

Control job configuration after policy changes

Export the existing requeue and revocation controls, and add the missing core path that moves waiting unpinned work onto a newly authorized profile.

Owned paths: `core/src/jobs/configuration/` (only for the new re-pin operation and its tests); `core/src/ffi/job_configuration.rs` and `ios/OhAndCoreBridge/CoreHandle+JobConfiguration.swift`; `core/tests/job_configuration_controls/` (new); `ios/Services/JobConfiguration/` (new); `ios/Tests/JobConfiguration/` (new); registrations under the capability client allowance.

Dependencies: V01b, V02b.

Acceptance:

1. Export `requeue_job_to_new_profile` (`core/src/jobs/configuration/mod.rs:239`), `get_requeue_record` (`:201`), and a bounded listing of jobs pinned to a profile (`get_jobs_pinned_to_profile`, `:59`, with item and status only).
2. Profile revocation is available outside reset. It reuses `revoke_profile` and the job retirement path (`:136`, `:148`) and respects the L05b credential claim fence.
3. **New: re-pin waiting work.**
   - An explicit, user-invoked operation re-pins unpinned `interpret` jobs that are waiting for configuration on a route, plus the uninterpreted or unprocessed items of that route that have no live job. They move to the profile the I08/V01b resolver now selects, only when that profile is authorized for the route.
   - It never runs implicitly from a grant, a default change or a classifier.
   - It is bounded and idempotent, it never changes a job pinned to another profile, and it returns the count it moved.
4. The client requires an authenticated session.
5. Tests:
   - Rust tests for each operation, including a revoked profile and a route without a grant.
   - A real-core root test: capture with no provider, configure and grant, re-pin, and the job dispatches to the fake provider.
   - `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A08

Configure retention and act on unresolved sole audio

Persist the user's retention period, and expose retention status, the sole-audio cases and their retry and delete actions to `app/`.

Owned paths: one migration step appended to `MIGRATIONS` in `core/src/store/schema/mod.rs` (the next version after V01b's) and, in `core/tests/migrations/main.rs`, only the schema-version expectations it forces and its own migration test; `core/src/lifecycle/retention/` (only the persisted-setting read and the new status query); `core/src/ffi/retention_controls.rs` and `ios/OhAndCoreBridge/CoreHandle+RetentionControls.swift`; `ios/Services/AudioRetention/`; `ios/Tests/AudioRetention/`; registrations under the capability client allowance, including one `LifecycleCoordinator` accessor for `audioRetention` (`ios/Services/Assembly/LifecycleCoordinator.swift:277`); in `core/src/ffi/audio_retention.rs`, only `Command::policy` reading the stored setting (after A14).

Dependencies: V01b (migration order), A14 (user-initiated removal decision; shared `ffi/audio_retention.rs`), C07 (transcription requeue).

Acceptance:

1. The retention period is a durable setting with a seven-day default after a successful transcription. Bounds are enforced. The sweep reads the stored setting instead of the init parameter, and changing it never deletes audio whose item is its sole source.
2. A status read returns the policy, the storage pressure the service already measures, and the unresolved sole-audio cases (item, capture time, reason).
3. "Retry transcription" re-queues the item's on-device transcription through the existing job path (C07's requeue function).
4. "Delete audio" removes only the audio of an item that has a transcript or text, through the existing cleanup ordering and A14's user-initiated decision inside the removal fence. Sole-source audio is refused and pointed to item deletion (A05).
5. Reads are gated and writes require an authenticated session. A sweep never blocks a new capture.
6. Tests: Rust tests and real-core root tests. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### A09

Play retained audio safely

Give `app/` a way to play an item's retained audio without exposing protected storage, and fence playback against removal.

Owned paths: `ios/Services/AudioRetention/` (only the playback seam), `ios/Services/Playback/` (new), `ios/Tests/Playback/` (new), registrations under the capability client allowance.

Grant (coordinator, 2026-10-16, after PR #252 round 1). A05 has landed. In addition to the paths above, A09 may make these edits and no others:

1. `ios/Services/Deletion/ItemDeletion/ItemDeletionService.swift`: one setter for a deletion-intent observer, following the `setCleanupProvider` pattern, and one call to it in `delete`. The call comes after `Self.recorded(from:)` returns, so the core has recorded the intent (a replay counts), and before cleanup runs. It is never made for a refused, stale, failed or unreadable deletion.
2. `ios/Services/Deletion/DeletionCleanupService.swift`, `DeletionLocalCleanup.swift` and, if needed, one new retry reason in `DeletionCleanupModels.swift`: an optional audio-removal fence. It defaults to none, so existing construction and tests are unchanged. `removeAudio` consults it. A held file stays in place, and the step stays pending for retry without being counted toward terminal failure.
3. In `ios/Services/AudioRetention/`, the fence covers every removal path of `AudioRetentionService`, including A14's `removeAudioForUser` as well as the sweep.
4. `ios/Services/Assembly/LifecycleCoordinator.swift`: one setter or late-bound provider that passes the fence to the `DeletionCleanupService` and `AudioRetentionService` built in `connect` (`:529-531`).
5. `ios/Services/Assembly/CompositionRoot.swift`: inside A09's own registration block, connect the playback service as the deletion-intent observer and as the lifecycle fence, and clear both in its shutdown line. Root behaviour otherwise does not change.

Resubmission adds a deterministic regression through the production `ItemDeletionService` and root seam. It shows that a stale or refused deletion leaves playback open, and that a recorded deletion ends playback before cleanup runs. The exact-head macOS CI run is linked in the PR body and the task result.

Dependencies: A08 (shared retention service).

Acceptance:

1. For an authenticated session and an item whose audio is retained, the root returns a playable handle, such as a URL valid only for the session or an `AVAudioPlayer` built by the service. It is never a path into `ProtectedStorageLayout` (`ios/Services/ProtectedStorage/StoreProtectionPolicy.swift:103`) that outlives the session. An item without retained audio reports that honestly.
2. While playback is open, the retention sweep and deletion cleanup do not remove that file. A deletion intent ends playback first. Relocking ends playback.
3. Tests cover the fence against both the sweep and deletion, a relock, and audio that is missing or unreadable. Existing retention tests pass. The exact-head macOS CI run is linked.

### A10

Route notification taps and reminder readiness to the app

Connect prompt taps to an app-visible handler, and let the app observe reminder readiness changes. Root chain.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Services/Notifications/NotificationEventIngestor.swift` (only `promptHandler` wiring), `ios/Tests/Services/CompositionRootTests.swift`.

Dependencies: T11d (root chain).

Acceptance:

1. The root builds its ingestor with a `promptHandler` (`CompositionRoot.swift:661`; today the default `{ _ in }`, `NotificationEventIngestor.swift:23`). A prompt tap reaches a public open-suggestion handler on the root, like `setOpenItemHandler`, and is never dropped. A tap that arrives before a handler is set is held and delivered once.
2. The root exposes an observer for reminder-report changes (`lastReminderReport`, `:897`). In particular it reports when a reminder becomes undeliverable because permission was never requested (`NotificationReadiness.swift:95`), so the app can ask at that point.
3. Real-core root tests cover a prompt tap reaching the handler and readiness changes notifying the observer. Existing tests pass. The exact-head macOS CI run is linked.

### A11

Compose export at the root

Build the existing export service in production, over the root's read session and storage layout, with its launch cleanup.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Services/Export/` (only construction and visibility), `ios/Tests/Services/CompositionRootTests.swift`, `ios/Tests/Export/`.

Dependencies: A10 (root chain).

Acceptance:

1. The root exposes `ExportService` (`ios/Services/Export/ExportService.swift:15`, never constructed in production today) built over the authenticated read session and the protected layout. The raw `CoreExportClient` stays private.
2. The export temp-file sweep (`sweepAtLaunch`) runs at launch in the lifecycle order without delaying capture.
3. Real-core root tests: export is refused before authentication, a real export round-trips, and a stale temp export is removed at the next launch. The exact-head macOS CI run is linked.

### A12

Compose reset at the root

Build the existing reset service in production: put the reset ingress gate into the capture path, and resume an interrupted reset at launch.

Owned paths: `ios/Services/Assembly/CompositionRoot.swift`, `ios/Services/Assembly/LifecycleCoordinator.swift` (only the reset resume step), `ios/Services/Reset/` (only construction and visibility), `ios/Tests/Services/`, `ios/Tests/Reset/`.

Dependencies: A11 (root chain).

Acceptance:

1. The root constructs `ResetService` and its collaborators. Today they are internal and never built. It exposes a typed reset control and progress read to `app/`, and requires an authenticated session.
2. `ResetIngressGate` sits in the root's ingress path. During a reset, new captures behave as the reset contract says, and a capture is never silently lost.
3. An interrupted reset resumes at launch before the job runner activates. The L05b credential fence and the reset generation hold.
4. Real-core root tests cover a full reset, a capture during reset and a resume after an interrupted reset. Existing reset tests pass. The exact-head macOS CI run is linked.

### A13

Present an item or suggestion from the app host

Let the management host navigate to an item's source detail or to the current suggestion when asked by a notification tap or another screen.

Owned paths: `ios/AppAssembly/ManagementNavigationModel.swift`, `ios/AppAssembly/AppRootView.swift`, `ios/AppAssembly/ManagementCatalog.swift` (only a destination registry for item and suggestion views, which U02b and U08b fill in under the management context allowance), `ios/Tests/AppAssembly/`.

Dependencies: none.

Acceptance:

1. `requestedItemID` (`ios/AppAssembly/ManagementNavigationModel.swift:84`) and a new requested-suggestion route present a destination once one is registered. If no destination is registered, the app shows an honest placeholder, not a blank screen.
2. Presenting either requires authentication first. A capture entry still always lands on capture.
3. The host exposes a single registration point, one line per destination, so that U02b registers source detail and U08b registers the suggestion view without editing host behaviour.
4. UI and unit tests cover a tap before and after authentication, a relock while presented, and the placeholder. The exact-head macOS CI run is linked.

### A14

Let the user remove an item's audio through the removal fence

Add a user-initiated removal decision to the L03b fence, so the audio of an item that has text can be removed on request and never the audio that is an item's only source.

Owned paths: `core/src/lifecycle/retention/mod.rs` (only the new user-initiated authorization) and its tests; `core/src/ffi/audio_retention.rs` and `core/src/ffi/audio_retention/lease.rs` (only selecting the new authorization for a user-initiated removal); `ios/OhAndCoreBridge/CoreHandle+AudioRetention.swift` if the command shape changes; and their tests.

Dependencies: none.

Acceptance:

1. A user-initiated removal is approved when the item is live, the audio reference still matches, and recoverable text exists: a usable transcript, or captured or user-written text (including `TextWithoutTranscript`). It is denied, with a fixed reason, when the audio is the item's only source, when the item is deleted (deletion owns cleanup), or when the reference no longer matches.
2. The decision is made inside the existing fence (`begin` in `lease.rs`) under the same store write lock, so a concurrent text correction that makes the audio sole-source cannot race the removal. Automatic sweeps still use `authorize_audio_removal` unchanged.
3. Rust tests cover approval with transcript, approval with typed text and no transcript, denial for sole-source audio, denial after deletion, denial after an audio-reference change, and a correction racing a begun removal. `make check` and `make test` pass. If the Swift extension changes, the exact-head macOS CI run is linked.

Source pointers: `core/src/lifecycle/retention/mod.rs:320`; `core/src/ffi/audio_retention/lease.rs:115`, `:136`.

### C07

Queue on-device transcription for imported voice captures

Create the `transcription_attachment` job that the native transcription handler (C05b) waits for. Today nothing in production creates one: import stores an audio-only capture as `audio_pending` (`core/src/ingress/import.rs:366`) and queues nothing, so voice captures are never transcribed.

Owned paths: `core/src/ingress/transcription/queue.rs` (new; its `mod` line in `core/src/ingress/transcription/mod.rs`), `core/src/ingress/import.rs` (only the call in the import transaction), one call at the start of each drain in `core/src/ffi/job_runner.rs`, and their tests.

Dependencies: none.

Acceptance:

1. **On import.** The import transaction that creates an audio-only item (`audio_pending`) also enqueues one `transcription_attachment` job for it at the item's revision, with no profile pin (transcription is on-device only and `authorize_job` treats it as local). An import retry that finds the existing item queues nothing new. A capture with text queues no transcription job.
2. **Backfill.** At the start of each job drain, every non-deleted `audio_pending` item with no queued, running or completed `transcription_attachment` job gets one, idempotently and in bounded batches. This covers voice captures already stored before this fix.
3. **Explicit retry.** A core function requeues transcription for one item: it moves `transcription_failed` or `transcription_unsupported` back to `audio_pending` and enqueues a new job in one transaction. It refuses a deleted item, an item whose audio was removed, an item already `transcribed` or `not_applicable`, and an item with a queued or running transcription job. It is a plain core function; A08 exports it.
4. **End to end.** A Rust test imports an audio-only capture, drains with a fake recognizer through the existing transcription apply path, and sees the transcript attached and the interpretation job queued. Further tests cover retry idempotency, backfill idempotency and each refusal. `make check` and `make test` pass.

Source pointers: `core/src/ingress/import.rs:366`; `core/src/privacy/routing/mod.rs:30`, `:226`; `core/src/jobs/queue/mod.rs:162`; `ios/Services/Transcription/TranscriptionJobHandler.swift:23`.

### I05b

Extend reminder-withdrawal grammar coverage

Add the withdrawal phrasings and over-rejection fixes that fall outside the bounded grammar recorded for I05, driven by a fixture corpus so future phrasings are added as data.

Owned paths: `core/src/interpretation/apply/`, `core/tests/proposal_application/`.

Dependencies: I05.

Acceptance:

1. A synthetic fixture corpus under `core/tests/proposal_application/` lists withdrawal phrasings that must stay unscheduled and controls that must schedule; the behavior test is driven by that corpus, and every I05 construction is carried over unchanged.
2. The known over-rejections are scheduled again without reopening withdrawals: a gerund or infinitive task object after "about" ("about cancelling the gym"), coordinated verbs inside the requested task ("call the roofer and cancel the order", "take the ladder back"), and "need to <verb>" complements ("so I don't need to worry").
3. Each withdrawal still produces an uninterpreted-for-reminder result with source and non-reminder facets intact, and no reminder rows or operations; each control still produces exactly one scheduled reminder.

Source pointers: `core/src/interpretation/apply/mod.rs` (`retracted_after`, `wish_has_ended`, `verb_is_negated_or_infinitive` on PR #96), `core/tests/proposal_application/main.rs`.

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

Note (2026-10-08): B01c's `ohand_core_start_save_capture` is a pre-import capture write that accepts any non-empty route identifier. C02a owns unknown-route rejection and the contracted ingestion acknowledgment; see the recorded decision under B01c.

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

Grant (coordinator, 2026-10-17, after PR #269 round 2): playback lives in `ios/Services/Playback/`, owned by A09, which has landed. U02b may make two narrow changes there.

1. **Natural completion.** `PlaybackSession` learns that its player finished, and `PlaybackEndReason` gains `.finished`. On finish the session:
   - ends itself;
   - releases its fence lease exactly once;
   - stays idempotent under a later `close()`, relock, deletion or shutdown, with no second release and no second end callback.

   Files: `ios/Services/Playback/PlaybackSession.swift` and `PlaybackFence.swift`.
2. **Availability before the tap.** Audio retention removes the file and records the removal only in its native ledger. The core's `audio_reference` stays set, so source detail cannot tell from the item read that the audio is gone. `PlaybackService` gains one read-only query, in `ios/Services/Playback/PlaybackService.swift`. For an item it reports whether the audio can be played now, using the same checks `open(itemID:)` makes before the fence:
   - the current authorization;
   - `readAudio`;
   - `finalizedAudioURL`;
   - `fileSystem.isPresent`.

   It takes no lease, opens no player, reads no audio bytes and needs an authenticated session. Its outcomes: available, no audio, missing, unreadable and not authenticated. `SourceDetailModel.load` uses it, so removed audio first renders as the disabled notice with no Play button. The tap-time `audioMissing` handling stays, for removals after load.

Owned-path additions: those three files (only the changes above) and service tests under `ios/Tests/Playback/`.

Required tests:
- **Service tests:**
  - completion gives `.finished`, releases once, and a later close or relock changes nothing;
  - availability for present, never-recorded and removed audio, and for a locked session.
- **Source detail test on the production retention path:** remove a synthetic item's audio through the real `AudioRetentionService` (an expiry sweep or a user removal) before opening detail. Assert that the first render shows the disabled notice and no Play button. Setting `captures.audio_reference` to NULL by hand does not count.

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

Dependencies: U05a, U01f (credential operations by profile version).

Rework (coordinator, 2026-10-17, after PR #267 round 1): use U01f's operations to replace, check and remove the credential of a saved profile version. A secret stored for a draft must never be orphaned. Either defer the Keychain add until the profile saves, or delete a reference this screen added but never saved when the draft is cleared, abandoned, edited or the session relocks. Test whichever is chosen. Reject a secret with leading or trailing whitespace instead of trimming it. The view tests must cover: the secret field emptied after store success and failure, on disappear and on leaving the foreground; the secret never in `renderReporter` or the accessibility value. Real-core cases must cover a stored-then-checked secret, a removed secret reporting `credentialMissing`, and an invalid reference reported redacted.

Grant (coordinator, 2026-10-17, after PR #283 round 1; posted on the PR): replace the durable reference journal with a Keychain sweep. Five review rounds found new crash windows in keeping a separate journal in step with saves. A sweep has no second record to drift.

- **List.** `CredentialService` gains a reference listing:
  - one `SecItemCopyMatching` on its service name, with `kSecMatchLimitAll` and `kSecReturnAttributes`, never `kSecReturnData`, so no secret bytes are read;
  - it returns the account names, which are the references;
  - a listing failure, including the device not yet being unlocked since restart, is an error, never an empty list.
- **Lease.** `CredentialService` keeps an in-process lease set, guarded by a lock.
  - `addCredential` leases the new reference before the Keychain insert.
  - The caller releases the lease only after the core has committed a profile that holds the reference, or after deleting the secret.
  - A dead process's leases vanish with it, which is correct.
- **Sweep.** A sweep in `ios/Services/ProviderConfiguration/` runs in this order:
  1. list the Keychain references K;
  2. read the leases L;
  3. read every reference any profile version holds, revoked ones included, plus the reset claims (the core's `profile_credentials` and `credential_claims`).

  It then deletes K minus all of those. This order is load-bearing:
  - a reference added after step 1 is not in K;
  - a save that commits and releases its lease between steps 2 and 3 is caught at step 3.

  It skips the whole pass while a reset is pending, and does nothing if either listing fails.
- **Trigger.** The sweep runs when the provider settings screen loads, before it accepts a save. That screen is the only creator of references, so each visit cleans up after any earlier interrupted run. The root's launch order is not changed.
- **Removed.** `UserDefaultsReferenceJournal`, `ProviderSettingsPendingSave` reconciliation and journal-based restart cleanup. A draft cleared, abandoned, edited away or relocked still deletes its reference at once, best effort. The sweep is the backstop.

Owned-path additions: the listing and lease in `ios/Services/Credentials/CredentialService.swift` and `KeychainBoundary.swift`; the sweep under `ios/Services/ProviderConfiguration/`; tests under `ios/Tests/Credentials/` and `ios/Tests/ProviderConfiguration/`; and the one-line DS06 call site in `ios/Tests/AppAssembly/SettingsRootViewTests.swift`, needed to compile against the new initializer.

Required tests:
- two interrupted saves of the same profile with exactly one committed: after a restart, the sweep keeps the committed secret and deletes the other;
- an interruption at each step of a save: before the insert, after the insert, during the core commit, and after commit but before the lease release;
- a sweep racing a save in the same process deletes nothing live;
- a listing failure deletes nothing;
- a pending reset skips the pass;
- a revoked version's reference is kept.

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

### DS01

Build the design-system foundations

Add the tokens, fonts and shared components from the M1 UI design, so every screen builds on one system instead of local styling.

Owned paths: `app/design-system/` (new), `ios/Tests/DesignSystem/` (new), `ios/AppAssembly/Info.plist` (only `UIAppFonts`), `ios/project.yml` (only what bundling the fonts needs).

Dependencies: F18 (shared `Info.plist`).

Acceptance:

1. Colour tokens exist as dynamic colours with the exact light and dark values in the design document's token table. Type styles, spacing, radius and elevation constants match it.
2. Instrument Serif regular and italic TTF are bundled with `OFL.txt`, registered in `UIAppFonts`, and used through `Font.custom(_:size:relativeTo:)`.
3. These components exist, each with a SwiftUI preview in light and dark: PillButton (primary, secondary, plain, destructive), GlassButton, GlassPill, Card, GroupedSection with ChoiceRow and ToggleRow, Chip, SavedPill, Composer (the three visual states, with no capture wiring), Mark. Each meets the 44 pt touch-target rule.
4. Tests:
   - a token test computes WCAG contrast for every text pair the document uses (ink, inkMuted, accent, destructive and onAccent on their grounds) in both themes and fails below 4.5:1, or 3:1 for 24 pt and larger;
   - a test asserts that the bundled font loads.
5. No existing screen changes in this task. The exact-head macOS CI run is linked.

### DS02

Produce the app icon and launch screen

Ship the icon and the launch screen in the M1 UI design: the italic & mark on brand indigo.

Owned paths: a new asset catalogue under `ios/AppAssembly/`, `ios/project.yml` (only the catalogue and `ASSETCATALOG_COMPILER_APPICON_NAME`), `ios/AppAssembly/Info.plist` (only `UILaunchScreen` contents), and a generator script under `ios/scripts/`.

Dependencies: DS01 (bundled font), F18 (shared `Info.plist` key).

Acceptance:

1. A script generates the mark from the bundled font's glyph outline (no hand-drawn approximation). It writes the 1024 px app icon in three variants: default (white mark on `brand` with a soft top light), dark (`#8E9BFF` mark on `#0A0B12`) and tinted (one luminance shape). The script is deterministic and its outputs are committed.
2. `UILaunchScreen` uses a background colour from the catalogue (`brand` in light, `#0A0B12` in dark) and the mark image, centred, with no text.
3. `check_project_config.py` still passes; the app builds with the icon set. The exact-head macOS CI run is linked, and its smoke screenshot shows the launch screen or the app at full screen.

### DS03

Try Face ID quietly at launch and on return to the foreground

Make an unobtrusive authentication attempt whenever the app becomes active, so the owner usually sees their recent captures without asking, while capture never waits on authentication.

Owned paths: `ios/Services/Authentication/` (the boundary and session only), `ios/Tests/Authentication/`, `ios/AppAssembly/AppServices.swift` (the launch and foreground trigger), and in `ios/Services/Assembly/CompositionRoot.swift` one public method beside `authenticateReads` that requests the quiet attempt.

Dependencies: A12 (root chain).

Acceptance:

1. The boundary gains a quiet mode. It uses `.deviceOwnerAuthenticationWithBiometrics` with `localizedFallbackTitle` set to empty. It returns `unavailable` without prompting when biometrics are unavailable, not enrolled or locked out. The existing explicit `authenticate` (biometrics with passcode fallback) is unchanged.
2. On launch and on each `willEnterForeground`, when the session is `capture_only`, the app makes one quiet attempt. A success authenticates the session exactly as today. A failure, cancellation or unavailability leaves `capture_only`, shows no error, and doesn't retry until the next foreground.
3. A quiet attempt in flight never blocks text entry, recording or saving. An explicit unlock tap supersedes it.
4. Relock on background is unchanged.
5. Tests use the fake boundary to cover success, each failure, unavailable, a foreground while an attempt is in flight (no second prompt), and an explicit unlock superseding a quiet one. The capture path stays usable throughout. The exact-head macOS CI run is linked.

### R06

Read recent captures for the home screen

Add a gated read that lists the most recent items, so the home can show recent captures without a search query.

Owned paths: a new `core/src/ffi/recent_items.rs` (and its tests), the minimal query it needs in `core/src/retrieval/` or `core/src/items/`, a new `ios/OhAndCoreBridge/CoreHandle+RecentItems.swift`, and a client and service under a new `ios/Services/RecentItems/` with a registration under the capability client allowance and one gated verb in `PrivateReadGate.swift`.

Dependencies: none.

Acceptance:

1. The read returns up to `limit` items (default 30, maximum 100), newest capture first, with a cursor for the next page. Each item has its id, current revision, item type (absent when not yet classified), the display text (the interpreted title when there is one, otherwise the source text), the capture time and the source kind (text or voice).
2. It applies the same read scopes and exclusions as retrieval: deleted and tombstoned items never appear, and a work-only read never returns personal items.
3. It goes through the private read gate. A `capture_only` session gets `read_not_authenticated`, and an answer that arrives after a relock is discarded.
4. Rust tests cover ordering, paging, scopes, deletion and unclassified items. Swift tests cover the gate. `make check` and `make test` pass. The exact-head macOS CI run is linked.

### DS04a

Rebuild the capture surface as the composer home

Replace the capture screen's layout with the composer home from the M1 UI design: a glass header, the composer at the bottom and an empty body. Text and voice capture behaviour is unchanged.

Owned paths: `ios/Capture/Entry/CaptureEntryView.swift`, `ios/Capture/Text/TextCaptureView.swift`, the voice capture view code under `ios/Capture/Voice/` (views only), `ios/AppAssembly/AppRootView.swift`, new files under `app/home/`, and their tests under `ios/Tests/CaptureEntry/`, `ios/Tests/TextCapture/` and `ios/Tests/VoiceCapture/`.

Dependencies: DS01.

Acceptance:

1. The header floats on glass: Search, the wordmark, and Settings, which becomes Done while the keyboard is up. The keyboard never covers the header or the composer, and Done dismisses it.
2. The composer follows the design.
   - At rest it shows the italic placeholder and the mic.
   - While typing it grows with the text and shows the record and Save buttons.
   - While recording it becomes the recording panel, with Discard and Stop.
   - After a save it clears, keeps the keyboard up, and shows the SavedPill for about 2 s.
   - Every existing outcome is still reported, including the honest unrecoverable and kept-for-retry outcomes; the status paragraph is replaced with short messages.
3. Text and voice ingress, handoff, recovery and durability are unchanged. Every existing test passes, updated only where it asserted layout or text.
4. At regular width, the composer is centred with a maximum width of about 500 pt. The layout passes the design checklist. The exact-head macOS CI run is linked.

### DS04b

Show recent captures and the lock states on the home

Fill the home body with recent captures when reads are authenticated, and show the unlocking and locked states otherwise.

Owned paths: `app/home/` and its tests.

Dependencies: DS04a, DS03, R06.

Acceptance:

1. When the session is `authenticated`, the body lists recent captures from R06. They are grouped under Today and Earlier, use Cards with a type dot, type and relative date, and load more pages on scroll. At regular width they form two columns under a large "Today".
2. While a quiet attempt is in flight, the body shows placeholder cards. When the session is `capture_only`, it shows the locked card, whose Unlock button runs the explicit authentication. In neither state does anything derived from stored history appear: no text, counts or badges.
3. A new save shows at the top as "Just now · sorting…" until the item is interpreted. A relock clears the list immediately.
4. Tests cover the three states, the relock clearing the list, and the fact that a read in `capture_only` is never attempted. The exact-head macOS CI run is linked.

### DS05

Restyle Search

Rebuild the Search screen's presentation per the M1 UI design: a glass header with a search pill, filter chips and result cards. Search behaviour and filters are unchanged.

Owned paths: `app/retrieval/` (views only) and `ios/Tests/Retrieval/` (layout assertions only).

Dependencies: DS01.

Acceptance:

1. The search field is a GlassPill and is focused on entry. The type, time and topic filters are Chips that open menus, and the active filters show as selected chips. Results are Cards, with the matched words highlighted and an "N results" line. The empty and no-match states use the empty-state pattern.
2. Every existing retrieval behaviour test passes. The read gate, locked state and scope filtering are unchanged.
3. At regular width, the results sit in a left column with room for an item pane, which U02b fills. The layout passes the design checklist. The exact-head macOS CI run is linked.

### DS06

Restyle Settings, Audio and Daily prompt

Rebuild the Settings root, Audio retention and Daily prompt presentation per the M1 UI design. Settings behaviour is unchanged.

Owned paths: `ios/AppAssembly/ManagementCatalog.swift` (`SettingsRootView` only), `app/data-settings/` (the retention views only), `app/prompt-settings/`, and their tests.

Dependencies: DS01, U07b (shared `app/data-settings/`).

Acceptance:

1. The Settings root has a large serif title and grouped sections with current values. "Erase everything" is a separate destructive card (wired by U07c), and the version line is at the bottom.
2. Audio and Daily prompt each have a serif title, one grouped section per decision, one footer line and an info sheet carrying the full explanation the screens show today.
3. Every behaviour and authentication test passes. The layout passes the design checklist. The exact-head macOS CI run is linked.

### DS07

Restyle Processing

Rebuild the provider settings presentation per the M1 UI design: the "Sort my captures" choice first, with providers and the advanced controls one level down.

Owned paths: `app/provider-settings/` (views only) and its tests.

Dependencies: DS01, U05b (shared path).

Acceptance:

1. The screen shows a "Sort my captures" choice with one-line details, one footer line, "Add provider", and an Advanced screen. The Advanced screen holds the profile, model, timeout, credential and connection-check controls built by U05a and U05b. No control is removed and no capability is lost.
2. Every behaviour test passes, including credential redaction. The layout passes the design checklist. The exact-head macOS CI run is linked.

### DS08

Capture a screenshot walkthrough in CI

Add a UI-test walkthrough that captures every main screen in both appearances, so reviewers can see what a change looks like.

Owned paths: a new UI-test target under `ios/UITests/Walkthrough/`, `ios/project.yml` (that target and its scheme only), `.github/workflows/ios.yml` (one job), and `ios/scripts/` for any helper.

Dependencies: F18.

Acceptance:

1. On every pull request, the walkthrough launches the app on a simulator with synthetic captures. It visits the home, Search with a query, Settings and each settings screen, in light and dark, at compact width and, when a regular-width simulator is available, at regular width. It writes one PNG per screen. Simulated Face ID enrolment and matching are scripted, and authentication is never bypassed in app code.
2. The job uploads the screenshots as an artifact with `retention-days: 7`. A screen the walkthrough cannot reach fails the job, not just the screenshot.
3. The exact-head CI run is linked, along with a list of the screenshots it produced.

Grant (2026-10-17, after DS06 landed): DS06 made the Settings rows and the Audio screen's Export row untappable, and this walkthrough found it. In run 39078862072, tapping "Daily prompt, Off" opened nothing for 90 seconds.

The cause is `DSRow` (`app/design-system/DSSurfaces.swift:80`). It is always a `Button`, with a no-op default action. Used as a `NavigationLink` label, the inner button took the tap. To stop that, DS06 put `.allowsHitTesting(false)` on the label (`ios/AppAssembly/ManagementCatalog.swift:256`, `app/data-settings/RetentionSettingsView.swift:92`), which removes the link's hit area entirely.

DS08 fixes this in the same PR, and the walkthrough is the regression test:

- `DSRow` renders without a `Button` when it has no action, or a label-only row is added for link labels. The whole row, padding included, is the hit area.
- Both `.allowsHitTesting(false)` modifiers are removed. No row inside a `NavigationLink` label contains a `Button`.
- The walkthrough taps each Settings row and the Export row, and fails if that row's screen does not open.
- The existing DS06 and design-system tests still pass. Run 39078862072 counts as the failing run without the fix; link it beside the passing run with the fix.

Owned-path additions: `DSRow` in `app/design-system/DSSurfaces.swift`, the link label in `ios/AppAssembly/ManagementCatalog.swift`, the Export link label in `app/data-settings/RetentionSettingsView.swift`, and tests for these.

The `AppServicesTests` "no result buffer leaks" failure in the same run is not caused by DS08; it is F20. DS08 does not change that test. If it fails again, rerun the job and say so in the PR.

### DS09

Restyle Export

Rebuild the export screen's presentation per the M1 UI design. Export behaviour is unchanged.

Owned paths: `app/data-settings/` (the export views only: `ExportSettingsView.swift`, `ExportSettingsText.swift`) and their tests.

Dependencies: DS01, U07c (shared `app/data-settings/`; DS06 and U07c also change it).

Acceptance:

1. Export has a serif title, one grouped section for what is exported and one for the action, a primary export button and one footer line. The full explanation goes in an info sheet.
2. Every export behaviour and authentication test passes: the authenticated share lifecycle and the share sheet are unchanged. The layout passes the design checklist. The exact-head macOS CI run is linked.

### F20

Stop the result-buffer leak check from failing on test order

`AppServicesTests` snapshots `CoreHandle.liveResultBufferCount`, a process-wide counter, in `setUpWithError`. In `tearDownWithError` it requires the counter to be back at that baseline (`ios/Tests/AppAssembly/AppServicesTests.swift:17`, `:25`).

DS08's run 39078862072 (head b45727f) changes no app or test code, yet `testProductionServicesResolveTheRealAdaptersAndTheLifecycleModule` failed with `("0") is not equal to ("2") - no result buffer leaks`. Two buffers were alive when the test started and gone by its end. So something earlier in the suite creates or holds result buffers after its own teardown has run. The likely culprit is async work that outlives `AppServices.shutDown()` (`ios/AppAssembly/AppServices.swift:162`): a `Task`, a `.task` load, a quiet authentication attempt, or a notification reconcile. The failure is intermittent and can fail any native PR. Found by the coordinator on 2026-10-17.

Owned paths:
- `ios/Tests/AppAssembly/AppServicesTests.swift`;
- the test file of the test that leaves the buffers behind;
- if the cause is production work that outlives shutdown: `AppServices.shutDown` and the cancellation in the service that owns that work.

Dependencies: none.

Acceptance:

1. **Diagnosis before change.** The PR names the test and the work that creates or holds result buffers after that test's teardown. Evidence is one of:
   - per-test counter values at setup and teardown, in the failing order;
   - a reproduction using `-only-testing` in the failing order, or using `-test-iterations` or `-run-tests-until-failure`.

   A cause it cannot confirm is labelled as a hypothesis.
2. The fix removes the cause.
   - If production work outlives `shutDown()`, `shutDown()` cancels it, and nothing calls into the core after `shutDown()` returns. A test shows this.
   - If a test only fails to wait for its own work, that test waits for it before teardown.
3. The leak checks are not weakened:
   - both counters are still compared for exact equality in teardown;
   - there is no settling delay, retry or tolerance around either snapshot;
   - no test is skipped, reordered or split out to hide the failure.
4. It holds under repetition. A CI run passes that repeats the involved classes at least five times in the failing order. A temporary workflow commit is fine for this: link its run, then remove the commit from the final branch. The normal exact-head macOS CI run is also linked, and `make check` passes.

Source pointers: `ios/Tests/AppAssembly/AppServicesTests.swift:12-27`; `ios/AppAssembly/AppServices.swift:162` (`shutDown`); DS03's quiet authentication tests in the same file (`makeQuietServices`).

Grant (2026-10-17, after the F20 worker's diagnosis): the cause is confirmed in the source.
- `OhAndTests` runs inside the app (`TEST_HOST` is `OhAnd.app`, `ios/project.yml:106`).
- `AppDelegate` (`ios/AppAssembly/AppDelegate.swift:12`) and `SceneDelegate` (`ios/AppAssembly/SceneDelegate.swift:17`) both call `launcher.launch()`.
- As a result, real production services run in the same process as every unit test: launch recovery, the job runner, the live notification reconcile and the quiet authentication attempt. They use the simulator's real app storage.
- `liveResultBufferCount` is process-wide, so the leak check sees their buffers.

F20 may change the following:
- `ios/AppAssembly/AppDelegate.swift`;
- `ios/AppAssembly/SceneDelegate.swift`;
- `AppLauncher` in `ios/AppAssembly/AppServices.swift`;
- tests for these.

The change:
- When the process is hosting unit tests (`XCTestConfigurationFilePath` is set in the environment), the app never builds production services: `AppServices.production()` is not called and no launch work starts. The scene still gets a window, so hosted window tests such as `FullScreenLaunchTests` keep working.
- The check is read in one place at the app's entry. Nothing else branches on it.
- It cannot change a normal launch. UI-test and smoke launches run the app as its own process without that variable, so the smoke test and DS08's walkthrough still start real services.
- A hosted test asserts that the host's launcher built no services during the test run.

Acceptance 1 can be met with this source evidence plus a CI run. Acceptance 3 still applies: no settling delay, retry or tolerance, and no test-side shutdown of the host's services as a substitute.

### E02a

Build a live end-to-end provider harness

E02 must run synthetic captures through two genuinely different live backends, selected by configuration, including the adapter and apply behaviour. On 2026-10-18 the coordinator found that no harness can do this:
- `tools/bench` refuses self-hosted profiles in practice ("the adapter declares no support for diagnostic calls"), and it has no store or apply step (`tools/bench/src/executor/mod.rs:10`).
- `core/tests/providers_self_hosted/live_smoke.rs` covers only the adapter, with three captures and one fixed model (`CHAT_VERIFIED_MODEL`).

This task builds the harness. It does not collect live evidence: the fleet holds no hosted-provider credential. The coordinator runs the harness with the maintainer's credentials and commits the sanitized evidence, and E02 then records it.

Owned paths:
- `core/tests/live_providers/` (new);
- one `[[test]]` entry in `core/Cargo.toml`, containing only `name` and a `path` inside that directory (the same terms as the test-target registration allowance).

Dependencies: none.

Acceptance:

1. **Configuration.** The backends are chosen by environment, never by code edits.
   - A list of profile specs `<protocol>:<model>` with protocol `self-hosted`, `anthropic` or `openai`. Any model the endpoint lists may be named; models without earlier chat evidence are allowed, because producing that evidence is the point.
   - The self-hosted endpoint is read from the provider configuration file `live_smoke.rs` already uses (`OHAND_PROVIDER_CONFIG_PATH`, provider key `ollama`).
   - Hosted credentials come from environment variables named on the command line, never from values.
   - A per-request timeout of up to 300 seconds.
   - Endpoints and credentials are never printed, logged or written to an artifact.
2. **Production path per fixture.** For every fixture in `fixtures/intent/contrastive-fixtures.json` and each configured backend, the harness:
   - stores the fixture as a synthetic capture through production ingress in a temporary store;
   - configures the provider profile and route grant through the production configuration path;
   - dispatches through the production adapter and applies through the production proposal application (`core/src/interpretation/apply/`).

   No step is replaced by a fake on the live path. Reuse an existing HTTPS transport (the `live_smoke.rs` transport, or the host transport in `tools/bench/src/transport/`) rather than writing a new client.
3. **Recorded per fixture and backend:**
   - provider, model, profile version, instruction version, build revision and collection time;
   - the semantic outcome against the fixture's oracle;
   - forbidden-mutation counts: the raw capture must be byte-identical after apply, and no authoritative field may be overwritten;
   - latency per call and provider-reported tokens;
   - a cost estimate only when a rates file is supplied at run time, labelled as an estimate.
4. **Switching.** After backend A has run, switching the configuration to backend B on the same store leaves every raw capture byte-identical. Earlier applied results stay attributed to A's profile version.
5. **Failure behaviour, each asserted:**
   - an unreachable endpoint;
   - an invalid credential on a hosted backend;
   - a revoked profile version, revoked through the production revoke path;
   - an invalid or malformed response.

   Each failure is classified as retriable or not. The capture stays durable and nothing is applied.
6. **Evidence.** Sanitized JSON artifacts go to a directory named by environment. They carry no endpoint, credential, captured text or model output text. Raw exchanges go to the private evidence directory, with the run id, file name, size and SHA-256 recorded in the artifact. This is the same convention as `live_smoke.rs`, and the harness refuses a private directory inside the repository. A leak check covers both.
7. **CI proof with no network.** A non-ignored test runs the whole harness against a local TLS fixture server serving self-hosted-shaped and Anthropic-shaped responses. It shows:
   - the production path and apply;
   - switching invariance;
   - every failure classification;
   - that artifacts contain none of the endpoint, the credential or the capture text.

   The live run is `#[ignore]`, and its exact command is documented in the module header.
8. Do not run the live path: no credential is available to workers. `make check` and `make test` pass.

Source pointers: `core/tests/providers_self_hosted/live_smoke.rs` (configuration, evidence conventions); `core/src/interpretation/dispatch/`, `core/src/interpretation/apply/`; `tools/bench/src/report/` (oracle scoring of forbidden mutations); `tools/bench/src/transport/` (credential binding to a granted origin).

### F21

Stop the self-hosted request from suppressing optional proposals

The self-hosted adapter sends `output_schema()` as a strict `json_schema` `response_format` (`core/src/providers/self_hosted/mod.rs:247-251`). In that schema only `operation` is required; `reminder_proposal`, `session_topic_proposal`, `abstention` and nested fields such as `instant` and `timezone_id` are optional.

Found by the coordinator on 2026-10-18, on the maintainer's Spark endpoint, at adapter revision a0fa6b4. The coordinator replayed the adapter's own request bodies, prompt and schema unchanged, against every model the endpoint serves under Ollama: `deepseek-flash-iq3:latest`, `qwen3.8:27b`, `gpt-oss:20b`, `gpt-oss:120b` and `qwen3.5:122b`.
- **Strict schema:** the synthetic capture "remind me to call the dentist tomorrow at 3pm" never produced a `reminder_proposal`, 10 of 10 tries across all five models. The prompt asks for one explicitly, and includes a worked example.
- **Same prompt, `json_object` or no format:** `gpt-oss:20b` returned an `explicit` reminder at the correct instant.
- **Top-level properties required but nullable:** `gpt-oss:20b` and `qwen3.8:27b` returned the reminder, and gave none for an idea capture. But nested optional fields were still dropped (`instant` came back null), and `gpt-oss:20b` filled `abstention` and `session_topic_proposal`.

Hypothesis: Ollama compiles the schema into a decoding grammar under which optional properties are effectively never emitted.

The adapter smoke run that marks `deepseek-flash-iq3:latest` `Supported` passed because each reply mapped to a valid proposal. It never checked that the reminder was there. The offline fast path (`core/src/interpretation/fast_path/`) catches plain "remind me …" commands, but every reminder phrased outside its grammar depends on this request.

The OpenAI adapter sends no strict schema, and the Anthropic adapter uses a tool call, so neither is affected.

Owned paths:
- `core/src/providers/self_hosted/`: request building, reply normalization and the compatibility boundary text;
- `core/tests/providers_self_hosted/`;
- new sanitized artifacts under `docs/validation/evidence/spark-adapter-smoke/`, plus their rows in the "V09 adapter smoke run" table of `docs/validation/spark-protocol.md`.

`output_schema()` (`core/src/interpretation/instructions/text.rs:66`) is not changed. The self-hosted wire schema is derived from it.

Dependencies: none.

Acceptance:

1. **Wire schema.** Preferred: the self-hosted request carries a schema derived recursively from `output_schema()`.
   - Every object property at every depth is listed in `required`.
   - Each property `output_schema()` leaves optional accepts `null`.
   - `additionalProperties: false` and every type, enum and nesting are preserved.

   It is derived at build time, so it cannot drift from `output_schema()`. A unit test walks every nested object and asserts the transform.

   If the live evidence in item 4 shows this does not bring the reminder back on the verified model, `json_object` with local validation is acceptable instead. The PR then shows the failing derived-schema run.
2. **Normalization.** Before validation, a `null` in an optional position is treated as absent. Validation and mapping are otherwise unchanged; no rule is relaxed, so an `abstention` that comes with a proposal is handled exactly as today. Unit tests with fixture replies cover:
   - a top-level null;
   - a nested null;
   - a non-null field that existing validation rejects.
3. **Smoke semantics.** `live_smoke.rs` checks meaning, not only shape.
   - The reminder capture must yield a `reminder_proposal` with `quality` `explicit` and an `instant` at the right moment: 2026-01-06T15:00 America/Chicago for its reference time.
   - The idea and plain-note captures must yield none.
   - The model can be chosen by environment, defaulting to `CHAT_VERIFIED_MODEL`. The artifact records the model and the semantic results, never output text.
4. **Live evidence.** Run the live smoke from the worker against `deepseek-flash-iq3:latest`, `gpt-oss:20b` and `qwen3.8:27b`, and commit the sanitized artifacts. Every model must pass item 3, with each run's artifact listed in the PR. A model that cannot pass is recorded honestly and is not counted as passed.
5. **Compatibility boundary.** `compatibility_boundary()` and the `Supported` claim are updated.
   - Each model is `Supported` only with a passing semantic artifact.
   - The suppression found here is recorded as an observed failure of the previous request shape.
6. `make check` and `make test` pass.

Source pointers: `core/src/providers/self_hosted/mod.rs:247` (request), `:104` (`Supported` claim); `core/tests/providers_self_hosted/live_smoke.rs:215` (fixed model); `core/src/interpretation/instructions/text.rs:66` (`output_schema`).

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

### F22

Declare the Face ID usage description so the app stops crashing at launch

Every launch on a device aborts. DS03 tries Face ID quietly at launch and on return to the foreground. The `LAContext.evaluatePolicy` call (`ios/Services/Authentication/ReadAuthenticationBoundary.swift:61`) reaches the privacy system, which kills an app that has no `NSFaceIDUsageDescription` in its `Info.plist`, and `ios/AppAssembly/Info.plist` has none. Device crash report (iPhone, iOS 26.6.2, build of `898a79d`): `EXC_CRASH (SIGABRT)`, termination namespace `TCC`, faulting thread on queue `com.apple.tcc.auth.kTCCServiceFaceID` under `-[LAClient evaluatePolicy:options:uiDelegate:synchronous:reply:]`. The simulator does not enforce the key, so neither CI nor the walkthrough saw it.

Owned paths: `ios/AppAssembly/Info.plist`, `ios/scripts/check_project_config.py`, `ios/scripts/test_check_project_config.py`.

Dependencies: none.

Acceptance:

1. `ios/AppAssembly/Info.plist` declares `NSFaceIDUsageDescription`. The string says, in the app's voice, that Face ID unlocks what you have saved and that capturing never needs it. For example: "Oh And uses Face ID to unlock the things you've saved. You can always capture without it." Keep it to one or two short sentences. Nothing else in the file changes.
2. `check_project_config.py` requires the key for the app. Add `NSFaceIDUsageDescription` to `APP_USAGE_STRINGS`. Also add a rule so the omission cannot recur for another target: any target whose Swift sources call `evaluatePolicy` or create an `LAContext` must declare the key in its `Info.plist`. `test_check_project_config.py` covers both: a synthetic project whose app plist lacks the key fails with a message naming the key and the target, and a synthetic target that uses `LAContext` without the key fails the same way. `make check` runs the check, so a Linux worker can prove it before macOS CI.
3. Link the exact-head macOS run, as with any native change. The device check is the coordinator's: after the task lands, the maintainer's install script installs main on the phone and the app must launch and stay running. The worker cannot do this and must not claim it.

### F23

Give abstention a single object shape on the provider wire

The provider-facing output schema lets `abstention` be either a string enum or an `{"Other": "…"}` object (`core/src/interpretation/instructions/text.rs:102`). In a paired benchmark on the synthetic corpus, Claude Sonnet 5.5 and Haiku 5.5 sent 8 of 10 string abstentions JSON-encoded a second time (`"\"Negated\""`). The mapping correctly rejects them, so negations, noise and unsupported spoken updates come back from Anthropic as invalid output although the model's decision was right. The string-or-object union is the trigger. Replace it on the wire with one object shape. The maintainer chose this over unwrapping the extra encoding.

Scope: the wire only. `AbstentionReason` in `core/src/interpretation/contracts/mod.rs`, its serde form, stored proposals, `fixtures/intent/`, the FFI and Swift are unchanged. The mapping translates the wire object into the existing domain value.

Owned paths:
- `core/src/interpretation/instructions/`;
- `core/src/providers/self_hosted/mod.rs` (its strict-schema derivation and unit tests, only as the new shape requires);
- `core/tests/interpretation_instructions/`, `core/tests/interpretation_dispatch/main.rs`, `core/tests/providers_self_hosted/`, `core/tests/live_providers/`;
- `tools/evaluation/fixtures/recorded-responses.json`, `tools/evaluation/comparison/tests.rs`, `tools/evaluation/tests/`.

Dependencies: none.

Acceptance:

1. **One shape.** In `output_schema()` the `abstention` property is an object: `reason` is required and is one of `UncertainTarget`, `Negated`, `Ambiguous`, `UnsupportedOperation`, `Other`; `detail` is an optional string; no other keys are allowed. `output_schema()` contains no `anyOf` or `oneOf` anywhere, and a test asserts that.
2. **The instruction text matches.** `M1_INSTRUCTION_TEXT` describes the object form, with one example each for `Negated` and for `Other` with a detail. Following the rule in `text.rs`, `M1_INSTRUCTION_VERSION` is republished as the digest of the new text, and the pinning tests are updated deliberately.
3. **The mapping translates exactly.** `{"reason": X}` for the four plain reasons maps to the existing `AbstentionReason` variant. `{"reason": "Other", "detail": D}` maps to `Other(D)`. `Other` with a missing, null or blank `detail` is invalid output. A `detail` on the four plain reasons is also invalid output, not ignored. The legacy string and `{"Other": …}` forms, and a JSON-encoded string such as `"\"Negated\""`, are invalid output: there is no fallback parsing. Each of these cases has a test through `map_output`.
4. **Self-hosted strict mode stays correct.** The derived strict schema makes `detail` required-but-nullable. A null `detail` with a plain reason maps to that reason. The existing null-as-absent handling is unchanged, and the self-hosted tests cover both forms.
5. **Callers that build replies are migrated.** Every test, the live-provider harness's synthetic reply builder (`core/tests/live_providers/main.rs:100` translates the fixture's domain `expected.abstention` into the wire object) and `tools/evaluation/fixtures/recorded-responses.json` use the new shape. The evaluation and comparison suites pass with unchanged results: an abstention recorded before still scores as the same abstention outcome.
6. `make check` and `make test` pass. No live-provider claim is made: the coordinator reruns the paired Anthropic benchmark and the E02 harness locally after landing.

### F24

Raise the Anthropic adapter's output cap so long captures are interpreted

`DEFAULT_MAX_OUTPUT_TOKENS` is 1024 (`core/src/providers/anthropic/mod.rs:34`). On the synthetic `very-long-transcript` fixture, Claude Sonnet 5.5 and Haiku 5.5 both reached the cap before finishing the `interpret` call. `stop_reason` was `max_tokens`, and the adapter correctly reports invalid output (`core/src/providers/anthropic/wire.rs:311`). So a long spoken capture is never interpreted through Anthropic. The self-hosted adapter already uses 4096.

Owned paths: `core/src/providers/anthropic/`, `core/tests/providers_anthropic/`.

Dependencies: none.

Acceptance:

1. `DEFAULT_MAX_OUTPUT_TOKENS` is 4096, and a test pins the value sent as `max_tokens` when no override is set. `with_max_output_tokens` keeps working and its validation is unchanged.
2. A reply whose `stop_reason` is `max_tokens` still maps to non-retriable invalid output, with its usage observations attached. A fixture-server test covers this, so a future cap change cannot turn truncation into a silent partial proposal.
3. No other request field changes. A test shows the request body is unchanged apart from `max_tokens`.
4. `make check` and `make test` pass. The coordinator reruns the live Anthropic leg after landing; the worker makes no live claim.

### F25

Make the live harness's oracle count only interpreted captures as matches

`SemanticOutcome::matches_oracle` (`core/tests/live_providers/oracle.rs:92`) only rejects facets scored `wrong`. A facet scored `missing` passes, and the run's terminal result is not consulted. So a capture whose interpretation failed (invalid output, a permanent failure) still counts as a match whenever its fixture expects no reminder, no topic and no abstention. In the first live run (`fe58be5`), `gpt-oss:20b` reported 22 of 33 matches although only 6 captures were applied. E02 cannot be certified on that number.

Owned paths: `core/tests/live_providers/`.

Dependencies: F23, which edits the same directory.

Acceptance:

1. A fixture matches the oracle only when its run ended `applied` or `abstained`, every facet is `match` or `none`, and there are no forbidden-rule hits. `missing` is a miss.
2. A fixture expecting an abstention matches only when the run ended `abstained`. One expecting a proposal matches only when the run ended `applied`.
3. The sanitized evidence reports, per backend: matched, applied, abstained, invalid output, other failures, and total. Each count comes from the run records, not from re-deriving the semantic outcome. The report states the scoring rule in one sentence.
4. Tests show:
   - a failed run with an all-`none` oracle is not a match;
   - a `missing` facet is not a match;
   - an applied run with every facet matching is a match;
   - an expected abstention that ended `applied` is not a match.
5. The networkless twin's assertion of at least 10 matches still holds under the new rule. If it cannot, fix the synthetic replies, not the threshold.
6. `make check` and `make test` pass.

### F26

Score a correctly stored reminder as correct in the evaluation suite

Against the durable stage, the evaluation scorer reports `missed_intent` ("a reminder without the instant the oracle has", `tools/evaluation/src/score.rs:363`) on `design-explicit-reminder`, `date-explicit-today` and `timezone-implicit-local`. It does so even when the candidate's instant equals the oracle's exactly. The paired benchmark found the oracle's own reference reply fails the same way. The likely cause is that the durable view reads the reminder's instant from stored state that applying a proposal does not resolve or schedule (`tools/evaluation/src/scratch.rs:93`). Confirm the cause before changing anything.

Owned paths: `tools/evaluation/src/`, `tools/evaluation/tests/`.

Dependencies: F23, which edits `tools/evaluation/tests/`.

Acceptance:

1. A test feeds the oracle's own expected proposal for each of the three fixtures through the suite. Each scores no defects at both the candidate and durable stages. The test fails on current main.
2. The durable stage still detects real defects: an applied reminder whose stored instant differs from the oracle's is `missed_intent` or `unsupported_claim` as today, and a missing stored reminder is still `missed_intent`. Tests cover both.
3. Fix the scorer's reading of durable state, not the oracle, the fixtures or core's apply path. If the evaluation genuinely needs a core seam it lacks, block with the exact need rather than reaching into `core/`.
4. The recorded-response evaluation's report changes only where these false defects disappear. List the changed cases in the PR.
5. `make check` and `make test` pass.
