import Foundation
import OhAndCoreBridge

/// What the user asked to export. Scopes are explicit (there is no default scope) and audio is opt-in.
public struct ExportSelection: Equatable {
    public var scopes: [CoreExportScope]
    public var includeAudio: Bool

    public init(scopes: [CoreExportScope], includeAudio: Bool = false) {
        self.scopes = scopes
        self.includeAudio = includeAudio
    }
}

/// Why an item's audio file is not part of the share. The export file still names the audio, so the recipient is
/// never told it holds audio that was in fact left out.
public enum ExportAudioUnavailableReason: Equatable {
    /// No file exists any more: the audio expired, was deleted, or was never finalized.
    case fileMissing
    /// The core's audio reference does not name a file inside the finalized audio store that is bound to the capture.
    case referenceUnresolvable
    /// The file exists but could not be copied into the protected share location.
    case copyFailed
}

public struct ExportUnavailableAudio: Equatable {
    public let itemID: String
    public let captureID: String
    public let reason: ExportAudioUnavailableReason

    public init(itemID: String, captureID: String, reason: ExportAudioUnavailableReason) {
        self.itemID = itemID
        self.captureID = captureID
        self.reason = reason
    }
}

/// The audio disposition of one export. `requested` is false when the user did not ask for audio; then nothing was
/// copied and nothing is unavailable.
public struct ExportAudioReport: Equatable {
    public let requested: Bool
    /// Capture identifiers whose audio file is in the share.
    public let includedCaptureIDs: [String]
    public let unavailable: [ExportUnavailableAudio]

    public init(requested: Bool, includedCaptureIDs: [String], unavailable: [ExportUnavailableAudio]) {
        self.requested = requested
        self.includedCaptureIDs = includedCaptureIDs
        self.unavailable = unavailable
    }
}

/// An export that is ready to hand to the share sheet. It is not a completed share: success is only known from the
/// share sheet's own completion (`ExportService.finishShare`).
public struct PreparedExport: Equatable {
    public let id: String
    public let exportFile: URL
    public let audioFiles: [URL]
    public let itemCount: Int
    public let scopes: [CoreExportScope]
    public let audio: ExportAudioReport

    /// The files to share: the export file first, then each copied audio file.
    public var shareItems: [URL] { [exportFile] + audioFiles }
}

public enum ExportFailure: Error, Equatable {
    case noScopeSelected
    /// The session is capture-only, so nothing was read.
    case notAuthenticated
    /// The session relocked or was superseded before the export could be handed over; its files were removed.
    case relocked
    case cancelled
    /// The protected temporary location could not be prepared, so nothing is shared from an unprotected place.
    case storageUnavailable
    /// The core refused or failed; the failure is normalized and content-free.
    case core(CoreFailure)
}

/// How a share ended, as reported to the caller.
public enum ExportShareResult: Equatable {
    /// The share sheet reported that an activity completed.
    case shared
    /// The user dismissed the share sheet without choosing an activity.
    case cancelled
    /// The share sheet reported an error.
    case failed
    /// The export was invalidated (relock, interruption or an unknown export) before a completion could be trusted,
    /// so success is not claimed.
    case interrupted
}

public struct ExportShareOutcome: Equatable {
    public let result: ExportShareResult
    /// False when a temporary file could not be removed. The next launch sweep retries it.
    public let temporariesRemoved: Bool

    public init(result: ExportShareResult, temporariesRemoved: Bool) {
        self.result = result
        self.temporariesRemoved = temporariesRemoved
    }
}

public struct ExportSweepReport: Equatable {
    public let removedCount: Int
    public let failedCount: Int

    public init(removedCount: Int, failedCount: Int) {
        self.removedCount = removedCount
        self.failedCount = failedCount
    }
}
