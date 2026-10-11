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

    static func generic(opaqueTargetID: OpaqueIdentifier) -> NotificationContent {
        NotificationContent(
            title: GenericNotificationWording.title,
            body: GenericNotificationWording.body,
            userInfo: [
                targetKey: opaqueTargetID.rawValue,
                kindKey: NotificationPayloadKind.generic.rawValue,
            ]
        )
    }
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
