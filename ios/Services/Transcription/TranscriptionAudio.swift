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

    /// Keeps removal of the file at `url` (the audio of `itemID`) out until the returned release is called, and returns
    /// nil when the file is not there to be read or its item was deleted since the job began. A removal that arrives
    /// while the lease is held is refused as audio in use, so the file never disappears under a digest or a
    /// recognition; it is retried by its own caller.
    func holdAgainstRemoval(itemID: String, at url: URL) -> (@Sendable () -> Void)?
}

/// No audio is ever held. The default for a handler that has no playback to wait for.
public struct NoTranscriptionAudioFence: TranscriptionAudioFencing {
    public init() {}

    public func isAudioInUse(at url: URL) -> Bool { false }

    public func holdAgainstRemoval(itemID: String, at url: URL) -> (@Sendable () -> Void)? { {} }
}

/// The playback fence the playback service takes a lease in while an item's audio is open.
extension PlaybackFence: TranscriptionAudioFencing {
    func isAudioInUse(at url: URL) -> Bool { isHeld(url) }

    func holdAgainstRemoval(itemID: String, at url: URL) -> (@Sendable () -> Void)? {
        struct AudioAbsent: Error {}
        let lease = LeaseBox()
        do {
            // The existence check runs under the fence's lock, so a removal either finished first (and the file is
            // absent) or waits for the release. A deletion of the item is not waited for: it ends the lease, as it
            // ends playback, and the core retires the job's late result.
            lease.identifier = try admit(
                itemID: itemID, url: url, generation: generation(of: itemID),
                verify: {
                    guard FileManager.default.fileExists(atPath: url.path) else { throw AudioAbsent() }
                },
                end: { [weak self, lease] reason in
                    guard reason == .deletionRequested, let identifier = lease.identifier else { return }
                    self?.release(identifier)
                })
            return { [self, lease] in
                if let identifier = lease.identifier { release(identifier) }
            }
        } catch {
            return nil
        }
    }
}

/// Holds the identifier a lease was admitted under, so its own end callback can release it.
private final class LeaseBox: @unchecked Sendable {
    var identifier: UUID?
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
