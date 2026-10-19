import Foundation

/// Why open playback ended before the listener closed it.
public enum PlaybackEndReason: Equatable, Sendable {
    /// The listener (or the app) closed the session.
    case closed
    /// The read session relocked or the service shut down.
    case relocked
    /// A deletion of the item was requested.
    case deletionRequested
    /// The clip played to its end.
    case finished
}

/// A removal was refused because the file is open for playback.
enum PlaybackFenceError: Error, Equatable {
    case audioInUse
    /// A deletion was requested for the item after the open began, so the open must not take a lease.
    case deletionRequested
}

/// Tracks which finalized audio files are open for playback, so removal can leave them alone and playback can be ended
/// when something stronger than the listener decides.
///
/// Admission and removal run under one lock: a file is either admitted (and then protected from `withUnheld`) or
/// removed first (and then admission's check finds it gone); never both. A deletion bumps the item's generation, so an
/// open that read the audio reference before the deletion cannot take a lease after it.
final class PlaybackFence: @unchecked Sendable {
    private struct Entry {
        let itemID: String
        let path: String
        let end: (PlaybackEndReason) -> Void
    }

    private let lock = NSLock()
    private var entries: [UUID: Entry] = [:]
    private var generations: [String: UInt64] = [:]

    /// Read before the audio reference is read; handed back to `admit`.
    func generation(of itemID: String) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return generations[itemID, default: 0]
    }

    /// Registers an open playback of `url` for `itemID`. `verify` runs under the lock, so what it observes about the
    /// file holds until the lease is released. `end` is called (outside the lock) when the fence ends this playback.
    func admit(
        itemID: String, url: URL, generation: UInt64, verify: () throws -> Void,
        end: @escaping (PlaybackEndReason) -> Void
    ) throws -> UUID {
        lock.lock()
        defer { lock.unlock() }
        guard generations[itemID, default: 0] == generation else { throw PlaybackFenceError.deletionRequested }
        try verify()
        let id = UUID()
        entries[id] = Entry(itemID: itemID, path: url.standardizedFileURL.path, end: end)
        return id
    }

    func release(_ id: UUID) {
        lock.lock()
        entries.removeValue(forKey: id)
        lock.unlock()
    }

    func isHeld(_ url: URL) -> Bool {
        let path = url.standardizedFileURL.path
        lock.lock()
        defer { lock.unlock() }
        return entries.values.contains { $0.path == path }
    }

    /// Runs `removal` only when no playback holds `url`, and keeps new playback of it out until `removal` returns.
    func withUnheld<Result>(_ url: URL, _ removal: () throws -> Result) throws -> Result {
        let path = url.standardizedFileURL.path
        lock.lock()
        defer { lock.unlock() }
        guard !entries.values.contains(where: { $0.path == path }) else { throw PlaybackFenceError.audioInUse }
        return try removal()
    }

    /// A deletion of `itemID` was requested: ends its playback and refuses opens already under way.
    func endPlayback(itemID: String) {
        lock.lock()
        generations[itemID, default: 0] += 1
        let ending = entries.values.filter { $0.itemID == itemID }
        lock.unlock()
        ending.forEach { $0.end(.deletionRequested) }
    }

    func endAll(reason: PlaybackEndReason) {
        lock.lock()
        let ending = Array(entries.values)
        lock.unlock()
        ending.forEach { $0.end(reason) }
    }
}

/// Refuses to remove a file that is open for playback. Every other operation is the wrapped file system's.
struct PlaybackFencedFileSystem: AudioRetentionFileSystem {
    let base: AudioRetentionFileSystem
    let fence: PlaybackFence

    init(base: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem(), fence: PlaybackFence) {
        self.base = base
        self.fence = fence
    }

    func fileExists(at url: URL) -> Bool { base.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try base.fileSize(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try base.entryNames(in: directory) }
    func read(from url: URL) throws -> Data { try base.read(from: url) }
    func writeDurably(_ data: Data, to url: URL) throws { try base.writeDurably(data, to: url) }
    func move(from source: URL, to destination: URL) throws { try base.move(from: source, to: destination) }
    func isPresent(at url: URL) throws -> Bool { try base.isPresent(at: url) }
    func replaceDurably(_ data: Data, at url: URL) throws { try base.replaceDurably(data, at: url) }

    func remove(at url: URL) throws {
        try fence.withUnheld(url) { try base.remove(at: url) }
    }
}
