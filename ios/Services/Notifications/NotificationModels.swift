import Foundation

/// Error classes shared with the core's normalized effect errors.
public enum NotificationErrorClass: String, Equatable, Sendable {
    case transient
    case permanent
    case unauthorized
    case cancelled
    case unsupported
}

/// A normalized failure of a notification effect. The message is fixed text per code: it never
/// carries identifiers, content, credentials or the text of an underlying system error.
public struct NotificationBridgeError: Error, Equatable, CustomStringConvertible, Sendable {
    public let errorClass: NotificationErrorClass
    public let code: String
    public let message: String

    public var description: String { "\(errorClass.rawValue)/\(code): \(message)" }

    private init(_ errorClass: NotificationErrorClass, _ code: String, _ message: String) {
        self.errorClass = errorClass
        self.code = code
        self.message = message
    }

    public static let invalidPayload = NotificationBridgeError(
        .permanent, "invalid_payload", "the notification payload is not allowed")
    public static let dueTimeInPast = NotificationBridgeError(
        .permanent, "due_time_in_past", "the due time is not in the future")
    public static let permissionDenied = NotificationBridgeError(
        .unauthorized, "permission_denied", "notification permission is not granted")
    public static let previewApprovalRequired = NotificationBridgeError(
        .permanent, "preview_approval_required", "a preview payload needs a valid approval reference")
    public static let previewNotEnabled = NotificationBridgeError(
        .unsupported, "preview_not_enabled", "preview payloads are not enabled in this bridge")
    public static let installNotConfirmed = NotificationBridgeError(
        .transient, "install_not_confirmed", "the system did not keep the scheduled request")
    public static let cancelNotConfirmed = NotificationBridgeError(
        .transient, "cancel_not_confirmed", "the system still holds the cancelled request")
    public static let centerFailed = NotificationBridgeError(
        .transient, "notification_center_failed", "the system notification center reported a failure")
    public static let timedOut = NotificationBridgeError(
        .transient, "timed_out", "the system notification center did not answer in time")
    public static let cancelled = NotificationBridgeError(
        .cancelled, "cancelled", "the notification operation was cancelled")

    /// Maps any thrown error to a normalized one without keeping its text.
    static func normalized(_ error: Error) -> NotificationBridgeError {
        if let bridgeError = error as? NotificationBridgeError { return bridgeError }
        if error is CancellationError { return .cancelled }
        return .centerFailed
    }
}

/// An identifier that conveys no meaning: 1 to 64 ASCII letters, digits, `-`, `_` or `.`.
/// It is the only form in which item, reminder, route and action identifiers enter a payload.
public struct OpaqueIdentifier: Hashable, Sendable, CustomStringConvertible {
    public static let maximumLength = 64

    public let rawValue: String

    public init?(_ candidate: String) {
        let bytes = candidate.utf8
        guard (1...Self.maximumLength).contains(bytes.count) else { return nil }
        guard bytes.allSatisfy({ Self.isAllowed($0) }) else { return nil }
        rawValue = candidate
    }

    private static func isAllowed(_ byte: UInt8) -> Bool {
        switch byte {
        case UInt8(ascii: "a")...UInt8(ascii: "z"),
             UInt8(ascii: "A")...UInt8(ascii: "Z"),
             UInt8(ascii: "0")...UInt8(ascii: "9"),
             UInt8(ascii: "-"), UInt8(ascii: "_"), UInt8(ascii: "."):
            return true
        default:
            return false
        }
    }

    public var description: String { rawValue }
}

/// The core-derived notification identifier, `<reminder id>#<schedule generation>`, used verbatim
/// as the OS request identifier. It is built and parsed only here on the native side, so it
/// round-trips exactly and a retry replaces rather than duplicates.
public struct NotificationIdentifier: Hashable, Sendable, CustomStringConvertible {
    public static let separator: Character = "#"

    public let reminderID: OpaqueIdentifier
    public let scheduleGeneration: Int64

    public init?(reminderID: String, scheduleGeneration: Int64) {
        guard scheduleGeneration >= 0, let validReminderID = OpaqueIdentifier(reminderID) else { return nil }
        self.reminderID = validReminderID
        self.scheduleGeneration = scheduleGeneration
    }

    /// Accepts only the canonical form: exactly one separator and a decimal generation without
    /// sign, padding or leading zeros.
    public init?(rawValue: String) {
        let parts = rawValue.split(separator: Self.separator, omittingEmptySubsequences: false)
        guard parts.count == 2,
              let generation = Int64(parts[1]),
              String(generation) == String(parts[1])
        else { return nil }
        self.init(reminderID: String(parts[0]), scheduleGeneration: generation)
    }

    public var rawValue: String { "\(reminderID.rawValue)\(Self.separator)\(scheduleGeneration)" }

    public var description: String { rawValue }
}

/// The core-derived identifier of one optional daily prompt, `prompt:<local date>` with the date as
/// `YYYY-MM-DD`. It is a kind of its own, never a reminder identifier: `NotificationIdentifier` stays exactly as
/// strict, and a reminder identifier is never accepted here. A prompt carries no item, so the identifier holds
/// nothing but the date.
public struct PromptIdentifier: Hashable, Sendable, CustomStringConvertible {
    public static let prefix = "prompt:"

    public let localDate: String

    public init?(rawValue: String) {
        guard rawValue.hasPrefix(Self.prefix) else { return nil }
        let date = String(rawValue.dropFirst(Self.prefix.count))
        let bytes = Array(date.utf8)
        guard bytes.count == 10, bytes[4] == UInt8(ascii: "-"), bytes[7] == UInt8(ascii: "-") else { return nil }
        let digitPositions = [0, 1, 2, 3, 5, 6, 8, 9]
        guard digitPositions.allSatisfy({ (UInt8(ascii: "0")...UInt8(ascii: "9")).contains(bytes[$0]) }) else {
            return nil
        }
        guard let month = Int(date.dropFirst(5).prefix(2)), (1...12).contains(month),
              let day = Int(date.dropFirst(8)), (1...31).contains(day)
        else { return nil }
        localDate = date
    }

    public var rawValue: String { Self.prefix + localDate }

    public var description: String { rawValue }
}

/// One prompt install from the core: the identifier and the instant it is due. The wording is fixed and carries
/// no item text, count or route.
public struct PromptScheduleRequest: Equatable, Sendable {
    public let identifier: PromptIdentifier
    public let dueInstant: Date

    public init(identifier: PromptIdentifier, dueInstant: Date) {
        self.identifier = identifier
        self.dueInstant = dueInstant
    }
}

/// What the OS holds for a prompt after the bridge scheduled it.
public struct InstalledPrompt: Equatable, Sendable {
    public let identifier: PromptIdentifier
    public let dueInstant: Date
}

/// One pending request that carries a prompt identifier.
public struct PendingPrompt: Equatable, Sendable {
    public let identifier: PromptIdentifier
    public let dueInstant: Date?
}

/// The fixed wording of every optional daily prompt.
public enum PromptNotificationWording {
    public static let title = "Oh And"
    public static let body = "Anything to jot down? Open Oh And."
}

public enum NotificationPayloadKind: String, Equatable, Sendable {
    case generic
    case previewApproved = "preview_approved"
}

/// Core's approval of an item-specific preview: the preview-safe route and policy version.
public struct PreviewApprovalReference: Equatable, Sendable {
    public let routeID: OpaqueIdentifier
    public let policyVersion: Int

    public init(routeID: OpaqueIdentifier, policyVersion: Int) {
        self.routeID = routeID
        self.policyVersion = policyVersion
    }
}

/// One schedule request from the core. Every identifier is already validated by its type, and
/// the only free text a request can carry is `previewText`, which the bridge refuses unless the
/// payload is an approved preview.
public struct NotificationScheduleRequest: Equatable, Sendable {
    public let identifier: NotificationIdentifier
    public let dueInstant: Date
    public let payloadKind: NotificationPayloadKind
    public let opaqueTargetID: OpaqueIdentifier
    public let previewText: String?
    public let previewApproval: PreviewApprovalReference?

    public init(
        identifier: NotificationIdentifier,
        dueInstant: Date,
        payloadKind: NotificationPayloadKind,
        opaqueTargetID: OpaqueIdentifier,
        previewText: String? = nil,
        previewApproval: PreviewApprovalReference? = nil
    ) {
        self.identifier = identifier
        self.dueInstant = dueInstant
        self.payloadKind = payloadKind
        self.opaqueTargetID = opaqueTargetID
        self.previewText = previewText
        self.previewApproval = previewApproval
    }

    public static func generic(
        identifier: NotificationIdentifier,
        dueInstant: Date,
        opaqueTargetID: OpaqueIdentifier
    ) -> NotificationScheduleRequest {
        NotificationScheduleRequest(
            identifier: identifier,
            dueInstant: dueInstant,
            payloadKind: .generic,
            opaqueTargetID: opaqueTargetID
        )
    }
}

/// What the OS holds for a request after the bridge scheduled it.
public struct InstalledNotification: Equatable, Sendable {
    public let identifier: NotificationIdentifier
    public let dueInstant: Date
}

/// One pending request that carries a core-derived identifier.
public struct PendingNotification: Equatable, Sendable {
    public let identifier: NotificationIdentifier
    public let dueInstant: Date?
    public let opaqueTargetID: OpaqueIdentifier?
}

/// The fixed wording of every generic notification. No item text, topic or route name is ever
/// substituted into it.
public enum GenericNotificationWording {
    public static let title = "Oh And"
    public static let body = "You have a reminder. Open Oh And to see it."
}

/// Everything the OS is given to display or to carry back on a tap.
public struct NotificationContent: Equatable, Sendable {
    public static let targetKey = "ohand.target"
    public static let kindKey = "ohand.kind"

    public let title: String
    public let body: String
    public let userInfo: [String: String]
    /// The registered action category, or nil for a notification that offers no actions.
    public var categoryIdentifier: String?

    static func generic(opaqueTargetID: OpaqueIdentifier) -> NotificationContent {
        NotificationContent(
            title: GenericNotificationWording.title,
            body: GenericNotificationWording.body,
            userInfo: [
                targetKey: opaqueTargetID.rawValue,
                kindKey: NotificationPayloadKind.generic.rawValue,
            ],
            categoryIdentifier: NotificationActionIdentifier.reminderCategory
        )
    }
}

extension NotificationContent {
    /// The prompt's payload: fixed wording, no actions, no target. The kind value is the only thing the app
    /// reads back from it.
    static func prompt() -> NotificationContent {
        NotificationContent(
            title: PromptNotificationWording.title,
            body: PromptNotificationWording.body,
            userInfo: [kindKey: promptKindValue],
            categoryIdentifier: nil
        )
    }

    public static let promptKindValue = "prompt"
}

public enum NotificationEventKind: Equatable, Sendable {
    case delivered
    case opened
    case action(OpaqueIdentifier)
}

/// OS evidence keyed by the core identifier. It is delivery evidence only: it never means the
/// person saw, read or acted on the reminder's subject.
public struct NotificationEvent: Equatable, Sendable {
    public let identifier: NotificationIdentifier
    public let kind: NotificationEventKind
    public let opaqueTargetID: OpaqueIdentifier?
    public let occurredAt: Date
}

public enum PromptNotificationEventKind: Equatable, Sendable {
    case delivered
    case opened
}

/// OS evidence that a prompt was delivered or tapped. It is never a reminder event: it is not journaled as a
/// reminder action. Routing a tap to the prompt activation belongs to the prompt service.
public struct PromptNotificationEvent: Equatable, Sendable {
    public let identifier: PromptIdentifier
    public let kind: PromptNotificationEventKind
    public let occurredAt: Date
}
