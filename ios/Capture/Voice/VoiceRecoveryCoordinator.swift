import Foundation

/// Finds recordings that were never handed to ingress (cancelled, or cut off before handoff) and carries out the
/// user's explicit choice for exactly one of them: finish it, add more audio to it, or delete it.
///
/// Nothing here runs by itself: discovery only reads, there is no cleanup queue, and nothing is deleted or submitted
/// without a call for that one recording. Discovery goes through the ingress writer's own recovery pass, so captures
/// already staged are retried there and are never offered here, and a pass that cannot tell who owns a file offers
/// nothing and says so. Normal capture never waits on this type.
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
        for name in unclaimedNames where name.hasSuffix(".wav") && !hiddenNames.contains(name) {
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
        let minimumBytes = Int64(limits.minimumContinuationSeconds) * limits.bytesPerSecond
        let canContinue = durationSeconds.map { duration in
            limits.maxDurationSeconds - duration >= limits.minimumContinuationSeconds
                && sizeBytes + minimumBytes <= limits.maxFileBytes
        } ?? false
        return RecoverableVoiceRecording(
            captureID: String(fileName.dropLast(4)),
            fileName: fileName,
            fileSizeBytes: sizeBytes,
            durationSeconds: durationSeconds,
            modifiedAt: environment.modificationDate(url),
            canContinue: canContinue)
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
