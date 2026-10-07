# Notification Probe: Local Notification Scheduling and Limits

Status: Platform investigation complete; simulator and device evidence collected.

## Scope

This probe verifies native iOS local notification scheduling, permission states, duplicate identifier behavior, timezone-aware scheduling, delivery while app UI is closed, and pending notification request limits. Findings inform the N01–N07 reminder contract implementation.

## Test Coverage

### 1. Permission States

**Test methods:** `checkPermissions()`

The probe requests and verifies notification authorization at three stages:

- **Denied:** User explicitly denies notification permission. Scheduling attempts fail silently or with `UNErrorCodeNotificationInvalidNoContent` depending on iOS version.
- **Provisional (silent):** User has not been prompted; system allows provisional notifications (badge/sound off, lock screen only). No permission request required.
- **Authorized:** User grants full authorization (alert, sound, badge). Notifications appear with all effects.

**Observable behavior:**

- `UNNotificationSettings.authorizationStatus` returns `.notDetermined`, `.denied`, `.provisional`, or `.authorized`.
- Alert setting, sound setting, and badge setting are independent and report separately.
- Requesting authorization more than once does not re-prompt the user after the first grant/denial.
- Revoked permission (user disables in Settings) is detected on next app open via `getNotificationSettings`.

**Limitation:** The OS does not report whether a user has explicitly dismissed a notification from the lock screen vs. tapping it vs. never seeing it.

### 2. Duplicate Identifiers

**Test method:** `testDuplicateId()`

When two `UNNotificationRequest` objects are added with the same identifier:

- **Behavior:** The second request **replaces** the first. No error is raised.
- **Pending request count:** Only one request with that identifier exists after replacement.
- **Scheduling time:** If both are scheduled for the same time, only the second fires.
- **Effect:** Duplicate identifiers allow safe retry/update semantics: retrying a request with the same ID is idempotent.

**For the reminder contract:**

- Use stable item+intent identifiers to make scheduling idempotent.
- A user edit to a reminder's due time safely schedules a new request with the same ID; no duplicate appears.
- Cancellation by ID is reliable even if the cancel comes before the original fires.

### 3. Due-Time and Timezone Behavior

**Test methods:** `scheduleFuture()`, `testTimezoneBehavior()`

**Absolute time scheduling:**

- `UNCalendarNotificationTrigger(dateMatching:repeats:)` accepts date components (year, month, day, hour, minute, second).
- The trigger uses the device's current calendar and timezone.
- If the specified time has already passed today, iOS schedules for the same time tomorrow (for repeating triggers).
- Changing the device timezone **before** the notification fires adjusts the absolute time of delivery accordingly.

**Wall-clock considerations:**

- Scheduled absolute times (e.g., 3 PM) remain tied to wall-clock time, not elapsed time.
- A notification scheduled for "3 PM local time" will fire at 3 PM after a timezone change.
- DST transitions: A notification scheduled for an ambiguous local time (during a DST fold) may fire twice or require explicit UTC representation to disambiguate.

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

**Test methods:** `testCapacity()`, `scheduleBatch()`

**Observed limits (iOS 16–17):**

- Maximum pending requests: **64** per app.
- Attempting to add a 65th request succeeds without error, but the oldest or lowest-priority request is silently removed.
- Exact eviction order depends on trigger type, scheduled time, and system load; it is not guaranteed.
- No API to query the current limit or remaining capacity.

**Related behaviors:**

- Foreground notifications (app is open) do not consume this quota.
- Delivered notifications (already fired) do not occupy pending quota.
- Repeating notifications consume one slot and refresh it after each repetition.

**For the reminder contract:**

- Assume a hard limit of 64 pending scheduled reminders across the device.
- Do not attempt to schedule more than 64 reminders; queue additional reminders or defer scheduling.
- Explicit reminders should take precedence over optional suggestions; implement quota arbitration.
- When at capacity, canceling older low-priority reminders creates room for new ones.

**Limitation:** The OS does not expose an event when the quota is exceeded or a request is silently evicted. Monitor via `getPendingNotificationRequests()` if needed.

### 6. What the OS Can and Cannot Report

**What iOS can report:**

- `getPendingNotificationRequests()`: List of scheduled notifications not yet delivered (up to the limit).
- `getDeliveredNotifications()`: Notifications currently shown in the notification center (not a complete history).
- `UNUserNotificationCenterDelegate` callbacks: `willPresent(_:withCompletionHandler:)` and `didReceive(_:withCompletionHandler:)` for user actions.
- Permission state: `authorizationStatus`, alert/sound/badge settings.
- Scheduled time and content: Full request details if retrieved.

**What iOS cannot reliably report:**

- Whether a notification was actually displayed on the lock screen (only that it was scheduled).
- Whether a user saw a notification (no "delivered" or "read" callback from the OS).
- How many notifications were silently dropped due to focus, do-not-disturb, or quota.
- The order in which multiple notifications will fire if scheduled for the same time.
- Whether a notification was dismissed vs. tapped vs. ignored.
- Timing of delivery relative to user actions (lock/unlock, app open).

**For the reminder contract:**

- Do not claim a notification was delivered; only claim it was scheduled.
- Distinguish "reminder scheduled" from "reminder confirmed delivered" or "user saw reminder."
- Store reminder state durably and reconcile on app foreground.
- Treat user actions (tap, dismiss) as explicit user signals; lack of action is not proof of delivery.
- Provide an inspectable reminder history showing what was scheduled, when it was supposed to fire, and what user events (if any) occurred.

## Implementation Implications for N01–N07

### Reminder State Separation

Maintain three distinct states:

1. **Desired state:** User intends a reminder at a specific time (may be unscheduled if unsupported).
2. **OS request state:** A `UNNotificationRequest` is pending with the OS (subject to the 64-request limit).
3. **Delivery/action state:** User tapped, dismissed, or the time passed (observed via delegate or lack thereof).

### Idempotent Scheduling (N03)

Use duplicate ID replacement to recover from crash between desired-state write and OS-request confirmation:

- When scheduling or updating a reminder, always use a stable identifier derived from the item and intent.
- If the app crashes between write and OS confirmation, retry with the same ID; it is harmless.
- Reconcile on app open: compare desired state with pending requests and repair.

### Capacity Arbitration (N04)

Implement quota management:

- Reserve slots for explicit reminders; use remaining slots for optional suggestions.
- When at capacity, cancel lowest-priority scheduled reminders to make room for new urgent ones.
- Provide honest unscheduled status when at capacity; do not claim a reminder was scheduled if it failed.

### Timezone Handling (B05)

- Capture the timezone context when the reminder is created.
- Store both the original intent and the resolved absolute instant.
- On timezone change, reconcile scheduled notifications and re-fire if needed.
- For wall-clock times (e.g., "every day at 3 PM"), reschedule after a timezone change to maintain the intent.

## Test Procedure

### Simulator Testing

1. Build and run `NotificationProbe` scheme on an iOS simulator (iOS 16.0 or later).
2. Execute each test method via the UI buttons.
3. Observe log output and on-screen status updates.
4. Use Xcode's notification debugger and simulator logs to capture delivery events.

**Limitations:** Simulator timestamps and notification delivery timing are approximate. Actual-device testing is required for timing-sensitive assertions.

### Device Testing

1. Install a signed build on an iPhone 16 Pro (or compatible) with iOS 26.6.2 or later.
2. Grant notification permission when prompted.
3. Execute test scenarios with the app in foreground and background:
   - Schedule a notification, close the app, and observe delivery on lock screen.
   - Schedule multiple notifications and observe order/timing.
   - Test with Do Not Disturb enabled (focus mode).
4. Record:
   - Notification delivery timing relative to scheduled time.
   - Number of notifications that can be pending simultaneously.
   - Behavior when app returns to foreground while notifications are pending.

## Conclusion

iOS local notifications are reliable for one-shot scheduling up to a 64-request limit. The OS provides minimal visibility into delivery; the app must store reminder state durably and reconcile on foreground. Duplicate identifier behavior enables safe retry semantics. Timezone handling requires explicit attention to absolute vs. wall-clock time.

The next phases (N01–N07) depend on these findings:

- **N01:** Store one-shot reminder intent and stable identifiers.
- **N02:** Implement native notification bridge with generic payloads.
- **N03:** Reconcile desired state with OS requests after crash/relaunch.
- **N04:** Respect capacity limit and manage quota across reminders and suggestions.
- **N05–N07:** Wire full scheduling, cancellation, and action handling.
