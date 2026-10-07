import Foundation

public struct OhAndCore {
    public init() {}

    public static let version = "0.1.0"

    public func placeholder() -> String {
        "Core framework placeholder for M1"
    }
}

// MARK: - Capture Bridge

/// Swift-friendly wrapper around Rust Capture FFI types.
public struct Capture {
    public let captureId: String
    public let text: String?
    public let audioReference: String?
    public let captureInstant: String
    public let timezoneId: String
    public let utcOffsetMinutes: Int32
    public let locale: String
    public let calendar: String
    public let itemScope: String
    public let routeId: String
    public let entryLocked: Bool
    public let createdAt: String
    public let sessionTopic: String?

    init(cCapture: OhAndCapture) {
        self.captureId = String(cString: cCapture.capture_id)
        self.text = cCapture.text != nil ? String(cString: cCapture.text) : nil
        self.audioReference = cCapture.audio_reference != nil ? String(cString: cCapture.audio_reference) : nil
        self.captureInstant = String(cString: cCapture.capture_instant)
        self.timezoneId = String(cString: cCapture.timezone_id)
        self.utcOffsetMinutes = cCapture.utc_offset_minutes
        self.locale = String(cString: cCapture.locale)
        self.calendar = String(cString: cCapture.calendar)
        self.itemScope = String(cString: cCapture.item_scope)
        self.routeId = String(cString: cCapture.route_id)
        self.entryLocked = cCapture.entry_locked != 0
        self.createdAt = String(cString: cCapture.created_at)
        self.sessionTopic = cCapture.session_topic != nil ? String(cString: cCapture.session_topic) : nil
    }
}

/// Errors that can occur when creating or manipulating captures.
public enum CaptureError: Error, Equatable {
    case invalidCapture(String)
    case ffiError(String)

    public static func == (lhs: CaptureError, rhs: CaptureError) -> Bool {
        switch (lhs, rhs) {
        case let (.invalidCapture(lMsg), .invalidCapture(rMsg)):
            return lMsg == rMsg
        case let (.ffiError(lMsg), .ffiError(rMsg)):
            return lMsg == rMsg
        default:
            return false
        }
    }
}

/// Create a new capture from Swift values.
public func createCapture(
    captureId: String,
    text: String? = nil,
    audioReference: String? = nil,
    captureInstant: String,
    timezoneId: String,
    utcOffsetMinutes: Int32,
    locale: String,
    calendar: String,
    itemScope: String,
    routeId: String,
    entryLocked: Bool = false,
    createdAt: String,
    sessionTopic: String? = nil
) throws -> Capture {
    var error: UnsafeMutablePointer<OhAndError>? = nil

    let cCapture = captureId.withCString { captureIdPtr in
        (text ?? "").withCString { textPtr in
            (audioReference ?? "").withCString { audioRefPtr in
                captureInstant.withCString { captureInstantPtr in
                    timezoneId.withCString { timezoneIdPtr in
                        locale.withCString { localePtr in
                            calendar.withCString { calendarPtr in
                                itemScope.withCString { itemScopePtr in
                                    routeId.withCString { routeIdPtr in
                                        createdAt.withCString { createdAtPtr in
                                            (sessionTopic ?? "").withCString { sessionTopicPtr in
                                                ohand_capture_new(
                                                    captureIdPtr,
                                                    text != nil ? textPtr : nil,
                                                    audioReference != nil ? audioRefPtr : nil,
                                                    captureInstantPtr,
                                                    timezoneIdPtr,
                                                    utcOffsetMinutes,
                                                    localePtr,
                                                    calendarPtr,
                                                    itemScopePtr,
                                                    routeIdPtr,
                                                    entryLocked ? 1 : 0,
                                                    createdAtPtr,
                                                    sessionTopic != nil ? sessionTopicPtr : nil,
                                                    &error
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    defer {
        if let err = error {
            ohand_error_free(err)
        }
        if let capture = cCapture {
            ohand_capture_free(capture)
        }
    }

    guard let cCap = cCapture else {
        if let err = error {
            let msg = String(cString: ohand_error_message(err))
            throw CaptureError.ffiError(msg)
        }
        throw CaptureError.invalidCapture("Failed to create capture")
    }

    return Capture(cCapture: cCap.pointee)
}
