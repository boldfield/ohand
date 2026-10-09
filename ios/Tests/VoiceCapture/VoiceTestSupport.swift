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
        let audioFile = try Self.makeWriter(at: destination)
        if framesWritten > 0 { try Self.write(frames: framesWritten, to: audioFile) }
        startCount += 1
        lastMaxDuration = maxDuration
        self.destination = destination
        openFile = audioFile
    }

    static func makeWriter(at destination: URL) throws -> AVAudioFile {
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false,
        ]
        return try AVAudioFile(forWriting: destination, settings: settings, commonFormat: .pcmFormatInt16, interleaved: true)
    }

    static func write(frames: AVAudioFrameCount, to audioFile: AVAudioFile) throws {
        let buffer = AVAudioPCMBuffer(pcmFormat: audioFile.processingFormat, frameCapacity: frames)!
        buffer.frameLength = frames
        let samples = buffer.int16ChannelData![0]
        for index in 0..<Int(frames) { samples[index] = Int16(truncatingIfNeeded: index % 100) }
        try audioFile.write(from: buffer)
    }

    /// A closed recording file as a killed or cancelled session leaves it, with no controller or engine involved.
    static func writeClosedRecording(at destination: URL, frames: AVAudioFrameCount) throws {
        let audioFile = try makeWriter(at: destination)
        if frames > 0 { try write(frames: frames, to: audioFile) }
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

/// What the app assembly does for restart recovery: run the ingress writer's recovery pass and report it in the
/// recorder's terms. Ownership of the in-progress files is reported as unknown whenever ingress could not establish it.
final class IngressVoiceRecovery: VoiceRecoveryIngress {
    private let service: ForegroundIngressService
    private(set) var recoveryPasses = 0

    init(service: ForegroundIngressService) {
        self.service = service
    }

    func recoverStagedCaptures(completion: @escaping (VoiceIngressRecoveryReport) -> Void) {
        recoveryPasses += 1
        service.recover { completion(Self.report(from: $0)) }
    }

    static func report(from ingress: IngressRecoveryReport) -> VoiceIngressRecoveryReport {
        var report = VoiceIngressRecoveryReport()
        for entry in ingress.entries {
            switch entry.outcome {
            case .saved:
                report.confirmedCaptureIDs.append(entry.captureID)
            case .itemDeleted:
                report.deletedItemCaptureIDs.append(entry.captureID)
            case .keptForRetry(_, let problem):
                report.pending.append(VoicePendingCapture(captureID: entry.captureID, reason: reason(for: problem)))
            case .notStaged:
                report.pending.append(VoicePendingCapture(captureID: entry.captureID, reason: .audioUnavailable))
            }
        }
        let ownershipKnown = ingress.listingFailure == nil && ingress.unclaimedAudioListingFailure == nil
            && !ingress.unclaimedAudioNotEvaluated
        report.unclaimedInProgressFileNames = ownershipKnown ? ingress.unclaimedInProgressAudio : nil
        return report
    }

    private static func reason(for problem: IngressPendingProblem) -> VoicePendingReason {
        switch problem {
        case .notCommitted, .commitUnknown, .coreUnavailable, .conflictingReuse: return .importUnconfirmed
        case .rejected(let code): return .coreRejected(code: code)
        case .sourceUnavailable: return .audioUnavailable
        case .recordUnreadable: return .stagingRecordUnreadable
        }
    }
}

enum UnexpectedOutcomeError: Error { case startSucceeded, notSaved }

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
    var joinShouldFail = false
    var replaceShouldFail = false
    private(set) var protectedURLs: [URL] = []
    private(set) var outcomes: [VoiceCaptureOutcome] = []
    private(set) var states: [VoiceRecorderState] = []
    private var captureCounter = 0

    var service: ForegroundIngressService!
    var handoff: IngressVoiceHandoff!
    var controller: VoiceRecordingController!
    var recovery: IngressVoiceRecovery!
    var coordinator: VoiceRecoveryCoordinator!

    override func setUpWithError() throws {
        try super.setUpWithError()
        service = try makeService(importer: importer, fileSystem: failingFileSystem)
        handoff = IngressVoiceHandoff(service: service, context: context)
        controller = makeController()
        recovery = IngressVoiceRecovery(service: service)
        coordinator = makeCoordinator()
    }

    override func tearDown() {
        coordinator = nil
        recovery = nil
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
            removeFile: { try? FileManager.default.removeItem(at: $0) },
            modificationDate: { url in
                let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                return attributes?[.modificationDate] as? Date
            },
            joinAudio: { [unowned self] first, second, destination in
                if joinShouldFail { return false }
                return VoiceRecordingEnvironment.joinAudioFiles(first, second, to: destination)
            },
            replaceFile: { [unowned self] original, replacement in
                if replaceShouldFail { return false }
                return VoiceRecordingEnvironment.replaceFileAtomically(original, with: replacement)
            },
            endsWithAudio: { whole, tail in VoiceRecordingEnvironment.audio(whole, endsWith: tail) })
    }

    func makeCoordinator(
        limits: VoiceRecordingLimits = VoiceRecordingLimits(),
        maxListedRecordings: Int = 20
    ) -> VoiceRecoveryCoordinator {
        VoiceRecoveryCoordinator(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
            ingress: recovery,
            handoff: handoff,
            controller: controller,
            environment: testEnvironment,
            limits: limits,
            maxListedRecordings: maxListedRecordings)
    }

    /// Leaves a closed recording in the in-progress store the way a cancelled or killed session does.
    @discardableResult
    func leaveRecording(_ captureID: String, frames: AVAudioFrameCount = 8000, modifiedAt: Date? = nil) throws -> URL {
        let url = recordingFile(captureID)
        try SyntheticVoiceEngine.writeClosedRecording(at: url, frames: frames)
        if let modifiedAt = modifiedAt {
            try FileManager.default.setAttributes([.modificationDate: modifiedAt], ofItemAtPath: url.path)
        }
        return url
    }

    func refreshListing() throws -> VoiceRecoveryListing {
        var listing: VoiceRecoveryListing?
        coordinator.refresh { listing = $0 }
        return try XCTUnwrap(listing, "refresh did not complete")
    }

    func audioFrames(_ url: URL) throws -> Int64 {
        try AVAudioFile(forReading: url).length
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
        case .success:
            XCTFail("start unexpectedly succeeded")
            throw UnexpectedOutcomeError.startSucceeded
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

    func recordingFile(_ captureID: String) -> URL { inProgressAudioURL("\(captureID).wav") }
    func finalizedRecording(_ captureID: String) -> URL { finalizedAudioURL("\(captureID).wav") }
}
