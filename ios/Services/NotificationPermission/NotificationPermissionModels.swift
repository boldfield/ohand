import Foundation

/// What the OS permission allows, reduced to what reminders need. A provisional or ephemeral grant
/// still lets the OS accept and deliver requests (quietly), so it counts as usable.
public enum NotificationPermissionState: Equatable, Sendable {
    case usable
    case notRequested
    case denied

    public init(_ authorization: NotificationAuthorization) {
        switch authorization {
        case .authorized, .provisional, .ephemeral: self = .usable
        case .notDetermined: self = .notRequested
        case .denied: self = .denied
        }
    }

    public var canAlert: Bool { self == .usable }
}

/// How the permission moved since the last pass the core fully took in.
public enum NotificationPermissionTransition: Equatable, Sendable {
    case firstObservation
    case unchanged
    case regained
    case lost
    /// Between the two states that both prevent alerting (never asked, denied).
    case unusableChanged

    public static func between(
        previous: NotificationPermissionState?,
        current: NotificationPermissionState
    ) -> NotificationPermissionTransition {
        guard let previous else { return .firstObservation }
        if previous == current { return .unchanged }
        if current.canAlert { return .regained }
        if previous.canAlert { return .lost }
        return .unusableChanged
    }
}

/// The supported events on which the permission is read again. There is no timer: the passage of
/// time alone never triggers, or substitutes for, a pass.
public enum NotificationReconcileTrigger: Equatable, Sendable {
    case launch
    case foreground
    case settingsReturn
    case explicit
}

/// The permission the last completed pass saw, kept so that a loss or regain is noticed even when
/// it happened while the app was not running. It holds no content.
public protocol NotificationPermissionObservationStore: Sendable {
    func lastObserved() -> NotificationPermissionState?
    func record(_ state: NotificationPermissionState)
}

public final class InMemoryNotificationPermissionObservationStore: NotificationPermissionObservationStore, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: NotificationPermissionState?

    public init(initial: NotificationPermissionState? = nil) {
        stored = initial
    }

    public func lastObserved() -> NotificationPermissionState? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    public func record(_ state: NotificationPermissionState) {
        lock.lock()
        defer { lock.unlock() }
        stored = state
    }
}

public final class UserDefaultsNotificationPermissionObservationStore: NotificationPermissionObservationStore, @unchecked Sendable {
    private static let key = "com.boldfield.ohand.notification-permission.last-observed"
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func lastObserved() -> NotificationPermissionState? {
        switch defaults.string(forKey: Self.key) {
        case "usable": return .usable
        case "not_requested": return .notRequested
        case "denied": return .denied
        default: return nil
        }
    }

    public func record(_ state: NotificationPermissionState) {
        let value: String
        switch state {
        case .usable: value = "usable"
        case .notRequested: value = "not_requested"
        case .denied: value = "denied"
        }
        defaults.set(value, forKey: Self.key)
    }
}

/// What the core stores about one reminder, as its status report states it. Delivery and
/// acknowledgment are carried through verbatim: this module never derives them.
public struct ReminderFacts: Equatable, Sendable {
    public let itemID: String
    public let requestState: String
    public let scheduleState: String?
    public let deliveryState: String?
    public let acknowledgmentState: String?
    public let unschedulableReason: String?

    public init(
        itemID: String,
        requestState: String,
        scheduleState: String?,
        deliveryState: String?,
        acknowledgmentState: String?,
        unschedulableReason: String?
    ) {
        self.itemID = itemID
        self.requestState = requestState
        self.scheduleState = scheduleState
        self.deliveryState = deliveryState
        self.acknowledgmentState = acknowledgmentState
        self.unschedulableReason = unschedulableReason
    }
}

/// An optional prompt that wants an OS slot. The core grants it only after every explicit
/// reminder, within its prompt limit and horizon.
public struct OptionalPromptCandidate: Equatable, Sendable {
    public let notificationID: String
    public let fireAt: Date

    public init(notificationID: String, fireAt: Date) {
        self.notificationID = notificationID
        self.fireAt = fireAt
    }
}

/// The input of one core pass after a permission read. The pass is the core's existing capacity
/// and reconciliation contract (reminders before optional prompts, stable identifiers); this
/// module only says what the permission now is.
public struct ReminderReconciliationRequest: Equatable, Sendable {
    public let permission: NotificationPermissionState
    public let transition: NotificationPermissionTransition
    public let trigger: NotificationReconcileTrigger
    /// Re-arm schedules the OS rejected as unauthorized before the pass, so that they are
    /// attempted again. Set only when the permission is usable and was not before.
    public let rearmRejectedSchedules: Bool
    /// Whether optional prompts may be given capacity. Explicit reminders never depend on it.
    public let allowOptionalPrompts: Bool
    public let promptCandidates: [OptionalPromptCandidate]
}

public struct ReminderReconciliationOutcome: Equatable, Sendable {
    public let reminders: [ReminderFacts]
    /// Prompt candidates the core gave capacity to. Installing them belongs to the prompt scheduler.
    public let promptsGranted: Int
    /// Prompt candidates the core could not fit, which a later pass considers again.
    public let promptsDeferred: Int
    /// Prompt candidates held back from the pass because the permission cannot alert.
    public let promptsWithheld: Int
    public let converged: Bool

    public init(
        reminders: [ReminderFacts],
        promptsGranted: Int,
        promptsDeferred: Int,
        promptsWithheld: Int = 0,
        converged: Bool
    ) {
        self.reminders = reminders
        self.promptsGranted = promptsGranted
        self.promptsDeferred = promptsDeferred
        self.promptsWithheld = promptsWithheld
        self.converged = converged
    }
}

/// The core's reconciliation, injected. `CoreReminderReconciler` is the production adapter: it runs the
/// core's capacity pass and reconciliation against the native bridge and returns the reminders' stored
/// facts afterwards. A pass is idempotent: repeating it with the same request changes nothing and installs
/// nothing twice.
public protocol ReminderReconciling: Sendable {
    func reconcile(_ request: ReminderReconciliationRequest) async throws -> ReminderReconciliationOutcome

    /// The reminders' stored facts without a pass, so status stays readable while a pass cannot run.
    func storedFacts() async throws -> [ReminderFacts]
}
