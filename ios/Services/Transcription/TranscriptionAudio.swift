import CryptoKit
import Foundation

/// Resolves the audio reference a capture stores to the file the recognizer reads.
public protocol TranscriptionAudioLocating: Sendable {
    /// The file the reference names, or nil when it does not name a file directly inside the finalized audio store.
    func audioURL(forReference reference: String) -> URL?
}

/// Whether a recording is held in place by something the transcription handler must not read around: playback.
/// Removal of the file is fenced by the retention service, not here (see `TranscriptionJobHandler`).
public protocol TranscriptionAudioFencing: Sendable {
    /// True while the file at `url` is open for playback.
    func isAudioInUse(at url: URL) -> Bool
}

/// No audio is ever held. The default for a handler that has no playback to wait for.
public struct NoTranscriptionAudioFence: TranscriptionAudioFencing {
    public init() {}

    public func isAudioInUse(at url: URL) -> Bool { false }
}

/// The playback fence the playback service takes a lease in while an item's audio is open.
extension PlaybackFence: TranscriptionAudioFencing {
    func isAudioInUse(at url: URL) -> Bool { isHeld(url) }
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
