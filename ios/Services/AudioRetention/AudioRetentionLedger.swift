import Darwin
import Foundation

/// The file operations the retention service needs: the ingress operations plus an atomic replace for the ledger.
/// Tests inject a wrapper that fails selected operations to prove an interruption never loses audio or reports a
/// removal that was not recorded.
protocol AudioRetentionFileSystem: IngressFileSystem {
    /// Writes `data` so that either the complete new content or the previous content is at `url`, never a mix. The data
    /// is flushed to storage before it becomes visible.
    func replaceDurably(_ data: Data, at url: URL) throws
}

struct FileManagerAudioRetentionFileSystem: AudioRetentionFileSystem {
    private let base = FileManagerIngressFileSystem()

    func fileExists(at url: URL) -> Bool { base.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try base.fileSize(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try base.entryNames(in: directory) }
    func read(from url: URL) throws -> Data { try base.read(from: url) }
    func writeDurably(_ data: Data, to url: URL) throws { try base.writeDurably(data, to: url) }
    func move(from source: URL, to destination: URL) throws { try base.move(from: source, to: destination) }
    func remove(at url: URL) throws { try base.remove(at: url) }

    func replaceDurably(_ data: Data, at url: URL) throws {
        let replacement = URL(fileURLWithPath: url.path + ".replacing")
        if base.fileExists(at: replacement) { try base.remove(at: replacement) }
        try base.writeDurably(data, to: replacement)
        guard Darwin.rename(replacement.path, url.path) == 0 else {
            let failure = POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            try? base.remove(at: replacement)
            throw failure
        }
    }
}

/// The durable acknowledgement of retention cleanup, kept by native code because the core's retention policy is
/// read-only. It records which audio is verified removed and how often removing other audio failed. It carries no
/// authority: the core is asked again before every removal, so an entry can never approve a removal, only spare a
/// rewrite or stop a removal that keeps failing. A ledger that cannot be decoded is treated as empty for that reason,
/// at the cost of repeating idempotent work; one that cannot be read at all fails the operation instead, because
/// "unknown" must not read as "empty". Entries hold identifiers and fixed codes only.
///
/// Not thread safe: the retention service serializes every call.
final class AudioRetentionLedger {
    static let fileName = "audio-retention-ledger.json"
    static let currentFormatVersion = 1

    enum State: String, Codable, Equatable {
        /// The file is verified absent.
        case removed
        /// A removal failed but may still succeed; `failedAttempts` counts toward the bound.
        case failing
        /// No further automatic attempt; the audio stays and the failure stays visible.
        case failed
    }

    struct Entry: Codable, Equatable {
        let itemID: String
        let audioReference: String
        var state: State
        var failedAttempts: Int
        /// Fixed code of the last failure (`AudioRetentionPermanentFailure` or retry reason raw value), if any.
        var lastFailure: String?
    }

    private struct Document: Codable {
        var formatVersion: Int
        var entries: [Entry]
    }

    private let fileURL: URL
    private let fileSystem: AudioRetentionFileSystem

    init(layout: ProtectedStorageLayout, fileSystem: AudioRetentionFileSystem) {
        fileURL = layout.directory(for: .configuration).appendingPathComponent(Self.fileName)
        self.fileSystem = fileSystem
    }

    func entry(itemID: String, audioReference: String) throws -> Entry? {
        try load().entries.first { $0.itemID == itemID && $0.audioReference == audioReference }
    }

    func allEntries() throws -> [Entry] {
        try load().entries
    }

    func recordRemoved(itemID: String, audioReference: String) throws {
        try update(itemID: itemID, audioReference: audioReference) { entry in
            entry.state = .removed
            entry.failedAttempts = 0
            entry.lastFailure = nil
        }
    }

    /// Records one failed attempt. The entry becomes `failed` when `permanent` is set or the attempts reach `limit`.
    @discardableResult
    func recordFailure(
        itemID: String, audioReference: String, code: String, permanent: Bool, limit: Int
    ) throws -> Entry {
        var result: Entry?
        try update(itemID: itemID, audioReference: audioReference) { entry in
            entry.failedAttempts += 1
            entry.lastFailure = code
            entry.state = permanent || entry.failedAttempts >= limit ? .failed : .failing
            result = entry
        }
        return result!
    }

    /// Forgets the failure so the next sweep attempts the removal again. A removal record is left alone.
    func resetFailure(itemID: String, audioReference: String) throws {
        guard let existing = try entry(itemID: itemID, audioReference: audioReference), existing.state != .removed else {
            return
        }
        var document = try load()
        document.entries.removeAll { $0.itemID == itemID && $0.audioReference == audioReference }
        try save(document)
    }

    // MARK: private

    private func update(itemID: String, audioReference: String, _ change: (inout Entry) -> Void) throws {
        var document = try load()
        if let index = document.entries.firstIndex(where: { $0.itemID == itemID && $0.audioReference == audioReference }) {
            change(&document.entries[index])
        } else {
            var entry = Entry(
                itemID: itemID, audioReference: audioReference, state: .failing, failedAttempts: 0, lastFailure: nil)
            change(&entry)
            document.entries.append(entry)
        }
        try save(document)
    }

    private func load() throws -> Document {
        guard fileSystem.fileExists(at: fileURL) else { return Document(formatVersion: Self.currentFormatVersion, entries: []) }
        let data = try fileSystem.read(from: fileURL)
        guard let document = try? JSONDecoder().decode(Document.self, from: data),
              document.formatVersion == Self.currentFormatVersion else {
            return Document(formatVersion: Self.currentFormatVersion, entries: [])
        }
        return document
    }

    private func save(_ document: Document) throws {
        var sorted = document
        sorted.entries.sort { ($0.itemID, $0.audioReference) < ($1.itemID, $1.audioReference) }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        try fileSystem.replaceDurably(try encoder.encode(sorted), at: fileURL)
    }
}
