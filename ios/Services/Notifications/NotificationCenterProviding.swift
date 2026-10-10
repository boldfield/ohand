import Foundation

public enum NotificationAuthorization: Equatable, Sendable {
    case notDetermined
    case denied
    case authorized
    case provisional
    case ephemeral
}

/// A request in the exact form handed to the OS. Tests inspect it to prove what a payload holds.
public struct NotificationCenterRequest: Equatable, Sendable {
    public let identifier: String
    public let dueInstant: Date
    public let content: NotificationContent
}

public struct NotificationCenterPendingRequest: Equatable, Sendable {
    public let identifier: String
    public let dueInstant: Date?
    public let userInfo: [String: String]

    public init(identifier: String, dueInstant: Date?, userInfo: [String: String]) {
        self.identifier = identifier
        self.dueInstant = dueInstant
        self.userInfo = userInfo
    }
}

public struct NotificationCenterDeliveredNotification: Equatable, Sendable {
    public let identifier: String
    public let deliveredAt: Date
    public let userInfo: [String: String]

    public init(identifier: String, deliveredAt: Date, userInfo: [String: String]) {
        self.identifier = identifier
        self.deliveredAt = deliveredAt
        self.userInfo = userInfo
    }
}

/// The OS notification primitives the bridge uses. `add` with an identifier already pending
/// replaces that request; removing an identifier that is not pending does nothing.
public protocol NotificationCenterProviding: Sendable {
    func authorization() async -> NotificationAuthorization
    func add(_ request: NotificationCenterRequest) async throws
    func pendingRequests() async throws -> [NotificationCenterPendingRequest]
    func removePending(identifiers: [String]) async
    func deliveredNotifications() async -> [NotificationCenterDeliveredNotification]
}
