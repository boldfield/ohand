# Local notification probe (P05)

P05 checks the native local-notification primitives that the reminder contract will rely on: permission states, request identifiers, cancel and list, due-time and time-zone behavior, the pending-request limit, and delivery while the app is not running.

Everything marked **simulator** below was observed by automated UI tests running on a GitHub-hosted macOS runner. **Nothing in this document was observed on a physical iPhone.** Physical-device checks are listed under [Not tested](#not-tested-physical-device-and-other-gaps) and are owned by the actual-device feasibility task; they are not claimed here.

## What exists

| Path | Role |
| --- | --- |
| `ios/NotificationProbe/Sources/ProbeScenarios.swift` | One async function per scenario. Each returns string observations. Also records `willPresent` delegate callbacks. |
| `ios/NotificationProbe/Sources/AppDelegate.swift` | Probe app: one button per scenario (`probe.run.<name>`), publishes the latest result as JSON in the accessibility label `probe.lastResult`, and logs it with `os.Logger`. |
| `ios/NotificationProbe/Tests/` | `NotificationProbeUITests` bundle (`XCUITest`): `NotificationProbeAuthorizedUITests` answers the system prompt with Allow, `NotificationProbeDeniedUITests` answers Don't Allow. Both print every result as `PROBE-RESULT {json}`. |
| `ios/NotificationProbe/scripts/run-evidence.sh` | Erases the simulator before each suite (uninstalling the app does not clear a recorded notification decision), then runs the suite with `xcodebuild test`. Called by the `Run NotificationProbe UI tests` step of `.github/workflows/ios.yml`. |

Run locally on macOS: `cd ios && ./scripts/generate.sh && ./NotificationProbe/scripts/run-evidence.sh <simulator-udid> /tmp/notification-evidence`. The script erases the chosen simulator.

## Evidence provenance

| Item | Value |
| --- | --- |
| Run | <https://github.com/boldfield/ohand/actions/runs/37721536062> (job "Simulator build, unit tests and launch smoke test", `ios.yml`), conclusion `success` |
| Commit | `7c9c5d7537f30d343f4a62fe7994f9c85a8829e6` |
| Runner and toolchain | `macos-15`, Xcode 16.4 |
| Simulator | iPhone 16, iOS 18.5 (build 22F77), simulator time zone `GMT` |
| Result | `NotificationProbeAuthorizedUITests.testAuthorizedSchedulingBehavior` and `NotificationProbeDeniedUITests.testDeniedPermissionBehavior` passed |
| Where to read the raw values | `PROBE-RESULT` lines in the step log, and `notification-probe/*.log` plus `*.xcresult` in the `ios-evidence` artifact (kept 7 days) |

The numbers below come from that run. Later commits on this branch changed only assertions, the document and removed an unused report file; the same observations are asserted by the tests on every subsequent `ios.yml` run, so a platform change that invalidates a row fails CI instead of silently leaving this document stale. The PR description links the run for the submitted commit.

An earlier run of the same scenario code (`37719967609`, commit `812b72c`) produced the same observations for the allowed-permission suite (duplicate, cancel, calendar, capacity and delivery); its denied suite had not yet been made to run from a clean permission state.

## Simulator results

### Permission states

| Step | Observation |
| --- | --- |
| Fresh install, before any request | `authorizationStatus = notDetermined`; alert, sound, badge, lock screen and notification center settings all `notSupported`; alert style `none` |
| Request `[.alert, .sound, .badge]`, user taps Allow | `granted = true`; `authorized`; alert, sound, badge, lock screen, notification center `enabled`; alert style `banner` |
| Second request after the decision | returned `true` again in tens of milliseconds, with no prompt (matches Apple: "Subsequent authorization requests don't prompt the person") |
| Request, user taps Don't Allow | `granted = false`; `denied`; alert, sound, badge, lock screen, notification center `disabled`; alert style `none` |
| Second request after denial | returned `false` immediately, no prompt |
| `add` of a request while denied | completes with **no error**, but the request is **not retained**: pending count `0`, request not listed |

Consequence: a successful `add` completion does not prove a reminder is scheduled. Read `notificationSettings()` before scheduling and read the pending list afterwards.

Not exercised: `.provisional` and `.ephemeral`. The probe never requests `.provisional`, so those states were not reached. Apple documents provisional delivery as quiet and only in Notification Center history, with the authorization status `provisional` until the person keeps or turns off the notifications. See [Not tested](#not-tested-physical-device-and-other-gaps).

### Duplicate identifiers and cancel

- Adding two requests with the same identifier (`First` at 3600 s, then `Second` at 7200 s): the pending list holds **one** request with that identifier, title `Second`, interval `7200`. The later `add` replaces the earlier one.
- `removePendingNotificationRequests(withIdentifiers:)` of one of three identifiers: the removed identifier was absent on the first read-back (1 poll); the other two remained. Apple documents that this method removes requests on a secondary thread, so the probe polls instead of assuming synchronous removal; production code should do the same before asserting.
- Removing an identifier that does not exist is a no-op: the two remaining requests were unchanged.
- `removeAllPendingNotificationRequests()` left `0` pending.

### Due time and time zone

All values are for the `GMT` simulator and a request three days ahead. "Delta" is the trigger's `nextTriggerDate()` minus the independently computed expected instant.

| Trigger | Result |
| --- | --- |
| `UNTimeIntervalNotificationTrigger` 3600 s | next trigger = scheduling time + 3600 s (delta 0 s) |
| Calendar trigger, components carrying `timeZone = Pacific/Auckland`, 09:30 local | fires at the Auckland instant (20:30 UTC the previous day); delta 0 s, read back from the pending request |
| Same with `America/Los_Angeles` | fires at the Los Angeles instant (16:30 UTC); delta 0 s |
| Calendar trigger with **no** `timeZone` in the components (floating) | fires at 09:30 in the device time zone (`GMT` here); delta 0 s |
| `America/New_York`, non-existent local time 02:30 on the spring-forward date | next trigger 07:30 UTC, that is 03:30 EDT (moved forward) |
| `America/New_York`, ambiguous local time 01:30 on the fall-back date | next trigger 05:30 UTC, that is 01:30 EDT (the first occurrence) |
| One-shot calendar trigger whose date is in the past | `add` reports **no error**, `nextTriggerDate()` is `nil`, the request is **not retained** |

The DST rows used the March and November transitions of the year after the run; the test asserts the UTC time-of-day offsets, not a calendar date.

A trigger that carries an explicit `timeZone` is an absolute instant. A floating trigger is interpreted in the device's time zone. **What happens to an already-scheduled floating trigger when the device time zone later changes was not tested** (the CI simulator cannot change its zone mid-run); see [Not tested](#not-tested-physical-device-and-other-gaps).

The probe schedules only positive, non-repeating intervals.

### Pending-request limit

**Primary documentation.** The current UserNotifications pages for `UNUserNotificationCenter`, `add(_:withCompletionHandler:)`, `getPendingNotificationRequests(completionHandler:)`, the scheduling article and the triggers do **not** state a numeric limit (their text was searched for it). The only Apple page that states one is the deprecated `UILocalNotification` reference:

> An app can have only a limited number of scheduled notifications; the system keeps the soonest-firing 64 notifications (with automatically rescheduled notifications counting as a single notification) and discards the rest.

That statement is about the deprecated API and is the only primary-source basis for the number 64 and for a "soonest-firing" retention rule. It is per app.

**Observation (simulator, iOS 18.5).** The probe added 100 non-repeating interval requests, one at a time, awaiting each `add`. Fire times were a permutation of 7200 s to 13140 s in 60 s steps, so insertion order and fire order differ. Every `add` completed without error (`addErrorCount = 0`). Then:

| Measure | Value |
| --- | --- |
| Pending requests listed | **64** |
| Retained among the 64 soonest-firing requests | 41 |
| Retained among the first 64 added | 28 |
| Retained among the last 64 added | **64** (identifiers `probe.cap.036` to `probe.cap.099`) |

So on this OS the limit is 64 per app, additions beyond it do **not** report an error, and the retained set was the **most recently added** 64, not the soonest-firing 64 described by the legacy page. The two sources disagree on retention order. The observation is for one OS version on a simulator; treat the retention rule as unconfirmed for the target device until the physical check passes, and do not design reminders around either rule. Instead keep fewer than 64 requests pending and plan the window explicitly (see [Implications](#implications-for-the-reminder-contract)).

Not measured: whether delivered notifications or repeating triggers consume pending slots.

### Delivery

- **Foreground, delegate installed.** A 2 s request produced a `willPresent` callback (`willPresentCalled = true`); afterwards the request was no longer pending and was listed by `getDeliveredNotifications`.
- **App not running.** A 5 s request was scheduled, the app was terminated by the UI test, and the test waited 15 s before relaunching. After relaunch the probe found the request in `getDeliveredNotifications` (delivered date 5 s after scheduling), no longer pending, and the delegate had received **no** `willPresent` callback in the new process. `removeAllDeliveredNotifications()` brought the delivered list to 0.

This is simulator behavior. Whether the iOS simulator's delivery with the app terminated matches a locked physical iPhone (Focus modes, scheduled summary, Low Power Mode, device reboot) is **not** established here.

## What the OS can and cannot report

Sources are the Apple pages listed below plus the observations above.

| Question | What the OS reports |
| --- | --- |
| Is it pending? | `getPendingNotificationRequests` lists scheduled local requests, and `nextTriggerDate()` on the trigger gives the next fire instant. Accepting an `add` does not guarantee presence (denied and past-date cases above). |
| Was it delivered? | Only indirectly: a request that is no longer pending and appears in `getDeliveredNotifications` was delivered. Apple documents that list as notifications "still present in Notification Center", so a notification the person cleared or the system removed is no longer visible, and absence is **not** evidence of non-delivery. |
| Was it seen? | No API reports that a notification was seen. Presence in the delivered list says it was delivered and not cleared, not that anyone looked at it. |
| Was it opened or acted on? | `userNotificationCenter(_:didReceive:withCompletionHandler:)` receives the response only when the person taps the notification or one of its actions. |
| Was it dismissed? | Only an explicit dismissal reaches the delegate, and only if the category was registered with `customDismissAction`. Apple states that ignoring a notification or flicking away a banner does not trigger the action. |
| Was it missed? | There is no "missed" signal. A notification that fired while the person was away, silenced by Focus, or cleared unseen looks the same to the app as one that was seen and cleared. |
| Can the app tell the decision? | `notificationSettings()` reports status and per-channel settings; the system enforces denied settings itself. |

Reminder state that matters (due, acknowledged, done, overdue) must therefore live in the app's own data. Notification delivery can only be a prompt, never the record.

## Implications for the reminder contract

These follow from the observations and are inputs to the notification design, not tested behavior:

1. Treat a successful `add` as insufficient. After scheduling, read back the pending list and compare, and check `notificationSettings()` first.
2. Derive due instants from authoritative reminder data and schedule absolute instants (explicit `timeZone` or computed `Date`), re-deriving on launch and after time-zone or clock changes.
3. Keep the number of pending requests below 64 and replace the soonest-N window as time passes; do not rely on the OS retention rule.
4. Use stable identifiers; re-adding an identifier replaces the earlier request (observed), which gives idempotent re-scheduling.
5. Do not infer "missed" or "seen" from the OS. Record acknowledgement only from an explicit user action.

## Not tested (physical device and other gaps)

These checks were **not** performed and no result is claimed:

- Any behavior on a physical iPhone, including the baseline target (iPhone 16 Pro, iOS 26.6.2): delivery with the screen locked and the app terminated, Focus and scheduled-summary effects, Low Power Mode, reboot, and the banner/lock-screen presentation the person sees. The simulator checks above do not substitute for these.
- The 64-request retention rule on the target OS.
- Provisional authorization (`.provisional`) and `.ephemeral`, and the transitions between states, including the person changing the decision in Settings while the app has pending requests.
- Changing the device time zone or clock after scheduling (floating and explicit-zone triggers), and the date-change/DST behavior on a device.
- Whether delivered notifications or repeating triggers count against the pending limit.
- Delivery of a request that is still pending when the OS is upgraded or the app is reinstalled.

Physical-device evidence is collected by the actual-device feasibility task, which must cite sanitized artifacts rather than this document.

## Primary sources

Fetched from Apple's documentation service on 2026-10-08; the quoted or paraphrased statements above come from these pages.

- <https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications> (authorization prompt behavior, provisional authorization, check status before scheduling)
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/add(_:withcompletionhandler:)>
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/getpendingnotificationrequests(completionhandler:)>
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/removependingnotificationrequests(withidentifiers:)> (asynchronous removal)
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenter/getdeliverednotifications(completionhandler:)> ("still present in Notification Center")
- <https://developer.apple.com/documentation/usernotifications/uncalendarnotificationtrigger/nexttriggerdate()>
- <https://developer.apple.com/documentation/usernotifications/untimeintervalnotificationtrigger>
- <https://developer.apple.com/documentation/usernotifications/unnotificationdismissactionidentifier> and <https://developer.apple.com/documentation/usernotifications/unnotificationcategoryoptions/customdismissaction>
- <https://developer.apple.com/documentation/usernotifications/unusernotificationcenterdelegate/usernotificationcenter(_:didreceive:withcompletionhandler:)>
- <https://developer.apple.com/documentation/uikit/uilocalnotification> (deprecated; the only page found that states the 64-notification limit)
