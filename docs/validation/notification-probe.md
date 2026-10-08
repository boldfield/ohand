# Notification Probe: Local Notification Scheduling and Limits

Status: Simulator XCTest automation in progress; device testing pending external input (physical device and signing required).

## Scope

This probe verifies native iOS local notification scheduling, permission states, duplicate identifier behavior, timezone-aware scheduling, delivery while app UI is closed, and pending notification request limits. Findings inform the N01–N07 reminder contract implementation.

## Test Coverage

### 1. Permission States

**Test method:** XCTest `testPermissionStateCanBeQueried()`

The probe verifies that notification authorization status can be queried reliably. The CI test does not request authorization to avoid blocking on system prompts in headless simulators; instead it queries the current authorization state, which is `notDetermined` by default.

- **Not Determined:** User has not been prompted; initial state and CI test state (no authorization requested).
- **Denied:** User explicitly denies notification permission. Scheduling attempts succeed but notifications are not delivered; no error is raised.
- **Provisional:** App must explicitly request provisional authorization with the `.provisional` option. Notifications appear silently in Notification Center without lock screen or banner; no user prompt. Not tested in CI.
- **Authorized:** User grants full authorization (alert, sound, badge). Notifications appear with full effects (lock screen, banner, sound, badge). Not tested in CI.

**Observable behavior (simulator only):**

- `UNNotificationSettings.authorizationStatus` returns one of the valid states for the authorization level the app has requested (or `notDetermined` if no request was made).
- Alert setting, sound setting, and badge setting are queryable separately.
- `getNotificationSettings` callback reliably returns the current state.

**CI limitation:** The test only verifies that status can be queried in the `notDetermined` state; it does not request authorization or test state transitions. Full permission state testing (requesting `.alert`, `.sound`, `.badge`, or `.provisional`) requires device testing or a separate setup step.

**Platform limitation:** The OS does not report whether a user has explicitly dismissed a notification from the lock screen vs. tapping it vs. never seeing it.

### 2. Duplicate Identifiers

**Test method:** XCTest `testDuplicateIdReplacement()`

**CI behavior (without authorization):**
The test verifies that `add()` succeeds for duplicate IDs in the `notDetermined` authorization state. Without authorization, `getPendingNotificationRequests()` returns empty, so the replacement semantics cannot be observed in CI.

**Device/authorized behavior:**
When two `UNNotificationRequest` objects are added with the same identifier and authorization is granted:

- **Behavior:** The second request **replaces** the first. No error is raised.
- **Pending request count:** Only one request with that identifier exists after replacement (verified by `getPendingNotificationRequests()`).
- **Content verification:** The pending request reflects the second request's content (title, body, etc.).
- **Effect:** Duplicate identifiers allow safe retry/update semantics: retrying a request with the same ID is idempotent.

**For the reminder contract:**

- Use stable item+intent identifiers to make scheduling idempotent.
- A user edit to a reminder's due time safely schedules a new request with the same ID; no duplicate appears.
- Cancellation by ID is reliable even if the cancel comes before the original fires.

### 3. Due-Time and Timezone Behavior

**Test method:** XCTest `testCalendarTriggerFireDate()` (simulator only)

**CI behavior (without authorization):**
The test verifies that calendar trigger objects can be created and added to the notification center without error. Without authorization, the request is not stored, so `nextTriggerDate()` cannot be observed.

**Calendar-based scheduling (authorized/device):**

- `UNCalendarNotificationTrigger(dateMatching:repeats:)` accepts date components (year, month, day, hour, minute, second).
- The trigger is built from the device's current calendar and timezone at scheduling time.
- The `nextTriggerDate()` property of a pending request reflects the computed absolute time in UTC.

**Timezone changes (Device only, not tested in simulator):**

- Changing device timezone after scheduling a calendar trigger may adjust when the notification fires. This requires device-level testing with actual timezone changes to verify.
- Daylight Saving Time transitions and their effects on calendar triggers require device-level testing.

**Time-interval triggers:**

- `UNTimeIntervalNotificationTrigger(timeInterval:repeats:)` fires after an elapsed duration and is not affected by timezone changes.

**Time interval scheduling:**

- `UNTimeIntervalNotificationTrigger(timeInterval:repeats:)` fires after a specified elapsed duration.
- Time-interval triggers are immune to timezone changes but are affected by system clock adjustments.

**For the reminder contract:**

- Store reminder intent with both captured timezone context and absolute instant (resolved at request time).
- Schedule using calendar triggers for wall-clock times (e.g., "3 PM"); these adjust with timezone changes.
- Repeating triggers beyond one-shot are not fully supported by iOS in M1; preserve the original intent as unscheduled.

### 4. Delivery with App UI Closed

**Observable behavior:**

- Notifications are delivered to the lock screen and notification center even if the app is not open.
- A closed app does not receive `UNUserNotificationCenterDelegate` callbacks until it is opened.
- The app's `application(_:didFinishLaunchingWithOptions:)` is called on app open; notification actions are processed then.
- Background app refresh has finite execution time and is not guaranteed; notification delivery is independent of background execution.

**Limitations:**

- The app cannot log which notifications were delivered or missed while closed (unless stored in app-level state before closure).
- The OS does not expose a "notification was delivered" callback; only user actions (tap, clear) are observed.
- No way to retrieve OS statistics about which notifications were actually shown vs. filtered/suppressed by Focus/Do Not Disturb.

**For the reminder contract:**

- Do not assume the app will be running to verify notification delivery.
- Store the desired reminder state durably; reconcile on app open.
- Use persistent push or scheduled background work sparingly; local notifications are the primary delivery mechanism.

### 5. Pending Notification Request Limits

**Test method:** XCTest `testCapacityLimit()` (simulator only)

**CI behavior (without authorization):**
The test verifies that 100 requests can be added without errors and that `getPendingNotificationRequests()` can be called. Without authorization, the list is empty.

**Per-app pending request limit (authorized/device):**

According to [Apple's UNUserNotificationCenter documentation](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter), the system enforces a limit on pending notification requests per app. Apple's documentation states that the system retains the soonest-firing 64 requests and discards the rest when this limit is exceeded.

**Observable behavior (simulator with authorization):**

- When 100 requests are scheduled, the system retains only a bounded subset. The test verifies that not all 100 are kept.
- When adding beyond capacity, the system silently does not store new requests beyond the limit; no error is raised.
- `getPendingNotificationRequests()` returns only the retained requests (up to the limit).
- No public API to query the current limit or remaining capacity.

**Related behaviors (Simulator only):**

- Delivered notifications (already fired) do not occupy pending quota.
- Repeating notifications consume one pending slot per app.

**For the reminder contract:**

- Assume a hard limit of 64 pending scheduled reminders per app (per Apple's documentation).
- Do not attempt to schedule more than 64 reminders per app; queue additional reminders or defer scheduling.
- Explicit reminders should take precedence over optional suggestions; implement quota arbitration.
- When at capacity, canceling older low-priority reminders creates room for new ones.

**Limitation:** The OS does not expose an event when the quota is exceeded or a request is silently evicted. Monitor via `getPendingNotificationRequests()` if needed.

### 6. What the OS Can and Cannot Report

**What iOS APIs are callable (CI-verified without authorization):**

- `add()`: Add notification requests. **Verified:** Requests can be added without error even without authorization.
- `getPendingNotificationRequests()`: List pending notifications. **Verified:** API is callable; returns empty list without authorization, bounded list with authorization.
- `removePendingNotificationRequests(withIdentifiers:)`: Remove requests by ID. **Verified:** API is callable and can be invoked.
- `getNotificationSettings()`: Query permission state. **Verified:** Returns `notDetermined` when no authorization is requested.
- `UNCalendarNotificationTrigger()`, `UNTimeIntervalNotificationTrigger()`: Create triggers. **Verified:** Can be created and added without error.

**What iOS can report with authorization (not tested in CI; device/authorized testing required):**

- `getPendingNotificationRequests()`: Returns the list of scheduled notifications (up to the system limit of 64 per app). **Verified in authorized scenarios only.**
- Pending request content and trigger details. **Verified in authorized scenarios only.**
- `UNCalendarNotificationTrigger.nextTriggerDate()`: Computed absolute fire time. **Verified in authorized scenarios only.**
- `getDeliveredNotifications()`: Notifications currently shown in the notification center. **Not currently tested.**
- Permission state transitions and settings (alert, sound, badge, provisional). **Partially tested — current state queryable; transitions not tested.**

**What iOS can report but is not currently tested:**

- `UNUserNotificationCenterDelegate.willPresent(_:withCompletionHandler:)`: Called when the app is in the foreground and a notification would be delivered. **Not tested** — no delegate is configured.
- `UNUserNotificationCenterDelegate.didReceive(_:withCompletionHandler:)`: Called when the user taps on a notification. **Not tested** — requires user interaction.
- Provisional authorization state request and behavior. **Not tested.**
- Timezone changes and their effect on calendar-based triggers. **Not tested in simulator; device testing required.**

**What iOS cannot reliably report:**

- Whether a notification was actually displayed on the lock screen or notification center (only that it was scheduled).
- Whether a user saw a notification (the OS does not provide delivery confirmation).
- How many or which notifications were silently dropped due to focus settings, Do Not Disturb, or system quota.
- The exact order in which multiple notifications scheduled for the same time will fire.
- Whether a notification was dismissed, tapped, or ignored (only user actions are reported via delegate callbacks when app is foreground; closed-app behavior is unobservable without device testing).
- Notification delivery timing relative to device lock/unlock, app foreground/background transitions.
- The reason a scheduled request was not delivered (permission change, system limits, etc.).

**For the reminder contract (API-verified behaviors):**

- Notification request APIs (`add()`, list, cancel) are reliably callable; authorization is required to actually store and manage requests.
- Use duplicate ID for idempotent updates (behavior verified with authorization).
- Do not claim a notification was delivered; only claim it was scheduled.
- Distinguish "reminder scheduled" from "reminder confirmed delivered" or "user saw reminder".
- Store reminder state durably and reconcile on app foreground.
- App-closed delivery confirmation requires device testing.
- Provide an inspectable reminder history showing what was scheduled (pending requests are readable with authorization).

## Implementation Implications for N01–N07

### Reminder State Separation

Maintain three distinct states (simulator verified):

1. **Desired state:** User intends a reminder at a specific time (may be unscheduled if unsupported).
2. **OS request state:** A `UNNotificationRequest` is pending with the OS (subject to system-imposed limit).
3. **Delivery/action state:** User tapped, dismissed, or the time passed (observed via delegate when app is foreground; app-closed delivery unobservable without device testing).

### Idempotent Scheduling (N03, verified in simulator)

Use duplicate ID replacement to recover from crash between desired-state write and OS-request confirmation:

- When scheduling or updating a reminder, always use a stable identifier derived from the item and intent.
- If the app crashes between write and OS confirmation, retry with the same ID; it is harmless (verified: second add replaces first).
- Reconcile on app open: compare desired state with pending requests and repair.

### Capacity Arbitration (N04, device testing required)

Implement quota management (specific limit behavior requires device verification):

- Respect the system limit on pending requests per app.
- When at or near capacity, cancel lower-priority scheduled reminders to make room for new urgent ones.
- Provide honest unscheduled status when at capacity; do not claim a reminder was scheduled if it failed.

### Timezone Handling (B05, device testing required)

- Capture the timezone context when the reminder is created.
- Store both the original intent and the resolved absolute instant.
- Calendar-based triggers (wall-clock times) behavior under timezone changes requires device-level testing.
- Time-interval triggers are not affected by timezone changes.

## Test Procedure

### CI Testing (automated via XCTest, notDetermined authorization state)

The NotificationProbe includes XCTest cases run by the iOS CI workflow (`.github/workflows/ios.yml`). These tests verify that the notification APIs are callable without error:

1. **Permission state reporting:** Verify `getNotificationSettings()` returns a valid status (notDetermined without authorization request).
2. **Request addition:** Verify `add()` succeeds without error for duplicate identifiers.
3. **Cancel by identifier:** Verify `removePendingNotificationRequests(withIdentifiers:)` is callable without error.
4. **Capacity behavior:** Verify 100 requests can be added and `getPendingNotificationRequests()` is callable (returns empty without authorization).
5. **Calendar trigger creation:** Verify calendar and time-interval triggers can be created and added without error.

**CI Evidence:** XCTest pass/fail status and test logs are captured in the iOS CI workflow output.

### Authorized Simulator Testing (requires setup outside CI)

For full behavior verification, authorization must be granted:

1. **Duplicate ID replacement:** With authorization, verify that a second request with the same identifier replaces the first, and `getPendingNotificationRequests()` returns the updated content.
2. **Request listing and cancellation:** Verify `getPendingNotificationRequests()` lists stored requests and `removePendingNotificationRequests()` removes them correctly.
3. **Capacity limits:** Measure the actual limit when 100 requests are scheduled and record which identifiers are retained.
4. **Calendar trigger resolution:** Verify `nextTriggerDate()` returns a future date for calendar-based triggers.

### Device Testing (External input, signing required)

Device testing is blocked pending access to a physical device and Apple signing credentials. When available:

1. Install a signed build on an iPhone 16 Pro (or compatible) with a current iOS version.
2. Grant notification permissions when prompted.
3. Execute test scenarios:
   - Schedule a notification, close the app, and verify delivery on the lock screen.
   - Schedule multiple notifications and record actual firing order and timing.
   - Test with Do Not Disturb and focus modes enabled.
   - Change device timezone and measure re-fire behavior of calendar-based triggers.
   - Verify behavior with app in background and app killed.
4. Record device OS version, build identifiers, and results for each scenario.

## Conclusion

### CI Findings (XCTest, notDetermined authorization state)

- Notification request APIs are callable and return without error (`add()`, `getPendingNotificationRequests()`, `removePendingNotificationRequests()`, `getNotificationSettings()`).
- Permission state is queryable via `getNotificationSettings()` (returns `notDetermined` without authorization request).
- Request content and trigger objects can be created and passed to the APIs (e.g., `UNCalendarNotificationTrigger`, `UNTimeIntervalNotificationTrigger`).
- Actual pending request storage and retrieval requires authorization (returns empty in `notDetermined` state).

### Authorized Simulator Testing (Requires pre-granted authorization)

With authorization, the following behaviors are observable:

- Duplicate identifier replacement: a second request with the same ID replaces the first without error.
- Pending request listing via `getPendingNotificationRequests()` and cancellation via `removePendingNotificationRequests(withIdentifiers:)`.
- The system enforces a per-app limit on pending requests; Apple documents this as 64 soonest-firing requests retained, rest discarded.
- Calendar-based triggers compute an absolute fire time accessible via `nextTriggerDate()`.

### Device Testing (Pending)

Device-level verification is required for:

- Actual notification delivery timing and appearance on lock screen.
- App-closed delivery confirmation (closed app receives no notification delegate callbacks).
- Behavior with Do Not Disturb, Focus modes, and other system restrictions.
- Timezone change effects on calendar-based triggers.
- Full authorization and provisional authorization request flows.

### Implications for N01–N07

The findings inform the implementation:

- **N01:** Store one-shot reminder intent and stable identifiers (duplicate ID replacement enables safe scheduling; requires authorization to verify).
- **N02:** Implement native notification bridge with generic payloads (request APIs are callable; actual queue management requires authorization).
- **N03:** Reconcile desired state with OS requests after crash/relaunch (requires authorized access to pending requests).
- **N04:** Respect 64 per-app pending request limit and manage quota across reminders and suggestions (limit documented by Apple; capacity behavior requires authorized testing).
- **N05–N07:** Wire full scheduling, cancellation, and action handling (APIs are callable; full delivery and action behavior pending device testing).
