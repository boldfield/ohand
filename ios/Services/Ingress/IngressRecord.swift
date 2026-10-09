import Foundation

/// Entry context and routing captured with an ingress record. These are the fields the core's import validates; the
/// route and scope come from the user's configured default and are never derived from a session.
struct IngressCaptureContext: Codable, Equatable {
    var captureInstant: String
    var timezoneID: String
    var utcOffsetMinutes: Int
    var locale: String
    var calendar: String
    var itemScope: String
    var routeID: String
    var entryLocked: Bool
    var createdAt: String
    var sessionTopic: String?
}

/// A finished recording handed to ingress. The file starts in the in-progress audio store and is moved, never copied,
/// to the finalized audio store, so exactly one file holds the audio at any time.
struct IngressAudioHandoff: Codable, Equatable {
    var inProgressFileName: String
    var finalizedFileName: String
}

enum IngressRecordProblem: Error, Equatable {
    case unsafeCaptureID
    case unsafeFileName
    /// The finalized audio name must start with `<captureID>.`, so two captures can never share a finalized file.
    case finalizedNameNotBoundToCapture
    case exactlyOneSourceRequired
    case emptyText
}

/// The durable native ingress record, written before anything is acknowledged and removed only after the core
/// confirms the import. It carries the capture ID, which is the idempotency key for every retry.
struct IngressRecord: Codable, Equatable {
    static let currentFormatVersion = 1
    static let fileExtension = "json"

    var formatVersion: Int
    var captureID: String
    var text: String?
    var audio: IngressAudioHandoff?
    var context: IngressCaptureContext

    init(captureID: String, text: String?, audio: IngressAudioHandoff?, context: IngressCaptureContext) {
        self.formatVersion = Self.currentFormatVersion
        self.captureID = captureID
        self.text = text
        self.audio = audio
        self.context = context
    }

    /// The reference stored in the core for audio-only captures: relative to the protected storage root, so it stays
    /// valid when the app container moves, and always inside the finalized audio store.
    var coreAudioReference: String? {
        audio.map { Self.audioReference(finalizedFileName: $0.finalizedFileName) }
    }

    static func audioReference(finalizedFileName: String) -> String {
        "\(StoreProtectionPolicy.policy(for: .finalizedAudio).directoryName)/\(finalizedFileName)"
    }

    /// Letters, digits, `-` and `_` only: a capture ID names a file, so it must not be able to name a path.
    static func isSafeCaptureID(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 128
            && value.unicodeScalars.allSatisfy { isIdentifierScalar($0) }
    }

    static func isSafeFileName(_ value: String) -> Bool {
        guard !value.isEmpty, value.utf8.count <= 200, !value.hasPrefix("."), !value.contains("..") else {
            return false
        }
        return value.unicodeScalars.allSatisfy { isIdentifierScalar($0) || $0 == "." }
    }

    private static func isIdentifierScalar(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar {
        case "a"..."z", "A"..."Z", "0"..."9", "-", "_":
            return true
        default:
            return false
        }
    }

    func validate() throws {
        guard Self.isSafeCaptureID(captureID) else { throw IngressRecordProblem.unsafeCaptureID }
        switch (text, audio) {
        case let (text?, nil):
            if text.isEmpty { throw IngressRecordProblem.emptyText }
        case let (nil, audio?):
            guard Self.isSafeFileName(audio.inProgressFileName), Self.isSafeFileName(audio.finalizedFileName) else {
                throw IngressRecordProblem.unsafeFileName
            }
            guard audio.finalizedFileName.hasPrefix(captureID + ".") else {
                throw IngressRecordProblem.finalizedNameNotBoundToCapture
            }
        default:
            throw IngressRecordProblem.exactlyOneSourceRequired
        }
    }
}
