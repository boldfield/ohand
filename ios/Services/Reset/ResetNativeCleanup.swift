import Foundation

/// Production staging for reset: the foreground ingress path for the import, and `DeletionLocalCleanup` (the per-item
/// ingress and audio executor) for the purge, so reset removes staged files exactly the way a deleted item's are removed.
final class NativeResetIngress: ResetIngressStaging {
    private static let incompleteRecordSuffix = "." + IngressRecord.fileExtension + IngressFileSystemNaming.incompleteWriteSuffix

    private let service: ForegroundIngressService
    private let staging: IngressStagingStore
    private let localCleanup: DeletionLocalCleanup

    init(
        service: ForegroundIngressService,
        layout: ProtectedStorageLayout,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) {
        self.service = service
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        localCleanup = DeletionLocalCleanup(layout: layout, fileSystem: fileSystem)
    }

    func snapshot() throws -> ResetStagingSnapshot {
        ResetStagingSnapshot(captureIDs: Self.captureIDs(in: try staging.listRecords()))
    }

    func importStaged() async -> Bool {
        let report = await withCheckedContinuation { (continuation: CheckedContinuation<IngressRecoveryReport, Never>) in
            DispatchQueue.main.async { [service] in
                service.recover { continuation.resume(returning: $0) }
            }
        }
        return report.listingFailure == nil
    }

    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge {
        let stillStaged: Set<String>
        do {
            stillStaged = Self.captureIDs(in: try staging.listRecords())
        } catch {
            return .unknown
        }
        var purged = 0
        var remaining = 0
        for captureID in snapshot.captureIDs.intersection(stillStaged).sorted() {
            do {
                try localCleanup.clearIngress(captureID: captureID)
                try localCleanup.removeAudio(captureID: captureID, audioReference: nil)
                purged += 1
            } catch {
                remaining += 1
            }
        }
        return remaining == 0 ? .purged(count: purged) : .incomplete(remaining: remaining)
    }

    /// Capture identifiers of staged records and of record writes that were interrupted before becoming a record.
    private static func captureIDs(in listing: IngressStagingStore.Listing) -> Set<String> {
        var identifiers = Set(listing.recordCaptureIDs)
        for name in listing.incompleteWrites where name.hasSuffix(incompleteRecordSuffix) {
            identifiers.insert(String(name.dropLast(incompleteRecordSuffix.count)))
        }
        return identifiers
    }
}

/// Removes the export and share temporaries under the protected temporary store. It reaches only the exports
/// directory, never a source store, and verifies absence afterwards.
final class ExportTemporaryCleaner: ResetExportTemporaries {
    private let exportsDirectory: URL
    private let fileSystem: ExportFileSystem

    init(layout: ProtectedStorageLayout, fileSystem: ExportFileSystem = FileManagerExportFileSystem()) {
        exportsDirectory = layout.directory(for: .temporaryFiles)
            .appendingPathComponent(ExportService.exportsDirectoryName, isDirectory: true)
        self.fileSystem = fileSystem
    }

    func removeAll() -> Bool {
        guard fileSystem.fileExists(at: exportsDirectory) else { return true }
        guard let names = try? fileSystem.entryNames(in: exportsDirectory) else { return false }
        var allRemoved = true
        for name in names.sorted() {
            let url = exportsDirectory.appendingPathComponent(name)
            try? fileSystem.remove(at: url)
            if fileSystem.fileExists(at: url) { allRemoved = false }
        }
        return allRemoved
    }
}

/// Keeps the reset markers as small files in the protected temporary store. A file holds no secret, identifier or
/// generation; its existence is the marker.
final class FileResetMarkerStore: ResetMarkerStore {
    static let removalFileName = "reset-credential-removal"
    static let outstandingFileName = "reset-cleanup-outstanding"

    private let removalURL: URL
    private let outstandingURL: URL
    private let fileSystem: IngressFileSystem

    init(layout: ProtectedStorageLayout, fileSystem: IngressFileSystem = FileManagerIngressFileSystem()) {
        let directory = layout.directory(for: .temporaryFiles)
        removalURL = directory.appendingPathComponent(Self.removalFileName)
        outstandingURL = directory.appendingPathComponent(Self.outstandingFileName)
        self.fileSystem = fileSystem
    }

    func isRemovalRequested() throws -> Bool { fileSystem.fileExists(at: removalURL) }
    func recordRemovalRequested() throws { try record(removalURL) }
    func clearRemovalRequest() throws { try clear(removalURL) }

    func isCleanupOutstanding() throws -> Bool { fileSystem.fileExists(at: outstandingURL) }
    func recordCleanupOutstanding() throws { try record(outstandingURL) }
    func clearCleanupOutstanding() throws { try clear(outstandingURL) }

    private func record(_ url: URL) throws {
        guard !fileSystem.fileExists(at: url) else { return }
        try fileSystem.writeDurably(Data("1".utf8), to: url)
    }

    private func clear(_ url: URL) throws {
        if fileSystem.fileExists(at: url) {
            try fileSystem.remove(at: url)
        }
    }
}
