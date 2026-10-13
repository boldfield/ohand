import Foundation

/// Production staging for reset: the foreground ingress path for the import of the snapshot's records only (never a
/// fresh listing, which could pull a later capture below the boundary), and `DeletionLocalCleanup` (the per-item
/// ingress and audio executor) for the purge, so reset removes staged files exactly the way a deleted item's are removed.
///
/// The ingress boundary is `ResetIngressGate`, which must be the importer of the `ForegroundIngressService` given here.
/// Listing and holding happen in one main-queue step, the queue every ingress submission stages on, so each input is
/// either in the list or held, never both and never neither.
final class NativeResetIngress: ResetIngressStaging {
    private static let incompleteRecordSuffix = "." + IngressRecord.fileExtension + IngressFileSystemNaming.incompleteWriteSuffix

    private let service: ForegroundIngressService
    private let gate: ResetIngressGate
    private let staging: IngressStagingStore
    private let localCleanup: DeletionLocalCleanup

    init(
        service: ForegroundIngressService,
        gate: ResetIngressGate,
        layout: ProtectedStorageLayout,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) {
        self.service = service
        self.gate = gate
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        localCleanup = DeletionLocalCleanup(layout: layout, fileSystem: fileSystem)
    }

    func snapshot() async throws -> ResetStagingSnapshot {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<ResetStagingSnapshot, Error>) in
            DispatchQueue.main.async { [staging, gate] in
                do {
                    let captureIDs = Self.captureIDs(in: try staging.listRecords())
                    gate.hold(stagedBeforeTheTap: captureIDs)
                    continuation.resume(returning: ResetStagingSnapshot(captureIDs: captureIDs))
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    func holdBoundary(for snapshot: ResetStagingSnapshot) async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.main.async { [gate] in
                gate.hold(stagedBeforeTheTap: snapshot.captureIDs)
                continuation.resume()
            }
        }
    }

    func endBoundary() {
        DispatchQueue.main.async { [gate] in gate.release() }
    }

    func importSnapshot(_ snapshot: ResetStagingSnapshot) async {
        for captureID in snapshot.captureIDs.sorted() {
            // Unreadable or interrupted writes are not records; the purge that follows removes them.
            guard let record = try? staging.load(captureID: captureID) else { continue }
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                DispatchQueue.main.async { [service] in
                    service.submit(record) { _ in continuation.resume() }
                }
            }
        }
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

/// Keeps the reset markers as small files in the protected temporary store. The choice and cleanup markers hold no
/// secret, identifier or generation; their existence is the marker.
///
/// A recorded tap is a numbered file holding the credential choice and the capture identifiers staged at the tap (local
/// identifiers that already name files in the same protected store; never content, paths or a generation). The highest
/// number is the request. A new tap is written in full under the next number before the earlier files are removed, so
/// an interruption or a failure at any point leaves either the earlier tap or the new one as the latest, never no
/// request and never an older choice in place of a newer one. Clearing removes the oldest first, so whatever survives a
/// failed clear is still the latest tap.
final class FileResetMarkerStore: ResetMarkerStore {
    static let removalFileName = "reset-credential-removal"
    static let outstandingFileName = "reset-cleanup-outstanding"
    static let requestFilePrefix = "reset-request-"

    private struct StoredRequest: Codable {
        let credentialChoice: String
        let stagedBeforeTheTap: [String]
    }

    struct UnreadableRequest: Error {}

    private let directory: URL
    private let removalURL: URL
    private let outstandingURL: URL
    private let fileSystem: IngressFileSystem

    init(layout: ProtectedStorageLayout, fileSystem: IngressFileSystem = FileManagerIngressFileSystem()) {
        directory = layout.directory(for: .temporaryFiles)
        removalURL = directory.appendingPathComponent(Self.removalFileName)
        outstandingURL = directory.appendingPathComponent(Self.outstandingFileName)
        self.fileSystem = fileSystem
    }

    func request() throws -> ResetRequest? {
        guard let latest = try requestNumbers().last else { return nil }
        let stored = try JSONDecoder().decode(StoredRequest.self, from: try fileSystem.read(from: requestURL(latest)))
        let choice: ResetCredentialChoice
        switch stored.credentialChoice {
        case "keep": choice = .keep
        case "remove": choice = .remove
        default: throw UnreadableRequest()
        }
        return ResetRequest(
            credentialChoice: choice, stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: Set(stored.stagedBeforeTheTap)))
    }

    func recordRequest(_ request: ResetRequest) throws {
        let earlier = try requestNumbers()
        let stored = StoredRequest(
            credentialChoice: request.credentialChoice == .remove ? "remove" : "keep",
            stagedBeforeTheTap: request.stagedBeforeTheTap.captureIDs.sorted())
        try fileSystem.writeDurably(try JSONEncoder().encode(stored), to: requestURL((earlier.last ?? 0) + 1))
        // The new tap is now the latest. An earlier file that cannot be removed here is never read again, and the next
        // clear removes it before the latest.
        for number in earlier {
            try? fileSystem.remove(at: requestURL(number))
        }
    }

    func clearRequest() throws {
        for number in try requestNumbers() {
            try fileSystem.remove(at: requestURL(number))
        }
    }

    func isRemovalRequested() throws -> Bool { fileSystem.fileExists(at: removalURL) }
    func recordRemovalRequested() throws { try record(removalURL) }
    func clearRemovalRequest() throws { try clear(removalURL) }

    func isCleanupOutstanding() throws -> Bool { fileSystem.fileExists(at: outstandingURL) }
    func recordCleanupOutstanding() throws { try record(outstandingURL) }
    func clearCleanupOutstanding() throws { try clear(outstandingURL) }

    /// Numbers of the complete request files, ascending. A write interrupted before it became a file is not a request.
    private func requestNumbers() throws -> [Int] {
        guard fileSystem.fileExists(at: directory) else { return [] }
        return try fileSystem.entryNames(in: directory)
            .filter { $0.hasPrefix(Self.requestFilePrefix) }
            .compactMap { Int($0.dropFirst(Self.requestFilePrefix.count)) }
            .sorted()
    }

    private func requestURL(_ number: Int) -> URL {
        directory.appendingPathComponent(Self.requestFilePrefix + String(number))
    }

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
