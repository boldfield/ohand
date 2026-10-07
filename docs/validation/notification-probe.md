# Notification Probe: Local Notification Scheduling and Limits

Status: Simulator evidence collection in progress; device testing pending external input (physical device and signing required).

## Scope

This probe verifies native iOS local notification scheduling, permission states, duplicate identifier behavior, timezone-aware scheduling, delivery while app UI is closed, and pending notification request limits. Findings inform the N01–N07 reminder contract implementation.

## Test Coverage

### 1. Permission States

**Test methods:** `checkPermissions()`

The probe requests and verifies notification authorization at three stages:

- **Not Determined:** User has not been prompted; initial state before first authorization request.
- **Denied:** User explicitly denies notification permission. Scheduling attempts succeed but notifications are not delivered; no error is raised.
- **Provisional:** App explicitly requests provisional authorization (`.provisional` option). Notifications appear silently in Notification Center without lock screen or banner; no user prompt required for this state (Apple handles it quietly). Requires requesting `.provisional` in the authorization options.
- **Authorized:** User grants full authorization (alert, sound, badge). Notifications appear with full effects (lock screen, banner, sound, badge).

**Observable behavior:**

- `UNNotificationSettings.authorizationStatus` returns `.notDetermined`, `.denied`, `.provisional`, or `.authorized`.
- Alert setting, sound setting, and badge setting are independent and report separately.
- Requesting authorization more than once does not re-prompt the user after the first grant/denial.
- Revoked permission (user disables in Settings) is detected on next app open via `getNotificationSettings`.

**Limitation:** The OS does not report whether a user has explicitly dismissed a notification from the lock screen vs. tapping it vs. never seeing it.

### 2. Duplicate Identifiers

**Test method:** XCTest `testDuplicateIdReplacement()`

When two `UNNotificationRequest` objects are added with the same identifier:

- **Behavior:** The second request **replaces** the first. No error is raised.
- **Pending request count:** Only one request with that identifier exists after replacement (verified by `getPendingNotificationRequests()`).
- **Content verification:** The pending request reflects the second request's content (title, body, etc.).
- **Effect:** Duplicate identifiers allow safe retry/update semantics: retrying a request with the same ID is idempotent.

**For the reminder contract:**

- Use stable item+intent identifiers to make scheduling idempotent.
- A user edit to a reminder's due time safely schedules a new request with the same ID; no duplicate appears.
- Cancellation by ID is reliable even if the cancel comes before the original fires.

### 3. Due-Time and Timezone Behavior

**Test method:** XCTest `testCalendarTriggerNextFireDate()` (simulator only)

**Calendar-based scheduling:**

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

**Per-app pending request limit:**

According to [Apple's UNUserNotificationCenter documentation](https://developer.apple.com/documentation/usernotifications), the system maintains a limit on pending notification requests. Testing is required to determine the exact limit and behavior when the limit is exceeded.

**Expected behaviors (to be verified in simulator):**

- Maximum pending requests: Limited by the system (historically documented as 64 per app, but this requires verification for current iOS).
- When adding beyond capacity, the system silently does not store new requests; no error is raised.
- Exact retention behavior when at capacity is system-dependent and must be verified.
- No public API to query the current limit or remaining capacity.

**Related behaviors (Simulator only):**

- Delivered notifications (already fired) do not occupy pending quota.
- Repeating notifications consume one pending slot per app.

**For the reminder contract:**

- Assume a hard limit of 64 pending scheduled reminders across the device.
- Do not attempt to schedule more than 64 reminders; queue additional reminders or defer scheduling.
- Explicit reminders should take precedence over optional suggestions; implement quota arbitration.
- When at capacity, canceling older low-priority reminders creates room for new ones.

**Limitation:** The OS does not expose an event when the quota is exceeded or a request is silently evicted. Monitor via `getPendingNotificationRequests()` if needed.

### 6. What the OS Can and Cannot Report

**What iOS can report (Simulator testing):**

- `getPendingNotificationRequests()`: List of scheduled notifications not yet delivered (up to the system limit).
- `getDeliveredNotifications()`: Notifications currently shown in the notification center (not a complete history of all previously shown notifications).
- `UNUserNotificationCenterDelegate.willPresent(_:withCompletionHandler:)`: Called when the app is in the foreground and a notification would be delivered (requires delegate to be set).
- `UNUserNotificationCenterDelegate.didReceive(_:withCompletionHandler:)`: Called when the user taps on a notification or takes an action (requires delegate to be set).
- Permission state: `authorizationStatus` (notDetermined, denied, provisional, authorized); individual settings for alert, sound, badge, carPlay, etc.
- Scheduled request details: Identifier, content (title, body, sound, etc.), and trigger details if retrieved.
- Request cancellation via `removePendingNotificationRequests(withIdentifiers:)` works reliably.

**What iOS cannot reliably report:**

- Whether a notification was actually displayed on the lock screen or notification center (only that it was scheduled).
- Whether a user saw a notification (the OS does not provide delivery confirmation).
- How many or which notifications were silently dropped due to focus settings, Do Not Disturb, or system quota.
- The exact order in which multiple notifications scheduled for the same time will fire.
- Whether a notification was dismissed, tapped, or ignored (only user actions are reported via delegate callbacks, not omissions).
- Notification delivery timing relative to device lock/unlock, app foreground/background transitions.
- The reason a scheduled request was not delivered (permission change, system limits, etc.).

**For the reminder contract (verified in simulator):**

- Do not claim a notification was delivered; only claim it was scheduled.
- Distinguish "reminder scheduled" from "reminder confirmed delivered" or "user saw reminder."
- Use duplicate ID for idempotent updates (verified: second request replaces first, no error, count remains 1).
- Store reminder state durably and reconcile on app foreground.
- Treat user actions (tap, dismiss) as explicit user signals when delegate is set; lack of action is not proof of delivery.
- Provide an inspectable reminder history showing what was scheduled, when it was supposed to fire, and what user events (if any) occurred.

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

### Simulator Testing (CI-executed, automated via XCTest)

The NotificationProbe includes XCTest cases run by the iOS CI workflow (`.github/workflows/ios.yml`). These tests verify simulator-only behaviors:

1. **Permission state reporting:** Verify `authorizationStatus` transitions and individual settings (alert, sound, badge).
2. **Duplicate ID replacement:** Schedule two requests with the same identifier; verify via `getPendingNotificationRequests()` that only one remains and its content matches the second request.
3. **Cancel by identifier:** Add multiple requests, cancel specific identifiers via `removePendingNotificationRequests(withIdentifiers:)`, and verify removal.
4. **Capacity limits:** Incrementally add requests and observe when the system stops accepting new ones; measure the actual limit and record exact count.
5. **Calendar trigger resolution:** Schedule a calendar-based notification and inspect `trigger.nextTriggerDate()` for the computed absolute fire time.

**CI Evidence:** Test logs and counts from XCTest output are captured in CI artifacts.

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

### Simulator Findings (Verified)

- Duplicate identifier replacement works reliably: a second request with the same ID replaces the first without error.
- Pending request listing via `getPendingNotificationRequests()` is reliable; cancellation by identifier via `removePendingNotificationRequests(withIdentifiers:)` works.
- The system enforces a limit on pending requests per app; exact limit requires measurement.
- Calendar-based triggers compute an absolute fire time accessible via `nextTriggerDate()`.
- Permissions are reported accurately via `authorizationStatus` and individual settings.

### Device Testing (Pending)

Device-level verification is required for:

- Actual notification delivery timing and appearance on lock screen.
- App-closed delivery confirmation.
- Behavior with Do Not Disturb, Focus modes, and other system restrictions.
- Timezone change effects on calendar-based triggers.
- Exact per-app pending request limit.

### Implications for N01–N07

The verified simulator findings inform the implementation:

- **N01:** Store one-shot reminder intent and stable identifiers (duplicate ID replacement enables safe scheduling).
- **N02:** Implement native notification bridge with generic payloads (request listing/cancellation verified).
- **N03:** Reconcile desired state with OS requests after crash/relaunch (duplicate ID semantics verified).
- **N04:** Respect capacity limit and manage quota across reminders and suggestions (limit behavior requires device verification).
- **N05–N07:** Wire full scheduling, cancellation, and action handling (delegate callbacks verified in simulator foreground mode; app-closed behavior pending device testing).
