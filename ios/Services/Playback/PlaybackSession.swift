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

/// A player that can say it played to the end. `PlaybackSession` ends itself when it does, which releases the file.
protocol FinishNotifyingPlayer: PlaybackPlayer {
    func setFinishHandler(_ handler: @escaping () -> Void)
}

#if canImport(AVFoundation)
final class AVPlaybackPlayer: NSObject, FinishNotifyingPlayer, AVAudioPlayerDelegate {
    private let player: AVAudioPlayer
    private let handlerLock = NSLock()
    private var finishHandler: (() -> Void)?

    init(url: URL) throws {
        player = try AVAudioPlayer(contentsOf: url)
        super.init()
        player.delegate = self
        guard player.prepareToPlay() else { throw PlaybackError.audioUnreadable }
    }

    func setFinishHandler(_ handler: @escaping () -> Void) {
        handlerLock.lock()
        finishHandler = handler
        handlerLock.unlock()
    }

    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        handlerLock.lock()
        let handler = finishHandler
        handlerLock.unlock()
        handler?()
    }

    func play() -> Bool { player.play() }
    func pause() { player.pause() }
    func stop() { player.stop() }
}

let avPlaybackPlayerFactory: PlaybackPlayerFactory = { url in try AVPlaybackPlayer(url: url) }
#endif

/// One item's retained audio, open for playback. It holds the audio in place while open and exposes no path: the
/// listener can play, pause and close, and nothing else. The session ends by `close()`, by the item's deletion being
/// requested, by the clip playing to its end, by the read session relocking or by the service shutting down;
/// afterwards `play()` does nothing and `endReason` says why.
public final class PlaybackSession: @unchecked Sendable {
    public let itemID: String

    private let lock = NSLock()
    private var player: PlaybackPlayer?
    private var ended: PlaybackEndReason?
    private let release: () -> Void
    private let audioSession: PlaybackAudioSessionControlling
    private let isRecordingActive: () -> Bool
    private let notificationCenter: NotificationCenter
    private var interruptionObserver: NSObjectProtocol?
    private var interruptionHandler: (() -> Void)?
    /// Whether this session holds the audio session active. Cleared by an interruption, which deactivates it.
    private var audioSessionActive = false

    init(
        itemID: String, player: PlaybackPlayer, audioSession: PlaybackAudioSessionControlling = NoPlaybackAudioSession(),
        isRecordingActive: @escaping () -> Bool = { false }, notificationCenter: NotificationCenter = .default,
        release: @escaping () -> Void
    ) {
        self.itemID = itemID
        self.player = player
        self.audioSession = audioSession
        self.isRecordingActive = isRecordingActive
        self.notificationCenter = notificationCenter
        self.release = release
        (player as? FinishNotifyingPlayer)?.setFinishHandler { [weak self] in self?.end(.finished) }
        interruptionObserver = notificationCenter.addObserver(
            forName: .playbackAudioInterruption, object: nil, queue: nil
        ) { [weak self] notification in self?.interruptionNotified(notification) }
    }

    /// Called once when an interruption begins while the session is open. The player has been paused by then.
    func setInterruptionHandler(_ handler: @escaping () -> Void) {
        lock.lock()
        interruptionHandler = handler
        lock.unlock()
    }

    private func interruptionNotified(_ notification: Notification) {
        let rawType = notification.userInfo?["AVAudioSessionInterruptionTypeKey"] as? UInt
        // `AVAudioSession.InterruptionType.began` is 1.
        guard rawType == 1 else { return }
        lock.lock()
        guard let current = player else {
            lock.unlock()
            return
        }
        // The system deactivated the session; the next play configures and activates it again.
        audioSessionActive = false
        current.pause()
        let handler = interruptionHandler
        lock.unlock()
        handler?()
    }

    public var isOpen: Bool { endReason == nil }

    public var endReason: PlaybackEndReason? {
        lock.lock()
        defer { lock.unlock() }
        return ended
    }

    /// Starts or resumes playback. Returns false when the session has ended, a recording is in progress, the audio
    /// session could not be configured or activated (the player is then never started), or the output refused to start.
    @discardableResult
    public func play() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let current = player else { return false }
        guard !isRecordingActive() else { return false }
        if !audioSessionActive {
            do {
                try audioSession.setPlaybackCategory()
                try audioSession.activate()
            } catch {
                return false
            }
            audioSessionActive = true
        }
        return current.play()
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
        let wasActive = audioSessionActive
        audioSessionActive = false
        let observer = interruptionObserver
        interruptionObserver = nil
        lock.unlock()
        if let observer { notificationCenter.removeObserver(observer) }
        ending?.stop()
        if wasActive { audioSession.deactivate() }
        release()
    }
}
