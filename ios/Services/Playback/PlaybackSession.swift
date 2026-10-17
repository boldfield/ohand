import Foundation

#if canImport(AVFoundation)
import AVFoundation
#endif

/// The audio output for one open playback. `PlaybackSession` is the only owner; the file behind it is never exposed.
protocol PlaybackPlayer: AnyObject {
    func play() -> Bool
    func pause()
    func stop()
}

/// Builds a player for a finalized audio file, or throws when the file cannot be played.
typealias PlaybackPlayerFactory = @Sendable (URL) throws -> PlaybackPlayer

#if canImport(AVFoundation)
final class AVPlaybackPlayer: PlaybackPlayer {
    private let player: AVAudioPlayer

    init(url: URL) throws {
        player = try AVAudioPlayer(contentsOf: url)
        guard player.prepareToPlay() else { throw PlaybackError.audioUnreadable }
    }

    func play() -> Bool { player.play() }
    func pause() { player.pause() }
    func stop() { player.stop() }
}

let avPlaybackPlayerFactory: PlaybackPlayerFactory = { url in try AVPlaybackPlayer(url: url) }
#endif

/// One item's retained audio, open for playback. It holds the audio in place while open and exposes no path: the
/// listener can play, pause and close, and nothing else. The session ends by `close()`, by the item's deletion being
/// requested, by the read session relocking or by the service shutting down; afterwards `play()` does nothing and
/// `endReason` says why.
public final class PlaybackSession: @unchecked Sendable {
    public let itemID: String

    private let lock = NSLock()
    private var player: PlaybackPlayer?
    private var ended: PlaybackEndReason?
    private let release: () -> Void

    init(itemID: String, player: PlaybackPlayer, release: @escaping () -> Void) {
        self.itemID = itemID
        self.player = player
        self.release = release
    }

    public var isOpen: Bool { endReason == nil }

    public var endReason: PlaybackEndReason? {
        lock.lock()
        defer { lock.unlock() }
        return ended
    }

    /// Starts or resumes playback. Returns false when the session has ended or the output refused to start.
    @discardableResult
    public func play() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return player?.play() ?? false
    }

    public func pause() {
        lock.lock()
        defer { lock.unlock() }
        player?.pause()
    }

    public func close() {
        end(.closed)
    }

    func end(_ reason: PlaybackEndReason) {
        lock.lock()
        guard ended == nil else {
            lock.unlock()
            return
        }
        ended = reason
        let ending = player
        player = nil
        lock.unlock()
        ending?.stop()
        release()
    }
}
