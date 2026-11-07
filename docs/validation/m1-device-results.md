# M1 device matrix: procedure and results

Status: T05a (procedure, case table, latency method, record format). The results section belongs to T05b and is empty here.

This page turns the functional matrix of [`m1-protocol.md`](m1-protocol.md) (F07's protocol, which this page maps and does not amend) into a step-by-step procedure on the app as it ships. The maintainer runs it on the trial iPhone with the one signed candidate, writes an `ohand.device-matrix.v1` record, and checks it with `make device-matrix-check`. The record format is documented in [`evidence/device-matrix/README.md`](evidence/device-matrix/README.md); the case list a tool enforces is [`tools/validation/device_matrix/required_cases.json`](../../tools/validation/device_matrix/required_cases.json). A test keeps the case table below and that file naming the same identifiers.

Screens and controls are named as they exist on `main` (checked against `app/`, `ios/AppAssembly/ManagementCatalog.swift`, `ios/Capture/` and `ios/Services/NotificationActions/`) and `docs/features/m1-ui-design.md`. If a name below does not match what the candidate shows, the candidate differs from `main`: record the difference in the case's `notes` and do not invent a substitute.

## 1. Before the session

1. **The candidate.** Install the signed build named by T10b's device record (`docs/releases/m1-trial/candidate-<build number>-<revision prefix>.json`). The record's `build_number` and `revision` must match that file. The app shows only its short version (Settings, last line: "Oh And 0.1.0"); the build number is read on the Mac with `xcrun devicectl device info apps` (bundle `com.boldfield.ohand.app`, `bundleVersion`). A probe build is never the candidate.
2. **The phone.** The trial iPhone (the model and iOS version go in the record; nothing else that identifies it). Passcode and Face ID set, Developer Mode on, a Mac for screen-recording frame timing, the normal data connection and Wi-Fi available, Settings → Privacy & Security permissions for Oh And at their first-run values (Microphone, Speech Recognition and Notifications not yet asked) at the start of the session.
3. **Synthetic data only.** Every capture is one of the phrases below or a variation of them. Nothing real is typed or spoken.

| Name | Text |
| --- | --- |
| `S-DENTIST` | "Remind me <D> 09:00:00 to call the dentist", where `<D>` is tomorrow's date as YYYY-MM-DD (the offline fast path takes only `YYYY-MM-DD HH:MM:SS` for an exact time and a closed content lexicon; "tomorrow", a weekday or a bare date give no instant offline) |
| `S-PLAIN` | "Synthetic note one about a garden bench" |
| `S-PAST` | "Remind me <P> 09:00:00 to check the synthetic ledger", where `<P>` is yesterday's date as YYYY-MM-DD (the content is outside the fast path's lexicon, so the fast path does not claim it and a configured provider interprets it; the fast path itself never records a past time, because it emits an ambiguous candidate that the core rejects) |
| `S-EVERY` | "Remind me every Monday to check the synthetic ledger" |
| `S-VAGUE` | "Remind me next week to review the synthetic plan" |
| `S-WORK` | "Synthetic work note about the quarterly checklist" (the shipped capture screens stamp the configured default scope, Personal, and expose no control to choose Work, so on a candidate that stamps only Personal this item is saved as Personal) |
| `S-PERSONAL` | "Synthetic personal note about a bicycle" (scope Personal) |
| `S-CORRECTED` | "Synthetic note with a mistaken word" corrected to "Synthetic note with the right word" |

   Voice captures read the same phrases aloud.
4. **Artifacts.** Screenshots, screen recordings and Status screenshots stay with the maintainer (`private_evidence_reference` in the record). Each is named `<case id>-<nn>.<extension>`, for example `reminder-3-01.png` or `state-1-02.mp4`. The record holds the names, never the files. A screenshot of the home screen, lock screen or a real notification is private.
5. **Time.** Write the time of each observation (`observed_at`, ISO 8601 with offset) from the phone's clock when the screenshot or recording was made.
6. **Order.** Sections 2 to 4 below may be run over as many sessions as it takes, but a case is observed on one candidate. Cases that change device state (reboot, reinstall, time zone, passcode) go last, in the order of the table.

## 2. Capabilities: which cases apply

The record's `capabilities_enabled` lists the optional capabilities the installed candidate enables. Read them from the candidate; a capability that is absent is not assumed.

| Capability (`capabilities_enabled` value) | How to read it from the installed candidate | Cases it gates |
| --- | --- | --- |
| `self_hosted_provider` | Settings → Processing → "Sort my captures". The third choice "With my own server" (detail "Sent only to the server you name") is selectable, and does not read "Not available in this build, so it cannot be the default." Settings → Processing → Advanced → "Saved profiles" can hold a profile of type "Private server". | `config-4`, `capability-self-hosted` |
| `shadow` | Settings → Privacy offers a "Quality review" grant ("Allow quality review via …") for a provider, and the candidate record or release notes claim shadow review. | `capability-shadow` |
| `previews` | The candidate record or release notes claim previews and a screen offers them. On `main` no screen offers a preview feature. (The Daily prompt screen's notification preview is part of that screen and is tested by `retrieval-6`.) | `capability-previews` |

Rules:

- A capability the candidate enables is tested: its gated cases get `passed`, `failed` or `blocked`.
- A capability the candidate does not enable gets `not_applicable` with the reason `capability not enabled in candidate`, for each case it gates. The checker refuses a gated case that has any other outcome while the capability is absent, and refuses `not_applicable` while it is present.
- The other `not_applicable` outcome is the protocol's own: Mac asleep with no Mac inlet (`state-6`, `plan-mac-asleep`), reason `Mac inlet not included in candidate`. M1 has no Mac inlet on `main` (DESIGN.md puts the Mac companion at M2), so these two are expected to be `not_applicable` unless the candidate adds one.
- No other case may be `not_applicable`. A case that cannot be run is `blocked` with its reason, which leaves M1 evidence incomplete (the protocol's "Blocked, not completed" rule).
- Each optional capability also needs its owning task's canaries before a candidate may enable it (`docs/features/m1-plan.md#t05`).

## 3. Case table

<!-- case-table:start -->
| Case | Protocol or source | Capability | Mac inlet |
| --- | --- | --- | --- |
| `offline-1` | Offline and interruption 1: offline capture | | |
| `offline-2` | Offline and interruption 2: interrupted voice recording | | |
| `offline-3` | Offline and interruption 3: interrupted import | | |
| `offline-4` | Offline and interruption 4: provider unavailable | | |
| `offline-5` | Offline and interruption 5: denied permissions | | |
| `offline-6` | Offline and interruption 6: permission changes | | |
| `state-1` | Device state 1: cold launch | | |
| `state-2` | Device state 2: warm launch | | |
| `state-3` | Device state 3: locked device capture | | |
| `state-4` | Device state 4: device lock during processing | | |
| `state-5` | Device state 5: background interruption | | |
| `state-6` | Device state 6: Mac asleep | | yes |
| `reminder-1` | Reminder 1: offline reminder scheduling | | |
| `reminder-2` | Reminder 2: explicit reminder with the UI closed | | |
| `reminder-3` | Reminder 3: past reminder time | | |
| `reminder-4` | Reminder 4: unsupported recurrence | | |
| `reminder-5` | Reminder 5: ambiguous time | | |
| `reminder-6` | Reminder 6: capacity boundary | | |
| `reminder-7` | Reminder 7: duplicate prevention | | |
| `reminder-8` | Reminder 8: repeated transport and idempotency (surrogate) | | |
| `reminder-9` | Reminder 9: cancel and edit | | |
| `reminder-10` | Reminder 10: notification action | | |
| `retrieval-1` | Retrieval 1: text search after offline save | | |
| `retrieval-2` | Retrieval 2: retrieval after a multi-day gap | | |
| `retrieval-3` | Retrieval 3: private read scope | | |
| `retrieval-4` | Retrieval 4: authenticated read | | |
| `retrieval-5` | Retrieval 5: correction retrieval | | |
| `retrieval-6` | Retrieval 6: optional prompt activation | | |
| `retrieval-7` | Retrieval 7: prompt after deletion | | |
| `config-1` | Configuration 1: two-backend configuration | | |
| `config-2` | Configuration 2: profile capability mismatch | | |
| `config-3` | Configuration 3: credential invalidation | | |
| `config-4` | Configuration 4: endpoint unavailable | `self_hosted_provider` | |
| `processing-1` | Processing 1: processing states visible | | |
| `processing-2` | Processing 2: forced worker failure (surrogate) | | |
| `processing-3` | Processing 3: partial batch failure (surrogate) | | |
| `deferred-speech-lock` | P09 amendment 2: offline speech recognition with an immediate lock | | |
| `deferred-speech-model-install` | P09 amendment 4: speech model install path for a missing locale | | |
| `deferred-capture-before-first-unlock` | P09 amendment 2: capture before first unlock after reboot | | |
| `deferred-termination-handoff-commit` | P09 amendment 2: termination between control handoff and commit | | |
| `deferred-low-free-space` | P09 amendment 2: refusal under low free space | | |
| `deferred-interruption-duration` | P09 amendment 4: interruption duration against the moment it began | | |
| `deferred-notification-focus-summary` | P09 amendment 2: notification survival of Focus and summaries | | |
| `deferred-low-power-mode` | P09 amendment 2: Low Power Mode | | |
| `deferred-reboot` | P09 amendment 2: reboot | | |
| `deferred-time-zone-change` | P09 amendment 2: a time-zone change after scheduling | | |
| `deferred-clock-date-dst` | P09 amendment 4: clock, date change and DST | | |
| `deferred-provisional-ephemeral-authorization` | P09 amendment 2: provisional and ephemeral authorization | | |
| `deferred-authorization-change-pending` | P09 amendment 4: authorization changed in Settings while requests are pending | | |
| `deferred-pending-slots-delivered` | P09 amendment 4: delivered notifications and pending slots | | |
| `deferred-pending-slots-repeating` | P09 amendment 4: repeating triggers and pending slots | | |
| `deferred-reinstall-os-upgrade` | P09 amendment 2: reinstall or OS upgrade | | |
| `deferred-keychain-passcode-removal` | P09 amendment 2: keychain behaviour after passcode removal | | |
| `deferred-audio-fixture-after-revoke` | P09 amendment 2: the previous audio fixture after a background permission revoke | | |
| `deferred-entry-latency-cold-warm` | P09 amendment 3: Shortcuts or Action-button entry latency, cold and warm | | |
| `deferred-untimed-voice-start` | P09 amendment 3: untimed voice Start | | |
| `deferred-text-field-ready` | P09 amendment 3: text-field trigger-to-ready | | |
| `plan-cold-locked-permission` | Plan T05: cold, locked and permission paths | | |
| `plan-offline-recording-text` | Plan T05: offline recording and text | | |
| `plan-process-interruption` | Plan T05: process interruption | | |
| `plan-reminders-ui-closed` | Plan T05: reminders with the UI closed | | |
| `plan-pipeline-faults` | Plan T05: pipeline faults | | |
| `plan-mac-asleep` | Plan T05: Mac asleep | | yes |
| `capability-self-hosted` | Plan T05 criterion 4: self-hosted provider | `self_hosted_provider` | |
| `capability-shadow` | Plan T05 criterion 4: shadow review | `shadow` | |
| `capability-previews` | Plan T05 criterion 4: previews | `previews` | |
<!-- case-table:end -->

The six `plan-*` cases are the plan's own wording. Each is `passed` only when the protocol cases it rolls up are `passed` (named in its procedure), observed in the same candidate, and its own artifact is a Status or Home screenshot taken at the end of the roll-up, named in `notes` by counts only.

## 4. Procedure

Common to every case: the **expected outcome** is the protocol's own criterion, quoted or closely paraphrased; **evidence** is the artifact pattern to capture; **settings** lists device settings the case touches (Airplane Mode, Focus, Low Power Mode, Settings → Privacy & Security permissions for Oh And). Reset any setting you changed before the next case unless the next case says otherwise. Home means the screen with the composer ("What's on your mind?") and the header with Search, "Oh And" and Settings. Status means Settings → Status.

### 4.1 Offline and interruption cases

#### `offline-1`
- **Prepare:** `S-PLAIN` as text; `S-DENTIST` as a voice capture.
- **Steps:** Turn Airplane Mode on (Wi-Fi and cellular off). On Home tap the field "What's on your mind?", type `S-PLAIN`, tap "Save". Tap "Record" (the mic), say `S-DENTIST`, tap "Stop recording". Open Settings → Status.
- **Expect:** (protocol) acknowledgment for both with no network and a persistent queue: the "Saved" pill, then a line under the composer such as "Not processed yet." / "Not transcribed yet." and "No reminder confirmed."; both items appear in Home recents ("Just now · sorting…"); Status shows work waiting ("Work is waiting"), not completed.
- **Evidence:** `offline-1-01.png` (Home after both saves), `offline-1-02.png` (Status). Counts in notes (2 saved, 2 waiting).
- **Settings:** Airplane Mode on.

#### `offline-2`
- **Prepare:** `S-DENTIST` spoken for 30 s (pad with synthetic words). Microphone and Speech Recognition granted.
- **Steps:** Tap "Record". Interrupt three ways in turn, each on a new recording and at a different point (start within 2 s; mid-recording; within 5 s of the end): (a) press the side button to lock; (b) switch app via the app switcher; (c) trigger a system alert (a timer or an incoming call from a second phone). Return to Oh And. Open Settings → Unfinished recordings.
- **Expect:** (protocol) recoverable or durable partial audio and an honest failure state: the composer message reads "Stopped when the phone locked.", "Stopped when the app left the screen." or "Stopped: audio was interrupted."; the partial recording is listed under Unfinished recordings with "Finish and save", "Add audio" and "Delete"; nothing is claimed saved that is not.
- **Evidence:** `offline-2-01.png` to `offline-2-03.png` (the message after each interruption), `offline-2-04.png` (Unfinished recordings listing the partials). Counts in notes (3 interrupted, N recoverable).
- **Settings:** none beyond Microphone granted. Silent mode off for (c).

#### `offline-3`
- **Prepare:** `S-PLAIN`.
- **Steps:** Type `S-PLAIN` and tap "Save". The moment the "Saved" pill appears, swipe the app away in the app switcher (force-quit). Repeat once with a voice capture: tap "Stop recording" and force-quit at once. Relaunch. Look at Home recents and Status.
- **Expect:** (protocol) the capture is neither duplicated nor lost: each phrase appears exactly once, or, if the force-quit landed before the commit, the text or partial recording is still kept (Unfinished recordings for audio), never twice.
- **Evidence:** `offline-3-01.png` (recents after relaunch), `offline-3-02.png` (Unfinished recordings). Notes: counts of copies per capture.
- **Settings:** none.

#### `offline-4`
- **Prepare:** a cloud provider set as default (Settings → Processing → "With a cloud provider", via "Add provider", with a synthetic or test key you control) and `S-PLAIN`. Skip the key if none exists and record `blocked` with the reason.
- **Steps:** Airplane Mode on. Capture `S-PLAIN`. Open the item from Home (tap its card) for Source detail, then Settings → Status.
- **Expect:** (protocol) the source is durable and queued work is inspectable: Source detail shows the original text; the item reads "Not yet sorted" or "Sorting…"; Status says "Work is waiting" or lists it under "Notes that did not sort" with a reason such as "The provider could not be reached". Turn Airplane Mode off: the work completes on its own, or "Try again" (Status or Source detail) starts it.
- **Evidence:** `offline-4-01.png` (Source detail), `offline-4-02.png` (Status).
- **Settings:** Airplane Mode on, then off.

#### `offline-5`
- **Prepare:** Settings → Privacy & Security on the phone: Microphone denied for Oh And; Notifications denied for Oh And. Speech Recognition denied as a third step.
- **Steps:** On Home tap "Record". Then open Settings → Permissions. Type and save `S-PLAIN`. Capture `S-DENTIST` as text and read the line under the composer. Repeat with Speech Recognition denied.
- **Expect:** (protocol) offline text capture still works, the permission state is honest, and there is no automatic fallback to another source: "Record" does not start a recording and says why (microphone unavailable); Settings → Permissions shows the "Microphone" headline "Voice recording is unavailable. …" and, for notifications, "Reminders are saved but cannot alert you, and the daily prompt stays quiet. …"; the text note saves; the reminder line reads "Reminder not scheduled: notifications are not allowed."
- **Evidence:** `offline-5-01.png` (Permissions with all three denied), `offline-5-02.png` (the composer message after the text save).
- **Settings:** Microphone, Speech Recognition and Notifications denied for Oh And.

#### `offline-6`
- **Prepare:** permissions denied as at the end of `offline-5`, then a text note `S-DENTIST` already saved with "Reminder not scheduled: notifications are not allowed."
- **Steps:** In iOS Settings → Oh And turn Microphone and Notifications on. Return to Oh And. Open Settings → Permissions, then Home. Revoke Microphone again (iOS Settings) while Oh And is open, return, and tap "Record".
- **Expect:** (protocol) the voice surface updates with the permission, queued work is reconciled, and there is no silent loss of acknowledgment: Permissions reads "You can record a voice capture." after restore and the held reminder becomes scheduled ("Reminder set for …" in Source detail), or says why not; after the second revoke "Record" is honest again; the earlier saved note remains.
- **Evidence:** `offline-6-01.png` (Permissions after restore), `offline-6-02.png` (Source detail of the reminder), `offline-6-03.png` (after the second revoke).
- **Settings:** Microphone and Notifications toggled in iOS Settings → Oh And.

### 4.2 Device state cases

#### `state-1`
- **Prepare:** five saved notes; one capture waiting (offline-4 state is fine).
- **Steps:** Force-quit Oh And. Start a screen recording on the phone. Launch the app from the Home Screen icon. Stop the recording after the composer is ready. Check Home recents and Status.
- **Expect:** (protocol) no startup delay for capture: the composer is ready to type or record on the first frame that shows the app; existing items are intact; pending work resumes (Status count falls or the waiting item completes). The timing itself is `trigger_to_ready.cold_launch` (section 5).
- **Evidence:** `state-1-01.mp4` (the recording), `state-1-02.png` (Home with the recents).
- **Settings:** none.

#### `state-2`
- **Prepare:** as `state-1`, app in the background.
- **Steps:** Open another app, then switch back to Oh And via the app switcher; again from the lock screen after the phone has been locked 10 s in the app.
- **Expect:** (protocol) quick capture readiness and no loss of state: text typed and not saved before the switch is still in the composer; the composer is ready on return. Timing: `trigger_to_ready.warm_switch`.
- **Evidence:** `state-2-01.mp4`, `state-2-02.png`.
- **Settings:** none.

#### `state-3`
- **Prepare:** the Control Center control "Oh And Voice" or "Oh And Note" added (Control Center → Add a Control → Oh And). Do not unlock the phone during the capture.
- **Steps:** With the phone locked, swipe to Control Center from the lock screen and tap "Oh And Note", dictate or type `S-PLAIN` when the capture opens, save. Do not authenticate. Try to find earlier items. Then unlock and open Home.
- **Expect:** (protocol) the handoff is durable and private history access is denied: the capture is saved; while locked the app shows no recents, counts or snippets (Home shows "Your captures are locked" with "Unlock"); after unlock the capture is in recents once. Timing: `trigger_to_ready.locked_handoff`.
- **Evidence:** `state-3-01.mp4`, `state-3-02.png` (the locked card), `state-3-03.png` (recents after unlock).
- **Settings:** Face ID/passcode on; Control Center access on the lock screen allowed (iOS Settings → Face ID & Passcode).

#### `state-4`
- **Prepare:** a provider that responds, or the on-device route; `S-DENTIST` text.
- **Steps:** Save `S-DENTIST`. Lock the phone within 2 s, wait 60 s, unlock. Repeat with a voice capture whose transcription is pending when the phone is locked.
- **Expect:** (protocol) work resumes when unlocked, no duplicates, and the source remains protected: the item exists once; its state moves on (sorted, transcribed or its reminder set) or Status lists it with a reason; the locked card shows nothing derived.
- **Evidence:** `state-4-01.png` (the locked card), `state-4-02.png` (Home after unlock), `state-4-03.png` (Status).
- **Settings:** Auto-Lock at 30 seconds helps.

#### `state-5`
- **Prepare:** a 20 s voice capture.
- **Steps:** Record, tap "Stop recording", press Home at once, wait 30 s, reopen.
- **Expect:** (protocol) acknowledgment occurs even if background work is cut short; the user is reminded if processing is pending and completion is not claimed: Home shows the item with "Not transcribed yet." or the final state, never "Saved" for the transcript before it exists.
- **Evidence:** `state-5-01.png` (Home after return), `state-5-02.png` (Status).
- **Settings:** none.

#### `state-6`
- **Applies:** only if the candidate has a Mac inlet. On `main` it has none.
- **Steps:** If there is none, record `not_applicable` with the reason `Mac inlet not included in candidate`. Otherwise: capture while the Mac is asleep, wake it.
- **Expect:** (protocol) no loss of acknowledgment and the queued work resumes when the companion reconnects.
- **Evidence:** for a run, `state-6-01.png`; none for `not_applicable` (the reason is the record).
- **Settings:** the Mac asleep.

### 4.3 Reminder and scheduling cases

All reminder cases need Notifications allowed for Oh And, unless the case says otherwise.

#### `reminder-1`
- **Prepare:** no provider configured (Settings → Processing → "On this phone"); `S-DENTIST`.
- **Steps:** Airplane Mode on. Save `S-DENTIST` as text. Open the item (tap its card on Home) and read the reminder section; open Settings → Status.
- **Expect:** (protocol) the reminder installs through the offline fast path with no network request: the line under the composer or the item says "Reminder set for <date>." with no provider configured and Airplane Mode on.
- **Evidence:** `reminder-1-01.png` (composer message), `reminder-1-02.png` (Source detail reminder, "Scheduled").
- **Settings:** Airplane Mode on.

#### `reminder-2`
- **Prepare:** a reminder one to two minutes ahead (the offline grammar takes only `YYYY-MM-DD HH:MM:SS`, not a relative duration: read the phone clock and type today's date with a time one to two minutes ahead, e.g. "Remind me 2026-11-08 15:42:00 to call the dentist").
- **Steps:** Save it. Force-quit the app. Lock the phone. When the notification arrives ("Oh And": "You have a reminder. Open Oh And to see it."), tap it.
- **Expect:** (protocol) the system notification is delivered with the UI closed and opens correctly when tapped: Oh And opens to the item.
- **Evidence:** `reminder-2-01.png` (the notification on the lock screen — private, keep local), `reminder-2-02.png` (the item after the tap).
- **Settings:** Focus off.

#### `reminder-3`
- **Prepare:** a cloud provider set as default and verified (Settings → Processing → "With a cloud provider", via "Add provider", with a synthetic-usage test key you control, as in `offline-4`); `S-PAST`. Without a key, record `blocked` with the reason: the offline fast path cannot produce this state.
- **Steps:** (1) Save `S-PAST` as text with the network on. When it has sorted, read the composer line and open the item's reminder. (2) In that item's reminder editor, set "Date and time" to a minute in the past and tap "Save time".
- **Expect:** (protocol) the explicit `time_in_past` state, no fabricated rescheduling: (1) "Reminder not scheduled: that time has passed." and, in the item, "Time has passed" with "Choose a later date and time."; the item keeps the time that was asked for and no notification is pending for it. (2) the editor refuses with "Choose a time in the future. Nothing was changed." and the stored time is unchanged. If the provider's answer is not recorded as `time_in_past` (the item reads "No time set" or shows no reminder), the case is `failed` with what the item shows in `notes`.
- **Route, checked against the code:** the provider's explicit candidate is checked against the core resolver and recorded as `unschedulable(time_in_past)` with the instant kept (`core/tests/proposal_application/main.rs`, `a_time_already_past_at_capture_is_unschedulable_without_adjustment`); the editor's refusal is `reminder_controls`' `time_in_past` outcome (`core/tests/reminder_controls/main.rs`).
- **Evidence:** `reminder-3-01.png` (composer line), `reminder-3-02.png` (item reminder), `reminder-3-03.png` (editor refusal).
- **Settings:** network on.

#### `reminder-4`
- **Prepare:** `S-EVERY`.
- **Steps:** Save it; open the item's reminder.
- **Expect:** (protocol) saved as `unsupported_recurrence`, never silently one-shot: "Reminder not scheduled: repeating reminders are not supported yet." and "Repeat not supported" in the item; no notification is pending for it.
- **Evidence:** `reminder-4-01.png`, `reminder-4-02.png`.
- **Settings:** none.

#### `reminder-5`
- **Prepare:** `S-VAGUE`.
- **Steps:** Save it; open the item; open Status.
- **Expect:** (protocol) saved as `not_scheduled_yet` with a correction path, not guessed: the item reads "No time set" (or "Saved" with "The time is saved in the app. It has not been confirmed as scheduled with the system yet."); Status shows "1 reminder needs a time"; the reminder editor offers "Date and time" and "Save time".
- **Evidence:** `reminder-5-01.png` (item), `reminder-5-02.png` (Status).
- **Settings:** none.

#### `reminder-6`
- **Prepare:** more reminders than the system holds (64 pending). Create 70 text captures "Remind me <date> <distinct time> to call the dentist" with distinct `YYYY-MM-DD HH:MM:SS` times over several days (use `S-DENTIST` variations; this takes time, batch it).
- **Steps:** Save them one after another. Open Status.
- **Expect:** (protocol) an explicit `capacity_exceeded` state, a documented refill policy and no silent dropping: the later reminders read "Reminder not scheduled: too many pending reminders." or "Held back"; Status says the system holds only so many and that the rest wait until there is room; counts add up (scheduled + held back = saved).
- **Evidence:** `reminder-6-01.png` (Status), `reminder-6-02.png` (one held-back item). Notes: counts only. Citations: P05 capacity finding in `docs/validation/evidence/device-feasibility/2026-10-09-device-matrix.json`.
- **Settings:** none.

#### `reminder-7`
- **Prepare:** `S-DENTIST`.
- **Steps:** Save `S-DENTIST` twice in a row as two captures at the same due time. In Source detail of the first, tap "Save time" without a change. Wait for delivery.
- **Expect:** (protocol) the OS notification is not duplicated for one reminder: one notification arrives for each reminder, none twice. (Two separate captures are two reminders; saving the same time again does not make a third.)
- **Evidence:** `reminder-7-01.png` (Notification Center after delivery — private, keep local). Notes: notifications received vs reminders.
- **Settings:** none.

#### `reminder-8`
- **Prepare:** a pending reminder and a pending interpretation.
- **Surrogate (a signed device build cannot replay transport):** (1) Save a capture, force-quit and relaunch three times within a minute, then count: one item, one reminder, one pending job in Status. (2) Toggle Airplane Mode on and off three times during a running job and count again. (3) From a Shortcut that runs "Open Oh And Capture" three times in a row, check no capture appears that was not typed or spoken.
- **Expect:** (protocol) no duplicate captures, lost acknowledgments or duplicate jobs after a retry or re-delivery.
- **What the surrogate shows and does not show:** it shows that app restarts, connectivity changes and repeated entry do not duplicate, using the real storage and scheduler. It does not replay a provider request, a transport retry or a handoff re-delivery of an already-processed capture; those are injected only in the automated tests.
- **Automated evidence (the injected form):** `ios/Tests/FailureJourneys/RestartAndDeletionFailureJourneyTests.swift`, `ios/Tests/JobRunner/JobRunnerBoundaryTests.swift`, `core/tests/job_runner/main.rs`, `core/tests/job_queue/`, `core/tests/interpretation_dispatch/`, and the T02b, T04a, T04b, T12a and T12b evidence.
- **Evidence:** `reminder-8-01.png` (Home with one item), `reminder-8-02.png` (Status counts).
- **Settings:** Airplane Mode toggled.

#### `reminder-9`
- **Prepare:** `S-DENTIST`.
- **Steps:** Save it. Open the item, change the time with "Date and time", tap "Save time". Then tap "Cancel reminder". Check Settings → Status and the notification list.
- **Expect:** (protocol) the OS notification is removed and state reconciled: after "Save time", one reminder at the new time ("Saved."); after "Cancel reminder", "Reminder cancelled."; the reminder history lists "Time saved" and "Cancelled"; no notification arrives at either time.
- **Evidence:** `reminder-9-01.png` (editor), `reminder-9-02.png` (history). Notes: notifications received (0 expected).
- **Settings:** none.

#### `reminder-10`
- **Prepare:** two reminders a minute ahead.
- **Steps:** With the phone locked, when the notification appears, press and hold it (or pull it down) to show actions. Tap "Acknowledge" on the first and "Done" on the second (it asks for authentication). Open each item and its history.
- **Expect:** (protocol) action handling is correct and does not corrupt state: history shows "Acknowledged" for the first; the second is completed ("This item is completed.", history shows the action) after authentication; no other item changes.
- **Evidence:** `reminder-10-01.png` (the actions — private, keep local), `reminder-10-02.png` (history of each).
- **Settings:** none.

### 4.4 Retrieval and resurfacing cases

#### `retrieval-1`
- **Prepare:** no provider configured; `S-PLAIN` text and `S-WORK`, saved over two different days if possible.
- **Steps:** Home → Search. Type "garden"; open the Type filter chip, the Time filter ("Today", "Past 7 days") and the Topic filter. Search for "zzzz".
- **Expect:** (protocol) literal search finds the original text with the date and type filters (and the topic filter) and shows an honest no-result state: "1 result" with the matched words highlighted; "No matches" for the unmatched search.
- **Evidence:** `retrieval-1-01.png` (results), `retrieval-1-02.png` (no matches).
- **Settings:** none.

#### `retrieval-2`
- **Prepare:** captures made at least 2 days before this step (N ≥ 2, record N in notes).
- **Steps:** Do not open the app for N days. Launch it.
- **Expect:** (protocol) prior captures are still accessible, and there is no catch-up wizard or overdue backlog: Home shows recents under "Earlier"; nothing asks the user to clear a backlog.
- **Evidence:** `retrieval-2-01.png`. Notes: N days.
- **Settings:** none.

#### `retrieval-3`
- **Prepare:** `S-PERSONAL` saved.
- **Steps:** Lock the phone, then open Oh And without unlocking (Face ID declined or covered). Attempt Search and open Source detail.
- **Expect:** (protocol) locked access is denied: Home shows "Your captures are locked. You can still save new ones. Unlock to see what you saved before."; Search and Source detail show "Locked" and no content.
- **Evidence:** `retrieval-3-01.png`.
- **Settings:** Face ID on; decline authentication.

#### `retrieval-4`
- **Prepare:** `S-PERSONAL` saved. A Work-scoped item cannot be created through the shipped screens: Home, the Capture entry and the Shortcut action stamp the configured default scope (Personal), and Source detail shows scope without a control to change it. Read the scope label of a saved `S-WORK` on the candidate; only if it reads "Work" (a candidate configured with a Work default) is the cross-scope half observable.
- **Steps:** Unlock with "Unlock" and authenticate. Search for each phrase and open each item. Search offers Type, Time and Topic chips and "Include done"; it has no scope chip, so scope is read from the "Work" or "Personal" label on each result and in Source detail.
- **Expect:** (protocol) personal and work items are both retrievable with no cross-scope leakage: each result is labelled "Work" or "Personal" correctly, and a search for one phrase never returns the other scope's item. When the candidate cannot produce a Work item, record `blocked` with the reason `no shipped control creates a Work-scoped item`, put "Personal label verified" in `notes` if the `S-PERSONAL` half held, and cite the automated cross-scope evidence at `core/tests/retrieval/main.rs`. Do not mark it passed on the Personal half alone.
- **Evidence:** `retrieval-4-01.png`, `retrieval-4-02.png`.
- **Settings:** Face ID on.

#### `retrieval-5`
- **Prepare:** `S-CORRECTED`.
- **Steps:** Open the item. Under "Correct this item", edit "Corrected words", tap "Save words". Search for the original word and the corrected word.
- **Expect:** (protocol) both original and corrected text are available with clear attribution: Source detail shows "Original text" and "Your correction"; Search shows "Matched the original words, which a correction replaced." for the original word.
- **Evidence:** `retrieval-5-01.png` (Source detail), `retrieval-5-02.png` (search).
- **Settings:** none.

#### `retrieval-6`
- **Prepare:** one eligible item saved; Notifications allowed.
- **Steps:** Settings → Daily prompt, switch the "Daily prompt" toggle on, set "Time" two minutes ahead; read the preview. Lock the phone. When the notification ("Oh And" / "Anything to jot down? Open Oh And.") arrives, tap it, and use the actions on the Suggestion screen ("Done", "Not now", "Stop suggesting this").
- **Expect:** (protocol) the prompt activates at the configured time and shows the current eligible item, and the response actions work; nothing from your notes appears on the lock screen.
- **Evidence:** `retrieval-6-01.png` (Daily prompt screen with preview), `retrieval-6-02.png` (Suggestion), `retrieval-6-03.png` (the lock screen — private).
- **Settings:** Notifications allowed.

#### `retrieval-7`
- **Prepare:** as `retrieval-6`, the prompt set for five minutes ahead, with exactly one eligible item.
- **Steps:** Before it fires, open the eligible item and "Delete this item" → "Delete permanently". Wait past the time; the generic prompt ("Anything to jot down? Open Oh And.") still fires because it carries no item. Tap it.
- **Expect:** (protocol) no stale item is resurfaced: the shipped prompt is generic and picks from current eligibility only when opened, so the deleted item does not appear on the Suggestion screen (which shows no eligible item), and nothing from the deleted note appears in the notification. Settings → Daily prompt "Next prompt" shows the next date.
- **Evidence:** `retrieval-7-01.png` (the Daily prompt screen after the deletion). Notes: whether the Suggestion screen showed the deleted item (it must not).
- **Settings:** Notifications allowed.

### 4.5 Configuration and switching

#### `config-1`
- **Prepare:** two cloud providers (OpenAI and Anthropic) with synthetic-usage test keys you control; `S-PLAIN`. If a key is unavailable, `blocked` with the reason.
- **Steps:** Settings → Processing → "Add provider" for each. In Advanced, "Check connection" for each and "Make default". Capture `S-PLAIN`, then switch the default and capture a similar phrase, several times. Open "Saved profiles" and Source detail.
- **Expect:** (protocol) sources and corrections unchanged; interpretations may differ but no stored data mutates; queued jobs keep the profile version they were pinned to; no secret displayed (the key field is a secure "Secret (API key)" field and the credential line reads "Credential name: …").
- **Evidence:** `config-1-01.png` (Advanced, saved profiles), `config-1-02.png` (an item sorted under each).
- **Settings:** network on.

#### `config-2`
- **Prepare:** a provider profile whose "Text interpretation" capability reads "unavailable, not verified" in Advanced. A profile with only Transcription or Embeddings unavailable does not qualify: a text capture uses text interpretation and may process legitimately. If no available profile has Text interpretation unavailable, record `blocked` with that reason.
- **Steps:** Settings → Processing → Advanced; read the capability lines; try "Make default" and capture `S-PLAIN` as text; open Status.
- **Expect:** (protocol) explicit unavailable or unsupported messaging: "Text interpretation: unavailable, not verified. It will not process notes until verification evidence exists." and the default notice "This default cannot process notes yet…"; the text capture is saved and waits on the device (Status shows it waiting), never sent to another provider.
- **Evidence:** `config-2-01.png`.
- **Settings:** none.

#### `config-3`
- **Prepare:** a working cloud provider and an item sorted with it.
- **Steps:** In Advanced, "Replace secret" with an invalid value (or remove it with "Remove secret"); capture `S-PLAIN`; open Status.
- **Expect:** (protocol) the next job fails explicitly with an authorization error and offers recovery: the note is under "Notes that did not sort" with a reason, Status offers "Open Provider settings" and "Try again" (or Source detail "Try again").
- **Evidence:** `config-3-01.png`, `config-3-02.png`.
- **Settings:** none.

#### `config-4`
- **Applies:** only when the candidate enables `self_hosted_provider` (section 2). Otherwise `not_applicable` with `capability not enabled in candidate`.
- **Prepare:** a self-hosted endpoint you control, added via "Add provider" with type "Private server" and the "https://server address" field.
- **Steps:** Make it the default ("With my own server"). Make the endpoint unreachable (stop it). Capture `S-PLAIN`. Open Status. Start the endpoint again.
- **Expect:** (protocol) the job fails with a network error and waits for recovery, never falling back to another provider: "The provider could not be reached" in Status; no other provider receives it; "Try again" succeeds after the endpoint is back.
- **Evidence:** `config-4-01.png`, `config-4-02.png`.
- **Settings:** same network as the endpoint.

### 4.6 Processing state and transitions

#### `processing-1`
- **Prepare:** a working provider or the on-device route; `S-DENTIST`.
- **Steps:** Save `S-DENTIST`. Watch Home through "Just now · sorting…", Source detail, and Status through each stage.
- **Expect:** (protocol) save, processing and reminder state are independently reportable and honest: never "processed" while pending ("Not processed yet."), never "scheduled" until the reminder is confirmed ("Reminder set for …").
- **Evidence:** `processing-1-01.png` (just saved), `processing-1-02.png` (sorted), `processing-1-03.png` (Source detail with the reminder).
- **Settings:** none.

#### `processing-2`
- **Prepare:** a cloud provider with a credential that will be rejected, or an endpoint that times out.
- **Surrogate (a signed build cannot inject a timeout or an invalid response):** replace the secret in Settings → Processing → Advanced with an invalid one ("Replace secret"), or make the self-hosted endpoint unreachable or Airplane Mode on while a job runs; capture `S-PLAIN`.
- **Expect:** (protocol) the source remains searchable, an explicit uninterpreted state is recorded and the item is retrievable without mandatory triage: the item is found by Search, reads "Not yet sorted", appears under "Notes that did not sort", and nothing forces the user to act.
- **What the surrogate shows and does not show:** it shows the failure path with a real rejected credential or unreachable network, end to end. It does not produce an invalid response body or a provider timeout of a chosen length; those are injected in the automated tests.
- **Automated evidence (the injected form):** `ios/Tests/FailureJourneys/ProviderAndPermissionFailureJourneyTests.swift`, `ios/Tests/JobRunner/JobRunnerBoundaryTests.swift`, `core/tests/job_runner/main.rs`, `core/tests/interpretation_dispatch/`, `core/tests/job_queue/`, and the T02b, T04a, T04b, T12a and T12b evidence.
- **Evidence:** `processing-2-01.png` (Status), `processing-2-02.png` (Search finding the item).
- **Settings:** Airplane Mode or a bad credential.

#### `processing-3`
- **Prepare:** two cloud profiles, one reachable and one not (invalid secret or unreachable server).
- **Surrogate (a signed build cannot force a partial batch failure):** capture five notes in a row; between the third and fourth, switch the default to the unavailable profile (or toggle Airplane Mode on for two of them), then back; let the queue drain.
- **Expect:** (protocol) all sources intact, successful items processed, failed items kept for retry or correction: the three notes sorted, the two failed under "Notes that did not sort" with "Try again", and each failed note can be sorted by "Try again" afterwards.
- **What the surrogate shows and does not show:** it shows a mixed batch end to end with real providers. It does not make a chosen job in one batch fail; the injected form is in the automated tests.
- **Automated evidence (the injected form):** `ios/Tests/FailureJourneys/ProviderAndPermissionFailureJourneyTests.swift`, `ios/Tests/JobRunner/JobRunnerBoundaryTests.swift`, `core/tests/job_runner/main.rs`, `core/tests/job_queue/`, `core/tests/interpretation_dispatch/`, and the T02b, T04a, T04b, T12a and T12b evidence.
- **Evidence:** `processing-3-01.png` (Status), `processing-3-02.png` (Home recents). Notes: counts (3 sorted, 2 failed, 2 retried).
- **Settings:** Airplane Mode or the default profile switched.

### 4.7 Items P09 deferred to this matrix

#### `deferred-speech-lock`
- **Prepare:** a 30 s voice capture of `S-DENTIST` padded; Speech Recognition granted; Airplane Mode on.
- **Steps:** Record, "Stop recording", lock the phone at once (within 1 s), wait 20 s, unlock.
- **Expect:** (P09 amendment 2) offline on-device speech recognition survives an immediate lock: the transcript appears or the item says "Not transcribed yet." and finishes after unlock; the audio is kept; no duplicate.
- **Evidence:** `deferred-speech-lock-01.png`.
- **Settings:** Airplane Mode on.

#### `deferred-speech-model-install`
- **Prepare:** a locale whose on-device model is not installed (iOS Settings → General → Keyboard → Dictation languages / Language & Region; the trial is en-US, whose model is present).
- **Steps:** Switch the phone's language to the missing locale, record `S-DENTIST` in it.
- **Expect:** (P09 amendment 4) the item says "Can't be transcribed on this phone." without a silent fallback; note the install path (size, network need) as seen in iOS.
- **Evidence:** `deferred-speech-model-install-01.png`. Notes: size and whether a network is needed (counts only).
- **Settings:** Language & Region changed, restore after.

#### `deferred-capture-before-first-unlock`
- **Prepare:** Control Center control "Oh And Note" added; reminders pending.
- **Steps:** Restart the phone. Do not unlock. Press the lock-screen control and capture `S-PLAIN`. Then unlock and open Home.
- **Expect:** (P09 amendment 2) the capture does not fail silently: the composer reads "Not unlocked since restart. Saving may fail, but what you enter is kept."; the text is kept; after unlock the capture is in recents once.
- **Evidence:** `deferred-capture-before-first-unlock-01.png` (taken after unlock; nothing can be captured before).
- **Settings:** none.

#### `deferred-termination-handoff-commit`
- **Prepare:** a Control Center entry.
- **Steps:** Press "Oh And Note", type text, and force-quit within 1 s of tapping "Save"; repeat 5 times. Relaunch; count items.
- **Expect:** (P09 amendment 2) never two copies; a lost capture is reported as such: Home shows 0 or 1 copy per attempt, and the text is in the composer or in Unfinished recordings (voice) if not committed.
- **Evidence:** `deferred-termination-handoff-commit-01.png`. Notes: attempts, copies found.
- **Settings:** none.

#### `deferred-low-free-space`
- **Prepare:** fill the phone's free space to under about 200 MB with synthetic filler (a large synthetic video recording).
- **Steps:** Record a voice capture and a text capture.
- **Expect:** (P09 amendment 2) refusal under low free space is explicit, not silent loss: "Stopped: storage is low." for audio, "Not saved. Your text is still here." for text; the work in progress is kept.
- **Evidence:** `deferred-low-free-space-01.png`.
- **Settings:** free space reduced; restore after.

#### `deferred-interruption-duration`
- **Prepare:** a recording.
- **Steps:** Start a screen recording; record 20 s with a stopwatch; trigger an interruption (alarm or timer) at 12 s. Compare the saved partial's length with the interruption time.
- **Expect:** (P09 amendment 4) the partial reaches to within about one second of the moment the interruption began: the item or Unfinished recording shows about 12 s.
- **Evidence:** `deferred-interruption-duration-01.mp4`. Notes: seconds recorded vs. seconds to the interruption.
- **Settings:** a timer set for the interruption.

#### `deferred-notification-focus-summary`
- **Prepare:** two reminders a few minutes ahead.
- **Steps:** Turn on a Focus mode that allows no apps; also enable Scheduled Summary for Oh And in iOS Settings → Notifications. Wait for the reminders. Leave Focus and check Notification Center and the summary.
- **Expect:** (P09 amendment 2) the notification is not lost to Focus or the summary: it is delivered to Notification Center or the summary; neither is dropped. The daily prompt and reminders show no item content on the lock screen.
- **Evidence:** `deferred-notification-focus-summary-01.png` (private, keep local).
- **Settings:** Focus on; Scheduled Summary on; restore after.

#### `deferred-low-power-mode`
- **Prepare:** a reminder a few minutes ahead.
- **Steps:** Turn Low Power Mode on; force-quit the app; wait for delivery; open the notification.
- **Expect:** (P09 amendment 2) delivery and opening are unaffected: the notification arrives and opens the item.
- **Evidence:** `deferred-low-power-mode-01.png`.
- **Settings:** Low Power Mode on.

#### `deferred-reboot`
- **Prepare:** two future reminders (10 minutes and 3 hours).
- **Steps:** Restart the phone. Unlock after the first reminder's time has passed or before. Check delivery of the 10-minute reminder and Status.
- **Expect:** (P09 amendment 2) pending reminders survive reboot: the 10-minute reminder is delivered (even before first unlock), the 3-hour one is still scheduled in the item.
- **Evidence:** `deferred-reboot-01.png`, `deferred-reboot-02.png` (Status).
- **Settings:** none.

#### `deferred-time-zone-change`
- **Prepare:** `S-DENTIST` scheduled for tomorrow at 09:00:00.
- **Steps:** In iOS Settings → General → Date & Time turn off Set Automatically and change the time zone by several hours. Open the item; open Settings → Status.
- **Expect:** (P09 amendment 2) the reminder keeps the zone it was scheduled in or says so honestly: the reminder editor reads "Time zone of this reminder: <zone>"; the due instant matches the zone the person said it in; nothing is rescheduled without a record.
- **Evidence:** `deferred-time-zone-change-01.png`.
- **Settings:** time zone changed; restore after.

#### `deferred-clock-date-dst`
This case always crosses both known US transitions in America/New_York; an ordinary date change alone is not evidence. The next ones after the session date are 2027-03-14 (02:00 EST becomes 03:00 EDT, a gap) and 2027-11-07 (02:00 EDT becomes 01:00 EST, so 01:00–01:59 happens twice, a fold). If the session runs later, use the next pair (second Sunday of March, first Sunday of November) and write the dates in `notes`.
- **Prepare:** iOS Settings → General → Date & Time: Set Automatically off, time zone New York. No provider needed. With the real date, save four text captures: "Remind me 2027-03-14 02:30:00 to call the dentist" (inside the gap), "Remind me 2027-11-07 01:30:00 to call the dentist" (inside the fold), "Remind me 2027-03-14 03:05:00 to call the dentist" and "Remind me 2027-11-07 02:05:00 to call the dentist" (just after each transition).
- **Steps:** (1) Open the first two items' reminders. (2) Set the clock to 2027-03-14 01:58 and keep Oh And closed; wait for the 03:05 reminder (about seven minutes, the clock jumps from 01:59:59 to 03:00:00); open the app and Status. (3) Set the clock to 2027-11-07 01:55 (EDT, before the fold) and keep Oh And closed; wait through 01:59:59 EDT → 01:00 EST and on to 02:05 EST (about 70 minutes); open the app and Status. (4) Move the date forward one more day and back to the real date; open Status. Restore Set Automatically.
- **Expect:** (P09 amendment 4) DST gap and fold times are not guessed and the clock and date changes neither duplicate nor lose a reminder: (1) both read "Time incomplete" with "… but a clock change means that time does not exist or happens twice, so nothing is scheduled. Choose a date and time." and nothing is pending for them; (2) the 03:05 reminder fires once, at 03:05 EDT; (3) the 02:05 reminder fires once, at 02:05 EST, not during the first pass through 01:55–02:00 EDT; (4) every reminder stays as recorded (delivered ones are not re-delivered; others read "Time has passed" only if their time passed) and Status counts are honest.
- **Route, checked against the code:** the fast path records the gap and fold phrases as `not_scheduled_yet` with the resolver's DST reason (`core/tests/fast_path/main.rs`, `dst_fold_and_gap_are_ambiguous_without_an_instant`; `core/src/time/resolver.rs`), which the editor shows as the clock-change problem (`app/reminders/ReminderEditText.swift`); the two after-transition phrases resolve to 07:05Z on each date. If the device will not take the date (a managed phone), record `blocked` with the reason.
- **Evidence:** `deferred-clock-date-dst-01.png` (gap item), `deferred-clock-date-dst-02.png` (fold item), `deferred-clock-date-dst-03.mp4` and `deferred-clock-date-dst-04.mp4` (lock screen through each delivery — private, keep local), `deferred-clock-date-dst-05.png` (Status after step 4). Notes: notifications received per reminder and the clock time of each.
- **Settings:** Set Automatically off, time zone New York, clock set to each transition; restore after.

#### `deferred-provisional-ephemeral-authorization`
The shipped app cannot obtain either state. `NotificationPermissionAsker` (`ios/Services/Permissions/NotificationPermissionAsker.swift`) requests only `.alert` and `.sound`; iOS grants provisional authorization only to a request that includes `.provisional`, and choosing Deliver Quietly after an ordinary grant leaves the status `authorized`, not `provisional`. Ephemeral authorization exists only for App Clips, and the candidate is the full app. A quiet authorized notification is not evidence for this case.
- **Prepare:** a fresh install (delete and reinstall the same candidate), Notifications not asked.
- **Steps:** Open Oh And and do nothing that asks for Notifications. Check iOS Settings → Notifications → Oh And: on this candidate it must not exist yet (it would exist, with quiet delivery, only if provisional had been granted without a prompt). Then trigger the ask (Settings → Permissions) and note the alert iOS shows.
- **Expect:** (P09 amendment 2) the outcome is `blocked`, reason `candidate requests only alert and sound authorization; provisional and ephemeral (App Clip only) states cannot be obtained`, unless the candidate differs from `main` and the steps show a provisional grant (Oh And listed in iOS Notifications settings before any prompt, with quiet delivery), in which case continue: schedule a reminder, and expect Permissions and Status to report what iOS granted without claiming full authorization and the reminder to arrive quietly in Notification Center. The ephemeral half stays `blocked` on the full app in any case; write it in `notes`.
- **Automated evidence (mapping only, not the device state):** `ios/Tests/NotificationPermission/NotificationReadinessTests.swift` and `ios/Tests/Permissions/PermissionsServiceTests.swift` map `.provisional` (and `ios/Services/NotificationPermission/NotificationPermissionModels.swift` maps `.ephemeral`) to usable.
- **Evidence:** `deferred-provisional-ephemeral-authorization-01.png` (iOS Notifications list before the ask), `deferred-provisional-ephemeral-authorization-02.png` (the alert iOS shows).
- **Settings:** fresh install; Notifications not asked.

#### `deferred-authorization-change-pending`
- **Prepare:** three pending reminders.
- **Steps:** While they are pending, turn Notifications off in iOS Settings, return to the app, check Status; turn them on again; check.
- **Expect:** (P09 amendment 4) the app notices: Status says "Notifications were turned off" / "Reminders will not be shown", and on restore the reminders are scheduled again with counts matching.
- **Evidence:** `deferred-authorization-change-pending-01.png`, `deferred-authorization-change-pending-02.png`.
- **Settings:** Notifications toggled.

#### `deferred-pending-slots-delivered`
- **Prepare:** the daily prompt on (Settings → Daily prompt), and three reminders a few minutes ahead.
- **Steps:** Let the three reminders be delivered and leave them in Notification Center. Then add reminders as in `reminder-6` until Status reports a hold; write down how many reminders were scheduled before the first was held back.
- **Expect:** (P09 amendment 4) delivered notifications do not use pending slots: the hold begins at the same count as with an empty Notification Center (64 pending requests in all, the three delivered ones not counted, the daily prompt's pending occurrences counted); Status reports the counts honestly ("<n> daily prompt(s) wait(s) for room …" only when true).
- **Evidence:** `deferred-pending-slots-delivered-01.png` (Status at the hold). Notes: counts only (delivered, scheduled, held back, prompt occurrences).
- **Settings:** none.

#### `deferred-pending-slots-repeating`
The shipped app schedules no repeating trigger, so whether one consumes a pending slot cannot be observed on the candidate. The daily prompt is planned as up to `PROMPT_HORIZON_DAYS` (3) separate dated occurrences (`core/src/suggestions/schedule/plan.rs`), and `SystemNotificationCenter.makeRequest` (`ios/Services/Notifications/SystemNotificationCenter.swift`) builds every request with `UNCalendarNotificationTrigger(..., repeats: false)`; each pending occurrence is one ordinary one-shot request, which `deferred-pending-slots-delivered` counts.
- **Steps:** none on the device, unless the candidate differs from `main` and schedules a repeating trigger: then run the steps of `deferred-pending-slots-delivered` with it pending and record whether it takes one slot.
- **Expect:** (P09 amendment 4) the outcome is `blocked`, reason `candidate schedules no repeating trigger; every request is one-shot`. Do not record the dated prompt occurrences as a repeating trigger.
- **Evidence:** none for `blocked` (the reason is the record).
- **Settings:** none.

#### `deferred-reinstall-os-upgrade`
- **Prepare:** reminders pending and notes saved on the candidate.
- **Steps:** Install the same candidate over itself (same build number), then delete the app and install again; if an OS update is pending, apply it and relaunch.
- **Expect:** (P09 amendment 2) data survives an update-in-place; a delete removes data, and restoring behaves as the product states (restore or fresh): record which, with counts; reminders after a delete do not fire for deleted data.
- **Evidence:** `deferred-reinstall-os-upgrade-01.png`. Notes: items before and after.
- **Settings:** none.

#### `deferred-keychain-passcode-removal`
- **Prepare:** a provider credential stored.
- **Steps:** Remove the passcode (iOS Settings → Face ID & Passcode → Turn Passcode Off), relaunch Oh And, open Settings → Processing; set the passcode again.
- **Expect:** (P09 amendment 2) the credential is not leaked and the app reports it honestly: the key is unavailable or still protected; "Secret (API key)" shows no key; Processing says what to do.
- **Evidence:** `deferred-keychain-passcode-removal-01.png`.
- **Settings:** passcode off; restore after (this erases Face ID data).

#### `deferred-audio-fixture-after-revoke`
- **Prepare:** a recorded voice capture whose audio is kept.
- **Steps:** Revoke Microphone in iOS Settings while Oh And is in the background; return; open the item and tap "Play audio".
- **Expect:** (P09 amendment 2) the earlier recording is still there and plays: playback does not need the microphone; the "Record" control is honest.
- **Evidence:** `deferred-audio-fixture-after-revoke-01.png`.
- **Settings:** Microphone revoked; restore after.

#### `deferred-entry-latency-cold-warm`
- **Prepare:** a Shortcut with the action "Open Oh And Capture" (Destination "Capture", "Voice capture" or "Silent capture"); there is no built-in shortcut, so create it in the Shortcuts app and assign it to the Action button (iOS Settings → Action Button → Shortcut) on a device that has one.
- **Steps:** Screen-record. Cold: force-quit Oh And, press the Action button, wait for ready. Warm: with Oh And in the background, press again. Five trials each (section 5).
- **Expect:** (P09 amendment 3) the latency is measured and reported as the two series `trigger_to_ready.shortcut_cold` and `trigger_to_ready.shortcut_warm`; the entry opens the capture surface.
- **Evidence:** `deferred-entry-latency-cold-warm-01.mp4` (cold), `deferred-entry-latency-cold-warm-02.mp4` (warm). Notes: sample counts.
- **Settings:** Action button assigned to the Shortcut.

#### `deferred-untimed-voice-start`
- **Prepare:** none.
- **Steps:** Tap "Record" on Home and watch for "Waiting for the microphone…" then "Recording".
- **Expect:** (P09 amendment 3) voice Start works without a timed measurement and is not blocked by the timer; no start failure.
- **Evidence:** `deferred-untimed-voice-start-01.mp4`.
- **Settings:** Microphone allowed.

#### `deferred-text-field-ready`
- **Prepare:** screen recording.
- **Steps:** Launch the app (cold and warm); measure from touch-down on the icon to a focused field "What's on your mind?" with the caret (section 5).
- **Expect:** (P09 amendment 3) the field takes input at once; timing reported as `trigger_to_ready.text_field`.
- **Evidence:** `deferred-text-field-ready-01.mp4`.
- **Settings:** none.

### 4.8 Plan items

Each roll-up is `passed` only if all the listed cases are `passed` in this candidate; any `failed` makes it `failed`; any `blocked` makes it `blocked`.

#### `plan-cold-locked-permission`
- **Rolls up:** `state-1`, `state-2`, `state-3`, `offline-5`, `offline-6`, `deferred-capture-before-first-unlock`.
- **Steps:** none beyond the cases; take one Home screenshot at the end.
- **Expect:** (plan T05) cold, locked and permission paths verified with the candidate's build identifiers.
- **Evidence:** `plan-cold-locked-permission-01.png`.

#### `plan-offline-recording-text`
- **Rolls up:** `offline-1`, `offline-4`, `reminder-1`, `deferred-speech-lock`.
- **Expect:** (plan T05) offline recording and text verified.
- **Evidence:** `plan-offline-recording-text-01.png`.

#### `plan-process-interruption`
- **Rolls up:** `offline-2`, `offline-3`, `state-4`, `state-5`, `deferred-termination-handoff-commit`.
- **Expect:** (plan T05) process interruption verified.
- **Evidence:** `plan-process-interruption-01.png`.

#### `plan-reminders-ui-closed`
- **Rolls up:** `reminder-2`, `reminder-7`, `reminder-10`, `deferred-reboot`.
- **Expect:** (plan T05) local reminders with the UI closed verified.
- **Evidence:** `plan-reminders-ui-closed-01.png`.

#### `plan-pipeline-faults`
- **Rolls up:** `processing-1`, `processing-2`, `processing-3`, `offline-4`, `config-3`.
- **Expect:** (plan T05) pipeline faults verified, with the surrogates' limits stated in the record's `notes`.
- **Evidence:** `plan-pipeline-faults-01.png`.

#### `plan-mac-asleep`
- **Rolls up:** `state-6`. `not_applicable` with `Mac inlet not included in candidate` exactly when `state-6` is.
- **Expect:** (plan T05) the Mac-asleep case per the protocol's own rule.
- **Evidence:** `plan-mac-asleep-01.png` when run.

### 4.9 Capability cases

#### `capability-self-hosted`
- **Applies:** when Settings → Processing → "With my own server" is selectable (section 2).
- **Steps:** Run `config-4`; additionally "Check connection" on the "Private server" profile and capture `S-DENTIST` through it.
- **Expect:** (plan T05 criterion 4) the capability the candidate claims works end to end, within its route grants (Settings → Privacy shows what may leave the device).
- **Evidence:** `capability-self-hosted-01.png`.

#### `capability-shadow`
- **Applies:** when Settings → Privacy offers a "Quality review" grant and the candidate claims shadow review.
- **Steps:** "Allow quality review via …"; capture `S-PLAIN`; confirm the review does not alter the stored item or change any state the user sees as authoritative; "Stop quality review" to revoke.
- **Expect:** nothing stored is mutated by the review, and revoking stops it.
- **Evidence:** `capability-shadow-01.png`.

#### `capability-previews`
- **Applies:** only when the candidate claims a preview feature and a screen offers it.
- **Steps:** use the feature on a synthetic item, as its owning task's canary describes.
- **Expect:** the feature behaves within its route grants and does not send anything the grants refuse.
- **Evidence:** `capability-previews-01.png`.

## 5. Latency method

The method is the one in [`evidence/device-feasibility/2026-10-10-latency-accessibility.json`](evidence/device-feasibility/2026-10-10-latency-accessibility.json) (`method.latency`): iOS screen recording (Control Center → Screen Record), frames extracted one-to-one with their timestamps, the trigger taken as the first frame in which the pressed control is highlighted (touch-down), and the ready frame the first frame in which the capture surface is steady and ready (text field focused, microphone indicator live or recording control ready). Resolution is one frame (about 16.7 ms at 60 fps). Because touch-up is not drawn, every figure is measured from touch-down, which makes it an upper bound on release-to-ready.

| Series (`latency` key) | Trigger and end point | Cases |
| --- | --- | --- |
| `trigger_to_ready.cold_launch` | icon touch-down on a force-quit app to ready | `state-1` |
| `trigger_to_ready.warm_switch` | switch from another app or from the lock screen to ready | `state-2` |
| `trigger_to_ready.locked_handoff` | Control Center control press on the lock screen to ready | `state-3` |
| `trigger_to_ready.shortcut_cold` | Action button or Shortcut press on a force-quit app to ready | `deferred-entry-latency-cold-warm` |
| `trigger_to_ready.shortcut_warm` | the same press with Oh And in the background to ready | `deferred-entry-latency-cold-warm` |
| `trigger_to_ready.text_field` | icon touch-down to a focused text field | `deferred-text-field-ready` |
| `end_of_input_to_save.voice` | "Stop recording" tapped to the "Saved" pill | `offline-1`, `deferred-untimed-voice-start` |
| `end_of_input_to_save.text` | "Save" tapped to the "Saved" pill | `offline-1` |

Rules:

- At least **five trials per series**, counted; cold and warm launches are separate series and each needs its five. The checker refuses a series with fewer than five samples.
- The record holds, per series, `sample_count`, `median_ms`, `p95_ms`, `min_ms` and `max_ms`, and nothing else from the recordings (no per-trial list, no recording content).
- Exclude network time; the end point of a save is the device-local acknowledgment, not interpretation.
- If the candidate contains F67 (the in-app content-free metrics export, Settings → Export → "Export metrics"), attach its export as a second source under its own artifact name, e.g. `metrics-export-01.json`, set as `second_source_artifact` on the series it covers. Do not wait for F67; a candidate without it records screen-recording timing only.
- Interpretation latency and reminder-scheduling latency from the protocol's measurement list are not in the record format; they come from the same F67 export when present, and from the trial (T08), not from this matrix.

## 6. Record format and checking

The record is `docs/validation/evidence/device-matrix/device-<collection date>-<candidate build number>.json`, format `ohand.device-matrix.v1`, described in [`evidence/device-matrix/README.md`](evidence/device-matrix/README.md). It holds `schema`, `collected_at`, `candidate_record`, `build_number`, `revision`, `device` (model and iOS version only), `capabilities_enabled`, `cases`, `latency`, `private_evidence_reference` and `issues`. It holds no capture text, no team, profile, certificate or device identifier, and no hash of one.

```
make device-matrix-check        # every record in docs/validation/evidence/device-matrix/
python3 tools/validation/device_matrix/check_device_matrix.py docs/validation/evidence/device-matrix/<record>.json
make device-matrix-test         # the checker's own tests
```

## 7. Results

The results section belongs to T05b. It is empty until the maintainer's record is committed.
