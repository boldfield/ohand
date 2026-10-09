import AVFoundation
import Foundation
import UIKit

struct VoiceAudioInspection: Equatable {
    var frames: Int64
    var sampleRate: Double
}

/// The clock, storage and platform facts the recording controller depends on, injected so tests can drive limits,
/// storage pressure and polling deterministically.
struct VoiceRecordingEnvironment {
    var now: () -> Date
    var isForeground: () -> Bool
    var freeSpaceBytes: (URL) -> Int64?
    var fileSizeBytes: (URL) -> Int64?
    /// Reads a closed file back as audio; nil when it holds no readable frames.
    var inspectAudio: (URL) -> VoiceAudioInspection?
    /// Applies the in-progress audio store's protection and backup policy to the new recording file.
    var protectRecordingFile: (URL) throws -> Void
    /// Repeats `handler` on the main queue every `interval` seconds and returns a function that stops it.
    var schedulePoll: (TimeInterval, @escaping () -> Void) -> () -> Void
    var removeFile: (URL) -> Void

    static func live(protectRecordingFile: @escaping (URL) throws -> Void) -> VoiceRecordingEnvironment {
        VoiceRecordingEnvironment(
            now: { Date() },
            isForeground: { UIApplication.shared.applicationState != .background },
            freeSpaceBytes: { directory in
                let values = try? directory.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
                return values?.volumeAvailableCapacityForImportantUsage
            },
            fileSizeBytes: { url in
                let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                return (attributes?[.size] as? NSNumber)?.int64Value
            },
            inspectAudio: { url in
                guard let audioFile = try? AVAudioFile(forReading: url), audioFile.length > 0 else { return nil }
                return VoiceAudioInspection(frames: audioFile.length, sampleRate: audioFile.processingFormat.sampleRate)
            },
            protectRecordingFile: protectRecordingFile,
            schedulePoll: { interval, handler in
                let timer = DispatchSource.makeTimerSource(queue: .main)
                timer.schedule(deadline: .now() + interval, repeating: interval)
                timer.setEventHandler(handler: handler)
                timer.resume()
                return { timer.cancel() }
            },
            removeFile: { try? FileManager.default.removeItem(at: $0) })
    }
}
