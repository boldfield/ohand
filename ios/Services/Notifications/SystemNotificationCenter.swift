import Foundation
import UserNotifications

/// `NotificationCenterProviding` over the real `UNUserNotificationCenter`.
public final class SystemNotificationCenter: NotificationCenterProviding, @unchecked Sendable {
    private let center: UNUserNotificationCenter

    public init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    public func authorization() async -> NotificationAuthorization {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined: return .notDetermined
        case .denied: return .denied
        case .authorized: return .authorized
        case .provisional: return .provisional
        case .ephemeral: return .ephemeral
        @unknown default: return .denied
        }
    }

    public func add(_ request: NotificationCenterRequest) async throws {
        do {
            try await center.add(Self.makeRequest(request))
        } catch let error as UNError where error.code == .notificationsNotAllowed {
            throw NotificationBridgeError.permissionDenied
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    public func pendingRequests() async throws -> [NotificationCenterPendingRequest] {
        await center.pendingNotificationRequests().map { request in
            NotificationCenterPendingRequest(
                identifier: request.identifier,
                dueInstant: Self.dueInstant(of: request.trigger),
                userInfo: Self.stringUserInfo(request.content.userInfo)
            )
        }
    }

    public func removePending(identifiers: [String]) async {
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    public func deliveredNotifications() async -> [NotificationCenterDeliveredNotification] {
        await center.deliveredNotifications().map { notification in
            NotificationCenterDeliveredNotification(
                identifier: notification.request.identifier,
                deliveredAt: notification.date,
                userInfo: Self.stringUserInfo(notification.request.content.userInfo)
            )
        }
    }

    /// The due instant is an absolute calendar trigger in UTC, so a later change of the device
    /// time zone cannot move it. It never repeats.
    static func makeRequest(_ request: NotificationCenterRequest) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = request.content.title
        content.body = request.content.body
        content.userInfo = request.content.userInfo
        content.sound = .default

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .current
        var components = calendar.dateComponents(
            [.year, .month, .day, .hour, .minute, .second],
            from: request.dueInstant
        )
        components.timeZone = calendar.timeZone
        let trigger = UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
        return UNNotificationRequest(identifier: request.identifier, content: content, trigger: trigger)
    }

    private static func dueInstant(of trigger: UNNotificationTrigger?) -> Date? {
        if let calendarTrigger = trigger as? UNCalendarNotificationTrigger {
            return calendarTrigger.nextTriggerDate()
        }
        if let intervalTrigger = trigger as? UNTimeIntervalNotificationTrigger {
            return intervalTrigger.nextTriggerDate()
        }
        return nil
    }

    static func stringUserInfo(_ userInfo: [AnyHashable: Any]) -> [String: String] {
        var result: [String: String] = [:]
        for (key, value) in userInfo {
            if let keyText = key as? String, let valueText = value as? String {
                result[keyText] = valueText
            }
        }
        return result
    }
}
