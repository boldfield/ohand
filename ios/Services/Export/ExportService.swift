import Foundation
import OhAndCoreBridge

/// Prepares an export for the platform share sheet and owns every temporary file that preparation creates.
///
/// A request needs an authenticated read session; the core re-checks that and owns the scope and deletion rules. The
/// core writes the portable export into a per-export directory inside the protected temporary store, and, only when the
/// user asked for audio, finalized audio files are copied next to it. Source stores are only ever read: the export
/// directory is the one place this service removes anything from, so no share outcome can delete captured content.
///
/// The directory is removed when the share completes, is cancelled or fails, when the export fails, when the read
/// session relocks (which includes entering the background) or the task is cancelled, and by `sweepAtLaunch` for
/// whatever an interrupted run left behind. A share completion that arrives after the export was invalidated is
/// reported as `interrupted`, never as success.
public final class ExportService: @unchecked Sendable {
    public static let exportsDirectoryName = "Exports"
    static let exportFileName = "ohand-export.jsonl"
    static let audioDirectoryName = "audio"

    private struct ActiveExport {
        let authorization: ReadAuthorization
        let directory: URL
        var isInvalidated: Bool
    }

    private let core: ExportCore
    private let session: ReadAuthenticationSession
    private let layout: ProtectedStorageLayout
    private let fileSystem: ExportFileSystem
    private let makeIdentifier: () -> String
    private let lock = NSLock()
    private var active: [String: ActiveExport] = [:]
    private var scopeObserver: SessionScopeObserverToken?

    init(
        core: ExportCore,
        session: ReadAuthenticationSession,
        layout: ProtectedStorageLayout,
        fileSystem: ExportFileSystem = FileManagerExportFileSystem(),
        makeIdentifier: @escaping () -> String = { UUID().uuidString.lowercased() }
    ) {
        self.core = core
        self.session = session
        self.layout = layout
        self.fileSystem = fileSystem
        self.makeIdentifier = makeIdentifier
        scopeObserver = session.observeScope { [weak self] transition in
            guard transition.scope == .captureOnly else { return }
            self?.invalidateAll()
        }
    }

    deinit {
        if let scopeObserver {
            session.removeScopeObserver(scopeObserver)
        }
    }

    public var exportsDirectory: URL {
        layout.directory(for: .temporaryFiles).appendingPathComponent(Self.exportsDirectoryName, isDirectory: true)
    }

    // MARK: Preparing

    /// Exports `selection` to protected temporaries. Throws `ExportFailure`; on any failure nothing of this export is
    /// left on disk.
    public func prepareExport(_ selection: ExportSelection) async throws -> PreparedExport {
        guard !selection.scopes.isEmpty else { throw ExportFailure.noScopeSelected }
        guard let authorization = session.currentAuthorization() else { throw ExportFailure.notAuthenticated }

        let exportID = makeIdentifier()
        let directory = exportsDirectory.appendingPathComponent(exportID, isDirectory: true)
        let audioDirectory = directory.appendingPathComponent(Self.audioDirectoryName, isDirectory: true)
        let exportFile = directory.appendingPathComponent(Self.exportFileName)
        register(exportID, authorization: authorization, directory: directory)

        do {
            try requireValid(exportID, authorization: authorization)
            do {
                try fileSystem.createPrivateDirectory(at: directory)
                if selection.includeAudio {
                    try fileSystem.createPrivateDirectory(at: audioDirectory)
                }
            } catch {
                throw ExportFailure.storageUnavailable
            }
            try requireValid(exportID, authorization: authorization)

            let request = CoreExportRequest(
                readAuthentication: .authenticated,
                scopes: selection.scopes,
                audio: selection.includeAudio ? .references : .excluded,
                outputPath: exportFile.path)
            let coreResult: Result<CoreExportAnswer, ExportFailure>
            do {
                coreResult = .success(try await core.export(request))
            } catch let failure as CoreFailure {
                coreResult = .failure(.core(failure))
            } catch {
                coreResult = .failure(.core(Self.unknownCoreFailure))
            }

            // A relock or cancellation outranks the core's answer: the directory may have been removed under it.
            try requireValid(exportID, authorization: authorization)
            let answer = try coreResult.get()
            guard fileSystem.fileExists(at: exportFile) else { throw ExportFailure.storageUnavailable }
            do {
                try fileSystem.protectFile(at: exportFile)
            } catch {
                throw ExportFailure.storageUnavailable
            }

            let copied = selection.includeAudio
                ? copyAudio(answer.audioReferences, into: audioDirectory)
                : CopiedAudio(files: [], includedCaptureIDs: [], unavailable: [])
            try requireValid(exportID, authorization: authorization)

            return PreparedExport(
                id: exportID,
                exportFile: exportFile,
                audioFiles: copied.files,
                itemCount: answer.itemCount,
                scopes: selection.scopes,
                audio: ExportAudioReport(
                    requested: selection.includeAudio,
                    includedCaptureIDs: copied.includedCaptureIDs,
                    unavailable: copied.unavailable))
        } catch {
            discard(exportID, directory: directory)
            throw (error as? ExportFailure) ?? ExportFailure.storageUnavailable
        }
    }

    private static let unknownCoreFailure = CoreFailure(
        errorClass: .permanent, code: "export_failed", message: "the export failed")

    // MARK: Audio

    private struct CopiedAudio {
        let files: [URL]
        let includedCaptureIDs: [String]
        let unavailable: [ExportUnavailableAudio]
    }

    /// Copies each referenced audio file the capture owns. A reference that does not resolve, a file that is gone and a
    /// copy that fails are each reported, so the export never silently claims audio it does not carry.
    private func copyAudio(_ references: [CoreExportAudioReference], into audioDirectory: URL) -> CopiedAudio {
        var files: [URL] = []
        var included: [String] = []
        var unavailable: [ExportUnavailableAudio] = []
        let finalizedStore = layout.directory(for: .finalizedAudio)
        let prefix = StoreProtectionPolicy.policy(for: .finalizedAudio).directoryName + "/"
        for reference in references {
            guard IngressRecord.isSafeCaptureID(reference.captureID),
                  reference.reference.hasPrefix(prefix) else {
                unavailable.append(Self.unavailableEntry(reference, .referenceUnresolvable))
                continue
            }
            let fileName = String(reference.reference.dropFirst(prefix.count))
            guard IngressRecord.isSafeFileName(fileName), fileName.hasPrefix(reference.captureID + ".") else {
                unavailable.append(Self.unavailableEntry(reference, .referenceUnresolvable))
                continue
            }
            let source = finalizedStore.appendingPathComponent(fileName)
            guard fileSystem.fileExists(at: source) else {
                unavailable.append(Self.unavailableEntry(reference, .fileMissing))
                continue
            }
            let destination = audioDirectory.appendingPathComponent(fileName)
            do {
                try fileSystem.copyFile(from: source, to: destination)
                files.append(destination)
                included.append(reference.captureID)
            } catch {
                try? fileSystem.remove(at: destination)
                unavailable.append(Self.unavailableEntry(reference, .copyFailed))
            }
        }
        return CopiedAudio(files: files, includedCaptureIDs: included, unavailable: unavailable)
    }

    private static func unavailableEntry(
        _ reference: CoreExportAudioReference, _ reason: ExportAudioUnavailableReason
    ) -> ExportUnavailableAudio {
        ExportUnavailableAudio(itemID: reference.itemID, captureID: reference.captureID, reason: reason)
    }

    // MARK: Share lifecycle

    /// Records how the share sheet ended and removes the export's temporaries. `shared` is reported only when the sheet
    /// completed an activity and the export was still valid; an export that was invalidated meanwhile is `interrupted`.
    @discardableResult
    public func finishShare(exportID: String, ending: ExportShareEnding) -> ExportShareOutcome {
        lock.lock()
        let entry = active[exportID]
        lock.unlock()

        guard let entry else {
            return ExportShareOutcome(result: .interrupted, temporariesRemoved: true)
        }
        let result: ExportShareResult
        if entry.isInvalidated || !session.isCurrent(entry.authorization) {
            result = .interrupted
        } else {
            switch ending {
            case .completed: result = .shared
            case .cancelled: result = .cancelled
            case .failed: result = .failed
            }
        }
        let removed = discard(exportID, directory: entry.directory)
        return ExportShareOutcome(result: result, temporariesRemoved: removed)
    }

    /// Removes an export that was prepared but will not be shared (for example the screen was dismissed first).
    @discardableResult
    public func cancelExport(exportID: String) -> ExportShareOutcome {
        lock.lock()
        let entry = active[exportID]
        lock.unlock()
        guard let entry else {
            return ExportShareOutcome(result: .cancelled, temporariesRemoved: true)
        }
        let removed = discard(exportID, directory: entry.directory)
        return ExportShareOutcome(result: .cancelled, temporariesRemoved: removed)
    }

    /// Removes every export directory left by an earlier run (an interrupted share, a crash, a failed removal). Call it
    /// at launch. Exports active in this process are left alone.
    @discardableResult
    public func sweepAtLaunch() -> ExportSweepReport {
        let root = exportsDirectory
        guard fileSystem.fileExists(at: root), let names = try? fileSystem.entryNames(in: root) else {
            return ExportSweepReport(removedCount: 0, failedCount: 0)
        }
        lock.lock()
        let activeIDs = Set(active.keys)
        lock.unlock()
        var removedCount = 0
        var failedCount = 0
        for name in names.sorted() where !activeIDs.contains(name) {
            let url = root.appendingPathComponent(name)
            try? fileSystem.remove(at: url)
            if fileSystem.fileExists(at: url) { failedCount += 1 } else { removedCount += 1 }
        }
        return ExportSweepReport(removedCount: removedCount, failedCount: failedCount)
    }

    // MARK: Bookkeeping

    private func register(_ exportID: String, authorization: ReadAuthorization, directory: URL) {
        lock.lock()
        active[exportID] = ActiveExport(authorization: authorization, directory: directory, isInvalidated: false)
        lock.unlock()
    }

    private func requireValid(_ exportID: String, authorization: ReadAuthorization) throws {
        lock.lock()
        let invalidated = active[exportID]?.isInvalidated ?? true
        lock.unlock()
        if invalidated || !session.isCurrent(authorization) { throw ExportFailure.relocked }
        if Task.isCancelled { throw ExportFailure.cancelled }
    }

    /// Relock or a failed attempt: nothing readable may remain, so every export directory goes now. The entries stay
    /// (marked) so a late share completion or a preparation still in flight sees that its export was invalidated.
    private func invalidateAll() {
        lock.lock()
        var directories: [URL] = []
        for exportID in Array(active.keys) {
            active[exportID]?.isInvalidated = true
            if let directory = active[exportID]?.directory { directories.append(directory) }
        }
        lock.unlock()
        for directory in directories {
            try? fileSystem.remove(at: directory)
        }
    }

    /// Forgets the export and removes its directory. Returns whether nothing is left on disk.
    @discardableResult
    private func discard(_ exportID: String, directory: URL) -> Bool {
        lock.lock()
        active.removeValue(forKey: exportID)
        lock.unlock()
        guard fileSystem.fileExists(at: directory) else { return true }
        try? fileSystem.remove(at: directory)
        return !fileSystem.fileExists(at: directory)
    }
}

/// How the share sheet ended, before the service checks that the export is still valid.
public enum ExportShareEnding: Equatable {
    case completed
    case cancelled
    case failed
}
