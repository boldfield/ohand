import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The export operations this section uses. `ExportService` is the production implementation; the section reuses its
/// operations and adds none.
protocol ExportControlling: AnyObject {
    func prepareExport(_ selection: ExportSelection) async throws -> PreparedExport
    func finishShare(exportID: String, ending: ExportShareEnding) -> ExportShareOutcome
    func cancelExport(exportID: String) -> ExportShareOutcome
}

extension ExportService: ExportControlling {}

/// What the export section is showing.
enum ExportSettingsPhase: Equatable {
    case idle
    case preparing
    /// The export is prepared and the share sheet is on screen.
    case sharing
    /// The last attempt ended; `message` says how, from the share sheet's or the service's own report.
    case finished(ExportSettingsOutcome)
    case locked
}

enum ExportSettingsOutcome: Equatable {
    case shared(String)
    case cancelled(String)
    case failed(String)
}

/// The state of the export section: the scopes and audio choice, the prepared export and how its share ended.
///
/// Rules this type keeps:
/// * No scope is selected by default and audio is off by default; an export is never started without a scope.
/// * Completion is reported only from the service's share outcome, which already distinguishes a completed activity
///   from a cancelled, failed or interrupted one. A cancelled or failed share is never shown as success.
/// * The share sheet's own completion handler is the only source of the ending; the sheet going off screen is not an
///   ending, because the handler runs only after the dismissal and, for share extensions, after they finish.
/// * Every prepared export is handed back to the service exactly once, through `finishShare` or `cancelExport`, so its
///   protected temporaries are removed whatever happens; leaving the screen or locking cancels what is pending.
/// * Messages carry counts only: no item, capture or file identifier, path or secret is shown.
@MainActor
final class ExportSettingsModel: ObservableObject {
    @Published private(set) var phase: ExportSettingsPhase = .idle
    @Published private(set) var selectedScopes: Set<CoreExportScope> = []
    @Published var includeAudio = false
    /// The export the share sheet should present; non-nil exactly while `phase == .sharing`.
    @Published private(set) var pendingShare: PreparedExport?

    private let controls: ExportControlling
    private let readScope: () -> CompositionReadScope
    /// Advanced by `clear()`; an answer for an earlier screen is discarded and its export removed.
    private var session = 0
    private var preparing: Task<Void, Never>?

    nonisolated init(controls: ExportControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    static let selectableScopes: [CoreExportScope] = [.personal, .work]

    var canStart: Bool {
        !selectedScopes.isEmpty && phase != .preparing && phase != .sharing && phase != .locked
    }

    func isSelected(_ scope: CoreExportScope) -> Bool { selectedScopes.contains(scope) }

    func setScope(_ scope: CoreExportScope, selected: Bool) {
        if selected { selectedScopes.insert(scope) } else { selectedScopes.remove(scope) }
    }

    /// Prepares an export of the chosen scopes and, when it is ready, presents it for sharing.
    func startExport() async {
        guard readScope() == .authenticated else { lock(); return }
        guard canStart else { return }
        let selection = ExportSelection(
            scopes: Self.selectableScopes.filter(selectedScopes.contains), includeAudio: includeAudio)
        let thisSession = session
        phase = .preparing
        let controls = self.controls
        let task = Task { [weak self] in
            let result: Result<PreparedExport, Error>
            do { result = .success(try await controls.prepareExport(selection)) } catch { result = .failure(error) }
            guard let self else {
                if case .success(let export) = result { _ = controls.cancelExport(exportID: export.id) }
                return
            }
            self.prepared(result, session: thisSession)
        }
        preparing = task
        await task.value
    }

    private func prepared(_ result: Result<PreparedExport, Error>, session answeredSession: Int) {
        preparing = nil
        switch result {
        case .success(let export):
            guard answeredSession == session, readScope() == .authenticated else {
                _ = controls.cancelExport(exportID: export.id)
                if answeredSession == session { lock() }
                return
            }
            pendingShare = export
            phase = .sharing
        case .failure(let error):
            guard answeredSession == session else { return }
            if let failure = error as? ExportFailure {
                if failure == .notAuthenticated || failure == .relocked { lock(); return }
                phase = .finished(.failed(ExportSettingsText.failure(failure)))
            } else {
                phase = .finished(.failed(ExportSettingsText.failure(.storageUnavailable)))
            }
        }
    }

    /// The share sheet's ending for `exportID`. An ending for an export that is no longer pending, such as a late
    /// answer from an earlier sheet, is ignored.
    func shareEnded(_ ending: ExportShareEnding, exportID: String) {
        guard pendingShare?.id == exportID else { return }
        shareEnded(ending)
    }

    /// Call with how the share sheet ended. Reports success only when the service confirms the share completed.
    func shareEnded(_ ending: ExportShareEnding) {
        guard let export = pendingShare else { return }
        pendingShare = nil
        let outcome = controls.finishShare(exportID: export.id, ending: ending)
        let removed = outcome.temporariesRemoved
        switch outcome.result {
        case .shared:
            phase = .finished(.shared(ExportSettingsText.shared(export, temporariesRemoved: removed)))
        case .cancelled:
            phase = .finished(.cancelled(ExportSettingsText.cancelled(temporariesRemoved: removed)))
        case .failed:
            phase = .finished(.failed(ExportSettingsText.shareFailed(temporariesRemoved: removed)))
        case .interrupted:
            phase = .finished(.failed(ExportSettingsText.interrupted(temporariesRemoved: removed)))
        }
        if readScope() != .authenticated { lock() }
    }

    /// Call when the screen goes away or the app locks: removes any pending export's files, ignores answers still
    /// pending and forgets the last result.
    func clear() {
        session += 1
        preparing?.cancel()
        preparing = nil
        if let export = pendingShare { _ = controls.cancelExport(exportID: export.id) }
        pendingShare = nil
        selectedScopes = []
        includeAudio = false
        phase = .idle
    }

    private func lock() {
        clear()
        phase = .locked
    }
}
