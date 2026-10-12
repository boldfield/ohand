import Darwin
import Foundation

/// The file operations the retention service needs: the ingress operations plus an atomic replace for the ledger and a
/// presence check that cannot mistake a failed lookup for absence. Tests inject a wrapper that fails selected operations
/// to prove an interruption never loses audio or reports a removal that was not recorded.
protocol AudioRetentionFileSystem: IngressFileSystem {
    /// True when something is at `url`, false only when the lookup verifies nothing is there. Throws when presence
    /// cannot be determined (access, protection or I/O failure), because "unknown" must never read as "absent".
    func isPresent(at url: URL) throws -> Bool

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

    func isPresent(at url: URL) throws -> Bool {
        var status = stat()
        if Darwin.lstat(url.path, &status) == 0 { return true }
        let failure = errno
        if failure == ENOENT { return false }
        throw POSIXError(POSIXErrorCode(rawValue: failure) ?? .EIO)
    }

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
/// The document is read from disk by `refresh()`, once per sweep, and held in memory in between: lookups never touch the
/// disk, and every write replaces the file and then the held copy. The file stays bounded because `prune(keeping:)`
/// drops entries whose audio the core no longer lists.
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

    struct Key: Hashable {
        let itemID: String
        let audioReference: String
    }

    private let fileURL: URL
    private let fileSystem: AudioRetentionFileSystem
    private var held: Document?

    init(layout: ProtectedStorageLayout, fileSystem: AudioRetentionFileSystem) {
        fileURL = layout.directory(for: .configuration).appendingPathComponent(Self.fileName)
        self.fileSystem = fileSystem
    }

    /// Reads the file again, replacing the held copy. Throws when the file exists but cannot be read.
    func refresh() throws {
        held = try load()
    }

    func entry(itemID: String, audioReference: String) throws -> Entry? {
        try document().entries.first { $0.itemID == itemID && $0.audioReference == audioReference }
    }

    func allEntries() throws -> [Entry] {
        try document().entries
    }

    /// Forgets every entry whose item and audio are not in `keys`, writing only when something changed. Entries that
    /// are kept are never altered.
    func prune(keeping keys: Set<Key>) throws {
        var current = try document()
        let before = current.entries.count
        current.entries.removeAll { !keys.contains(Key(itemID: $0.itemID, audioReference: $0.audioReference)) }
        if current.entries.count != before { try save(current) }
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
        var current = try document()
        current.entries.removeAll { $0.itemID == itemID && $0.audioReference == audioReference }
        try save(current)
    }

    // MARK: private

    private func update(itemID: String, audioReference: String, _ change: (inout Entry) -> Void) throws {
        var current = try document()
        if let index = current.entries.firstIndex(where: { $0.itemID == itemID && $0.audioReference == audioReference }) {
            change(&current.entries[index])
        } else {
            var entry = Entry(
                itemID: itemID, audioReference: audioReference, state: .failing, failedAttempts: 0, lastFailure: nil)
            change(&entry)
            current.entries.append(entry)
        }
        try save(current)
    }

    private func document() throws -> Document {
        if let held { return held }
        let loaded = try load()
        held = loaded
        return loaded
    }

    private func load() throws -> Document {
        guard try fileSystem.isPresent(at: fileURL) else { return Document(formatVersion: Self.currentFormatVersion, entries: []) }
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
        held = sorted
    }
}
