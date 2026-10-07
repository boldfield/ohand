# M1 Device and Two-Week Trial Evidence Protocol

Status: preregistered protocol, 2026-10-07. This document specifies the evidence collection methodology, device targets, success criteria, and pass/fail outcomes before the two-week M1 trial runs. It defines what will be measured, how to judge whether M1 is successful, and what observations trigger a loop revision rather than acceptance.

This protocol is the reference for task T08 (evaluate the two-week M1 trial), which is blocked on this task's completion.

## Device and Platform Target

### Primary Device
- **Hardware**: iPhone 16 Pro
- **Target OS**: iOS 26.6.2 or later (record the actual observed build during testing)
- **Record**: exact OS build identifier, app version/build identifier, signing route and validity period

### Baseline Assumptions
- Fresh or prior install with seeded data (no production traffic in the trial)
- Synthetic captures may be used for scripted functional test cases only; they do not count toward sample-adequacy minimums
- Authorized local data only; no personal private content commits to the repository
- Sample-adequacy counts (captures, interpretations, retrievals, reminders, resurfacing) must come from actual trial use, not synthetic or seeded fixtures
- If hardware is unavailable, this task blocks with explicit evidence needs; continue independent work and create a corrective follow-up task

## Test Scenarios and Functional Matrix

### Offline and Interruption Cases

These are required:

1. **Offline capture**: save captures (voice and text) with no network connectivity; verify acknowledgment and persistent queuing.
2. **Interrupted voice recording**: force recording to be interrupted (lock device, switch app, system notification) at various points (start, mid-recording, near end); verify recoverable or durable partial audio and honest failure state.
3. **Interrupted import**: close the app or force termination during or immediately after capture acknowledgment; relaunch and verify the capture is neither duplicated nor lost.
4. **Provider unavailable**: configure a cloud provider as default, make the network unreachable, capture and attempt interpretation; verify the source is durable and queued work is inspectable.
5. **Denied permissions**: test microphone denied (at system level and from settings), speech framework unavailable (if applicable), notification permission denied; verify offline text capture, honest permission state, and no automatic fallback to other sources.
6. **Permission changes**: revoke a permission after initial grant, and restore it; verify voice capture surface availability updates, queued work is reconciled, and no silent loss of acknowledgment.

### Device State Cases

These are required:

1. **Cold launch**: force-close the app, relaunch from a fresh state; verify no startup delay for capture, existing items are intact, and pending work resumes.
2. **Warm launch**: switch to the app from another app or lock screen; verify quick capture readiness and no loss of state.
3. **Locked device capture**: attempt capture with device locked (if supported by entry mechanism); record whether handoff is durable and whether private history access is denied.
4. **Device lock during processing**: lock the device while a job is running (transcription, interpretation, reminder scheduling); verify work resumes when unlocked, no duplicates, and source remains protected.
5. **Background interruption**: record capture, return to home screen, and verify acknowledgment occurs even if background work is cut short; remind the user if processing is pending, do not claim completion.
6. **Mac asleep** (required if a Mac inlet is present in the trial build): verify behavior with the companion device unavailable (e.g., asleep, disconnected); ensure no loss of acknowledgment and queued work is resumable when the companion reconnects. If no Mac inlet is present, record N/A with documented reason (e.g., "Mac inlet not included in trial build" or "M1 architecture specifies no Mac client").

### Reminder and Scheduling Cases

These are required (use synthetic content only):

1. **Offline reminder scheduling**: schedule a reminder using the offline fast-path recognizer with no provider configured; verify it installs without a network request.
2. **Explicit reminder with UI closed**: schedule a reminder, close the app; verify the system notification delivers and opens correctly when tapped.
3. **Past reminder time**: attempt to schedule a reminder for a time that has already passed; verify explicit `time_in_past` state and no fabricated rescheduling.
4. **Unsupported recurrence**: request a repeating reminder (e.g., "every Monday"); verify it is saved as `unsupported_recurrence`, never silently reduced to one-shot.
5. **Ambiguous time**: request a reminder with ambiguous wording (e.g., "next week" without a day); verify it is saved as `not_scheduled_yet` with a correction path, not guessed.
6. **Capacity boundary**: schedule reminders until the OS pending-request limit is reached; verify explicit `capacity_exceeded` state, documented refill policy, and no silent dropping.
7. **Duplicate prevention**: schedule the same reminder twice (or retry the same request); verify the OS notification is not duplicated.
8. **Repeated transport and idempotency**: simulate transport retry, provider request replay, or handoff re-delivery of an already-processed capture or job; verify the system is idempotent—no duplicate captures, lost acknowledgments, or duplicate jobs result from the retry.
9. **Cancel and edit**: schedule a reminder, edit its time, cancel it; verify the OS notification is removed and state is reconciled.
10. **Notification action**: deliver a notification, tap it, acknowledge or complete the action; verify action handling is correct and does not corrupt state.

### Retrieval and Resurfacing Cases

These are required (use synthetic content only):

1. **Text search after offline save**: capture items with no interpretation configured; retrieve original text using literal search with date/type/scope filters; verify matches and honest no-result state.
2. **Retrieval after multi-day gap**: perform captures, stop using the app for N days (N ≥ 2, recorded), relaunch; verify prior captures are still accessible and no catch-up wizard or overdue backlog is imposed.
3. **Private read scope**: capture a private (personal scope) item, attempt to read it without unlocking the device; verify locked access is denied.
4. **Authenticated read**: unlock/authenticate, retrieve both personal and work items; verify scope filters work and no cross-scope leakage.
5. **Correction retrieval**: capture an item, correct its text, retrieve both versions; verify both original and corrected text are available and attribution is clear.
6. **Optional prompt activation**: enable the daily optional prompt, activate it at the configured time, retrieve the current eligible item; verify the item is correct and response actions work.
7. **Prompt after deletion**: trigger an optional prompt for an item, delete it, verify the pending prompt is removed and no stale notification remains.

### Configuration and Switching

These are required:

1. **Two-backend configuration**: configure two genuinely different provider backends (e.g., Anthropic and OpenAI), switch between them multiple times using the settings UI; capture and interpret the same item with each backend; verify:
   - Sources and corrections are unchanged
   - Interpretation results may differ but no stored data is mutated
   - Queued jobs retain their pinned profile version
   - No private key material is displayed
2. **Profile capability mismatch**: configure a provider that does not support a required capability; attempt to use it; verify explicit unavailable/unsupported messaging.
3. **Credential invalidation**: set a valid credential, use it, then invalidate or revoke it (if possible); verify the next job fails explicitly with authorization error and offers recovery options.
4. **Endpoint unavailable**: configure a custom self-hosted endpoint, make it unreachable, attempt interpretation; verify the job fails with network error and waits for recovery, never falls back to another provider.

### Processing State and Transitions

These are required:

1. **Processing states visible**: capture an item, interpret it, schedule a reminder; verify that save state, processing state, and reminder state are independently reportable and honest (never claim processed if pending, never claim scheduled if not confirmed).
2. **Forced worker failure**: inject a simulated provider timeout or invalid response during interpretation; verify the source remains searchable, an explicit uninterpreted state is recorded, and the item is still retrievable without a mandatory triage.
3. **Partial batch failure**: capture multiple items, force some to fail interpretation while others succeed; verify sources are all intact, successful items are processed, failed items remain available for retry or manual correction.

## Latency Measurements

Measure and report the following as **content-free metrics** (timing distributions only, no source content):

### Trigger-to-Ready Latency
- **Definition**: from user action (press system control, open app from home screen, or tap notification) to the capture UI is ready (text field focused, microphone indicator live, or recording control ready).
- **Separate measurement points**: cold launch, warm app switch, locked device handoff.
- **Report**: median, p95, min/max for each launch type, across all captured measurements.
- **Exclude**: voice framework load time is separate from handoff time.

### End-of-Input-to-Save Latency
- **Definition**: from user confirms save (releases microphone, taps save button) to a durable acknowledgment is shown to the user on the device.
- **Measurement points**: separate for voice (includes silence detection and audio commit) and text.
- **Report**: median, p95, min/max per capture type.
- **Exclude**: network delay; measure only the device-local acknowledgment point.

### Interpretation Latency
- **Definition**: from interpretation job dispatch to proposal received.
- **Report**: median, p95 by provider and model, only for successful jobs.
- **Exclude**: network latency separate from model latency if possible; record timeouts separately.

### Reminder Scheduling Latency
- **Definition**: from reminder request (fast-path or proposal received) to OS notification installed.
- **Report**: median, p95; separate for offline fast-path and provider-based reminders.

### Device Lock Impact
- **Definition**: measure interpretation and reminder scheduling latency while the device is locked (if process is allowed to continue) vs. unlocked.
- **Report**: comparison of medians, p95.

## Evidence Citation Format

Every observed result, measurement, and finding must include:

1. **Named sanitized artifact**: reference name for the capture, log, or screenshot (e.g., "capture-001", "reminder-batch-05", "retrieval-log-day-7")
2. **Exact build/revision**: app version, build number, and commit hash or tag
3. **Collection time**: ISO 8601 timestamp (date and time, timezone included)

Example: "Capture-001: app v0.8 build 42 (commit abc1234), 2026-10-10T14:23:45+00:00. Voice recording interrupted at 3 seconds; audio preserved and recovery prompted."

Do not cite results by prose assertion alone. Every success criterion, failure case, and measurement must be traceable to a named artifact, exact version, and time.

## Sample Adequacy

The trial is adequate if it includes:

### Capture Volume
- **Minimum**: 15 distinct captures across the two-week window
- **Distribution**: include both voice and text; include offline and online scenarios
- **Spread**: captures must be distributed across both weeks (at least 2 captures in week 1, at least 2 in week 2); nonuse between captures is allowed. Do not cluster all captures on a single day.

### Interpretation Attempts
- **Minimum**: 10 successful interpretations and at least 1 failed/abstained interpretation
- **Providers**: at least 5 captures interpreted by each of the two configured backends
- **Types**: include note, action and idea annotations in the results

### Retrieval and Search
- **Minimum**: 10 distinct retrievals using literal search with various filters (date range, type, scope)
- **Scenarios**: include at least 1 search during or immediately after the capture, 1 after a multi-day gap, 1 after the device has been locked

### Reminders and Notifications
- **Minimum**: 5 distinct reminders scheduled, at least 2 delivered and opened, at least 1 explicit `not_scheduled` or `unschedulable` state recorded
- **Scenarios**: include fast-path offline reminder, provider-proposed reminder, ambiguous-time handling

### Resurfacing and Return Encounters
- **Minimum**: at least 5 distinct resurfacing/return encounters (either via optional prompt activation, explicit retrieval request, or reminder notification tap) across the two-week window
- **Measurement**: record each encounter, the mechanism (prompt activation, search, reminder tap), the user response (selected item, dismissed, muted), and whether a useful result was found
- **Report**: whether resurfacing mechanisms (prompts, reminders, search) help the user recall and act on prior captures

### Optional Prompts (if enabled)
- **Minimum**: prompt activated at least 3 times; user responded (selected item, dismissed, muted) to at least 2 activations
- **Report**: whether prompts are actually useful or consistently muted

### Capture Avoidance and Burden Assessment
- **Definition of "intentional interaction session"**: any user-initiated action where capture is presented and could have been used, including: opening the capture UI (whether from control, notification, or app launch), tapping the save button area (but not completing save), receiving a retrieval search result page, or dismissing a notification. Each session is a discrete encounter with a capture opportunity.
- **Collection method for skips**: maintain a dated skip log throughout the trial with entries in the form: "YYYY-MM-DD HH:MM:SS: [skip reason] (in context: [activity name or search query]). App version X.Y build Z (commit ABC)." Pair each skip entry with the same citation tuple required for evidence (named artifact, exact build, ISO 8601 timestamp).
- **Threshold for capture avoidance**: if more than 20% of intentional interaction sessions result in skipped capture or prompt dismissal (calculated as: count of skip log entries divided by count of total interaction sessions), the loop is not frictionless enough; define a UI/flow revision as required by loop-revision outcome below.
- **Muting the only proactive prompt**: if the only configured optional prompt is muted (disabled via settings, cleared via notification action, or not re-enabled within one day of dismissal), this is an unmet criterion regardless of the 20% threshold. This triggers loop revision per DESIGN M1: "If people avoid capture or mute the only prompt, revise the loop."
- **Burden indicators**: record prompt mute actions (with timestamp and context), settings changes to disable optional prompts, app-close actions during or immediately after capture UI presentation, and any user feedback (in a private log) suggesting capture is too burdensome.
- **Generic vs. Explicitly Enabled Preview Comparison** (required if preview support is enabled; otherwise record N/A): when preview support is configured, record whether user retrieval attempts use generic previews versus explicitly enabled non-private previews; document which mechanism led to successful item recall and whether one was consistently preferred, ignored, or caused friction. If preview support is disabled or unavailable, record N/A with reason (e.g., "Preview support not enabled in trial configuration" or "Provider does not support previews").

## Multi-Day Lapse

The trial must include at least one gap of 2+ consecutive days with no app use (no capture, no search, no interaction). This tests:
- Return to the app after nonuse without a catch-up ceremony
- Reminder reconciliation after a lapse
- App startup performance and state consistency

**Record**: start and end dates of the gap; verify that the app is used normally after the gap (capture and retrieval work, no forced cleanup).

## Two-Week Observation Window

- **Duration**: at least 14 full calendar days (336 hours) between the first capture and the final measurement
- **Record**: date and time of first and last event (capture, retrieval, reminder scheduled, prompt activation, etc.)
- **Continuous use not required**: include the multi-day gap as part of the window; the goal is to judge durability and return after absence, not daily habit

## Configuration Evidence

### Provider Switching
- **Record**: configuration state before and after each switch (provider name, model, endpoint if self-hosted)
- **Verify**: sources are unchanged; no duplicate or missing captures
- **Report** (content-free): number of configuration changes, number of captures interpreted before and after each change

### Self-Hosted Endpoint
- **If tested**: record the actual serving software, endpoint, and model from V08 evidence
- **Verify**: the exact configuration passes the recorded protocol fixtures and the phone can reach the endpoint
- **Report**: compatibility matrix and verification that it is the actual tested version, not a generic claim

## Content-Free Metrics for Public Reporting

The following may be published without private-content exposure:

1. **Latency distributions**: trigger-to-ready, end-of-input-to-save, interpretation, reminder scheduling (median, p95, count)
2. **Sample counts**: total captures, by type (voice/text); successful interpretations, by provider; reminders scheduled; retrievals; resurfacing/return encounters (prompt activations, reminder taps, explicit retrieval requests)
3. **Resurfacing and usefulness**: count of resurfacing encounters that resulted in successful item recall/action versus encounters with no useful result; success rate (%) of resurfacing mechanisms
4. **Burden and avoidance**: total intentional interaction sessions recorded; count of skipped captures (from skip log); percentage of sessions with skip or prompt dismissal; count of prompt mute/disable actions
5. **Error summary**: count of timeout, denied-permission, capacity-exceeded, unsupported-language failures (types only, no source content)
6. **Lapse recovery**: confirmation that multi-day gap has no forced cleanup
7. **Configuration evidence**: provider names and versions used; confirmation of two-backend switching; recording of whether generic and explicitly enabled previews were both tested (as applicable)
8. **Accessibility observations**: if tested, presence/absence of issues, fix counts (not detailed user experience)

## Private Evidence (Local-Only)

The following must remain private and not committed to the repository:

1. **Captured content**: any text from voice or text captures, even sanitized excerpts or session topics
2. **Audio recordings**: raw audio files, even transcribed or anonymized
3. **Search queries** and retrieval results showing item content
4. **Private endpoint credentials**, API keys, or authentication tokens
5. **Device identifiers** beyond the model and OS build (UDID, serial number)
6. **Personal scheduling**: actual reminder times that could infer user schedule
7. **Provider response details** beyond model name and latency (structured output content, full API responses)

Preserve private evidence locally with an auditable reference (filename, hash, timestamp) so reviewers can inspect it if needed under proper access controls, but do not publish it.

## Success Criteria

M1 is **accepted** if all of the following are met:

1. **Functional matrix**: Every required test case in the scenarios section passes and is cited with a named artifact, build version, and timestamp. No acknowledged capture is lost. No reminder is duplicated or invented. No private history is leaked to locked access.
2. **Latency measured**: Trigger-to-ready and end-of-input-to-save are measured and reported separately with artifact citations and collection times. No latency claim is made without measurements and traceable evidence.
3. **Sample adequate**: Minimum capture, interpretation, retrieval, and reminder counts are met over 14+ calendar days with a 2+ day lapse included. All counts include only real trial use, not synthetic or seeded data. Counts are traceable to named artifacts and timestamps.
4. **Configuration verified**: Two distinct provider backends are successfully switched; configuration state is inspectable in both; sources unchanged. Switch evidence is cited with artifacts and times.
5. **Processing state honest**: Save, processing, reminder request and reminder schedule states are independently visible and reflect reality. A pending or failed state never claims success. All states are tied to specific artifacts and timestamps.
6. **Recovery from faults**: Interrupted processes, permission denials, provider outages, and device lock transitions are handled without loss of acknowledged captures or invention of reminders. Each recovery scenario is cited with a named artifact and exact time. A reset is explicitly flagged and recovery options are offered.
7. **Offline capability verified**: Silent text capture, offline fast-path reminders, and local search work without a configured provider. Offline cases are documented with artifacts and timestamps.
8. **Evidence cited**: Every observed result, measurement, and finding includes a named sanitized artifact, exact build/revision, and ISO 8601 collection time. No prose assertions without traceable evidence.

## Unmet Evidence and Loop Revision

M1 is **not met** and requires a **loop revision** if:

1. **Prompt muting (automatic loop revision)**: the only configured proactive optional prompt is muted, disabled via settings, or not re-enabled within one day of dismissal during the trial. This is not conditioned on the 20% avoidance threshold. DESIGN M1 requires loop revision before adding features: "If people avoid capture or mute the only prompt, revise the loop." Define what aspect of the prompt (timing, content, presentation, or frequency) caused the mute and create a focused follow-up task with a proposed change.
2. **Capture avoidance**: More than 20% of intentional interaction sessions (calculated from the skip log: skip count ÷ session count) result in skipped capture or dismissed prompts. The loop is not frictionless enough. Define a concrete UI/flow revision and create a focused follow-up task addressing the specific friction points identified in the skip log.
3. **Retrieval failure**: Original text cannot be reliably retrieved after capture, or the only available path requires unacceptable authentication friction. Define what part of the retrieval loop failed and create a follow-up task.
4. **Insufficient resurfacing**: Fewer than 5 distinct resurfacing/return encounters occur during the 14-day window (counted from the resurfacing log), indicating inadequate observation. Do not declare loop revision; instead continue observation until the 5-encounter minimum is met. If the window extends beyond 14 days and encounters remain below 5, evaluate whether the resurfacing mechanism needs redesign versus whether M1 scope requires adjustment.
5. **Resurfacing mechanism failure**: resurfacing encounters are present but consistently unsuccessful (fewer than 50% of resurfacing attempts result in useful item recall or action), or users mute the optional prompt entirely. The return mechanism is not helping users recall captures. Define whether the mechanism needs redesign (e.g., better selection criteria, different presentation) or whether M1 scope requires adjustment.
6. **Silent losses or duplicates**: Despite passing the functional matrix, an uncontrolled loss of captures, retrieval failures, or duplicate reminders are observed in real use. Define the root cause and create a corrective task.
7. **Provider instability**: Both configured backends fail interpretation consistently or in unexpected ways, preventing assessment of the provider-switching requirement. Document the failures and create a provider-specific follow-up task.
8. **Performance blockers**: Trigger-to-ready or end-of-input-to-save latency exceeds 3 seconds (median) under normal conditions, making capture impractical. Define the bottleneck and create a performance task.
9. **Device-specific blockers**: Unresolved issues with the target device/OS (e.g., background work cannot be constrained, notifications cannot be reliable) that prevent the trial from proceeding. Document the blocker precisely and determine whether M2 or M3 is required.

If any condition above is observed, stop M1 acceptance. Instead:
- Record the exact evidence (sample counts, error logs, reproduction steps)
- Define the minimal change needed to fix the loop
- Create a focused follow-up task with clear acceptance criteria
- Continue independent work on other M1 features while the revision is addressed

## Insufficient Evidence Outcomes

When evidence is insufficient, the trial result is explicitly **unmet**, not completed. The following outcomes are defined:

1. **Continue observation**: if the trial window is less than 14 days, sample counts are below minimums, the multi-day gap is missing, or resurfacing encounters are below 5 within the current window, extend the observation window and collect additional data. Do not declare the trial complete until the full 14-day window is met AND all sample minimums (including at least 5 resurfacing encounters) are reached. Resurfacing inadequacy is an observation deficit, not a mechanism failure.

2. **Blocked, not completed**: if a required test case cannot be tested due to platform limitations, unavailable hardware, or unresolved blockers (excluding Mac-asleep, which is N/A when no Mac inlet is present), record the reason and mark the case as explicitly blocked. A blocked required case means M1 evidence is incomplete. Do not accept M1 until the blocker is resolved or a deliberate scope amendment is recorded in writing.

3. **Loop revision required**: if the trial completes the 14-day window with adequate samples but a success criterion is not met (e.g., the only prompt is muted, capture avoidance exceeds 20%, resurfacing encounters are present but consistently unsuccessful, performance blocker, provider failure), follow the loop-revision path in the "Unmet Evidence and Loop Revision" section above. Record the exact evidence and define the minimal fix.

**Pass/fail is binary and pre-registered**: a trial either meets all success criteria or it does not. Incomplete evidence (short window, inadequate samples, blocked cases) and unmet criteria (functional failure, performance blocker, usability barrier, prompt muting) are both reasons to continue observation or define a revision, never to accept without evidence.

## Reviewer Checklist for T08 (Two-Week Trial Evaluation)

When evaluating the trial against this protocol:

1. ✓ Verify the trial window is 14+ calendar days with recorded start/end times
2. ✓ Confirm the multi-day gap (2+ days) is included in the window
3. ✓ Check functional matrix: every required test case is documented and passed; if any required case is blocked, M1 evidence is incomplete and the trial is not met
4. ✓ Validate sample adequacy: counts meet minimums (including at least 5 resurfacing encounters); distributions make sense (not all captures on one day, captures in both weeks)
5. ✓ Inspect latency reports: median and p95 are present for each measurement point; outliers are explained
6. ✓ Verify content-free metrics only; no source text, audio, or private endpoints in the public report; confirm resurfacing counts, skip/avoidance counts, and prompt mute/disable counts are reported
7. ✓ Confirm configuration evidence: exact provider names, models, and switch records; no credentials exposed; recording of whether generic and explicitly enabled previews were both tested (as applicable)
8. ✓ Check fault recovery: all applicable required failure scenarios in the functional matrix (interruption, permission denial, provider outage, device lock) are tested and resolution is documented with cited artifacts
9. ✓ Verify prompt muting outcome: if the only proactive prompt was muted or disabled, confirm loop revision is triggered (not acceptance)
10. ✓ If success: record the trial as complete and M1 as accepted
11. ✓ If unmet: identify which success criterion failed; create a follow-up task and note the evidence that triggered it

## Reference Documents

- [DESIGN.md](../../DESIGN.md) — M1 exit evidence section
- [m1-plan.md](../features/m1-plan.md) — M1 executable specification
- [m1-contracts.md](../architecture/m1-contracts.md) — Architecture contracts and state model
- [T08 Task Specification](../features/m1-plan.md#t08) — Two-week trial evaluation

