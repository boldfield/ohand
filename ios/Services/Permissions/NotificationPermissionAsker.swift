import Foundation
import UserNotifications

/// Shows the OS notification prompt. It is the only place the app asks, and `NotificationPermissionAsker` is the only
/// caller.
public protocol NotificationAuthorizationRequesting: Sendable {
    /// Returns whether the person allowed notifications. A failure counts as not allowed.
    func requestAuthorization() async -> Bool
}

public struct SystemNotificationAuthorizationRequester: NotificationAuthorizationRequesting {
    public init() {}

    public func requestAuthorization() async -> Bool {
        (try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])) ?? false
    }
}

public enum NotificationPermissionAskOutcome: Equatable, Sendable {
    /// The OS had already decided (allowed, denied, or provisional), so nothing was shown. A denial is respected.
    case alreadyDecided(NotificationPermissionState)
    /// The prompt was shown and the person answered; the state is what the OS reports afterwards.
    case asked(NotificationPermissionState)
}

public protocol NotificationPermissionAsking: Sendable {
    /// Shows the OS prompt only if the permission was never requested. Call it only at a feature boundary while the app is
    /// in the foreground: the first reminder that cannot alert, and turning on the daily prompt.
    func askIfNeverRequested() async -> NotificationPermissionAskOutcome
}

/// Asks for notification permission at most once, and never when the OS already holds an answer. Concurrent callers share
/// one prompt.
public actor NotificationPermissionAsker: NotificationPermissionAsking {
    private let center: NotificationCenterProviding
    private let requester: NotificationAuthorizationRequesting
    private var inFlight: Task<NotificationPermissionAskOutcome, Never>?

    public init(center: NotificationCenterProviding, requester: NotificationAuthorizationRequesting) {
        self.center = center
        self.requester = requester
    }

    public func askIfNeverRequested() async -> NotificationPermissionAskOutcome {
        if let inFlight { return await inFlight.value }
        let center = center
        let requester = requester
        // Started before any suspension so that a second caller finds it and joins instead of prompting again.
        let task = Task<NotificationPermissionAskOutcome, Never> {
            let before = NotificationPermissionState(await center.authorization())
            guard before == .notRequested else { return .alreadyDecided(before) }
            _ = await requester.requestAuthorization()
            return .asked(NotificationPermissionState(await center.authorization()))
        }
        inFlight = task
        let outcome = await task.value
        inFlight = nil
        return outcome
    }
}
