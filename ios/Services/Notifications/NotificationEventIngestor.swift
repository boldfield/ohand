import Foundation
import UserNotifications

public typealias NotificationEventHandler = @Sendable (NotificationEvent) -> Void

/// Turns OS notification facts into events keyed by the core identifier and passes them on.
/// A fact whose request identifier is not core-derived, or whose action is not an opaque
/// identifier, is dropped: the bridge never forwards anything it cannot validate.
///
/// The same fact can arrive again (for example the delivered list read on every launch), so
/// the receiver must treat events as idempotent evidence.
public struct NotificationEventIngestor: Sendable {
    private let now: @Sendable () -> Date
    private let handler: NotificationEventHandler

    public init(now: @escaping @Sendable () -> Date = { Date() }, handler: @escaping NotificationEventHandler) {
        self.now = now
        self.handler = handler
    }

    @discardableResult
    public func ingestDelivered(
        requestIdentifier: String,
        userInfo: [String: String],
        deliveredAt: Date? = nil
    ) -> Bool {
        forward(
            requestIdentifier: requestIdentifier,
            kind: .delivered,
            userInfo: userInfo,
            occurredAt: deliveredAt ?? now()
        )
    }

    /// A tap on the notification (the default action) is `opened`; any other action identifier
    /// that is a valid opaque identifier is passed through as `action` for the action handler to
    /// interpret.
    @discardableResult
    public func ingestResponse(
        requestIdentifier: String,
        actionIdentifier: String,
        userInfo: [String: String]
    ) -> Bool {
        let kind: NotificationEventKind
        if actionIdentifier == UNNotificationDefaultActionIdentifier {
            kind = .opened
        } else if let action = OpaqueIdentifier(actionIdentifier) {
            kind = .action(action)
        } else {
            return false
        }
        return forward(requestIdentifier: requestIdentifier, kind: kind, userInfo: userInfo, occurredAt: now())
    }

    private func forward(
        requestIdentifier: String,
        kind: NotificationEventKind,
        userInfo: [String: String],
        occurredAt: Date
    ) -> Bool {
        guard let identifier = NotificationIdentifier(rawValue: requestIdentifier) else { return false }
        let target = userInfo[NotificationContent.targetKey].flatMap { OpaqueIdentifier($0) }
        handler(NotificationEvent(
            identifier: identifier,
            kind: kind,
            opaqueTargetID: target,
            occurredAt: occurredAt
        ))
        return true
    }
}

/// Receives the OS delegate callbacks and hands them to the ingestor. The composition root
/// installs it (or forwards to it from its own delegate); this type never installs itself, so it
/// does not compete with the action handler for the delegate slot.
public final class NotificationEventReceiver: NSObject, UNUserNotificationCenterDelegate {
    private let ingestor: NotificationEventIngestor
    private let foregroundPresentation: UNNotificationPresentationOptions

    public init(
        ingestor: NotificationEventIngestor,
        foregroundPresentation: UNNotificationPresentationOptions = [.banner, .list]
    ) {
        self.ingestor = ingestor
        self.foregroundPresentation = foregroundPresentation
        super.init()
    }

    public func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        ingestor.ingestDelivered(
            requestIdentifier: notification.request.identifier,
            userInfo: SystemNotificationCenter.stringUserInfo(notification.request.content.userInfo),
            deliveredAt: notification.date
        )
        completionHandler(foregroundPresentation)
    }

    public func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        ingestor.ingestResponse(
            requestIdentifier: response.notification.request.identifier,
            actionIdentifier: response.actionIdentifier,
            userInfo: SystemNotificationCenter.stringUserInfo(response.notification.request.content.userInfo)
        )
        completionHandler()
    }
}
