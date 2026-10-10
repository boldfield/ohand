import Foundation

/// Finds recordings that were never handed to ingress (cancelled, or cut off before handoff) and carries out the
/// user's explicit choice for exactly one of them: finish it, add more audio to it, or delete it.
///
/// Nothing here runs by itself: there is no cleanup queue, and nothing is deleted or submitted without a call for that
/// one recording. The one thing discovery removes is a leftover joined copy that was made from two files that both
/// still exist (see `settleContinuationLeftovers`). Discovery goes through the ingress writer's
/// own recovery pass, so captures already staged are retried there and are never offered here, and a pass that cannot
/// tell who owns a file offers nothing and says so. Normal capture never waits on this type.
///
/// Confined to the main queue. Callbacks arrive there.
final class VoiceRecoveryCoordinator {
    private struct Candidate {
        let fileName: String
        let modifiedAt: Date?
    }

    private let inProgressDirectory: URL
    private let ingress: VoiceRecoveryIngress
    private let handoffReceiver: VoiceRecordingHandoffReceiving
    private let controller: VoiceRecordingController
    private let environment: VoiceRecordingEnvironment
    private let limits: VoiceRecordingLimits
    private let maxListedRecordings: Int
    private var busyFileNames = Set<String>()

    init(
        inProgressDirectory: URL,
        ingress: VoiceRecoveryIngress,
        handoff: VoiceRecordingHandoffReceiving,
        controller: VoiceRecordingController,
        environment: VoiceRecordingEnvironment,
        limits: VoiceRecordingLimits = VoiceRecordingLimits(),
        maxListedRecordings: Int = 20
    ) {
        self.inProgressDirectory = inProgressDirectory
        self.ingress = ingress
        self.handoffReceiver = handoff
        self.controller = controller
        self.environment = environment
        self.limits = limits
        self.maxListedRecordings = max(1, maxListedRecordings)
    }

    // MARK: Discovery

    /// Runs the ingress recovery pass and lists what it left in the in-progress store. Safe to call on every launch
    /// and whenever the recovery view appears.
    func refresh(completion: @escaping (VoiceRecoveryListing) -> Void) {
        ingress.recoverStagedCaptures { report in
            completion(self.makeListing(from: report))
        }
    }

    private func makeListing(from report: VoiceIngressRecoveryReport) -> VoiceRecoveryListing {
        var listing = VoiceRecoveryListing()
        listing.pendingCaptures = report.pending
        listing.confirmedCaptureIDs = report.confirmedCaptureIDs
        guard let unclaimedNames = report.unclaimedInProgressFileNames else {
            listing.recordingsUnknown = true
            return listing
        }

        let hiddenNames = controller.activeFileNames.union(busyFileNames)
        var candidates: [Candidate] = []
        let visibleNames = settleContinuationLeftovers(in: unclaimedNames, hidden: hiddenNames)
        for name in visibleNames where name.hasSuffix(".wav") && !hiddenNames.contains(name) {
            guard VoiceRecordingController.isSafeCaptureID(String(name.dropLast(4))) else { continue }
            let url = inProgressDirectory.appendingPathComponent(name)
            guard (environment.fileSizeBytes(url) ?? 0) > 0 else { continue }
            candidates.append(Candidate(fileName: name, modifiedAt: environment.modificationDate(url)))
        }
        candidates.sort(by: Self.isNewer)

        listing.notShownCount = max(0, candidates.count - maxListedRecordings)
        listing.recordings = candidates.prefix(maxListedRecordings).compactMap { describe(fileName: $0.fileName) }
        return listing
    }

    private static let segmentSuffix = "-continued.wav"
    private static let joinedSuffix = "-joined.wav"

    /// A continuation writes the added audio to `<id>-continued.wav`, joins it with `<id>.wav` through
    /// `<id>-joined.wav`, replaces `<id>.wav` and then removes the segment. A process that died inside that sequence
    /// leaves helper files next to the recording, and listing a joined copy as a recording of its own could submit the
    /// same audio twice. Only one case is provable and removed: a joined file while both `<id>.wav` and
    /// `<id>-continued.wav` exist, because the joined file is then only a copy made from files that are still there.
    /// A segment is never removed here: whether it was already joined cannot be told from the files alone (the
    /// recording may simply end with the same samples, such as silence), so it is kept and listed and the user decides.
    private func settleContinuationLeftovers(in names: [String], hidden: Set<String>) -> [String] {
        let present = Set(names)
        var removed = Set<String>()
        for name in names where !hidden.contains(name) {
            let captureID: String
            guard name.hasSuffix(Self.joinedSuffix) else { continue }
            captureID = String(name.dropLast(Self.joinedSuffix.count))
            let baseName = captureID + ".wav"
            let segmentName = captureID + Self.segmentSuffix
            guard !captureID.isEmpty, present.contains(baseName), present.contains(segmentName),
                  !hidden.contains(baseName), !hidden.contains(segmentName) else { continue }
            let url = inProgressDirectory.appendingPathComponent(name)
            environment.removeFile(url)
            if environment.fileSizeBytes(url) == nil { removed.insert(name) }
        }
        return names.filter { !removed.contains($0) }
    }

    private static func isNewer(_ lhs: Candidate, _ rhs: Candidate) -> Bool {
        if let left = lhs.modifiedAt, let right = rhs.modifiedAt, left != right { return left > right }
        if (lhs.modifiedAt != nil) != (rhs.modifiedAt != nil) { return lhs.modifiedAt != nil }
        return lhs.fileName < rhs.fileName
    }

    /// The current facts about one in-progress file, read from the file; nil when it holds no bytes.
    private func describe(fileName: String) -> RecoverableVoiceRecording? {
        let url = inProgressDirectory.appendingPathComponent(fileName)
        guard fileName.hasSuffix(".wav"), let sizeBytes = environment.fileSizeBytes(url), sizeBytes > 0 else {
            return nil
        }
        var durationSeconds: Double?
        if let inspection = environment.inspectAudio(url), inspection.sampleRate > 0 {
            durationSeconds = Double(inspection.frames) / inspection.sampleRate
        }
        let captureID = String(fileName.dropLast(4))
        let hasLeftoverFile = [Self.segmentSuffix, Self.joinedSuffix].contains { suffix in
            environment.fileSizeBytes(inProgressDirectory.appendingPathComponent(captureID + suffix)) != nil
        }
        // The suffix alone is the evidence: generated capture IDs never end in it. The base may already have been
        // finished or deleted, and the added audio may still be part of a saved base, so the label stays either way.
        let addedAudioSuffix = String(Self.segmentSuffix.dropLast(4))
        let isAddedAudio = captureID.count > addedAudioSuffix.count && captureID.hasSuffix(addedAudioSuffix)
        let minimumBytes = Int64(limits.minimumContinuationSeconds) * limits.bytesPerSecond
        let hasRoom = durationSeconds.map { duration in
            limits.maxDurationSeconds - duration >= limits.minimumContinuationSeconds
                && sizeBytes + minimumBytes <= limits.maxFileBytes
        } ?? false
        return RecoverableVoiceRecording(
            captureID: captureID,
            fileName: fileName,
            fileSizeBytes: sizeBytes,
            durationSeconds: durationSeconds,
            modifiedAt: environment.modificationDate(url),
            canContinue: hasRoom && !hasLeftoverFile,
            isUnjoinedAddedAudio: isAddedAudio,
            hasUnjoinedAddedAudio: hasLeftoverFile)
    }

    // MARK: Finish

    /// Hands the recording to ingress. The result says saved only after the core confirmed; otherwise the audio is
    /// durable in ingress staging and the result says it is kept.
    func finish(
        _ recording: RecoverableVoiceRecording,
        completion: @escaping (Result<VoiceCaptureOutcome, VoiceRecoveryFailure>) -> Void
    ) {
        let fileName = recording.fileName
        if let refusal = claim(fileName) {
            completion(.failure(refusal))
            return
        }
        verifyUnowned(fileName) { [self] verification in
            if case .failure(let failure) = verification {
                busyFileNames.remove(fileName)
                completion(.failure(failure))
                return
            }
            handOffVerifiedRecording(fileName: fileName, completion: completion)
        }
    }

    private func handOffVerifiedRecording(
        fileName: String,
        completion: @escaping (Result<VoiceCaptureOutcome, VoiceRecoveryFailure>) -> Void
    ) {
        guard let current = describe(fileName: fileName) else {
            busyFileNames.remove(fileName)
            completion(.failure(.recordingNotFound))
            return
        }
        guard let durationSeconds = current.durationSeconds else {
            busyFileNames.remove(fileName)
            completion(.failure(.notReadable))
            return
        }

        var protectionApplied = true
        do {
            try environment.protectRecordingFile(inProgressDirectory.appendingPathComponent(fileName))
        } catch {
            protectionApplied = false
        }
        let startedAt = (current.modifiedAt ?? environment.now()).addingTimeInterval(-durationSeconds)
        let summary = VoiceRecordingSummary(
            captureID: current.captureID,
            inProgressFileName: fileName,
            startedAt: startedAt,
            durationSeconds: durationSeconds,
            fileSizeBytes: Int(current.fileSizeBytes),
            end: .recoveredOnReentry,
            closedCleanly: false,
            protectionApplied: protectionApplied)
        let handoff = VoiceRecordingHandoff(
            captureID: summary.captureID,
            inProgressFileName: fileName,
            finalizedFileName: fileName,
            startedAt: startedAt,
            durationSeconds: durationSeconds,
            fileSizeBytes: summary.fileSizeBytes,
            end: .recoveredOnReentry)
        var answered = false
        handoffReceiver.handOff(handoff) { [self] result in
            guard !answered else { return }
            answered = true
            busyFileNames.remove(fileName)
            completion(.success(VoiceRecordingController.outcome(for: result, summary: summary)))
        }
    }

    // MARK: Continue

    /// Starts recording more audio onto this recording, within the bounds a new recording has. The outcome arrives
    /// through the controller's `onOutcome` like any other recording.
    func continueRecording(
        _ recording: RecoverableVoiceRecording,
        completion: @escaping (Result<VoiceRecordingStarted, VoiceRecoveryFailure>) -> Void
    ) {
        let fileName = recording.fileName
        if let refusal = claim(fileName) {
            completion(.failure(refusal))
            return
        }
        verifyUnowned(fileName) { [self] verification in
            if case .failure(let failure) = verification {
                busyFileNames.remove(fileName)
                completion(.failure(failure))
                return
            }
            guard let current = describe(fileName: fileName) else {
                busyFileNames.remove(fileName)
                completion(.failure(.recordingNotFound))
                return
            }
            controller.continueRecording(current) { [self] result in
                busyFileNames.remove(fileName)
                completion(result.mapError { VoiceRecoveryFailure.recorderRefused($0) })
            }
        }
    }

    // MARK: Delete

    /// Removes this one recording after the user chose to delete it. Refused, with the file untouched, unless the
    /// ingress pass confirms no staged capture owns it.
    func delete(
        _ recording: RecoverableVoiceRecording,
        completion: @escaping (Result<VoiceDeletedRecording, VoiceRecoveryFailure>) -> Void
    ) {
        let fileName = recording.fileName
        if let refusal = claim(fileName) {
            completion(.failure(refusal))
            return
        }
        verifyUnowned(fileName) { [self] verification in
            defer { busyFileNames.remove(fileName) }
            if case .failure(let failure) = verification {
                completion(.failure(failure))
                return
            }
            guard let current = describe(fileName: fileName) else {
                completion(.failure(.recordingNotFound))
                return
            }
            let url = inProgressDirectory.appendingPathComponent(fileName)
            environment.removeFile(url)
            guard environment.fileSizeBytes(url) == nil else {
                completion(.failure(.removalFailed))
                return
            }
            completion(.success(VoiceDeletedRecording(
                captureID: current.captureID,
                durationSeconds: current.durationSeconds,
                fileSizeBytes: current.fileSizeBytes)))
        }
    }

    // MARK: Shared checks

    /// Marks the file as being handled. Returns the reason to refuse when it already is or the recorder owns it.
    private func claim(_ fileName: String) -> VoiceRecoveryFailure? {
        if controller.activeFileNames.contains(fileName) { return .recordingActive }
        if busyFileNames.contains(fileName) { return .alreadyInProgress }
        busyFileNames.insert(fileName)
        return nil
    }

    private func verifyUnowned(_ fileName: String, completion: @escaping (Result<Void, VoiceRecoveryFailure>) -> Void) {
        ingress.recoverStagedCaptures { [self] report in
            guard let unclaimedNames = report.unclaimedInProgressFileNames else {
                completion(.failure(.ingressStateUnknown))
                return
            }
            guard unclaimedNames.contains(fileName) else {
                let stillExists = environment.fileSizeBytes(inProgressDirectory.appendingPathComponent(fileName)) != nil
                completion(.failure(stillExists ? .ownedByStagedCapture : .recordingNotFound))
                return
            }
            guard !controller.activeFileNames.contains(fileName) else {
                completion(.failure(.recordingActive))
                return
            }
            completion(.success(()))
        }
    }
}
