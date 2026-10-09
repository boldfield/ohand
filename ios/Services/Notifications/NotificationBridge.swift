import Foundation

/// Native side of the core's notification effect interface: schedule, cancel, list pending and
/// delivered-evidence ingestion.
///
/// Rust decides what should be installed; this type executes and reports. It transports only
/// validated opaque identifiers and fixed wording. Authenticated reads and user mutations (and
/// therefore any credential) are outside it. Scheduling reads the permission first and the
/// pending list afterwards, because the OS accepts an `add` that it then does not keep.
public final class NotificationBridge: Sendable {
    private let center: NotificationCenterProviding
    private let now: @Sendable () -> Date
    private let ingestor: NotificationEventIngestor
    private let removalPollInterval: TimeInterval
    private let maximumRemovalPolls: Int

    public init(
        center: NotificationCenterProviding,
        ingestor: NotificationEventIngestor,
        now: @escaping @Sendable () -> Date = { Date() },
        removalPollInterval: TimeInterval = 0.05,
        maximumRemovalPolls: Int = 20
    ) {
        self.center = center
        self.ingestor = ingestor
        self.now = now
        self.removalPollInterval = removalPollInterval
        self.maximumRemovalPolls = max(1, maximumRemovalPolls)
    }

    /// Installs the request, replacing any pending request with the same identifier, and returns
    /// what the OS reports pending afterwards.
    public func schedule(_ request: NotificationScheduleRequest) async throws -> InstalledNotification {
        do {
            let content = try Self.content(for: request)
            guard request.dueInstant.timeIntervalSince(now()) >= 1 else {
                throw NotificationBridgeError.dueTimeInPast
            }
            switch await center.authorization() {
            case .authorized, .provisional, .ephemeral:
                break
            case .notDetermined, .denied:
                throw NotificationBridgeError.permissionDenied
            }
            try Task.checkCancellation()

            let identifier = request.identifier.rawValue
            try await center.add(
                NotificationCenterRequest(identifier: identifier, dueInstant: request.dueInstant, content: content)
            )
            let pending = try await center.pendingRequests()
            guard let installed = pending.first(where: { $0.identifier == identifier }) else {
                throw NotificationBridgeError.installNotConfirmed
            }
            return InstalledNotification(
                identifier: request.identifier,
                dueInstant: installed.dueInstant ?? request.dueInstant
            )
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Removes the pending request and waits until the OS no longer lists it, because removal is
    /// asynchronous. Idempotent: cancelling an identifier that is not pending succeeds.
    public func cancel(_ identifier: NotificationIdentifier) async throws {
        do {
            await center.removePending(identifiers: [identifier.rawValue])
            for attempt in 0..<maximumRemovalPolls {
                try Task.checkCancellation()
                let pending = try await center.pendingRequests()
                if !pending.contains(where: { $0.identifier == identifier.rawValue }) { return }
                if attempt + 1 < maximumRemovalPolls {
                    try await Task.sleep(nanoseconds: UInt64(removalPollInterval * 1_000_000_000))
                }
            }
            throw NotificationBridgeError.cancelNotConfirmed
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Pending requests that carry a core-derived identifier, soonest first. Requests with any
    /// other identifier are not ours to report.
    public func pendingNotifications() async throws -> [PendingNotification] {
        let pending: [NotificationCenterPendingRequest]
        do {
            pending = try await center.pendingRequests()
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
        return pending.compactMap { request -> PendingNotification? in
            guard let identifier = NotificationIdentifier(rawValue: request.identifier) else { return nil }
            return PendingNotification(
                identifier: identifier,
                dueInstant: request.dueInstant,
                opaqueTargetID: request.userInfo[NotificationContent.targetKey].flatMap { OpaqueIdentifier($0) }
            )
        }
        .sorted { left, right in
            switch (left.dueInstant, right.dueInstant) {
            case let (leftDue?, rightDue?) where leftDue != rightDue:
                return leftDue < rightDue
            case (nil, _?):
                return false
            case (_?, nil):
                return true
            default:
                return left.identifier.rawValue < right.identifier.rawValue
            }
        }
    }

    /// Reports every notification the OS still lists as delivered, for the launch-time check of
    /// requests that fired while the app was not running. Returns how many events were forwarded.
    @discardableResult
    public func ingestDeliveredNotifications() async -> Int {
        var forwarded = 0
        for delivered in await center.deliveredNotifications() {
            let accepted = ingestor.ingestDelivered(
                requestIdentifier: delivered.identifier,
                userInfo: delivered.userInfo,
                deliveredAt: delivered.deliveredAt
            )
            if accepted { forwarded += 1 }
        }
        return forwarded
    }

    /// The only place a payload is built. Generic payloads get fixed wording plus the opaque
    /// target; a generic request that carries preview text or an approval is refused rather than
    /// silently stripped. Preview payloads are not enabled in this bridge.
    static func content(for request: NotificationScheduleRequest) throws -> NotificationContent {
        switch request.payloadKind {
        case .generic:
            guard request.previewText == nil, request.previewApproval == nil else {
                throw NotificationBridgeError.invalidPayload
            }
            return NotificationContent.generic(opaqueTargetID: request.opaqueTargetID)
        case .previewApproved:
            guard let text = request.previewText, !text.isEmpty,
                  let approval = request.previewApproval, approval.policyVersion > 0
            else {
                throw NotificationBridgeError.previewApprovalRequired
            }
            throw NotificationBridgeError.previewNotEnabled
        }
    }
}
