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

    /// The staging record (or its interrupted write) is the durable retry handle of each listed input, so it goes last:
    /// every finalized file bound to the capture first, then `clearIngress`, which removes the audio the record names
    /// before the record itself. An input whose record is gone has no audio left, whether it was imported (the core then
    /// owns its audio) or purged by an earlier pass, so only inputs that are still staged are visited.
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
                try localCleanup.removeAudio(captureID: captureID, audioReference: nil)
                try localCleanup.clearIngress(captureID: captureID)
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

/// Keeps the reset markers as small files in the protected temporary store. No marker holds a secret, content, a path
/// or a generation. The cleanup marker is its existence alone. The removal marker holds what the `remove` choice binds:
/// the profile versions to revoke and the opaque Keychain references whose secrets to delete, so a retry after a failure
/// or a restart removes exactly the profiles that existed at the tap and never one configured since.
///
/// A recorded tap is a numbered file holding the credential choice, what a `remove` choice binds, the capture
/// identifiers staged at the tap (local identifiers that already name files in the same protected store), whether it
/// awaits an earlier pending reset and the core's generation at the tap (a reference to the core's state). The highest
/// number is the request. A new tap is written in full under the next number before the earlier files are removed, so
/// an interruption or a failure at any point leaves either the earlier tap or the new one as the latest, never no
/// request and never an older choice in place of a newer one. Clearing removes the oldest first, so whatever survives a
/// failed clear is still the latest tap.
final class FileResetMarkerStore: ResetMarkerStore {
    static let removalFileName = "reset-credential-removal"
    static let outstandingFileName = "reset-cleanup-outstanding"
    static let requestFilePrefix = "reset-request-"

    private struct StoredProfile: Codable {
        let profileVersion: String
        let credentialReference: String?
    }

    private struct StoredRemoval: Codable {
        let profiles: [StoredProfile]
    }

    private struct StoredRequest: Codable {
        let credentialChoice: String
        let profilesToRemove: [StoredProfile]?
        let stagedBeforeTheTap: [String]
        let awaitsEarlierReset: Bool?
        let latestGenerationAtTheTap: Int64?
    }

    struct UnreadableRequest: Error {}
    struct UnreadableRemoval: Error {}

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
        let removal: ResetCredentialRemoval?
        switch stored.credentialChoice {
        case "keep":
            choice = .keep
            removal = nil
        case "remove":
            // A remove tap always records what it binds; a file without the list is not a tap this store wrote.
            guard let profiles = stored.profilesToRemove else { throw UnreadableRequest() }
            choice = .remove
            removal = Self.removal(from: profiles)
        default:
            throw UnreadableRequest()
        }
        return ResetRequest(
            credentialChoice: choice, credentialRemoval: removal,
            stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: Set(stored.stagedBeforeTheTap)),
            awaitsEarlierReset: stored.awaitsEarlierReset ?? false,
            latestGenerationAtTheTap: stored.latestGenerationAtTheTap)
    }

    func recordRequest(_ request: ResetRequest) throws {
        let earlier = try requestNumbers()
        let stored = StoredRequest(
            credentialChoice: request.credentialChoice == .remove ? "remove" : "keep",
            profilesToRemove: request.credentialRemoval.map(Self.stored(from:)),
            stagedBeforeTheTap: request.stagedBeforeTheTap.captureIDs.sorted(),
            awaitsEarlierReset: request.awaitsEarlierReset,
            latestGenerationAtTheTap: request.latestGenerationAtTheTap)
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

    func removalRequest() throws -> ResetCredentialRemoval? {
        guard fileSystem.fileExists(at: removalURL) else { return nil }
        let data = try fileSystem.read(from: removalURL)
        guard let stored = try? JSONDecoder().decode(StoredRemoval.self, from: data) else { throw UnreadableRemoval() }
        return Self.removal(from: stored.profiles)
    }

    /// A marker that already exists was written for the same started reset (a later tap waits until an owed removal is
    /// carried out and its marker cleared), so it binds the same profiles and stays as it is.
    func recordRemovalRequested(_ removal: ResetCredentialRemoval) throws {
        guard !fileSystem.fileExists(at: removalURL) else { return }
        let stored = StoredRemoval(profiles: Self.stored(from: removal))
        try fileSystem.writeDurably(try JSONEncoder().encode(stored), to: removalURL)
    }

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

    private static func stored(from removal: ResetCredentialRemoval) -> [StoredProfile] {
        removal.profiles
            .sorted { $0.profileVersion < $1.profileVersion }
            .map { StoredProfile(profileVersion: $0.profileVersion, credentialReference: $0.credentialReference) }
    }

    private static func removal(from profiles: [StoredProfile]) -> ResetCredentialRemoval {
        ResetCredentialRemoval(profiles: profiles.map {
            ResetBoundProfile(profileVersion: $0.profileVersion, credentialReference: $0.credentialReference)
        })
    }

    private func clear(_ url: URL) throws {
        if fileSystem.fileExists(at: url) {
            try fileSystem.remove(at: url)
        }
    }
}
