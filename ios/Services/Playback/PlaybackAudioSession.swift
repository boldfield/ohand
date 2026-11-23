import Foundation

#if canImport(AVFoundation)
import AVFoundation
#endif

/// The three audio-session calls playback needs. `PlaybackSession` is the only caller: it configures before the first
/// play of an open session and deactivates when the session ends. The `AVAudioSession` implementation is
/// `AVPlaybackAudioSession`; tests record the calls.
protocol PlaybackAudioSessionControlling: AnyObject, Sendable {
    /// Category `.playback`, mode `.spokenAudio`: audible with the silent switch on, and an output route exists even
    /// after the recorder left the category at `.record`.
    func setPlaybackCategory() throws
    func activate() throws
    /// Deactivates with `.notifyOthersOnDeactivation`. A failure is not actionable once playback has ended.
    func deactivate()
}

/// Leaves the audio session alone. For callers that build a service without a device session (tests, probes).
final class NoPlaybackAudioSession: PlaybackAudioSessionControlling {
    func setPlaybackCategory() throws {}
    func activate() throws {}
    func deactivate() {}
}

#if canImport(AVFoundation)
final class AVPlaybackAudioSession: PlaybackAudioSessionControlling {
    func setPlaybackCategory() throws {
        try AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
    }

    func activate() throws {
        try AVAudioSession.sharedInstance().setActive(true)
    }

    func deactivate() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}

extension Notification.Name {
    /// Posted when playback's audio session is interrupted or the interruption ends.
    static let playbackAudioInterruption = AVAudioSession.interruptionNotification
}
#else
extension Notification.Name {
    static let playbackAudioInterruption = Notification.Name("AVAudioSessionInterruptionNotification")
}
#endif
