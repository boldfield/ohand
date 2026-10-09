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
    /// True only when the last frames of `whole` are exactly all of `tail`'s frames, sample for sample. Used to tell that
    /// an added segment was already joined onto its recording before the process died; false whenever unsure.
    var endsWithAudio: (_ whole: URL, _ tail: URL) -> Bool

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
            replaceFile: { original, replacement in replaceFileAtomically(original, with: replacement) },
            endsWithAudio: { whole, tail in audio(whole, endsWith: tail) })
    }

    /// Compares the last frames of `whole` with all of `tail` in bounded chunks, byte for byte.
    static func audio(_ whole: URL, endsWith tail: URL) -> Bool {
        guard let wholeFile = try? AVAudioFile(forReading: whole, commonFormat: .pcmFormatInt16, interleaved: true),
              let tailFile = try? AVAudioFile(forReading: tail, commonFormat: .pcmFormatInt16, interleaved: true),
              wholeFile.processingFormat.sampleRate == tailFile.processingFormat.sampleRate,
              wholeFile.processingFormat.channelCount == tailFile.processingFormat.channelCount,
              tailFile.length > 0, wholeFile.length >= tailFile.length else {
            return false
        }
        wholeFile.framePosition = wholeFile.length - tailFile.length
        let chunkFrames: AVAudioFrameCount = 16_384
        guard let wholeBuffer = AVAudioPCMBuffer(pcmFormat: wholeFile.processingFormat, frameCapacity: chunkFrames),
              let tailBuffer = AVAudioPCMBuffer(pcmFormat: tailFile.processingFormat, frameCapacity: chunkFrames) else {
            return false
        }
        while tailFile.framePosition < tailFile.length {
            let remaining = tailFile.length - tailFile.framePosition
            let frames = AVAudioFrameCount(min(Int64(chunkFrames), remaining))
            do {
                try wholeFile.read(into: wholeBuffer, frameCount: frames)
                try tailFile.read(into: tailBuffer, frameCount: frames)
            } catch {
                return false
            }
            guard wholeBuffer.frameLength == frames, tailBuffer.frameLength == frames else { return false }
            let wholeBuffers = UnsafeMutableAudioBufferListPointer(wholeBuffer.mutableAudioBufferList)
            let tailBuffers = UnsafeMutableAudioBufferListPointer(tailBuffer.mutableAudioBufferList)
            guard wholeBuffers.count == tailBuffers.count else { return false }
            for index in 0..<wholeBuffers.count {
                let wholeBytes = wholeBuffers[index]
                let tailBytes = tailBuffers[index]
                guard wholeBytes.mDataByteSize == tailBytes.mDataByteSize,
                      let wholeData = wholeBytes.mData, let tailData = tailBytes.mData,
                      memcmp(wholeData, tailData, Int(wholeBytes.mDataByteSize)) == 0 else {
                    return false
                }
            }
        }
        return true
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
