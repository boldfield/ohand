import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The metrics operations this section uses. `MetricsReportControl` is the production implementation.
protocol MetricsReportControlling: AnyObject {
    func recordingEnabled() async -> Bool?
    func setRecordingEnabled(_ enabled: Bool) async -> Bool
    func clearAll() async -> Bool
    func prepareExport() async throws -> PreparedMetricsExport
    func finishShare(exportID: String, ending: ExportShareEnding) -> ExportShareOutcome
    func cancelExport(exportID: String) -> ExportShareOutcome
}

extension MetricsReportControl: MetricsReportControlling {}

/// What the metrics part of the export section is showing.
enum MetricsExportPhase: Equatable {
    case idle
    case preparing
    /// The report file is prepared and the share sheet is on screen.
    case sharing
    /// The last action ended; the text says how, from the service's own report.
    case finished(String)
    /// The session is capture-only, so the controls are not offered.
    case locked
}

/// The state of the metrics part of the export section: the report export and the recording controls.
///
/// Rules this type keeps:
/// * Every operation needs an authenticated read session; in a capture-only session nothing is requested and the
///   section says why.
/// * The file shared is the metrics service's report as it is. Completion is reported only from the service's share
///   outcome, so a cancelled or failed share is never shown as success.
/// * Every prepared file is handed back exactly once, through `finishShare` or `cancelExport`; leaving the screen or
///   locking cancels what is pending.
@MainActor
final class MetricsExportSettingsModel: ObservableObject {
    @Published private(set) var phase: MetricsExportPhase = .idle
    /// Whether the core is recording. Recording is on by default; this follows the core once it has been read.
    @Published private(set) var isRecording = true
    /// The report file the share sheet should present; non-nil exactly while `phase == .sharing`.
    @Published private(set) var pendingShare: PreparedMetricsExport?

    private let controls: MetricsReportControlling
    private let readScope: () -> CompositionReadScope
    /// Advanced by `clear()`; an answer for an earlier screen is discarded and its file removed.
    private var session = 0

    nonisolated init(controls: MetricsReportControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isAvailable: Bool { phase != .locked }
    var canExport: Bool { isAvailable && phase != .preparing && phase != .sharing }

    /// Reads the access and the recording state when the section appears.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        if phase == .locked { phase = .idle }
        let thisSession = session
        let enabled = await controls.recordingEnabled()
        guard thisSession == session, let enabled else { return }
        isRecording = enabled
    }

    func startExport() async {
        guard readScope() == .authenticated else { lock(); return }
        guard canExport else { return }
        let thisSession = session
        phase = .preparing
        let result: Result<PreparedMetricsExport, Error>
        do { result = .success(try await controls.prepareExport()) } catch { result = .failure(error) }
        switch result {
        case .success(let export):
            guard thisSession == session, readScope() == .authenticated else {
                _ = controls.cancelExport(exportID: export.id)
                if thisSession == session { lock() }
                return
            }
            pendingShare = export
            phase = .sharing
        case .failure(let error):
            guard thisSession == session else { return }
            switch error as? MetricsReportFailure {
            case .notAuthenticated, .interrupted: lock()
            case .unavailable, .none: phase = .finished(ExportSettingsText.metricsExportFailed)
            }
        }
    }

    /// The share sheet's ending for `exportID`; an ending for a file that is no longer pending is ignored.
    func shareEnded(_ ending: ExportShareEnding, exportID: String) {
        guard let export = pendingShare, export.id == exportID else { return }
        pendingShare = nil
        let outcome = controls.finishShare(exportID: export.id, ending: ending)
        let removed = outcome.temporariesRemoved
        switch outcome.result {
        case .shared: phase = .finished(ExportSettingsText.metricsShared(temporariesRemoved: removed))
        case .cancelled: phase = .finished(ExportSettingsText.cancelled(temporariesRemoved: removed))
        case .failed: phase = .finished(ExportSettingsText.shareFailed(temporariesRemoved: removed))
        case .interrupted: phase = .finished(ExportSettingsText.interrupted(temporariesRemoved: removed))
        }
        if readScope() != .authenticated { lock() }
    }

    func setRecording(_ enabled: Bool) async {
        guard readScope() == .authenticated else { lock(); return }
        guard canExport else { return }
        let thisSession = session
        let applied = await controls.setRecordingEnabled(enabled)
        guard thisSession == session else { return }
        if applied {
            isRecording = enabled
            phase = .finished(enabled ? ExportSettingsText.metricsResumed : ExportSettingsText.metricsPaused)
        } else if readScope() != .authenticated {
            lock()
        } else {
            phase = .finished(ExportSettingsText.metricsActionFailed)
        }
    }

    func clearMetrics() async {
        guard readScope() == .authenticated else { lock(); return }
        guard canExport else { return }
        let thisSession = session
        let applied = await controls.clearAll()
        guard thisSession == session else { return }
        if applied {
            phase = .finished(ExportSettingsText.metricsCleared)
        } else if readScope() != .authenticated {
            lock()
        } else {
            phase = .finished(ExportSettingsText.metricsActionFailed)
        }
    }

    /// Call when the screen goes away or the app locks: removes a pending file, ignores answers still pending and
    /// forgets the last result.
    func clear() {
        session += 1
        if let export = pendingShare { _ = controls.cancelExport(exportID: export.id) }
        pendingShare = nil
        phase = .idle
    }

    private func lock() {
        clear()
        phase = .locked
    }
}

extension MetricsExportSettingsModel {
    /// The model as the app assembles it: the root's metrics report control, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> MetricsExportSettingsModel {
        MetricsExportSettingsModel(
            controls: root.metricsReport, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
