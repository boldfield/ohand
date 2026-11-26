# Accessibility validation (M1)

This page records how M1 accessibility is checked. Part 1 is the automated gate (task U10a), which runs on every pull
request. Part 2 is what only a device can show and the step-by-step procedure for it (task U10b). Part 4 is the
evidence record format for U10b. The device-results section belongs to U10b and is left empty here.

## 1. Automated gate

`ios/UITests/Accessibility/AccessibilityAuditUITests.swift` is the `OhAndAccessibilityUITests` target. The CI job
`accessibility` in `.github/workflows/ios.yml` is a matrix of two jobs, `accessibility (group a)` and `accessibility (group b)`.
Each builds the target, then `ios/scripts/accessibility-simulator.sh <udid> <evidence> <group>` runs that group's tests on its
own iPhone simulator twice: at the default content size and then at the largest accessibility size
(`UICTContentSizeCategoryAccessibilityXXXL`, set with the `-UIPreferredContentSizeCategoryName` launch argument). The
fixtures are the synthetic ones the walkthrough uses (`ios/UITests/Fixtures`).

**The groups.** `ios/scripts/accessibility-groups.txt` partitions the test methods of the scheme into groups `a` and `b`; the
script turns a group's lines into `-only-testing` arguments, and `ios/scripts/test_accessibility_groups.py` (in `make check`)
fails unless every test is in exactly one group and no group names a missing test, so a new test must be given a group. The
sizes cannot be split between jobs: the largest-size run settles an element it could not bring clear of the glass from
`settled-default.txt`, which the default-size run of the same job writes (`settleFromTheDefaultSize`). The cut is across the
tests, and a group must be closed under that settlement: every `CLEAR-AT-DEFAULT` line of the largest size names a default-size
screen whose test is in the same group as the test that owed the element. In run 44477039781 (one job, all tests) there were
two such lines, each settled by the owing test itself:

| `CLEAR-AT-DEFAULT` (audit-xxxl.log) | Owing test (group) | Settling screen's test (group) |
| --- | --- | --- |
| `||Oh And`: owed since `capture-text-entry`, settled at `capture-text-entry` | `testCaptureSilentTextEntry` (b) | `testCaptureSilentTextEntry` (b) |
| `Action · , Remind me 2099-01-15 11:00:00 to call mom||Action`: owed since `home-populated`, settled at `home-populated` | `testHomePopulated` (a) | `testHomePopulated` (a) |

Per-test durations of that run (default size plus largest size, seconds): `testSettingsScreens` 395.5, `testHomePopulated` 131.6,
`testSourceDetailWithReminderAndControls` 131.3, `testDataSettingsEraseConfirmation` 119.7, `testSearchWithNoResults` 93.8,
`testCaptureSilentTextEntry` 92.1, `testSearchWithResults` 87.1, `testStatusWithFailedNote` 82.1,
`testStatusWithReminderNeedingATime` 80.8, `testSettingsPermissionsOnFirstRun` 74.5, `testHomeFramesMatchWhatIsDrawn` 72.2,
`testCaptureVoiceRecording` 67.1, `testLockedScreen` 64.7, `testSavingACaptureShowsTheOutcomeLine` 63.5, `testHomeEmpty` 50.6,
`testCaptureVoiceIdle` 46.8, `testReminderNeedingACorrectionIsAnnounced` 33.4, the eight `FixtureLaunchUITests` 260 in all, and
the nineteen `GlassContrastLedgerTests` about 5. Group a (the first four, `testSearchWithNoResults`, `testHomeEmpty` and
`testCaptureVoiceIdle`) is 969 s of audits; group b (everything else, with the fixture and ledger tests) is about 982 s. When
tests change, rebalance from a new run's durations, keeping each test's closure as above.

### What is audited

Each screen is audited with `XCUIApplication.performAccessibilityAudit(for:)`: every default type at the largest size, and
every default type except `.dynamicType` at the default size (see "Dynamic Type"):

| Screen | Test | Fixture |
| --- | --- | --- |
| Home, empty | `testHomeEmpty` | empty-first-run |
| Home, populated | `testHomePopulated` | populated |
| Home, populated: the accessibility frames match what is drawn after a scroll and after an audit (a tap check, no audit) | `testHomeFramesMatchWhatIsDrawn` (and the end of `testHomePopulated`) | populated |
| Capture, voice idle | `testCaptureVoiceIdle` | empty-first-run |
| Capture, voice recording | `testCaptureVoiceRecording` | empty-first-run |
| Capture, silent text entry | `testCaptureSilentTextEntry` | empty-first-run |
| Search with results, and with none | `testSearchWithResults`, `testSearchWithNoResults` | populated |
| Source detail with a reminder, the correction and item controls | `testSourceDetailWithReminderAndControls` | needs-correction |
| Settings root; Daily prompt; Unfinished recordings; Processing (provider settings), its Add provider and Advanced screens; Privacy; Status; Audio; Export | `testSettingsScreens` | populated |
| First-run permissions (Settings, Permissions) | `testSettingsPermissionsOnFirstRun` | empty-first-run |
| Settings, Audio with a voice note to turn into text again (the section and its button; not the state while the replacement runs, which is over within a second of the tap) | `testSettingsAudioWithRecordingsToRedo` | redoable-recording |
| Data settings: Erase everything and its confirmation (nothing is confirmed) | `testDataSettingsEraseConfirmation` | populated |
| Status with a failed note; Status with a reminder that needs a time | `testStatusWithFailedNote`, `testStatusWithReminderNeedingATime` | failed-sorting, needs-correction |
| Locked screen | `testLockedScreen` | locked |

Any audit issue fails the test, and so does an audit that throws instead of running (the log says `ERROR`; the test
fails, it never records a pass). The job log and the artifact `ios-accessibility-<group>` (one per job; retention 2 days, per the CI artifact
budget) hold `accessibility/audit-default.log` and `accessibility/audit-xxxl.log`, `accessibility/settled-default.txt` and `accessibility/settled-xxxl.txt` (what each size audited unobstructed), the large-text screenshots (`accessibility/xxxl-*.jpg`, compressed from the PNGs; among them, for the Audio redo stop, one picture of the stop at both sizes, `xxxl-data-audio-redo-beside-default-*.jpg`, the `BOTH-SIZES` line below) and `accessibility-summary.txt`. A failed job
uploads no `.xcresult` bundle; it runs `ios/scripts/export-failure-diagnostics.sh` and uploads the exported test summary and the
failed tests' attachments as `ios-accessibility-<group>-failure` (retention 1 day, under 100 MB per job).

The audit log has one line per decision:

| Line | Meaning | Fails the job |
| --- | --- | --- |
| `ISSUE` | an audit issue that is not exempt | yes |
| `ERROR` | the audit threw and did not run | yes |
| `UNVERIFIED` | an element excused under or on glass that the test never audited unobstructed | yes |
| `FAIL` / `PASS` | the screen's result | `FAIL` does |
| `EXEMPT` | an issue exempted by the table under "Exemptions" | no |
| `OWED` | a contrast issue on a glass-list element while it is obstructed; the test owes an unobstructed audit | no, until it is `UNVERIFIED` |
| `CLEAR` | a glass-list element audited unobstructed with no contrast issue, which settles what it owed | no |
| `MEASURED` | text drawn on glass, with nothing overlapping it, measured from the screenshot at 4.5:1 or above; or content scrolled under glass that the audit flagged in a pass during which it moved the content, measured from the screenshot taken before that pass, where it lay wholly in the clear band | no |
| `RETRY` | the first pass of an audit found something and was repeated after 3 seconds | no |
| `SETTLE` | the test scrolled to audit an owed element unobstructed | no |
| `STOP` | a stop reached by drags (`dragToStop`): after how many drags, where it left the element, the section and the screen title, with the clear band and the window | no |
| `BOTH-SIZES` | at the largest size, the picture written of a stop at both sizes (`recordBothSizes`): the default-size run's screenshot of the stop beside this run's, each at half scale, named by file; a `NOTE` instead when one of the screenshots is missing | no |
| `CLEAR-AT-DEFAULT` | at the largest size, an owed element settled by the default-size run's unobstructed audit of it | no |
| `MOVED` | the audit scrolled the screen by the given number of points; a second screenshot (`<screen>-audited`) shows where it left it. Issues are still decided against the snapshot taken before the audit | no |
| `FRAMES` | a tap at the card the accessibility frames place in the middle of the clear band opened that card (a mismatch is an `ISSUE`) | no |
| `RELAUNCH` | the first launch did not reach home and the app was launched once more | no |

`ios/scripts/accessibility-simulator.sh` fails the job on any `ISSUE`, `ERROR`, `UNVERIFIED` or `FAIL` line, or when a
size has no `PASS` line, whatever xcodebuild returned.

On the hosted runner the app's main thread can be busy for half a minute after launch, and XCTest then records "Failed to
get matching snapshots: Timed out while evaluating UI query" for the first query. The audit test launches with the
walkthrough's fixture argument and waits for the composer and the fixture's `ready` marker as the shared launcher does, but
treats that message, and "Lost connection to the application" (a launch that hung), during those two waits only, as
an expected failure and keeps polling. A launch that does not reach home within 90 seconds is relaunched once, and the
log says `RELAUNCH`; any other failure counts, and a second launch that does not reach home, or a fixture that never
reports ready, fails.

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

  | Event | Announcement | Tests |
  | --- | --- | --- |
  | Saving a capture | the outcome line, posted by `CaptureEntryView` | hosted `CaptureEntryViewTests.testSavingACaptureAnnouncesItsOutcome`; UI `testSavingACaptureShowsTheOutcomeLine` reads it from the debug announcement log |
  | A processing failure | "N notes failed to sort." (Status) | hosted `StatusFailedNotesTests.testAProcessingFailureIsAnnounced`; UI `testStatusWithFailedNote` |
  | A reminder that needs correction | "A reminder needs a time." (Home) | hosted `HomeBodyViewTests.testAReminderThatNeedsATimeIsAnnounced`; UI `testReminderNeedingACorrectionIsAnnounced` |
  | Locking | "Your captures are locked. Unlock to see what you saved before." (Home) | hosted `HomeBodyViewTests.testLockingIsAnnounced`; UI `testLockedScreen` |

  The hosted tests replace `AccessibilityAnnouncer.current` (the injectable announcer in `app/design-system/`) with a
  recorder and assert the message posted; the UI tests assert the same text in the real app.
  What the test proves is that the app posts the announcement text at that moment. That VoiceOver speaks it, in a sensible
  order, is a device check (part 2).
- The locked screen's Unlock control is labelled "Unlock", enabled and hittable (`testLockedScreen`).

### Exemptions

An exemption is one issue on one named element, handled in `isExempt` with a comment, and listed here. No exemption
excludes an audit type. The one audit type left out of a run is `.dynamicType` at the default size, by the decision under
"Dynamic Type" below.

| Element | Issue | Reason |
| --- | --- | --- |
| `ui-fixture.status` | any | A 4-point, near-transparent marker added by debug builds only, so a test knows the fixture finished seeding. It is not part of the app a person uses. |
| `accessibility.announcement-log` | any | The same kind of debug-only marker, holding the announcements a test reads. |
| `provider-settings.field.model`, `provider-settings.field.secret` | Text clipped (text fields only) | A single-line text field cannot wrap; the audit's "may be clipped at larger Dynamic Type sizes" is a heuristic every single-line field raises. The fields use a scaling font, keep their 44 pt minimum height and grow with it, and the largest-size screenshot in the artifact shows them whole. |
| the keyboard's prediction cells (`TUIPredictionViewCell`) | Element has no description | The system keyboard's predictive bar, drawn by iOS and not by the app. |

Contrast issues on the glass list below are not exempt; they are decided as "Content under and on glass" describes.

### Settling

An audit that reports something is repeated once after three seconds, because a card still fading in is sampled against the
wrong background. Only issues the settled screen still has count; each of them fails the test, and the log records the first
pass as a `RETRY` line.

### Content under and on glass

The header buttons, the back button and the composer are translucent glass. The audit measures text scrolled under the
glass, or drawn on it, against the blurred overlay, which it cannot resolve, and reports a contrast failure. For the
elements below (`glassList` in the test), `GlassContrastLedger` decides such an issue from where the element is in a
snapshot of the screen taken just before the audit:

- **Scrolled under glass.** If the element's frame lies wholly inside the clear band between the overlays (and on
  screen), the issue is real and fails. Otherwise it is excused at that stop (`OWED`), and the same test must audit the
  element wholly inside the clear band with no contrast issue (`CLEAR`). After its own stops, the test drags each owed
  element into the middle of the clear band, dragging down the left margin beside the cards (a drag started on a
  card would press it; the right edge would catch the scroll indicator, which scrubs the whole list), waits until two snapshots a second apart agree, and audits there (`SETTLE`, a screen named
  `<first screen of the test>-settle-<n>`). The drag starts only on screen, below the status bar and above the home
  indicator (`draggableBand`), whatever the clear band says: on a screen whose back button scrolls with the content, the
  band starts 12 points below the button wherever it is, so with the button scrolled off the top the band starts above the
  screen, and a drag started there touches nothing (F76, below); an
  element that is still owed when the test ends fails it (`UNVERIFIED`). What is measured is the element itself, wherever
  it is on the list, never something else that shares its frame.
- **Drawn on glass.** If no other element's frame overlaps the text (nothing scrolled behind the glass there and nothing
  else drawn on it; the field around the placeholder and the pill around the mark are its ancestors and do not count),
  the contrast is measured from the screenshot pixels inside the element's frame, below the status bar: the most common
  colour is the background, the pixel furthest from it in luminance is the text. Since nothing else is in the crop, that
  pixel is the element's own glyph. 4.5:1 or above settles it (`MEASURED`); below fails. With something overlapping,
  it is owed as above, and the test scrolls to the top of the screen (for the header) or its end (for the composer),
  where nothing is behind the glass, to audit or measure it there. A scroll indicator ("Horizontal scroll bar") is not
  content and overlaps nothing. **A container is not an obstruction:** a node whose frame contains the whole screen frame
  (iOS 27 lists a labelled `Toolbar` of the screen's size, `CONTAINER` in the log with its element type and identifier,
  once per run) obstructs nothing, whatever its label or type, and `GlassContrastLedger.overlapping` decides it from the
  screen frame and not from the label. Every smaller node, including that container's labelled descendants, still
  obstructs. This cannot hide a failure: text drawn on glass is settled only by an audit with no issue at an unobstructed
  stop or by measuring the screenshot's pixels.
- **The header wordmark.** `HomeHeader` exposes the pill as one element labelled `Oh And`; iOS 27 reports contrast on it
  while the snapshot still lists the `&` glyph beside it. The two are one control with one ledger key and do not obstruct
  each other. The wordmark is settled only by measurement, at a stop where nothing else overlaps the pill, whether or not
  the audit flagged it there (`MEASURED`): the `&` glyph's own frame (the indigo mark, the weaker of the two) and the
  pill's frame with the glyph's frame excluded (the ink of the words) must both reach 4.5:1. An audit with no issue does
  not settle it; a wordmark below 4.5:1 on either measurement is a real finding.
- **`Discard` and the status line.** `home.composer.discard` and `capture-entry.message` (the outcome line drawn over the
  list above the composer) are on the glass list by identifier, drawn on glass, and settled the same way: a stop where
  nothing overlaps, measured from the screenshot at 4.5:1 or above. A measurement below that is a defect in a design token
  and is reported, not worked around.

- **A pass in which the audit moved the content.** The iOS 27 audit scrolls a list back towards its top while it
  evaluates (`MOVED`), and on a slow host the scroll is still in flight: run 42891673229 flagged one caption at two frames
  and six card texts that the same run audited at rest with no issue. A contrast issue such a pass raises on an element
  scrolled under glass is therefore decided from the snapshot and the screenshot taken before the pass, for every element
  with the flagged identifier and label (the audit's mid-scroll frame does not say which one it sampled): one that lay
  wholly in the clear band is measured over its own frame, and 4.5:1 or above settles it while less is a real finding; one
  that did not is owed, as always. When the audit did not move the content, an issue on an element clear of the glass is
  real, with no measurement. Content that lies under the composer when the list is at its top is settled this way, because
  the audit never evaluates it anywhere else.
- **The wordmark where the snapshot does not separate the glyph.** While `Done` is shown, and at the largest text size, the
  snapshot gives the "&" node the pill's frame. The pill is then measured whole, and the glyph's own measurement comes
  from a stop where the two are separate: the same test's (the text-entry test measures the wordmark before the keyboard
  comes up), or one recorded earlier in the run in `wordmark-glyph-<size>.txt` (the default-size file serves the largest
  size, as `settled-default.txt` does). With no separated glyph measurement the wordmark cannot be measured and fails.
- **The keyboard's introduction.** A new simulator shows the slide-to-type introduction over the keyboard; the text-entry
  test dismisses it and fails if it is still on screen at the audit.
- **A contrast issue with no element.** On a simulator that has just been erased, the audit raises the contrast issue of
  the wordmark's words with no element at the text-entry stop; on a simulator that has shown a keyboard before it names
  `Oh And` there, and removing the words from the app removes the issue. In one pass, the first contrast issue with no
  element is taken as the words' when the same pass names the "&" mark and does not name `Oh And`, and it is decided by
  the wordmark's measurement. The log line says the audit named no element. Any other issue with no element fails.

The clear band runs from 12 points below the lowest of the header's glass elements (its buttons and the wordmark pill, which is taller than the buttons at the largest text size; or the back button) to the top of the composer's
glass, which is 20 points above its field, placeholder or "Recording" line (the test allows 24); a status line or the
Saved pill above the composer lowers it to 8 points above that line. A settle stop can owe something new, such as a card
it scrolls under the header; the test keeps settling until each excuse has had its own stop (at most 12).

**The audit scrolls.** `performAccessibilityAudit` scrolls a screen while it checks its elements and leaves it
elsewhere, at both sizes and with or without `.dynamicType`: a scrolled Home list ends near its top (run 41758689823:
Today at y = 139 after the audit at the largest size, wherever the list was before it), and Settings, source detail and
the erase confirmation move by more than 1000 points at the largest size (run 41761876914). It checks what is on screen
where it stands before it scrolls: deciding issues against a snapshot taken after it (tried in runs 41761876914 and
41767301394) judged contrast issues raised under the glass at positions clear of it, and never settled the last card. So the test decides every issue against the snapshot and the
screenshot taken just before the audit, as before, logs `MOVED` with the distance when the audit has moved the screen,
and saves a second screenshot (`<screen>-audited`, half width) of where it left it. That is where a repeated audit
(`RETRY`) starts, and the repeat decides against its own snapshot taken there.

Earlier rounds read this as stale accessibility frames (runs 41702917752, 41711242088, 41720210504): the screenshot was
taken before the audit and the logged frames after it. The frames are not stale. Diagnostic runs 41752990749,
41755533965 and 41758689823 (temporary instrumentation, since removed) compared, after scrolling and after audits, at
both sizes: XCTest's snapshot frames, the frames the app's own accessibility elements report in process (what VoiceOver
reads), the frames SwiftUI draws the cards at, the screenshot, and the card a tap at the reported frame opens. All five
agreed in every case. The gate keeps checking it: `testHomeFramesMatchWhatIsDrawn` taps the card the frames place in the
middle of the clear band with the list scrolled to its end, and `testHomePopulated` does the same after the end stop's
audit has moved the list (`FRAMES`; a mismatch fails). The device procedure checks the same on hardware with VoiceOver
(part 3, step 12).

**At the largest size, Home.** An audit that found something is repeated from where the first pass left the screen,
and the audit returns a scrolled Home list near its top, where at the largest size only the first card lies between the
header and the composer (run 41730405313: `home-populated-end` was repeated on the list back at its top). So a card
further down may never be audited clear there. An element still owed at the end of a test at the largest size is therefore settled by the
default-size run's unobstructed audit of the same element (`CLEAR-AT-DEFAULT`; the default run writes what it settled
to `settled-default.txt`). Contrast is a property of the element's colours, which do not change with the text size; the
largest size only lowers the bar for large text. A Home card is named by its text without the age in its caption, so
the same card matches across launches and sizes. Only what neither size settles is scrolled to and audited.

**An audit that times out.** `performAccessibilityAudit` can throw "Audit failed to complete in time" (code -56). An audit
that throws still fails, but a stop whose combined audit throws -56 is audited again one audit type at a time (each type
of the run's audit types in its own `performAccessibilityAudit(for:)` call, `RETRY-BY-TYPE` in the log). The stop counts
as audited only if every type completes, and its issues are the union; no type is omitted, and a type that still throws
fails the stop. When a first pass found something and is repeated, its issues are logged (`RETRY-ISSUE`) before the
repeat.

**The end of the Home list.** The test swipes the list up until its last card stops moving and logs where that card ends
against the clear band (`SCROLL-END`). On a failed job the default-size screenshots upload too (`ios-accessibility-<group>-failure`,
compressed, within the failure budget), so where the last card stands is readable from the artifact.

`GlassContrastLedgerTests` (in the same target, without the app) holds the negative cases: a screen-sized node that is a
container while a labelled child of it and a node one point smaller than the screen still obstruct, the wordmark pill and
its glyph that do not obstruct each other, a low-contrast element under
the back button that later fails clear of it, an excuse never settled, a placeholder with a card behind it that is not
measured, a measured 3.1:1 that fails, a dark glyph from another element that would read 21:1 if it were not excluded,
and two cards with the same caption that are settled separately.

| Element (identifier, label) | Placement | Screens |
| --- | --- | --- |
| "What's on your mind?" (composer placeholder) | drawn on the composer glass | Home, capture, locked |
| the header wordmark: the pill "Oh And" and its "&" glyph (one ledger key), only in the header, starting above the clear band | drawn on the header glass | Home, capture |
| "Discard" (`home.composer.discard`) and the outcome line (`capture-entry.message`) | drawn on the composer glass, or over the list above it | capture |
| "Done" (`home.done`), shown in the header while the keyboard is up | drawn on the header glass | capture text entry |
| the Home card texts ("Synthetic typed …", "Synthetic spoken …", "Remind me 2099-01-15 …"), captions ("Note · …", "Action · …", "Idea · …", matched by prefix because the age changes from run to run, and told apart by their card) and the "Today" list heading (`home.recents.list`) | scrolled under the header or the composer | Home |
| "Save topic" (`item-controls.topic.save`), "Save words" (`item-controls.text.save`), "What you wrote", "Type", and the reminder text "Remind me soon to call the plumber about the leaking tap" | scrolled under the back button | source detail |
| the screen title "Audio" and the "None" line (`data-settings.retention.none-unresolved`) | scrolled under the status bar, or off screen below, while the redo section is shown at the largest size (run 45056680973: the audit sampled each mid-scroll). The audit flags the title in some runs and not in others from the same stop (F76, below); when it does, the title is owed where the audit's own scroll left it, just above the status bar, and the settle stop drags it back down to where the screen opens, clear of the back button, and audits it there | Settings, Audio with a recording to redo |

### Dynamic Type

At the default size the audit omits `.dynamicType` and passes every other default type (contrast, element detection, hit
region, sufficient element description, text clipped, trait and the rest of `.all`) to `performAccessibilityAudit(for:)`. The largest-size
run keeps every type. The decision (coordinator, 2026-11-01): the probe grows the font step by step and watches each
element's frame; in a scrolling list the growth above pushes the elements below out of the scroll view's bounds, so they are
reported "partially unsupported" wherever the scroll settles, which measures the audit's geometry and not the fonts. No
per-element exemption is added for these findings. The evidence for Dynamic Type is instead the largest-size run (text
clipped, contrast, and the screenshots in the artifact) and `ios/scripts/check_font_tokens.py` (run by `make check`), which
fails when a `Font` token in `app/design-system/DSMetrics.swift` is not built from a text style or derived from another
token.

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
12. Home, populated, at the largest text size with VoiceOver on (the gate's `FRAMES` check, part 1, is the simulator's
    view of this): scroll the list to its end, once with a one-finger drag (VoiceOver off for the drag, then
    on) and once with a three-finger swipe up. At each end position, drag one finger slowly over the cards (touch
    exploration), then swipe right through them. Confirm that the focus ring surrounds the card drawn under the finger,
    that the card read aloud is the card drawn there, and that a double tap opens that card. Record any mismatch as a
    defect, with the scroll method and the card positions.

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

## A Home card keeps one name however its age is worded (F72)

The glass ledger names an element by its card, with the age in the card's caption removed. `ScreenElement.cardName` removes what stands between the type's separator and the first comma, so "now", "4 sec. ago", "yesterday" and "in 4 sec." give one name. It once removed only `N unit ago`, so a card excused as "Action · now, …" was named "Action · , …" at the next stop and its excuse could never be settled.

The audit also leaves the Home list elsewhere (the `MOVED` lines). The settle loop used to give up silently when the owed element was off screen at the start of its stop; it now scrolls to the top and down to find it, and logs `SETTLE-DRAG` (position and clear band at each drag) and `FIND` lines in `audit-<size>.log`.

## The Audio title at the largest size (F76)

`testSettingsAudioWithRecordingsToRedo` failed at the largest size with `'||Audio' was excused under or on glass at
data-audio-redo and never audited unobstructed`. F76 was filed as four failures on CI09's pull request #433 and none on
F75's; the job logs (kept 90 days; the `accessibility (group b)` summary of every attempt) say otherwise. The same test
code failed six times and passed four times in the ten executions of that job on the two pull requests (an attempt that
re-ran only other jobs carries the earlier log, byte for byte, and is not counted):

| Run, attempt | Pull request | Audit scroll (`MOVED`) | Title in the snapshot before the audit | After the settle drags | Outcome |
| --- | --- | --- | --- | --- | --- |
| 45113396496, 1 | CI09 #433 | 2290 pt, then 459 pt on the retry pass | retry pass: y = −75.33 | y = −75.33 | `UNVERIFIED` |
| 45123317888, 1 | CI09 #433 | 2231 pt | y = −1847.33, owed | y = −75.33 | `UNVERIFIED` |
| 45123317888, 2 | CI09 #433 | 2264 pt | y = −1880.67, owed | y = −75.33 | `UNVERIFIED` |
| 45142839927, 2 | CI09 #433 | 2264 pt | y = −1880.33, owed | y = −75.33 | `UNVERIFIED` |
| 45142839927, 3 | CI09 #433 | 2264 pt | not flagged | — | pass |
| 45101431672, 1 | F75 | 2264 pt | y = −1881.00, owed | y = −75.33 | `UNVERIFIED` |
| 45101431672, 2 | F75 | 2271 pt | not flagged | — | pass |
| 45143002077, 1 | F75 | 2265 pt | not flagged | — | pass |
| 45201994160, 1 | F75 #434 | 2293 pt, then 459 pt on the retry pass | retry pass: y = −75.33 | y = −75.33 | `UNVERIFIED` |
| 45201994160, 2 | F75 #434 | 2273 pt | not flagged | — | pass |

At the default size every execution passed the same way (`MOVED` 311 pt, a retry, `MOVED` 260 pt, `CLEAR` "None" at
y = 839). What the rows show:

- **The stop was not the same place every run.** The test reached it with `swipeUp()` flings, and the title stood between
  y = −1847 and −1890 in the snapshot before the audit, 43 points apart. (The audit scroll distance does not separate the
  rows: 2264 points in three failures and one pass.)
- **Whether the audit flags the title is decided inside the audit.** `performAccessibilityAudit` scrolls the content to
  reach the rest of the screen and samples the title as it passes under the status bar's edge effect; it raised a contrast
  issue on it in six executions and not in four, with the same code, image, simulator and stop. When the first pass found
  something else and was repeated, the second pass started where the audit had left the screen and flagged it every time.
- **The audit leaves the title in the same place every run, and the settle could not move it.** After the audit the title
  is at y = −75.33 (its bottom 22 points under the status bar; `xxxl-data-audio-redo-audited` is the same picture in every
  run) and the back button is above the screen, so the clear band starts at y = −87.33. The settle dragged from 20 points
  below the band's top, y = −67, off the screen: six drags in a row logged the title at −75.33 (`SETTLE-DRAG`, runs
  45680574940 and 45688129991, which kept the earlier test code's settle), and the settle stop audited it there again
  (`OWED data-audio-redo-settle-1`), so the excuse was never settled.
- **The generated project is not the cause.** F75's branch predates CI09 and built XcodeGen with mint; it failed the same
  way. CI09's comparison run 45110743096 generated the project with the release binary and with the mint build of the same
  pinned version: `project.pbxproj` byte-identical (`cmp`), SHA-256
  `7e4a83b4c3f685aa71b0b9cb58a785018ba49ba3b1780fecedbbc96d2cd11f86` and 375,993 bytes for both, and every other generated
  file identical.

The change: the stop is reached by `dragToStop`, slow drags with a hold (`scrollContent`, no fling) from the top of the
screen by the distance the frames give, until the redo button's bottom is 24 points above the bottom of the draggable band
(within 12 points), logged as `STOP` with the button, the summary, the title, the band and the window; with the same test
code, two runs logged the button at exactly the same frame at each size (default: `(100.67, 776.0, 201.0, 50.0)` after one
drag; largest: `(61.0, 700.67, 280.33, 125.33)` after three). The settle, and the search for an owed element, drag only
from the visible part of the band (`draggableBand`), so the title is dragged down to where the screen opens, y = 146 with
the band from 134, where it is clear of the back button and audited `CLEAR` at both sizes (the title keeps the same 12
points below the band's top wherever the content stands, because the back button scrolls with it). Nothing is excused that
is not audited clear in the same test; no exemption, glass-list entry, assertion or wait changed.

**The stop at both sizes in a green run's artifact.** F71c's criterion 3 asks for the audit to run with the redo section on
screen, shown by the run's screenshot at both sizes. The default-size screenshot of the stop, `default-data-audio-redo-01.png`,
is written to the workspace and attached to the result bundle as before, but the routine artifact carries only the
largest-size run's screenshots (above; the default-size ones upload with the failure diagnostics), and the upload list is
not this test's to change. The largest-size run runs second in the same job and already reads the default-size run's
`settled-default.txt` from the shared output directory; after its audit of the stop it also reads the default-size run's
screenshot of it and writes one picture, `xxxl-data-audio-redo-beside-default-<n>.png`, with that screenshot beside its own,
each at half scale under a caption naming its file (`BOTH-SIZES` in `audit-xxxl.log`). The picture is named as the
largest-size run's and goes up with its screenshots, compressed like them, so a green run shows the audited stop at both
sizes. It is evidence only: a missing screenshot is a `NOTE`, not a failure, and the assertions on the stop are unchanged.
Budget: the two groups' green artifacts were 5.8 MB and 3.0 MB on run 45764121728; at half scale the picture adds about
0.2 MB, inside the 10 MB a green run's accessibility artifacts may total.
