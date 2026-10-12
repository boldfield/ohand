import Foundation
import OhAndCoreBridge

/// What the user asked to export. Scopes are explicit (there is no default scope) and audio is opt-in.
struct ExportSelection: Equatable {
    var scopes: [CoreExportScope]
    var includeAudio: Bool

    init(scopes: [CoreExportScope], includeAudio: Bool = false) {
        self.scopes = scopes
        self.includeAudio = includeAudio
    }
}

/// Why an item's audio file is not part of the share. The export file still names the audio, so the recipient is
/// never told it holds audio that was in fact left out.
enum ExportAudioUnavailableReason: Equatable {
    /// No file exists any more: the audio expired, was deleted, or was never finalized.
    case fileMissing
    /// The core's audio reference does not name a file inside the finalized audio store that is bound to the capture.
    case referenceUnresolvable
    /// The file exists but could not be copied into the protected share location.
    case copyFailed
}

struct ExportUnavailableAudio: Equatable {
    let itemID: String
    let captureID: String
    let reason: ExportAudioUnavailableReason
}

/// The audio disposition of one export. `requested` is false when the user did not ask for audio; then nothing was
/// copied and nothing is unavailable.
struct ExportAudioReport: Equatable {
    let requested: Bool
    /// Capture identifiers whose audio file is in the share.
    let includedCaptureIDs: [String]
    let unavailable: [ExportUnavailableAudio]
}

/// An export that is ready to hand to the share sheet. It is not a completed share: success is only known from the
/// share sheet's own completion (`ExportService.finishShare`).
struct PreparedExport: Equatable {
    let id: String
    let exportFile: URL
    let audioFiles: [URL]
    let itemCount: Int
    let scopes: [CoreExportScope]
    let audio: ExportAudioReport

    /// The files to share: the export file first, then each copied audio file.
    var shareItems: [URL] { [exportFile] + audioFiles }
}

enum ExportFailure: Error, Equatable {
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
enum ExportShareResult: Equatable {
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

struct ExportShareOutcome: Equatable {
    let result: ExportShareResult
    /// False when a temporary file could not be removed. The next launch sweep retries it.
    let temporariesRemoved: Bool
}

struct ExportSweepReport: Equatable {
    let removedCount: Int
    let failedCount: Int
}
