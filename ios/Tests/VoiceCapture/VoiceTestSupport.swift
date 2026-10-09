import AVFoundation
import Foundation
import UIKit
import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Writes a real 16 kHz mono PCM WAV prefix on start so the controller's read-back runs against genuine files. Faults
/// a simulator cannot produce (finish failure, corrupt close, engine events, refused start) are injected here.
final class SyntheticVoiceEngine: VoiceCaptureEngine {
    enum CloseBehavior {
        case clean
        case reportFailure
        case corruptFile
        case truncateToEmpty
    }

    var onEvent: ((VoiceEngineEvent) -> Void)?
    var startError: VoiceEngineStartError?
    /// When set, an empty file is created before the start fails, as a recorder that fails after opening its file would.
    var createsEmptyFileBeforeFailingToStart = false
    var framesWritten: AVAudioFrameCount = 8000
    var closeBehavior = CloseBehavior.clean
    /// Runs inside `stopAndClose`, where a real recorder spins the run loop and other events can arrive.
    var duringClose: (() -> Void)?
    private(set) var startCount = 0
    private(set) var closeCount = 0
    private(set) var lastMaxDuration: TimeInterval?
    private var destination: URL?
    private var openFile: AVAudioFile?

    func start(writingTo destination: URL, maxDuration: TimeInterval) throws {
        if let startError = startError {
            if createsEmptyFileBeforeFailingToStart { FileManager.default.createFile(atPath: destination.path, contents: nil) }
            throw startError
        }
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false,
        ]
        let audioFile = try AVAudioFile(
            forWriting: destination, settings: settings, commonFormat: .pcmFormatInt16, interleaved: true)
        if framesWritten > 0 {
            let buffer = AVAudioPCMBuffer(pcmFormat: audioFile.processingFormat, frameCapacity: framesWritten)!
            buffer.frameLength = framesWritten
            let samples = buffer.int16ChannelData![0]
            for index in 0..<Int(framesWritten) { samples[index] = Int16(truncatingIfNeeded: index % 100) }
            try audioFile.write(from: buffer)
        }
        startCount += 1
        lastMaxDuration = maxDuration
        self.destination = destination
        openFile = audioFile
    }

    func stopAndClose() -> Bool {
        closeCount += 1
        duringClose?()
        openFile = nil
        switch closeBehavior {
        case .clean:
            return true
        case .reportFailure:
            return false
        case .corruptFile:
            if let destination = destination { try? Data([0x00, 0x01, 0x02]).write(to: destination) }
            return true
        case .truncateToEmpty:
            if let destination = destination { try? Data().write(to: destination) }
            return true
        }
    }
}

final class FakeMicrophonePermission: MicrophonePermissionProviding {
    var status: MicrophonePermissionStatus
    /// Answer given to a prompt at once; nil holds the prompt until `answerPending`.
    var promptAnswer: Bool?
    private(set) var requestCount = 0
    private var pendingCompletion: ((Bool) -> Void)?

    init(status: MicrophonePermissionStatus = .granted) {
        self.status = status
    }

    func request(completion: @escaping (Bool) -> Void) {
        requestCount += 1
        guard let answer = promptAnswer else {
            pendingCompletion = completion
            return
        }
        status = answer ? .granted : .denied
        completion(answer)
    }

    func answerPending(_ answer: Bool) {
        status = answer ? .granted : .denied
        let completion = pendingCompletion
        pendingCompletion = nil
        completion?(answer)
    }
}

/// Holds the poll handler so a test decides when a poll happens.
final class ManualPoller {
    private(set) var handler: (() -> Void)?
    private(set) var scheduledInterval: TimeInterval?
    private(set) var cancelCount = 0

    func schedule(interval: TimeInterval, handler: @escaping () -> Void) -> () -> Void {
        scheduledInterval = interval
        self.handler = handler
        return { [unowned self] in
            self.handler = nil
            self.cancelCount += 1
        }
    }

    var isPolling: Bool { handler != nil }

    func tick() {
        handler?()
    }
}

/// What the app assembly does with the ingress writer: turn a closed recording into an ingress record and report the
/// ingress outcome in the recorder's terms.
final class IngressVoiceHandoff: VoiceRecordingHandoffReceiving {
    private let service: ForegroundIngressService
    private let baseContext: IngressCaptureContext
    private(set) var handoffs: [VoiceRecordingHandoff] = []

    init(service: ForegroundIngressService, context: IngressCaptureContext) {
        self.service = service
        self.baseContext = context
    }

    func handOff(_ handoff: VoiceRecordingHandoff, completion: @escaping (VoiceHandoffResult) -> Void) {
        handoffs.append(handoff)
        var context = baseContext
        context.captureInstant = ISO8601DateFormatter().string(from: handoff.startedAt)
        let record = IngressRecord(
            captureID: handoff.captureID,
            text: nil,
            audio: IngressAudioHandoff(
                inProgressFileName: handoff.inProgressFileName, finalizedFileName: handoff.finalizedFileName),
            context: context)
        service.submit(record) { completion(Self.result(for: $0)) }
    }

    static func result(for outcome: IngressOutcome) -> VoiceHandoffResult {
        switch outcome {
        case .saved(let acknowledgment):
            return .saved(
                itemID: acknowledgment.itemID, savedAt: acknowledgment.savedAt,
                alreadyImported: acknowledgment.alreadyImported)
        case .keptForRetry:
            return .keptForRetry
        case .notStaged:
            return .notStaged
        case .itemDeleted:
            return .itemDeleted
        }
    }
}

/// A protected storage tree, a scripted ingress importer behind the real ingress service, and a recording controller
/// wired to a synthetic engine, a private notification center and a manual poller.
class VoiceCaptureTestCase: IngressStorageTestCase {
    let engine = SyntheticVoiceEngine()
    let permission = FakeMicrophonePermission()
    let notificationCenter = NotificationCenter()
    let poller = ManualPoller()
    let importer = FakeForegroundIngressImporter()
    let failingFileSystem = FailingIngressFileSystem()

    var now = Date(timeIntervalSince1970: 1_790_000_000)
    var isForeground = true
    var freeSpace: Int64? = 1_000_000_000
    var protectionError: Error?
    private(set) var protectedURLs: [URL] = []
    private(set) var outcomes: [VoiceCaptureOutcome] = []
    private(set) var states: [VoiceRecorderState] = []
    private var captureCounter = 0

    var service: ForegroundIngressService!
    var handoff: IngressVoiceHandoff!
    var controller: VoiceRecordingController!

    override func setUpWithError() throws {
        try super.setUpWithError()
        service = try makeService(importer: importer, fileSystem: failingFileSystem)
        handoff = IngressVoiceHandoff(service: service, context: context)
        controller = makeController()
    }

    override func tearDown() {
        controller = nil
        handoff = nil
        service = nil
        super.tearDown()
    }

    var testEnvironment: VoiceRecordingEnvironment {
        VoiceRecordingEnvironment(
            now: { [unowned self] in now },
            isForeground: { [unowned self] in isForeground },
            freeSpaceBytes: { [unowned self] _ in freeSpace },
            fileSizeBytes: { url in
                let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                return (attributes?[.size] as? NSNumber)?.int64Value
            },
            inspectAudio: { url in
                guard let audioFile = try? AVAudioFile(forReading: url), audioFile.length > 0 else { return nil }
                return VoiceAudioInspection(frames: audioFile.length, sampleRate: audioFile.processingFormat.sampleRate)
            },
            protectRecordingFile: { [unowned self] url in
                protectedURLs.append(url)
                if let protectionError = protectionError { throw protectionError }
            },
            schedulePoll: { [unowned self] interval, handler in poller.schedule(interval: interval, handler: handler) },
            removeFile: { try? FileManager.default.removeItem(at: $0) })
    }

    func makeController(limits: VoiceRecordingLimits = VoiceRecordingLimits()) -> VoiceRecordingController {
        let newController = VoiceRecordingController(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
            engine: engine,
            permission: permission,
            handoff: handoff,
            environment: testEnvironment,
            limits: limits,
            notificationCenter: notificationCenter,
            makeCaptureID: { [unowned self] in
                captureCounter += 1
                return "voice-test-\(captureCounter)"
            })
        newController.onOutcome = { [unowned self] in outcomes.append($0) }
        newController.onStateChange = { [unowned self] in states.append($0) }
        return newController
    }

    func startRecording() throws -> VoiceRecordingStarted {
        var result: Result<VoiceRecordingStarted, VoiceStartFailure>?
        controller.start { result = $0 }
        return try XCTUnwrap(result, "start did not complete").get()
    }

    func startFailure() throws -> VoiceStartFailure {
        var result: Result<VoiceRecordingStarted, VoiceStartFailure>?
        controller.start { result = $0 }
        switch try XCTUnwrap(result, "start did not complete") {
        case .success: throw XCTSkip("start unexpectedly succeeded")
        case .failure(let failure): return failure
        }
    }

    func postInterruption(_ type: AVAudioSession.InterruptionType) {
        notificationCenter.post(
            name: AVAudioSession.interruptionNotification, object: nil,
            userInfo: [AVAudioSessionInterruptionTypeKey: type.rawValue])
    }

    func postBackground() {
        notificationCenter.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
    }

    func postDeviceLocking() {
        notificationCenter.post(name: UIApplication.protectedDataWillBecomeUnavailableNotification, object: nil)
    }

    func confirmNextImport(_ captureID: String) {
        importer.results = [FakeForegroundIngressImporter.confirmation(captureID)]
    }

    func lastOutcome() throws -> VoiceCaptureOutcome {
        try XCTUnwrap(outcomes.last, "no outcome was delivered")
    }

    func summary(of outcome: VoiceCaptureOutcome) throws -> VoiceRecordingSummary {
        switch outcome {
        case .saved(let summary, _), .keptForRetry(let summary, _), .retainedUnsubmitted(let summary), .itemDeleted(let summary):
            return summary
        case .unrecoverable:
            throw XCTSkip("outcome has no summary: \(outcome)")
        }
    }

    func recordingFile(_ captureID: String) -> URL { inProgressAudioURL("\(captureID).wav") }
    func finalizedRecording(_ captureID: String) -> URL { finalizedAudioURL("\(captureID).wav") }
}
