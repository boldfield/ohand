import Foundation
import UserNotifications

enum ProbeScenario: String, CaseIterable {
    case settings
    case requestAuthorization
    case schedulingProbe
    case duplicateIdentifier
    case cancelByIdentifier
    case calendarTrigger
    case capacity
    case foregroundDelivery
    case scheduleClosedAppDelivery
    case reportDelivered
    case clearDelivered
}

/// Records willPresent callbacks so foreground delivery is observable.
final class ForegroundPresentationRecorder: NSObject, UNUserNotificationCenterDelegate {
    static let shared = ForegroundPresentationRecorder()

    private let lock = NSLock()
    private var presentedIdentifiers: [String] = []

    var identifiers: [String] {
        lock.lock()
        defer { lock.unlock() }
        return presentedIdentifiers
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        lock.lock()
        presentedIdentifiers.append(notification.request.identifier)
        lock.unlock()
        completionHandler([.banner, .list])
    }
}

/// Each scenario returns string observations; the UI test and the evidence document assert on them.
final class NotificationProbeScenarios {
    static let capacityRequestCount = 100
    static let closedAppIdentifier = "probe.closed"
    static let closedAppIntervalSeconds: TimeInterval = 5

    private let center = UNUserNotificationCenter.current()
    private let authorizationOptions: UNAuthorizationOptions = [.alert, .sound, .badge]

    func run(_ scenario: ProbeScenario) async throws -> [String: String] {
        switch scenario {
        case .settings: return await settingsObservations()
        case .requestAuthorization: return try await requestAuthorization()
        case .schedulingProbe: return await schedulingProbe()
        case .duplicateIdentifier: return try await duplicateIdentifier()
        case .cancelByIdentifier: return try await cancelByIdentifier()
        case .calendarTrigger: return await calendarTrigger()
        case .capacity: return await capacity()
        case .foregroundDelivery: return try await foregroundDelivery()
        case .scheduleClosedAppDelivery: return try await scheduleClosedAppDelivery()
        case .reportDelivered: return await reportDelivered()
        case .clearDelivered: return await clearDelivered()
        }
    }

    // MARK: Permission

    private func settingsObservations() async -> [String: String] {
        var observations = describe(await center.notificationSettings())
        observations["osVersion"] = ProcessInfo.processInfo.operatingSystemVersionString
        observations["timeZone"] = TimeZone.current.identifier
        return observations
    }

    private func requestAuthorization() async throws -> [String: String] {
        var observations: [String: String] = [:]
        let granted = try await center.requestAuthorization(options: authorizationOptions)
        observations["granted"] = String(granted)
        for (key, value) in describe(await center.notificationSettings()) {
            observations[key] = value
        }
        let secondRequestStart = Date()
        let secondGranted = try await center.requestAuthorization(options: authorizationOptions)
        observations["secondRequestGranted"] = String(secondGranted)
        observations["secondRequestMilliseconds"] = String(Int(Date().timeIntervalSince(secondRequestStart) * 1000))
        return observations
    }

    private func describe(_ settings: UNNotificationSettings) -> [String: String] {
        [
            "authorizationStatus": name(of: settings.authorizationStatus),
            "alertSetting": name(of: settings.alertSetting),
            "soundSetting": name(of: settings.soundSetting),
            "badgeSetting": name(of: settings.badgeSetting),
            "lockScreenSetting": name(of: settings.lockScreenSetting),
            "notificationCenterSetting": name(of: settings.notificationCenterSetting),
            "alertStyle": name(of: settings.alertStyle),
        ]
    }

    private func name(of status: UNAuthorizationStatus) -> String {
        switch status {
        case .notDetermined: return "notDetermined"
        case .denied: return "denied"
        case .authorized: return "authorized"
        case .provisional: return "provisional"
        case .ephemeral: return "ephemeral"
        @unknown default: return "unknown"
        }
    }

    private func name(of setting: UNNotificationSetting) -> String {
        switch setting {
        case .notSupported: return "notSupported"
        case .disabled: return "disabled"
        case .enabled: return "enabled"
        @unknown default: return "unknown"
        }
    }

    private func name(of style: UNAlertStyle) -> String {
        switch style {
        case .none: return "none"
        case .banner: return "banner"
        case .alert: return "alert"
        @unknown default: return "unknown"
        }
    }

    // MARK: Scheduling primitives

    private func schedulingProbe() async -> [String: String] {
        let identifier = "probe.single"
        var observations: [String: String] = [:]
        await clearPending()
        let scheduledAt = Date()
        do {
            try await center.add(intervalRequest(identifier: identifier, title: "Single", interval: 3600))
            observations["addError"] = "none"
        } catch {
            observations["addError"] = describe(error)
        }
        let pending = await waitForPending(timeout: 2) { requests in requests.contains { $0.identifier == identifier } }
        observations["pendingCountAfterAdd"] = String(pending.requests.count)
        observations["pendingContainsRequest"] = String(pending.satisfied)
        if let trigger = pending.requests.first(where: { $0.identifier == identifier })?.trigger
            as? UNTimeIntervalNotificationTrigger {
            observations["intervalTriggerDeltaSeconds"] = delta(
                trigger.nextTriggerDate(), scheduledAt.addingTimeInterval(3600)
            )
        }
        await clearPending()
        return observations
    }

    private func duplicateIdentifier() async throws -> [String: String] {
        let identifier = "probe.duplicate"
        await clearPending()
        try await center.add(intervalRequest(identifier: identifier, title: "First", interval: 3600))
        try await center.add(intervalRequest(identifier: identifier, title: "Second", interval: 7200))
        let pending = await waitForPending(timeout: 2) { requests in requests.contains { $0.identifier == identifier } }
        let matching = pending.requests.filter { $0.identifier == identifier }
        var observations: [String: String] = [
            "pendingTotal": String(pending.requests.count),
            "matchingCount": String(matching.count),
            "matchingTitle": matching.first?.content.title ?? "none",
        ]
        if let trigger = matching.first?.trigger as? UNTimeIntervalNotificationTrigger {
            observations["matchingIntervalSeconds"] = String(Int(trigger.timeInterval))
        }
        await clearPending()
        return observations
    }

    private func cancelByIdentifier() async throws -> [String: String] {
        await clearPending()
        for identifier in ["probe.cancel.a", "probe.cancel.b", "probe.cancel.c"] {
            try await center.add(intervalRequest(identifier: identifier, title: identifier, interval: 3600))
        }
        let beforeRemoval = await waitForPending(timeout: 2) { requests in requests.count == 3 }
        center.removePendingNotificationRequests(withIdentifiers: ["probe.cancel.a"])
        let afterOne = await waitForPending(timeout: 5) { requests in !requests.contains { $0.identifier == "probe.cancel.a" } }
        center.removePendingNotificationRequests(withIdentifiers: ["probe.does.not.exist"])
        let afterUnknown = await waitForPending(timeout: 1) { _ in false }
        center.removeAllPendingNotificationRequests()
        let afterAll = await waitForPending(timeout: 5) { requests in requests.isEmpty }
        return [
            "pendingBeforeRemoval": String(beforeRemoval.requests.count),
            "removedIdentifierGone": String(afterOne.satisfied),
            "removedIdentifierPollAttempts": String(afterOne.attempts),
            "remainingAfterOneRemoval": afterOne.requests.map { $0.identifier }.sorted().joined(separator: ","),
            "remainingAfterUnknownRemoval": String(afterUnknown.requests.count),
            "pendingAfterRemoveAll": String(afterAll.requests.count),
        ]
    }

    // MARK: Due time and time zone

    private func calendarTrigger() async -> [String: String] {
        var observations: [String: String] = [:]
        observations["deviceTimeZone"] = TimeZone.current.identifier
        await clearPending()

        let zones = [("auckland", "Pacific/Auckland"), ("losAngeles", "America/Los_Angeles")]
        for (key, zoneIdentifier) in zones {
            guard let zone = TimeZone(identifier: zoneIdentifier) else { continue }
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = zone
            let future = Date().addingTimeInterval(3 * 86_400)
            var components = calendar.dateComponents([.year, .month, .day], from: future)
            components.hour = 9
            components.minute = 30
            components.second = 0
            components.calendar = calendar
            components.timeZone = zone
            let expected = calendar.date(from: components)
            let nextTrigger = await scheduleAndReadNextTrigger(
                identifier: "probe.calendar.\(key)", components: components
            )
            observations["\(key).localTarget"] = "\(components.year ?? 0)-\(components.month ?? 0)-\(components.day ?? 0) 09:30 \(zoneIdentifier)"
            observations["\(key).expectedUTC"] = format(expected)
            observations["\(key).nextTriggerUTC"] = format(nextTrigger.beforeAdd)
            observations["\(key).pendingNextTriggerUTC"] = format(nextTrigger.fromPending)
            observations["\(key).deltaSeconds"] = delta(nextTrigger.fromPending, expected)
            observations["\(key).addError"] = nextTrigger.addError
        }

        var floating = Calendar.current.dateComponents([.year, .month, .day], from: Date().addingTimeInterval(3 * 86_400))
        floating.hour = 9
        floating.minute = 30
        floating.second = 0
        let floatingExpected = Calendar.current.date(from: floating)
        let floatingTrigger = await scheduleAndReadNextTrigger(identifier: "probe.calendar.floating", components: floating)
        observations["floating.expectedUTC"] = format(floatingExpected)
        observations["floating.nextTriggerUTC"] = format(floatingTrigger.fromPending)
        observations["floating.deltaSeconds"] = delta(floatingTrigger.fromPending, floatingExpected)

        let nextYear = Calendar(identifier: .gregorian).component(.year, from: Date()) + 1
        observations["springForwardGap.nextTriggerUTC"] = format(
            await edgeCaseNextTrigger(key: "gap", year: nextYear, month: 3, weekdayOrdinal: 2, hour: 2, minute: 30)
        )
        observations["fallBackOverlap.nextTriggerUTC"] = format(
            await edgeCaseNextTrigger(key: "overlap", year: nextYear, month: 11, weekdayOrdinal: 1, hour: 1, minute: 30)
        )

        var past = Calendar.current.dateComponents([.year, .month, .day], from: Date())
        past.year = (past.year ?? 2026) - 1
        past.hour = 9
        past.minute = 30
        let pastTrigger = await scheduleAndReadNextTrigger(identifier: "probe.calendar.past", components: past)
        observations["past.addError"] = pastTrigger.addError
        observations["past.nextTriggerUTC"] = format(pastTrigger.beforeAdd)
        observations["past.pendingContainsRequest"] = String(pastTrigger.fromPending != nil)

        await clearPending()
        return observations
    }

    private struct NextTriggerObservation {
        var addError: String
        var beforeAdd: Date?
        var fromPending: Date?
    }

    private func scheduleAndReadNextTrigger(identifier: String, components: DateComponents) async -> NextTriggerObservation {
        let trigger = UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
        var observation = NextTriggerObservation(addError: "none", beforeAdd: trigger.nextTriggerDate(), fromPending: nil)
        let request = UNNotificationRequest(identifier: identifier, content: content(title: identifier), trigger: trigger)
        do {
            try await center.add(request)
        } catch {
            observation.addError = describe(error)
        }
        let pending = await waitForPending(timeout: 2) { requests in requests.contains { $0.identifier == identifier } }
        observation.fromPending = (pending.requests.first { $0.identifier == identifier }?.trigger
            as? UNCalendarNotificationTrigger)?.nextTriggerDate()
        return observation
    }

    private func edgeCaseNextTrigger(key: String, year: Int, month: Int, weekdayOrdinal: Int, hour: Int, minute: Int) async -> Date? {
        guard let zone = TimeZone(identifier: "America/New_York") else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let sunday = DateComponents(calendar: calendar, timeZone: zone, year: year, month: month, weekday: 1, weekdayOrdinal: weekdayOrdinal)
        guard let sundayDate = calendar.date(from: sunday) else { return nil }
        var components = calendar.dateComponents([.year, .month, .day], from: sundayDate)
        components.hour = hour
        components.minute = minute
        components.calendar = calendar
        components.timeZone = zone
        return await scheduleAndReadNextTrigger(identifier: "probe.calendar.\(key)", components: components).fromPending
    }

    // MARK: Capacity

    private func capacity() async -> [String: String] {
        await clearPending()
        let total = Self.capacityRequestCount
        func rank(of index: Int) -> Int { (index * 37) % total }
        func identifier(of index: Int) -> String { String(format: "probe.cap.%03d", index) }

        var addErrorCount = 0
        for index in 0..<total {
            let interval = 7200 + TimeInterval(rank(of: index) * 60)
            do {
                try await center.add(intervalRequest(identifier: identifier(of: index), title: "cap", interval: interval))
            } catch {
                addErrorCount += 1
            }
        }
        _ = await waitForPending(timeout: 2) { _ in false }
        let pending = await waitForPending(timeout: 1) { _ in false }
        let retained = Set(pending.requests.map { $0.identifier })
        let soonest = Set((0..<total).filter { rank(of: $0) < 64 }.map(identifier(of:)))
        let firstAdded = Set((0..<64).map(identifier(of:)))
        let observations: [String: String] = [
            "requestsAdded": String(total),
            "addErrorCount": String(addErrorCount),
            "pendingCount": String(retained.count),
            "retainedAmongSoonest64": String(retained.intersection(soonest).count),
            "retainedAmongFirstAdded64": String(retained.intersection(firstAdded).count),
            "retainedIdentifiers": retained.sorted().joined(separator: ","),
        ]
        await clearPending()
        return observations
    }

    // MARK: Delivery

    private func foregroundDelivery() async throws -> [String: String] {
        let identifier = "probe.foreground"
        try await center.add(intervalRequest(identifier: identifier, title: "Foreground", interval: 2))
        var presented = false
        for _ in 0..<40 {
            if ForegroundPresentationRecorder.shared.identifiers.contains(identifier) {
                presented = true
                break
            }
            try await Task.sleep(nanoseconds: 250_000_000)
        }
        let delivered = await center.deliveredNotifications()
        let pending = await center.pendingNotificationRequests()
        let observations: [String: String] = [
            "willPresentCalled": String(presented),
            "deliveredContainsRequest": String(delivered.contains { $0.request.identifier == identifier }),
            "pendingContainsRequest": String(pending.contains { $0.identifier == identifier }),
        ]
        center.removeDeliveredNotifications(withIdentifiers: [identifier])
        return observations
    }

    private func scheduleClosedAppDelivery() async throws -> [String: String] {
        try await center.add(
            intervalRequest(
                identifier: Self.closedAppIdentifier, title: "Closed app", interval: Self.closedAppIntervalSeconds
            )
        )
        return [
            "scheduledAtUTC": format(Date()),
            "intervalSeconds": String(Int(Self.closedAppIntervalSeconds)),
        ]
    }

    private func reportDelivered() async -> [String: String] {
        let delivered = await center.deliveredNotifications()
        let pending = await center.pendingNotificationRequests()
        let closed = delivered.first { $0.request.identifier == Self.closedAppIdentifier }
        return [
            "reportedAtUTC": format(Date()),
            "deliveredIdentifiers": delivered.map { $0.request.identifier }.sorted().joined(separator: ","),
            "pendingIdentifiers": pending.map { $0.identifier }.sorted().joined(separator: ","),
            "closedAppDeliveredPresent": String(closed != nil),
            "closedAppDeliveredDateUTC": format(closed?.date),
            "closedAppStillPending": String(pending.contains { $0.identifier == Self.closedAppIdentifier }),
            "willPresentCallbackCountSinceLaunch": String(ForegroundPresentationRecorder.shared.identifiers.count),
        ]
    }

    private func clearDelivered() async -> [String: String] {
        center.removeAllDeliveredNotifications()
        for _ in 0..<20 {
            if await center.deliveredNotifications().isEmpty { break }
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        return ["deliveredAfterRemoval": String(await center.deliveredNotifications().count)]
    }

    // MARK: Helpers

    private func content(title: String) -> UNMutableNotificationContent {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = "Synthetic probe notification"
        return content
    }

    private func intervalRequest(identifier: String, title: String, interval: TimeInterval) -> UNNotificationRequest {
        UNNotificationRequest(
            identifier: identifier,
            content: content(title: title),
            trigger: UNTimeIntervalNotificationTrigger(timeInterval: interval, repeats: false)
        )
    }

    private func clearPending() async {
        center.removeAllPendingNotificationRequests()
        _ = await waitForPending(timeout: 5) { requests in requests.isEmpty }
    }

    /// removePending... executes asynchronously, so state is polled instead of read once.
    private func waitForPending(
        timeout: TimeInterval,
        until predicate: ([UNNotificationRequest]) -> Bool
    ) async -> (satisfied: Bool, attempts: Int, requests: [UNNotificationRequest]) {
        let deadline = Date().addingTimeInterval(timeout)
        var attempts = 0
        while true {
            attempts += 1
            let requests = await center.pendingNotificationRequests()
            if predicate(requests) { return (true, attempts, requests) }
            if Date() >= deadline { return (false, attempts, requests) }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
    }

    private func describe(_ error: Error) -> String {
        let nsError = error as NSError
        return "\(nsError.domain)#\(nsError.code)"
    }

    private func format(_ date: Date?) -> String {
        guard let date else { return "nil" }
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }

    private func delta(_ actual: Date?, _ expected: Date?) -> String {
        guard let actual, let expected else { return "nil" }
        return String(Int(actual.timeIntervalSince(expected).rounded()))
    }
}
