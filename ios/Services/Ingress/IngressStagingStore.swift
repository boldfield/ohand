import Foundation

/// Reads and writes ingress staging records and moves recorded audio into the finalized store. It never deletes a
/// record or audio file on its own initiative: the service removes a record only after the core confirmed it.
final class IngressStagingStore {
    let layout: ProtectedStorageLayout
    private let fileSystem: IngressFileSystem
    private let encoder: JSONEncoder
    private let decoder = JSONDecoder()

    init(layout: ProtectedStorageLayout, fileSystem: IngressFileSystem) {
        self.layout = layout
        self.fileSystem = fileSystem
        encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
    }

    private var recordsDirectory: URL { layout.directory(for: .ingressStagingRecords) }
    private var inProgressAudioDirectory: URL { layout.directory(for: .ingressInProgressAudio) }
    private var finalizedAudioDirectory: URL { layout.directory(for: .finalizedAudio) }

    func recordURL(captureID: String) -> URL {
        recordsDirectory.appendingPathComponent("\(captureID).\(IngressRecord.fileExtension)")
    }

    func inProgressAudioURL(fileName: String) -> URL {
        inProgressAudioDirectory.appendingPathComponent(fileName)
    }

    func finalizedAudioURL(fileName: String) -> URL {
        finalizedAudioDirectory.appendingPathComponent(fileName)
    }

    /// Resolves a core audio reference to its file, or nil when it does not name a file directly inside the
    /// finalized audio store.
    func finalizedAudioURL(forReference reference: String) -> URL? {
        let prefix = StoreProtectionPolicy.policy(for: .finalizedAudio).directoryName + "/"
        guard reference.hasPrefix(prefix) else { return nil }
        let fileName = String(reference.dropFirst(prefix.count))
        guard IngressRecord.isSafeFileName(fileName) else { return nil }
        return finalizedAudioURL(fileName: fileName)
    }

    enum StageResult: Equatable {
        case created
        /// An identical record was already staged by an earlier delivery; nothing was written.
        case alreadyStaged
    }

    /// Makes `record` durable. An existing record with the same capture ID is left alone: identical content means a
    /// repeated delivery, different content is a conflict.
    func stage(_ record: IngressRecord) throws -> StageResult {
        do {
            try record.validate()
        } catch let problem as IngressRecordProblem {
            throw IngressStagingFailure.invalidRecord(problem)
        }
        if let existing = try load(captureID: record.captureID) {
            guard existing == record else { throw IngressStagingFailure.conflictingStagingRecord }
            return .alreadyStaged
        }
        let data: Data
        do {
            data = try encoder.encode(record)
        } catch {
            throw IngressStagingFailure.storage(IngressStorageError(error))
        }
        do {
            try fileSystem.writeDurably(data, to: recordURL(captureID: record.captureID))
        } catch {
            throw IngressStagingFailure.storage(IngressStorageError(error))
        }
        return .created
    }

    /// The staged record for `captureID`, or nil when none exists. A record that exists but cannot be read (for
    /// example while the device is locked) throws rather than reporting "absent".
    func load(captureID: String) throws -> IngressRecord? {
        let url = recordURL(captureID: captureID)
        guard fileSystem.fileExists(at: url) else { return nil }
        return try decode(from: url)
    }

    private func decode(from url: URL) throws -> IngressRecord {
        let data: Data
        do {
            data = try fileSystem.read(from: url)
        } catch {
            throw IngressStagingFailure.unreadableRecord(IngressStorageError(error))
        }
        let record: IngressRecord
        do {
            record = try decoder.decode(IngressRecord.self, from: data)
        } catch {
            throw IngressStagingFailure.corruptRecord
        }
        guard record.formatVersion <= IngressRecord.currentFormatVersion else {
            throw IngressStagingFailure.unsupportedRecordVersion(record.formatVersion)
        }
        let expectedName = "\(record.captureID).\(IngressRecord.fileExtension)"
        guard url.lastPathComponent == expectedName else { throw IngressStagingFailure.corruptRecord }
        return record
    }

    struct Listing: Equatable {
        var recordCaptureIDs: [String] = []
        var incompleteWrites: [String] = []
    }

    func listRecords() throws -> Listing {
        let names: [String]
        do {
            names = try fileSystem.entryNames(in: recordsDirectory)
        } catch {
            throw IngressStagingFailure.storage(IngressStorageError(error))
        }
        var listing = Listing()
        let recordSuffix = "." + IngressRecord.fileExtension
        for name in names.sorted() {
            if name.hasSuffix(recordSuffix) {
                listing.recordCaptureIDs.append(String(name.dropLast(recordSuffix.count)))
            } else if name.hasSuffix(IngressFileSystemNaming.incompleteWriteSuffix) {
                listing.incompleteWrites.append(name)
            }
        }
        return listing
    }

    func removeRecord(captureID: String) throws {
        let url = recordURL(captureID: captureID)
        guard fileSystem.fileExists(at: url) else { return }
        try fileSystem.remove(at: url)
    }

    /// Audio files in the in-progress store that none of `claimedFileNames` owns.
    func unclaimedInProgressAudio(claimedFileNames: Set<String>) -> [String] {
        let names = (try? fileSystem.entryNames(in: inProgressAudioDirectory)) ?? []
        return names.filter { !claimedFileNames.contains($0) && !$0.hasPrefix(".") }.sorted()
    }

    enum AudioFinalization: Equatable {
        /// The audio is now (or already was) in the finalized store; `moved` says whether this call moved it.
        case ready(moved: Bool)
    }

    /// Moves the recording to the finalized store so the reference the core keeps outlives the staging record. The
    /// step is idempotent: a file already in the finalized store means an earlier attempt completed the move.
    func finalizeAudio(_ handoff: IngressAudioHandoff) throws -> AudioFinalization {
        let source = inProgressAudioURL(fileName: handoff.inProgressFileName)
        let destination = finalizedAudioURL(fileName: handoff.finalizedFileName)
        if fileSystem.fileExists(at: destination) {
            try requireNonEmpty(destination)
            return .ready(moved: false)
        }
        guard fileSystem.fileExists(at: source) else { throw IngressStagingFailure.audioSourceMissing }
        try requireNonEmpty(source)
        do {
            try fileSystem.move(from: source, to: destination)
        } catch {
            throw IngressStagingFailure.storage(IngressStorageError(error))
        }
        return .ready(moved: true)
    }

    private func requireNonEmpty(_ url: URL) throws {
        let size: Int
        do {
            size = try fileSystem.fileSize(at: url)
        } catch {
            throw IngressStagingFailure.storage(IngressStorageError(error))
        }
        guard size > 0 else { throw IngressStagingFailure.audioSourceEmpty }
    }
}
