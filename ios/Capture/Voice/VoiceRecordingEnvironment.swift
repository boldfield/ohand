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
    var modificationDate: (URL) -> Date?
    /// Writes `first` followed by `second` into the new file `destination`. Neither input is touched. False when the
    /// destination could not be fully written; the caller verifies it by reading it back.
    var joinAudio: (_ first: URL, _ second: URL, _ destination: URL) -> Bool
    /// Atomically puts the contents of `replacement` at `original`. False when `original` was left as it was.
    var replaceFile: (_ original: URL, _ replacement: URL) -> Bool

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
            removeFile: { try? FileManager.default.removeItem(at: $0) },
            modificationDate: { url in
                let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                return attributes?[.modificationDate] as? Date
            },
            joinAudio: { first, second, destination in joinAudioFiles(first, second, to: destination) },
            replaceFile: { original, replacement in replaceFileAtomically(original, with: replacement) })
    }

    /// Copies two recordings written by this recorder (same sample rate, one channel) into one 16-bit PCM file in
    /// bounded chunks.
    static func joinAudioFiles(_ first: URL, _ second: URL, to destination: URL) -> Bool {
        guard let firstFile = try? AVAudioFile(forReading: first, commonFormat: .pcmFormatInt16, interleaved: true),
              let secondFile = try? AVAudioFile(forReading: second, commonFormat: .pcmFormatInt16, interleaved: true),
              firstFile.processingFormat.sampleRate == secondFile.processingFormat.sampleRate,
              firstFile.processingFormat.channelCount == secondFile.processingFormat.channelCount else {
            return false
        }
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: firstFile.processingFormat.sampleRate,
            AVNumberOfChannelsKey: Int(firstFile.processingFormat.channelCount),
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false,
        ]
        guard let output = try? AVAudioFile(
            forWriting: destination, settings: settings, commonFormat: .pcmFormatInt16, interleaved: true) else {
            return false
        }
        for source in [firstFile, secondFile] {
            guard let buffer = AVAudioPCMBuffer(pcmFormat: source.processingFormat, frameCapacity: 16_384) else {
                return false
            }
            while source.framePosition < source.length {
                do {
                    try source.read(into: buffer)
                    guard buffer.frameLength > 0 else { return false }
                    try output.write(from: buffer)
                } catch {
                    return false
                }
            }
        }
        return true
    }

    static func replaceFileAtomically(_ original: URL, with replacement: URL) -> Bool {
        do {
            _ = try FileManager.default.replaceItemAt(original, withItemAt: replacement)
            return true
        } catch {
            return false
        }
    }
}
