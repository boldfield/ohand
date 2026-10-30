# Accessibility validation (M1)

This page records how M1 accessibility is checked. Part 1 is the automated gate (task U10a), which runs on every pull
request. Part 2 is what only a device can show and the step-by-step procedure for it (task U10b). Part 4 is the
evidence record format for U10b. The device-results section belongs to U10b and is left empty here.

## 1. Automated gate

`ios/UITests/Accessibility/AccessibilityAuditUITests.swift` is the `OhAndAccessibilityUITests` target. The CI job
`accessibility` in `.github/workflows/ios.yml` builds it, then `ios/scripts/accessibility-simulator.sh` runs it on one
iPhone simulator twice: at the default content size and at the largest accessibility size
(`UICTContentSizeCategoryAccessibilityXXXL`, set with the `-UIPreferredContentSizeCategoryName` launch argument). The
fixtures are the synthetic ones the walkthrough uses (`ios/UITests/Fixtures`).

### What is audited

Each screen is audited with `XCUIApplication.performAccessibilityAudit()` and the default audit types (all of them):

| Screen | Test | Fixture |
| --- | --- | --- |
| Home, empty | `testHomeEmpty` | empty-first-run |
| Home, populated | `testHomePopulated` | populated |
| Capture, voice idle | `testCaptureVoiceIdle` | empty-first-run |
| Capture, voice recording | `testCaptureVoiceRecording` | empty-first-run |
| Capture, silent text entry | `testCaptureSilentTextEntry` | empty-first-run |
| Search with results, and with none | `testSearchWithResults`, `testSearchWithNoResults` | populated |
| Source detail with a reminder, the correction and item controls | `testSourceDetailWithReminderAndControls` | needs-correction |
| Settings root; Daily prompt; Unfinished recordings; Processing (provider settings), its Add provider and Advanced screens; Privacy; Status; Audio; Export | `testSettingsScreens` | populated |
| First-run permissions (Settings, Permissions) | `testSettingsPermissionsOnFirstRun` | empty-first-run |
| Data settings: Erase everything and its confirmation (nothing is confirmed) | `testDataSettingsEraseConfirmation` | populated |
| Status with a failed note; Status with a reminder that needs a time | `testStatusWithFailedNote`, `testStatusWithReminderNeedingATime` | failed-sorting, needs-correction |
| Locked screen | `testLockedScreen` | locked |

Any audit issue fails the test. The job log and the artifact `ios-accessibility` (retention 7 days) hold
`accessibility/audit-default.log` and `accessibility/audit-xxxl.log` (one `ISSUE`, `EXEMPT`, `PASS` or `FAIL` line per
finding and screen), the screenshots of both sizes (`accessibility/xxxl-*.png` are the large-text pictures), the
`.xcresult` bundles and `accessibility-summary.txt`.

### Large text

The same audits run at the largest accessibility size. The audit's text-clipping and Dynamic Type checks are the test
for "nothing is truncated without a way to read it in full"; the screenshots at that size are in the artifact for a person
to look at.

### VoiceOver, as far as the simulator can show

- Every button, switch, field, slider, stepper and link on each audited screen has a non-empty label
  (`assertInteractiveElementsAreLabelled`); the audit's own element description check covers the rest.
- A stateful control exposes its state: the Daily prompt toggle reports a value, the Erase credential choice reports
  `isSelected`.
- Announcements. A UI test cannot read the announcement queue, so the app's `AccessibilityAnnouncer`
  (`app/design-system/AccessibilityAnnouncer.swift`) posts the announcement and, in a debug build launched with
  `-OhAndAnnouncementLog`, also writes it to a near-invisible element (`accessibility.announcement-log`) that tests read.

  | Event | Announcement | Test |
  | --- | --- | --- |
  | Saving a capture | the outcome line, posted by `CaptureEntryView` (not edited by this task) | `testSavingACaptureShowsTheOutcomeLine` asserts the outcome line it announces appears |
  | A processing failure | "N notes failed to sort." (Status) | `testStatusWithFailedNote` |
  | A reminder that needs correction | "A reminder needs a time." (Home) | `testReminderNeedingACorrectionIsAnnounced` |
  | Locking | "Your captures are locked. Unlock to see what you saved before." (Home) | `testLockedScreen` |

  What the test proves is that the app posts the announcement text at that moment. That VoiceOver speaks it, in a sensible
  order, is a device check (part 2).
- The locked screen's Unlock control is labelled "Unlock", enabled and hittable (`testLockedScreen`).

### Exemptions

An exemption is one issue on one element, handled in `isExempt` with a comment, and listed here. No audit type is ever excluded.

| Element | Issue | Reason |
| --- | --- | --- |
| `ui-fixture.status` | any | A 4-point, near-transparent marker added by debug builds only, so a test knows the fixture finished seeding. It is not part of the app a person uses. |
| `accessibility.announcement-log` | any | The same kind of debug-only marker, holding the announcements a test reads. |

### Scope of fixes

Fixes made for this gate change accessibility attributes and the layout large text needs, nothing else. Visible copy is
unchanged; a label adds words only where the visible text is ambiguous.

## 2. What only a device can show

The simulator cannot show: VoiceOver gestures (swipe, double tap, magic tap, escape, scrolling by three fingers), the rotor,
reading order as a person hears it, whether announcements are spoken and not dropped, Dynamic Type in real use (with real
content and with Bold Text and Larger Accessibility Sizes combined), and Full Keyboard Access with a hardware keyboard.
Keyboard navigation is not asserted in the simulator.

## 3. Device procedure (U10b)

Use the trial iPhone with synthetic data. Run the whole list once with VoiceOver on, once with Full Keyboard Access on and a
hardware keyboard attached, and once at the largest accessibility text size (Settings, Accessibility, Display & Text Size,
Larger Text, Larger Accessibility Sizes on, slider at the maximum).

For each screen below, note: reading order is the visual order; every control is announced with a name and, if stateful,
its state; each control can be activated; the screen can be left (Back or two-finger scrub); nothing is cut off at the largest
size; and, with a keyboard, Tab reaches every control and Space or Return activates it.

1. Home, empty: header (Search, Oh And, Settings), composer. Type a note, Save, listen for the outcome announcement.
2. Capture by voice: Record, listen for "Recording started.", Stop recording, Discard.
3. Home, populated: cards are read as one element each with their type, time and text; open one.
4. Search: type a query, submit; a result; a query with no results.
5. Source detail with a reminder: text, reminder editor, correction and item controls; change a type, set a time.
6. Settings root, then each row: Daily prompt (toggle state), Unfinished recordings, Processing (Add provider, Advanced), Privacy, Permissions, Status, Audio, Export.
7. Data settings: Erase everything, the credential choice (selected state is announced), the confirmation. Cancel; do not erase.
8. Status with a failed note: listen for "N notes failed to sort."; Try again.
9. Status with a reminder that needs a time: open the note from Status.
10. Locked screen: background and reopen the app, listen for the lock announcement; reach and activate Unlock; confirm the authentication prompt appears and can be answered, and that there is no state with nothing to activate.
11. First run: with permissions not yet asked, open Settings, Permissions.

## 4. Evidence record

One JSON file per session under `docs/validation/evidence/accessibility/`, named `<date>-device-accessibility.json`,
modelled on `docs/validation/evidence/device-feasibility/2026-10-10-latency-accessibility.json`:

```json
{
  "evidence_id": "accessibility-<date>-device",
  "revision": 1,
  "task": "U10b",
  "collector": "who operated the phone",
  "collected_at": "UTC start to end, with the device time zone",
  "device": "model (label trial-phone)",
  "ios_version": "version (build)",
  "build": "app revision or commit",
  "method": { "voiceover": "how it was observed (Caption Panel, screen recording)", "keyboard": "keyboard model, Full Keyboard Access settings", "text_size": "setting used" },
  "private_evidence_reference": "where recordings are kept; never committed",
  "sections": {
    "<screen slug from part 3>": {
      "voiceover": { "reading_order": "pass|fail", "labels_and_state": "pass|fail", "announcements": "what was heard", "notes": "" },
      "keyboard": { "reachable": "pass|fail", "activation": "pass|fail", "notes": "" },
      "large_text": { "truncation": "none|describe", "notes": "" }
    }
  },
  "defects": [ { "screen": "slug", "description": "", "task_or_issue": "" } ],
  "result": "pass|fail"
}
```

## 5. Device results

Recorded by U10b.
