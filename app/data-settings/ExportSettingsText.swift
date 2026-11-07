import Foundation
import OhAndCoreBridge
import OhAndServices

/// The words the export section uses. Kept apart from the model so each statement is checked against the case that
/// produces it.
enum ExportSettingsText {
    static let heading = "Export"
    static let explanation =
        "Export writes a copy of the notes in the areas you pick to a file, then opens the share sheet so you can send it "
        + "to an app you choose."
    static let limitsNote =
        "An export is a one-off copy, not a backup. This app cannot restore from it, and it cannot take back or delete a "
        + "copy once another app has it. Device backups made by the system are separate and are not controlled here."
    static let audioNote =
        "Audio is left out unless you turn this on. Recordings that no longer exist on this device cannot be included, "
        + "and the export will say how many were left out."
    static let audioOffFooter = "Audio is left out unless you turn it on."
    static let audioOnFooter = "Recordings still on this device are included."

    /// The one line under the areas: the missing-choice prompt, or what the audio switch currently means.
    static func footer(hasScope: Bool, includeAudio: Bool) -> String {
        guard hasScope else { return noScopeNote }
        return includeAudio ? audioOnFooter : audioOffFooter
    }
    static let noScopeNote = "Pick at least one area to export."
    static let lockedText = "Your saved items are locked. Go back and open Settings again to unlock them."
    static let preparingText = "Preparing the export…"
    static let readyText = "The export is ready. Choose where to send it."

    static func scopeTitle(_ scope: CoreExportScope) -> String {
        switch scope {
        case .personal: return "Personal"
        case .work: return "Work"
        }
    }

    static func itemCount(_ count: Int) -> String { count == 1 ? "1 item" : "\(count) items" }

    /// Said only after the share sheet reported that an activity completed. It does not claim the recipient got it.
    static func shared(_ export: PreparedExport, temporariesRemoved: Bool) -> String {
        var text = "Shared \(itemCount(export.itemCount)). The app you chose now holds a copy that this app cannot recall."
        if let audio = audioSummary(export.audio) { text += " " + audio }
        if !temporariesRemoved { text += " " + leftoverNote }
        return text
    }

    static func cancelled(temporariesRemoved: Bool) -> String {
        let text = "Sharing was cancelled. Nothing was sent."
        return temporariesRemoved ? text : text + " " + leftoverNote
    }

    static func shareFailed(temporariesRemoved: Bool) -> String {
        let text = "The share sheet reported an error, so the export may not have been sent."
        return temporariesRemoved ? text : text + " " + leftoverNote
    }

    static func interrupted(temporariesRemoved: Bool) -> String {
        let text = "The export was interrupted, for example because the app locked, so it is not reported as shared. Export "
            + "again if you still need it."
        return temporariesRemoved ? text : text + " " + leftoverNote
    }

    static let leftoverNote = "Some temporary export files could not be removed yet; they are removed the next time the app starts."

    /// Counts only; never names an item or a capture.
    static func audioSummary(_ audio: ExportAudioReport) -> String? {
        guard audio.requested else { return nil }
        let included = audio.includedCaptureIDs.count
        let left = audio.unavailable.count
        var text = included == 1 ? "1 recording was included." : "\(included) recordings were included."
        if left > 0 {
            text += left == 1
                ? " 1 recording could not be included because its file is not available."
                : " \(left) recordings could not be included because their files are not available."
        }
        return text
    }

    // MARK: Metrics

    static let metricsHeading = "Metrics"
    static let metricsExportTitle = "Export metrics"
    static let metricsPauseTitle = "Pause recording"
    static let metricsResumeTitle = "Resume recording"
    static let metricsClearTitle = "Clear metrics"
    static let metricsHolds =
        "The metrics file holds timings and counts for this build: how long the capture screen took to be ready, how long "
        + "saving took, and how often processing or reminder scheduling failed, with the build's version and revision."
    static let metricsHoldsNot =
        "It holds no note content: no text, audio, titles, times, names or identifiers of your notes, and nothing about "
        + "what you did with them. It is created only when you export, and this app sends it nowhere itself."
    static let metricsRecordingOnFooter = "Recording is on. Pausing keeps what is stored; clearing removes it."
    static let metricsRecordingOffFooter = "Recording is paused. Nothing new is stored until you resume."
    static let metricsLockedText =
        "Metrics are available only after you unlock your saved items. Go back and open Settings again to unlock them."
    static let metricsExportFailed = "The metrics file could not be prepared. Nothing was sent; try again."
    static let metricsActionFailed = "The change could not be made. Try again."
    static let metricsPaused = "Recording is paused. Stored figures are kept."
    static let metricsResumed = "Recording is on."
    static let metricsCleared = "Stored metrics were removed."
    static func metricsShared(temporariesRemoved: Bool) -> String {
        let text = "Shared the metrics file. The app you chose now holds a copy that this app cannot recall."
        return temporariesRemoved ? text : text + " " + leftoverNote
    }
    static let metricsInfoParagraphs = [metricsHolds, metricsHoldsNot]

    static func failure(_ failure: ExportFailure) -> String {
        switch failure {
        case .noScopeSelected: return noScopeNote
        case .notAuthenticated, .relocked: return lockedText
        case .cancelled: return "The export was cancelled. Nothing was sent."
        case .storageUnavailable:
            return "The app could not prepare a protected place for the export. Nothing was sent; try again."
        case .core: return "The export could not be completed. Nothing was sent; try again."
        }
    }
}
