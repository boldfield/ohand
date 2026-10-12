import Foundation

/// The optional safety-net alert for a capture whose processing may not finish before background time runs out.
public protocol PendingProcessingAlerting: Sendable {
    /// Installs the single alert, replacing any earlier one. Never asks for permission.
    @discardableResult func arm() async -> PendingProcessingAlertResult
    /// Removes the alert. Idempotent.
    func disarm() async
}

public enum PendingProcessingAlertResult: Equatable, Sendable {
    case armed(dueInstant: Date)
    /// Notification permission is not granted; nothing was installed and nothing was requested.
    case notPermitted
    case failed
}

/// When the alert fires. Bounded on both sides: far enough out that a capture the app finishes right away never
/// shows it, close enough that it still arrives while the capture is fresh.
public struct PendingProcessingAlertPolicy: Equatable, Sendable {
    public static let minimumDelay: TimeInterval = 60
    public static let maximumDelay: TimeInterval = 3600

    public let delay: TimeInterval

    /// `nil` when `delay` is outside `minimumDelay...maximumDelay`.
    public init?(delay: TimeInterval) {
        guard delay >= Self.minimumDelay, delay <= Self.maximumDelay else { return nil }
        self.delay = delay
    }
}

/// The fixed wording of the pending-processing alert. It never uses the reminder wording and says outright that no
/// reminder has been set.
public enum PendingProcessingAlertWording {
    public static let title = "Oh And"
    public static let body = "A capture is saved but not processed yet, so no reminder from it is set. Open Oh And to finish."
}

/// One local notification, one-shot, under one fixed identifier.
///
/// * **Bounded.** The identifier is fixed, so arming again replaces the request: at most one is ever pending, and it
///   fires once. It never repeats and never retries; a failed install is reported and left alone.
/// * **Separately identified.** The identifier is not a `<reminder id>#<generation>` and the payload carries no
///   target, so the bridge's reminder list, the delivery-evidence ingestion and the core's reminder reconciliation
///   never take it for a reminder (the core's capacity pass counts it as a foreign request and leaves it alone).
/// * **Never the reminder.** Its wording and `kind` differ from the generic reminder payload.
public actor PendingProcessingAlert: PendingProcessingAlerting {
    public static let identifier = "ohand.pending-processing"
    public static let kindValue = "pending_processing"

    private let center: NotificationCenterProviding
    private let policy: PendingProcessingAlertPolicy
    private let now: @Sendable () -> Date
    private let effectTimeout: TimeInterval

    public init(
        center: NotificationCenterProviding,
        policy: PendingProcessingAlertPolicy,
        now: @escaping @Sendable () -> Date = { Date() },
        effectTimeout: TimeInterval = 5
    ) {
        self.center = center
        self.policy = policy
        self.now = now
        self.effectTimeout = effectTimeout
    }

    @discardableResult
    public func arm() async -> PendingProcessingAlertResult {
        let center = self.center
        do {
            switch try await BoundedNativeCall.run(timeout: effectTimeout, { await center.authorization() }) {
            case .authorized, .provisional, .ephemeral:
                break
            case .notDetermined, .denied:
                return .notPermitted
            }
            let dueInstant = now().addingTimeInterval(policy.delay)
            let request = NotificationCenterRequest(
                identifier: Self.identifier,
                dueInstant: dueInstant,
                content: NotificationContent(
                    title: PendingProcessingAlertWording.title,
                    body: PendingProcessingAlertWording.body,
                    userInfo: [NotificationContent.kindKey: Self.kindValue]))
            try await BoundedNativeCall.run(timeout: effectTimeout) { try await center.add(request) }
            let listed = try await BoundedNativeCall.run(timeout: effectTimeout) { try await center.pendingRequests() }
            guard listed.contains(where: { $0.identifier == Self.identifier }) else { return .failed }
            return .armed(dueInstant: dueInstant)
        } catch {
            return .failed
        }
    }

    public func disarm() async {
        let center = self.center
        _ = try? await BoundedNativeCall.run(timeout: effectTimeout) {
            await center.removePending(identifiers: [Self.identifier])
        }
    }
}
