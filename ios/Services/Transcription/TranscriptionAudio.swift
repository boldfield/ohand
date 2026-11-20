import CryptoKit
import Foundation

/// Resolves the audio reference a capture stores to the file the recognizer reads.
public protocol TranscriptionAudioLocating: Sendable {
    /// The file the reference names, or nil when it does not name a file directly inside the finalized audio store.
    func audioURL(forReference reference: String) -> URL?
}

/// The fence the transcription handler shares with playback and with audio removal: a recording open for playback is
/// waited for, and a recording being read is kept from removal. Removal itself is done by the retention service under
/// the same fence (`PlaybackFence.withUnheld`).
public protocol TranscriptionAudioFencing: Sendable {
    /// True while the file at `url` is open for playback.
    func isAudioInUse(at url: URL) -> Bool

    /// The deletion generation of `itemID`. Read as soon as the job knows its item, before the audio is resolved, and
    /// handed back to `holdAgainstRemoval`.
    func removalGeneration(of itemID: String) -> UInt64

    /// Keeps removal of the file at `url` (the audio of `itemID`) out until the returned release is called, and returns
    /// nil when the file is not there to be read or a deletion of the item was requested since `generation` was read.
    /// A removal that arrives while the lease is held is refused as audio in use, so the file never disappears under a
    /// digest or a recognition; it is retried by its own caller. A deletion requested while the lease is held ends it.
    func holdAgainstRemoval(itemID: String, at url: URL, generation: UInt64) -> (@Sendable () -> Void)?
}

/// No audio is ever held. The default for a handler that has no playback to wait for.
public struct NoTranscriptionAudioFence: TranscriptionAudioFencing {
    public init() {}

    public func isAudioInUse(at url: URL) -> Bool { false }

    public func removalGeneration(of itemID: String) -> UInt64 { 0 }

    public func holdAgainstRemoval(itemID: String, at url: URL, generation: UInt64) -> (@Sendable () -> Void)? { {} }
}

/// The playback fence the playback service takes a lease in while an item's audio is open.
extension PlaybackFence: TranscriptionAudioFencing {
    func isAudioInUse(at url: URL) -> Bool { isHeld(url) }

    func removalGeneration(of itemID: String) -> UInt64 { generation(of: itemID) }

    func holdAgainstRemoval(itemID: String, at url: URL, generation: UInt64) -> (@Sendable () -> Void)? {
        holdAgainstRemoval(itemID: itemID, at: url, generation: generation, afterAdmission: {})
    }

    /// `afterAdmission` runs between the admission and the publication of the lease's identifier; tests use it to land
    /// a deletion in that window.
    func holdAgainstRemoval(
        itemID: String, at url: URL, generation: UInt64, afterAdmission: () -> Void
    ) -> (@Sendable () -> Void)? {
        struct AudioAbsent: Error {}
        let lease = RemovalLease()
        let identifier: UUID
        do {
            // The existence check runs under the fence's lock, so a removal either finished first (and the file is
            // absent) or waits for the release. A deletion of the item is not waited for: it ends the lease, as it
            // ends playback, and the core retires the job's late result.
            identifier = try admit(
                itemID: itemID, url: url, generation: generation,
                verify: {
                    guard FileManager.default.fileExists(atPath: url.path) else { throw AudioAbsent() }
                },
                end: { [weak self, lease] reason in
                    guard reason == .deletionRequested, let identifier = lease.end() else { return }
                    self?.release(identifier)
                })
        } catch {
            return nil
        }
        afterAdmission()
        // A deletion that ended the lease before its identifier was published found nothing to release; release now.
        guard lease.publish(identifier) else {
            release(identifier)
            return nil
        }
        return { [weak self, lease] in
            if let identifier = lease.end() { self?.release(identifier) }
        }
    }
}

/// The identifier a removal lease was admitted under, published after admission. Ending it hands the identifier to
/// exactly one caller: the deletion callback or the job's release, whichever comes first. An end that arrives before
/// publication is remembered, so the publisher releases instead.
private final class RemovalLease: @unchecked Sendable {
    private let lock = NSLock()
    private var identifier: UUID?
    private var hasEnded = false

    /// Publishes `identifier`; false when the lease already ended, in which case the caller releases it.
    func publish(_ identifier: UUID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !hasEnded else { return false }
        self.identifier = identifier
        return true
    }

    /// Ends the lease. Returns the identifier to release, or nil when it was not published yet or already ended.
    func end() -> UUID? {
        lock.lock()
        defer { lock.unlock() }
        hasEnded = true
        let ending = identifier
        identifier = nil
        return ending
    }
}

/// Resolves references against the finalized audio directory of the protected storage layout.
struct ProtectedStorageAudioLocator: TranscriptionAudioLocating {
    let layout: ProtectedStorageLayout

    func audioURL(forReference reference: String) -> URL? {
        IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
            .finalizedAudioURL(forReference: reference)
    }
}

/// What reading an audio file for its digest found.
enum TranscriptionAudioDigest: Equatable {
    case digest(String)
    /// No file exists at the location.
    case missing
    /// The file exists but cannot be read now (for example while the device is locked).
    case unreadable

    /// Lowercase hex SHA-256 of the file's bytes, read in bounded chunks.
    static func read(from url: URL) -> TranscriptionAudioDigest {
        guard FileManager.default.fileExists(atPath: url.path) else { return .missing }
        do {
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            var hasher = SHA256()
            while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
                hasher.update(data: chunk)
            }
            return .digest(hasher.finalize().map { String(format: "%02x", $0) }.joined())
        } catch {
            return .unreadable
        }
    }
}
