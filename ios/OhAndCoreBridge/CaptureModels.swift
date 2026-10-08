import Foundation

/// A capture as it crosses the native boundary. `createdAt` is the idempotency timestamp: a
/// retry of the same save must send the same value, or the core reports `capture_conflict`.
public struct CaptureRecord: Codable, Equatable, Sendable {
    public var captureID: String
    public var text: String?
    public var audioReference: String?
    public var captureInstant: String
    public var timezoneID: String
    public var utcOffsetMinutes: Int
    public var locale: String
    public var calendar: String
    public var itemScope: String
    public var routeID: String
    public var entryLocked: Bool
    public var createdAt: String
    public var sessionTopic: String?

    public init(
        captureID: String,
        text: String? = nil,
        audioReference: String? = nil,
        captureInstant: String,
        timezoneID: String,
        utcOffsetMinutes: Int,
        locale: String,
        calendar: String,
        itemScope: String,
        routeID: String,
        entryLocked: Bool,
        createdAt: String,
        sessionTopic: String? = nil
    ) {
        self.captureID = captureID
        self.text = text
        self.audioReference = audioReference
        self.captureInstant = captureInstant
        self.timezoneID = timezoneID
        self.utcOffsetMinutes = utcOffsetMinutes
        self.locale = locale
        self.calendar = calendar
        self.itemScope = itemScope
        self.routeID = routeID
        self.entryLocked = entryLocked
        self.createdAt = createdAt
        self.sessionTopic = sessionTopic
    }

    enum CodingKeys: String, CodingKey {
        case captureID = "capture_id"
        case text
        case audioReference = "audio_reference"
        case captureInstant = "capture_instant"
        case timezoneID = "timezone_id"
        case utcOffsetMinutes = "utc_offset_minutes"
        case locale
        case calendar
        case itemScope = "item_scope"
        case routeID = "route_id"
        case entryLocked = "entry_locked"
        case createdAt = "created_at"
        case sessionTopic = "session_topic"
    }
}

/// Delivered only after the core committed the save. `alreadySaved` is true when the identical
/// capture was stored before, so a retry converges instead of duplicating.
public struct SaveCaptureAcknowledgment: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let alreadySaved: Bool
    public let capture: CaptureRecord

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case alreadySaved = "already_saved"
        case capture
    }
}

public struct CaptureReadout: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let capture: CaptureRecord

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case capture
    }
}

/// Independent facts about one item. State values are the core's own strings (for example
/// `saved_local`, `unprocessed`, `not_scheduled`); reminder facts are `nil` until a reminder
/// exists, never a default.
public struct ItemStatusReport: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let itemID: String
    public let saveState: String
    public let syncState: String
    public let processingState: String
    public let transcriptionState: String
    public let processingJobStatus: String?
    public let reminderRequestState: String?
    public let reminderScheduleState: String?
    public let reminderDeliveryState: String?
    public let reminderAcknowledgmentState: String?
    public let unschedulableReason: String?

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case itemID = "item_id"
        case saveState = "save_state"
        case syncState = "sync_state"
        case processingState = "processing_state"
        case transcriptionState = "transcription_state"
        case processingJobStatus = "processing_job_status"
        case reminderRequestState = "reminder_request_state"
        case reminderScheduleState = "reminder_schedule_state"
        case reminderDeliveryState = "reminder_delivery_state"
        case reminderAcknowledgmentState = "reminder_acknowledgment_state"
        case unschedulableReason = "unschedulable_reason"
    }
}
