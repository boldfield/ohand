# M1 Actual-Device Feasibility Evidence

Status: in-progress, device evidence collected 2026-10-09; acceptance criteria incomplete.

## Device and Build Information

**Primary Device**: iPhone 16 Pro (trial-phone)

**Actual OS**: iOS 26.6.2 (build 23G90, verified via xcrun devicectl)

**Device Time Zone**: America/Los_Angeles

**Collection Period**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Collection Method**: Maintainer performed tests on the trial phone; coordinator extracted app containers with xcrun devicectl and recorded values from maintainer screenshots.

## Evidence Artifacts and Sources

**Primary evidence manifest**: `docs/validation/evidence/device-feasibility/2026-10-09-device-matrix.json` — comprehensive test results matrix with per-probe results and artifact citations.

**Sanitized artifacts**: Referenced by name in the evidence (audio-probe/recording-*.wav, transcription-probe/p04-*.png, etc.). Screenshots and WAV recordings are private and not committed to the repository.

**Private evidence**: Raw evidence retained locally on the maintainer's Mac at `~/.ohand-private-evidence/p09/` (not committed): full-resolution screenshots, app container exports, logs, and signing records.

**Build Revisions** — from manifest `builds` section:
- **BridgeProbe**: 6ac3d1f8 (signed, apple-signing-20261009T000932Z-c2cb6485.json)
- **AudioProbe**: 257343f3 (signed, apple-signing-20261009T164701Z-5b3f5c34.json)
- **TranscriptionProbe**: 257343f3 (signed, apple-signing-20261009T164718Z-e2dc1dae.json)
- **NotificationProbe**: 257343f3 (signed, apple-signing-20261009T164732Z-b11e7a0d.json)
- **CredentialProbe**: 257343f3 (manual archive + export, development profile; no tooling record)
- **CaptureProbe**: 257343f3 (manual archive + export; app + control extension; no tooling record)
- **TauriProbe**: 257343f3 (tauri ios build --debug, development route; no tooling record)

## Acceptance Criteria Assessment

Per parent task spec, 6 acceptance criteria:

### Parent AC1: Record Measured Cold/Warm/Locked/Unlocked Capture, Permission States, Interruptions, Offline ASR, Notifications, Handoff, Accessibility, Secure Credential Behavior

**Status**: Partially tested; unobservable and untested cases marked below.

#### P03: Audio Recording (docs/validation/audio-probe.md)
- **Build**: AudioProbe 257343f3
- **Artifacts**: audio-probe/recording-*.wav (8 files, 16 kHz mono Int16)
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Permission grant/revoke**: Granted: "Recording..." (3.33 s file saved). Revoked: "Failed to start recording: Microphone permission denied".
- **Normal stop**: 6.92 s file, size consistent with 32000 B/s; plays.
- **Cancel mid-recording**: 3.86 s partial file (127,732 bytes); file matches exactly and plays.
- **Audio session interruption**: 5.86 s partial file (191,764 bytes); file matches and plays.
- **Background recording (10 s)**: 9.71 s captured (314,676 bytes); signal present across entire span (no UIBackgroundModes: audio required).
- **Recording while locked (10 s)**: 8.19 s captured (266,100 bytes); signal continuous.
- **Extended recording (300+ s)**: UI timer continued past 500 s; stop reported 300.00 s (9,604,096 bytes); header consistent.
- **Process death**: Force-quit left unfinalized WAV (54,594 bytes, duration 0.000 s); not playable, as documented.

**Untested**:
- Low-free-space refusal: not tested.

#### P04: Transcription (Offline On-Device) (docs/validation/transcription-probe.md)
- **Build**: TranscriptionProbe 257343f3
- **Artifacts**: transcription-probe/p04-*.png
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Permission state**: Before request: "Pending: speech recognition permission not requested yet." After grant: en-US "Ready: on-device recognition is available"; fr-FR "Pending: on-device recognition unavailable"; zz-ZZ rejected with "language not supported".
- **Offline on-device transcription**: Airplane mode, all radios off: fixture transcribed in 0.32 s offline (mean confidence 0.98: "Remind me to call the dentist tomorrow at nine"). Same fixture with network on: 0.15 s (on-device confirmed).
- **Fail-closed behavior**: fr-FR with network on remains "Pending"; no cloud fallback attempted.
- **Permission revoked while backgrounded**: iOS relaunched app; on-device flag honored; no transcript attempted. Audio retained (note: survival of previous fixture not verified).
- **Timing measurements**: [0.32, 0.15, 0.28, 0.15, 0.15, 0.15, 0.15] seconds. Cold: 0.32 s; warm: 0.15 s.

**Not observable**:
- Voice recording interruption (fixture transcribes faster than lock/app-switch can occur, 0.15-0.32 s << interrupt latency).
- Voice recording cancellation (same reason).

#### P05: Notifications (docs/validation/notification-probe.md)
- **Build**: NotificationProbe 257343f3
- **Artifacts**: notification-probe/p05-*.png (p05-04 and p05-06 PERSONAL)
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Authorization**: Granted with alert, badge, sound, lock screen, notification center, and banner alerts enabled.
- **Foreground delivery**: Banner shown; willPresentCalled true; deliveredContainsRequest true.
- **Closed-app delivery**: App force-quit, screen on, notification scheduled 5 s out: banner appeared on relaunch. Reported: closedAppDeliveredDateUTC 2026-10-09T18:20:47Z, closedAppDeliveredPresent true, stillPending false.
- **Locked delivery**: Notification shown on lock screen; reportDelivered: delivered 2026-10-09T18:21:50Z, present true.
- **Capacity boundary**: 100 requested; OS retained 64 (most-recent-first); oldest silently dropped.
- **Calendar trigger**: Pacific/Auckland target 2026-10-13 09:30 → nextTrigger 2026-10-12T20:30:00Z (delta 0 s).

**Not tested**:
- Focus modes and notification summaries.
- Low Power Mode.
- Reboot survival.
- Time-zone change after scheduling.
- Provisional and ephemeral authorization.
- Reinstall or OS upgrade survival.

#### P07: Handoff (Tauri to Capture) (docs/validation/tauri-handoff.md)
- **Build**: TauriProbe 257343f3, CaptureProbe 257343f3
- **Artifacts**: tauri-handoff/p07-*.png, container-tauri-probe/, container-capture/, run-events.log
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Cold launch**: Tauri probe force-quit; handoff listed identifier (CB02991B); handoff file webviewReady false (URL arrived before webview ready). CaptureProbe committed 2026-10-09T18:38:51.727Z; Tauri received 2026-10-09T18:38:56.688Z (latency: 4.961 s).
- **Warm launch**: Second handoff listed both identifiers; webviewReady true.
- **Hostile URL rejection**: ohand-tauri://capture?captureId=not-a-uuid rejected with "Handoffs rejected: 1"; list unchanged.
- **Large-text accessibility**: Tauri probe scales, nothing clipped. CaptureProbe "Review in management app" label overflows button (defect). AudioProbe Start/Stop labels truncate; CredentialProbe "Arm Locked Retrieval" truncates; BridgeProbe and NotificationProbe render correctly; TranscriptionProbe Cancel below fold but reachable.
- **Secret isolation**: Each handoffs/<id>.json contains exactly captureId, receivedAtUnixMs, webviewReady; capture text never appears. All listed identifiers exist in both containers; non-handed-off entries exist only in CaptureProbe.

**Not tested**:
- Keyboard navigation.
- VoiceOver (screen reader).

#### P11: Secure Credentials (Keychain with Lock/Relaunch) (docs/validation/credential-probe.md)
- **Build**: CredentialProbe 257343f3
- **Artifacts**: credential-probe/p11-*.png, container/locked-retrieval.log
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Locked retrieval** (armed 18:56:18Z, backgrounded 18:56:22Z):
  - Offsets 1, 3, 6 s (protectedDataAvailable true): All five classes status 0.
  - Offsets 10, 15, 20 s (protectedDataAvailable false, locked): WhenUnlocked, WhenUnlockedThisDeviceOnly, WhenPasscodeSetThisDeviceOnly return -25308 errSecInteractionNotAllowed; **AfterFirstUnlock and AfterFirstUnlockThisDeviceOnly status 0 with matching values** (readable while locked).
  - Background task expired 25 s after backgrounding.
- **Unlocked retrieval**: All five classes status 0, protectedDataAvailable true.
- **Relaunch** (force-quit, relaunch, retrieve): All five classes status 0.
- **Reboot** (store, reboot, first unlock, retrieve): All five classes status 0.

**Not tested**:
- Passcode removal.

#### P02: Capture Entry (Control Center) (docs/validation/capture-entry.md)
- **Build**: CaptureProbe 257343f3
- **Artifacts**: capture-probe/p02-*.png, container/CaptureProbe/records/
- **Collection Window**: 2026-10-09T17:30:00Z to 2026-10-09T19:12:00Z

**Tested scenarios**:
- **Control Center to entry**: ~2 s (maintainer estimate, not measured precisely).
- **Control from lock screen**: App opened showing new entry; entry reported protectedDataAvailable true (device was already unlocked by app foreground).
- **Entry state**: controlIntent, warm launch, protected data available, saved.
- **Distinct records**: Four control presses produced four distinct records; no duplicate perform() observed.

**Not tested**:
- Before first unlock after reboot.
- System termination between handoff and commit.

**Note**: The "lock screen entry" scenario did not measure capture WITH device physically locked. The device had already been unlocked when the app foregrounded (passive unlock not instrumented). This does not measure locked-capture timing or locked-data-access constraints.

### Parent AC2: Measure Trigger-to-Ready and End-of-Input-to-Durable-Save Separately; Physical-Device Evidence Cannot Be Substituted

**Status**: UNMET. Measurements do not match protocol definitions.

**Issues**:
- **Trigger-to-ready (Control Center → capture UI ready)**: Only a maintainer estimate (~2 s) is available; no measured distribution (median, p95, min/max).
- **Trigger-to-ready (Handoff cold/warm)**: The value 4.961 s (CaptureProbe commit 18:38:51.727Z → Tauri receipt 18:38:56.688Z) is post-save handoff duration, not UI-ready-to-capture latency.
- **End-of-input → durable-save (voice)**: No measurement provided. The 6.92 s value is recording duration, not save latency.
- **End-of-input → durable-save (transcription)**: The 0.15-0.32 s values are transcription latency, not save latency (protocol requires release/confirm to durable-ack).

Per m1-protocol.md §Latency Measurements, these must report median, p95, min/max for each trigger/input type. Without these distributions, AC2 cannot be marked satisfied.

### Parent AC3: Baseline Target: iPhone 16 Pro, iOS 26.6.2; Record Actual Observed OS Build

**Status**: Target achieved.
- **Hardware**: iPhone 16 Pro (trial-phone)
- **Actual OS Build**: iOS 26.6.2 (build 23G90), verified via xcrun devicectl
- **Signing records**: Per-probe builds documented in evidence manifest

### Parent AC4: Offline Speech Availability and Time-to-Transcript are Physical-Device Checks

**Status**: Offline transcription confirmed; time-to-transcript recorded.

**Offline test**:
- Setup: iPhone in airplane mode, all radios off
- Fixture: "Remind me to call the dentist tomorrow at nine" (audio fixture)
- Result: On-device transcription 0.32 s cold, 0.15 s warm
- Mean confidence: 0.98
- Comparison: With network on, same fixture: 0.15 s (on-device operation confirmed by offline match)

**Language availability**:
- en-US: "Ready: on-device recognition is available"
- fr-FR: "Pending: on-device recognition unavailable"

### Parent AC5: Every Observed Result Cites Named Sanitized Evidence Artifact, Build/Revision, and Collection Time; Preserve Raw Private Evidence with Auditable Reference

**Status (Sanitized artifacts)**: Primary evidence manifest (2026-10-09-device-matrix.json) contains results with artifact citations. This document cross-references per-probe builds and collection window (2026-10-09T17:30:00Z–19:12:00Z). Per-result ISO timestamps not recorded in manifest; broad collection window applies to all results.

**Status (Private evidence reference)**: Raw evidence retained at `~/.ohand-private-evidence/p09/` (maintainer's Mac, not committed); auditable reference in manifest's `private_evidence_reference` field.

## Coverage and Unmet Checks

Required protocol scenarios (m1-protocol.md) status:

| Scenario | Status | Evidence |
| --- | --- | --- |
| Offline capture | ✓ Tested | P04: offline on-device transcription (airplane mode); P03 audio recording not run offline |
| Interrupted voice recording | ✓ Tested | P03: cancel, audio session interruption, background; recording during lock not interrupted |
| Interrupted import | ✗ Unmet | P07 does not measure app termination between handoff and commit |
| Provider unavailable | ✗ Unmet | No cloud provider configured in trial; fallback not tested |
| Denied permissions | ✓ Tested | P03/P04: revoke flows; explicit failure states |
| Permission changes | ✓ Tested | P03/P04: grant/revoke cycles |
| Cold launch | ✗ Unmet | P07 measured handoff post-save latency (4.961 s), not trigger-to-ready distribution |
| Warm launch | ✗ Unmet | P07 measured handoff post-save latency, not trigger-to-ready distribution |
| Locked device capture | ✗ Unmet | P02 measured control entry from already-unlocked device; true locked-capture and locked-data-access not measured |
| Device lock during processing | ✓ Tested | P03 recording during lock; P11 keychain during lock |
| Background interruption | ✓ Tested | P03 10 s background; P05 closed-app notification |
| Mac asleep | N/A | M1 architecture specifies no Mac client; no inlet in trial build |
| Offline reminder scheduling | ✗ Unmet | P04 offline transcription confirmed; P05 reminder scheduling not tested offline |
| Explicit reminder with UI closed | ✓ Tested | P05 closed-app notification delivery |
| Past reminder time | ✗ Unmet | Not tested on device |
| Unsupported recurrence | ✗ Unmet | Not tested on device |
| Ambiguous time | ✗ Unmet | Not tested on device |
| Capacity boundary | ✓ Tested | P05: 100 requested, 64 retained, oldest-added eviction |
| Duplicate prevention | ✗ Unmet | Design assertion; not device-tested |
| Repeated transport/idempotency | ✗ Unmet | Design assertion; not device-tested |
| Text search after offline save | ✗ Unmet | Not tested on device |
| Retrieval after multi-day gap | ✗ Unmet | Not tested on device |
| Private read scope | ✗ Unmet | Design assertion; not device-tested |
| Authenticated read | ✗ Unmet | Design assertion; not device-tested |
| Correction retrieval | ✗ Unmet | Not tested on device |
| Optional prompt activation | ✗ Unmet | Not tested on device |
| Prompt after deletion | ✗ Unmet | Not tested on device |
| Two-backend configuration | ✗ Unmet | Single provider in trial (offline only); not tested |
| Profile capability mismatch | ✗ Unmet | Not tested on device |
| Credential invalidation | ✗ Unmet | Not tested on device |
| Endpoint unavailable | ✗ Unmet | Design assertion; not device-tested |
| Processing states visible | ✗ Unmet | Design assertion; not device-tested |
| Forced worker failure | ✗ Unmet | Design assertion; not device-tested |
| Partial batch failure | ✗ Unmet | Design assertion; not device-tested |

## Headline Findings

From the evidence matrix:

1. **Offline on-device transcription confirmed**: 0.32 s cold, 0.15 s warm, all radios off.
2. **Local notifications deliver** with app force-quit and device locked.
3. **Keychain access policy**: AfterFirstUnlock classes are readable while locked; ~25 s background window.
4. **Recording resilience**: Survives 10 s background and 10 s lock without background audio mode; process death leaves unfinalized WAV.
5. **Pending-notification limit**: OS retains 64 with oldest-added eviction.
6. **Handoff security**: Identifier only, hostile URLs rejected, cold-launch URL arrives pre-webview-ready.
7. **Accessibility defect**: Large text clips labels in CaptureProbe, AudioProbe, CredentialProbe.

## Conclusion

**Task P09 evidence collection is incomplete.** Critical acceptance criteria are unmet:

- **AC2 (Trigger-to-ready and end-of-input-to-save latencies)**: No measured distributions; estimates and derived values do not satisfy protocol requirements.
- **Locked-device-capture timing**: Not measured with device physically locked.
- **Reminder scheduling**: Offline path confirmed (P04); reminder delivery and scheduling workflow not tested.
- **Device-state resilience**: Many required scenarios (multi-day gap, time-zone change, low power, focus modes, reinstall, reboot survival after scheduled reminder) not tested; P11 reboot was tested.
- **Configuration and backend handling**: Single provider (offline); two-backend and capability-mismatch scenarios not tested.

**Evidence is insufficient for M1 feasibility certification.** The collected results demonstrate that offline transcription, local notifications, and keychain separation function as designed on iPhone 16 Pro iOS 26.6.2. However, the absence of latency distributions, locked-device measurements, multi-day resilience tests, and two-provider configuration tests means the task acceptance criteria remain unmet per Parent AC2 and protocol requirements.

A follow-up trial run or focused completion task is required to:
1. Measure and report trigger-to-ready and end-of-input-to-save as median/p95/min/max.
2. Test capture entry and credential access with device physically locked.
3. Test reminder scheduling workflow (propose → schedule → deliver).
4. Include multi-day gap, reboot, and time-zone scenarios.
5. Configure and test two backends; verify capability-mismatch handling.
6. Add keyboard and VoiceOver accessibility testing.
