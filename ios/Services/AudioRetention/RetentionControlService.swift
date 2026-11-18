import Foundation
import OhAndCoreBridge

/// The retention period as the core stores it. `isDefault` is true until the user stores one.
public struct RetentionPolicySetting: Equatable, Sendable {
    /// The core accepts one second through 3650 days.
    public static let minimumPeriodSeconds: Int64 = 1
    public static let maximumPeriodSeconds: Int64 = 3650 * 86_400
    public static let defaultPeriodSeconds: Int64 = 7 * 86_400

    public let periodSeconds: Int64
    public let isDefault: Bool

    public init(periodSeconds: Int64, isDefault: Bool) {
        self.periodSeconds = periodSeconds
        self.isDefault = isDefault
    }
}

/// One live item whose audio is its only source. Such audio never expires, whatever the period is; the user resolves
/// it explicitly.
public struct UnresolvedSoleAudioCase: Equatable, Sendable {
    public let itemID: String
    /// The capture time as stored (RFC 3339).
    public let capturedAt: String
    public let reason: AudioUnresolvedReason
    /// The explicit actions the core offers for this audio.
    public let options: [AudioRetentionOption]

    public init(
        itemID: String, capturedAt: String, reason: AudioUnresolvedReason, options: [AudioRetentionOption]
    ) {
        self.itemID = itemID
        self.capturedAt = capturedAt
        self.reason = reason
        self.options = options
    }
}

/// What the retention settings screen shows: the policy, the storage pressure measured on the audio volume, and the
/// unresolved sole-audio cases, oldest capture first. Pressure only adds a notice; it never makes audio eligible.
public struct RetentionStatus: Equatable, Sendable {
    public let policy: RetentionPolicySetting
    public let storagePressure: CoreRetentionPressure
    public let unresolved: [UnresolvedSoleAudioCase]

    public init(
        policy: RetentionPolicySetting, storagePressure: CoreRetentionPressure, unresolved: [UnresolvedSoleAudioCase]
    ) {
        self.policy = policy
        self.storagePressure = storagePressure
        self.unresolved = unresolved
    }
}

/// What the retention settings can offer to redo: the notes whose transcript an earlier recognizer made and whose
/// recording is still on the phone, and those whose recording is already gone. Content-free counts only.
public struct RetranscriptionStatus: Equatable, Sendable {
    /// The notes that can be transcribed again now.
    public let redoableCount: Int
    /// The notes that cannot, because their recording was already removed or is not there.
    public let recordingGoneCount: Int
    /// True when the core knows of more candidates than one status lists; a count is then a lower bound.
    public let moreCandidates: Bool
    /// Replacement transcriptions queued or running.
    public let inFlightCount: Int

    public init(redoableCount: Int, recordingGoneCount: Int, moreCandidates: Bool, inFlightCount: Int) {
        self.redoableCount = redoableCount
        self.recordingGoneCount = recordingGoneCount
        self.moreCandidates = moreCandidates
        self.inFlightCount = inFlightCount
    }
}

/// What a `retranscribe` request did: how many replacement jobs were queued and how many named items were left out,
/// by the core's content-free reason.
public struct RetranscriptionReceipt: Equatable, Sendable {
    public let queued: Int
    /// Items beyond the core's batch bound that were not looked at; ask again for them.
    public let deferred: Int
    public let skipped: [String: Int]

    public init(queued: Int, deferred: Int, skipped: [String: Int]) {
        self.queued = queued
        self.deferred = deferred
        self.skipped = skipped
    }
}

/// Which candidates' recordings are still on the phone. Decided natively, with the file-system seam and the retention
/// ledger `AudioRetentionService` uses, because the core never opens an audio file.
protocol RecordingPresenceChecking: Sendable {
    /// The candidates whose file exists at the layout's path for their reference and that the ledger does not record
    /// as removed, in order.
    func presentRecordings(among candidates: [RetranscriptionCandidateAnswer]) -> [RetranscriptionCandidateAnswer]
}

/// Presence from the finalized audio store and the retention ledger.
struct StoredRecordingPresence: RecordingPresenceChecking, @unchecked Sendable {
    private let layout: ProtectedStorageLayout
    private let fileSystem: AudioRetentionFileSystem

    init(
        layout: ProtectedStorageLayout,
        fileSystem: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem()
    ) {
        self.layout = layout
        self.fileSystem = fileSystem
    }

    func presentRecordings(among candidates: [RetranscriptionCandidateAnswer]) -> [RetranscriptionCandidateAnswer] {
        let staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        let ledger = AudioRetentionLedger(layout: layout, fileSystem: fileSystem)
        // A ledger that cannot be read decides nothing: the file itself is still checked.
        let ledgerIsReadable = (try? ledger.refresh()) != nil
        return candidates.filter { candidate in
            guard let reference = candidate.audioReference, let url = staging.finalizedAudioURL(forReference: reference)
            else { return false }
            if ledgerIsReadable,
                let entry = try? ledger.entry(itemID: candidate.itemID, audioReference: reference),
                entry.state == .removed
            {
                return false
            }
            return (try? fileSystem.isPresent(at: url)) ?? false
        }
    }
}

/// Asks the job runner to drain, once it exists. The service is built before the runner, so the root binds this after
/// attaching the runner; until then a request does nothing (the jobs wait for the next drain).
final class LateBoundDrainRequest: @unchecked Sendable {
    private let lock = NSLock()
    private var request: (@Sendable () -> Void)?

    func bind(_ request: @escaping @Sendable () -> Void) {
        lock.lock()
        self.request = request
        lock.unlock()
    }

    func unbind() {
        lock.lock()
        request = nil
        lock.unlock()
    }

    func callAsFunction() {
        lock.lock()
        let current = request
        lock.unlock()
        current?()
    }
}

/// How an explicit audio deletion ended: the file is verified absent and the completion is recorded.
public enum AudioDeletionReceipt: Equatable, Sendable {
    case removed
    /// The file was already gone; the completion is recorded.
    case alreadyRemoved
}

/// Why a retention read or command did not succeed. Every case except `core`, `unreadable` and
/// `audioRemovalIncomplete` means the core decided (or the gate refused) and nothing changed.
public enum RetentionControlError: Error, Equatable, Sendable {
    /// The read session is capture-only, or relocked before the answer; nothing was delivered.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty ID or a period outside the bounds).
    case invalidRequest
    /// No readable item has that ID (unknown, deleted or outside the session's scopes; the core does not say which).
    case itemNotFound
    case itemDeleted
    /// The item holds no audio.
    case noAudio
    /// The audio is the item's only source, so it cannot be deleted on its own. The user may retry transcription, or
    /// delete the item itself (A05), which is the only way to remove this audio.
    case soleSourceUseItemDeletion
    /// A retry was asked for an item whose audio is already removed.
    case audioRemoved
    /// A transcription job for the item is already queued or running.
    case jobInFlight
    /// The core does not retry transcription in the item's current state (`transcriptionState` is the core's word).
    case notRetryable(transcriptionState: String)
    /// The core refused the removal at the moment of removal and the file was left in place.
    case removalDenied(AudioRemovalDenial)
    /// Audio removal is not running yet: the lifecycle coordinator has not been connected to the root.
    case audioRemovalUnavailable
    /// The removal did not complete and nothing is recorded as done; asking again repeats it safely.
    case audioRemovalIncomplete(AudioRetentionRetryReason)
    /// The audio reference does not name a file inside the finalized audio store, so it cannot be removed.
    case audioReferenceUnresolvable
    case core(CoreFailure)
    case unreadable
}

/// Removes one item's audio at the user's request through the core's fence. `AudioRetentionService` is the production
/// implementation.
protocol UserAudioRemoving {
    func removeAudioForUser(itemID: String, audioReference: String) async -> AudioRemovalOutcome
}

extension AudioRetentionService: UserAudioRemoving {}

/// Configures audio retention and acts on audio that is an item's only source, through the core and nothing else.
///
/// * **The period is the core's.** `setPeriod` stores it durably; the retention sweep reads the stored value, so
///   changing it only moves when transcribed audio expires. It never deletes audio, and audio that is an item's only
///   source never expires, whatever the period is.
/// * **Reads are gated.** `status` goes through the private read gate: a capture-only session gets
///   `notAuthenticated` and the core is not asked, and an answer that arrives after a relock is dropped undecoded.
/// * **Writes need an authenticated session**, checked when the call is made; a capture-only session gets
///   `notAuthenticated` and neither the core nor a file is touched.
/// * **Retry transcription** re-queues through the core's own transcription requeue and reports its refusals.
/// * **Delete audio** removes only audio of an item that keeps a transcript or text. The core decides inside its
///   store-wide fence while native code removes the one file; audio that is the item's only source is refused and the
///   user is pointed to deleting the item.
/// * **A sweep never blocks a capture.** Nothing here is on the capture path; every call is a separate request to the
///   core's worker, behind or ahead of a capture's own request, never holding it up.
public final class RetentionControlService: @unchecked Sendable {
    private let core: RetentionControlCore
    private let session: ReadAuthenticationSession
    private let pressureProbe: AudioStoragePressureProbing
    private let removerProvider: () -> UserAudioRemoving?
    private let recordings: RecordingPresenceChecking
    private let requestDrain: @Sendable () -> Void

    /// `recordings` decides which transcript-replacement candidates still have their recording; `requestDrain` starts
    /// the job runner after replacements are queued, so they run without waiting for the next capture.
    init(
        core: RetentionControlCore,
        session: ReadAuthenticationSession,
        pressureProbe: AudioStoragePressureProbing,
        removerProvider: @escaping () -> UserAudioRemoving?,
        recordings: RecordingPresenceChecking,
        requestDrain: @escaping @Sendable () -> Void
    ) {
        self.core = core
        self.session = session
        self.pressureProbe = pressureProbe
        self.removerProvider = removerProvider
        self.recordings = recordings
        self.requestDrain = requestDrain
    }

    /// The stored policy, the storage pressure and the unresolved sole-audio cases.
    public func status() async throws -> RetentionStatus {
        let answer = try await readStatus()
        var cases: [UnresolvedSoleAudioCase] = []
        for entry in answer.unresolved {
            guard let reason = AudioUnresolvedReason(rawValue: entry.reason) else {
                throw RetentionControlError.unreadable
            }
            let options = try entry.options.map { raw -> AudioRetentionOption in
                guard let option = AudioRetentionOption(rawValue: raw) else { throw RetentionControlError.unreadable }
                return option
            }
            cases.append(
                UnresolvedSoleAudioCase(
                    itemID: entry.itemID, capturedAt: entry.capturedAt, reason: reason, options: options))
        }
        return RetentionStatus(
            policy: RetentionPolicySetting(
                periodSeconds: answer.policy.periodSeconds, isDefault: answer.policy.isDefault),
            storagePressure: pressureProbe.currentPressure(), unresolved: cases)
    }

    /// Stores the retention period in seconds, from one second to 3650 days. Deletes nothing.
    @discardableResult
    public func setPeriod(seconds: Int64) async throws -> RetentionPolicySetting {
        guard session.currentAuthorization() != nil else { throw RetentionControlError.notAuthenticated }
        guard seconds >= RetentionPolicySetting.minimumPeriodSeconds,
            seconds <= RetentionPolicySetting.maximumPeriodSeconds
        else { throw RetentionControlError.invalidRequest }
        let answer = try await send(.setPeriod(seconds: seconds))
        guard answer.outcome == "stored", let stored = answer.periodSeconds else {
            throw RetentionControlError.unreadable
        }
        return RetentionPolicySetting(periodSeconds: stored, isDefault: false)
    }

    /// Re-queues on-device transcription for the item. Returns the queued job's identifier.
    @discardableResult
    public func retryTranscription(itemID: String) async throws -> String {
        guard session.currentAuthorization() != nil else { throw RetentionControlError.notAuthenticated }
        guard !itemID.isEmpty else { throw RetentionControlError.invalidRequest }
        let answer = try await send(.retryTranscription(itemID: itemID))
        switch answer.outcome {
        case "requeued":
            guard let jobID = answer.jobID else { throw RetentionControlError.unreadable }
            return jobID
        case "item_deleted":
            throw RetentionControlError.itemDeleted
        case "audio_removed":
            throw RetentionControlError.audioRemoved
        case "job_in_flight":
            throw RetentionControlError.jobInFlight
        case "not_retryable":
            throw RetentionControlError.notRetryable(transcriptionState: answer.transcriptionState ?? "")
        default:
            throw RetentionControlError.unreadable
        }
    }

    /// How many notes can be transcribed again now (their transcript came from an earlier recognizer and their recording
    /// is on the phone) and how many cannot because the recording is gone. Read through the private read gate, one
    /// bounded page after another; past `maximumRetranscriptionPages` pages the counts are a lower bound and
    /// `moreCandidates` is true.
    public func retranscriptionStatus() async throws -> RetranscriptionStatus {
        let scan = try await scanRetranscriptionCandidates(stopAfterPresent: nil)
        return RetranscriptionStatus(
            redoableCount: scan.present.count, recordingGoneCount: scan.goneCount, moreCandidates: scan.hasMore,
            inFlightCount: scan.inFlightCount)
    }

    /// The items whose recording is on the phone, for `retranscribe`: the same decision `retranscriptionStatus` counts,
    /// at most `RetranscriptionAnswer.maximumBatch` of them. Pages past candidates whose recording is gone, so a long
    /// history of removed recordings never hides a present one.
    public func redoableItemIDs() async throws -> [String] {
        let scan = try await scanRetranscriptionCandidates(stopAfterPresent: RetranscriptionAnswer.maximumBatch)
        return Array(scan.present.prefix(RetranscriptionAnswer.maximumBatch)).map(\.itemID)
    }

    /// The most pages one status or redo scan reads: each page is at most `RetranscriptionAnswer.maximumBatch`
    /// candidates, so a scan is bounded however long the history is.
    static let maximumRetranscriptionPages = 50

    private func scanRetranscriptionCandidates(
        stopAfterPresent limit: Int?
    ) async throws -> (present: [RetranscriptionCandidateAnswer], goneCount: Int, inFlightCount: Int, hasMore: Bool) {
        var present: [RetranscriptionCandidateAnswer] = []
        var goneCount = 0
        var inFlightCount = 0
        var cursor: String?
        for _ in 0..<Self.maximumRetranscriptionPages {
            let answer = try await readRetranscriptionStatus(after: cursor)
            inFlightCount = answer.replacementsInFlight
            let found = recordings.presentRecordings(among: answer.candidates)
            present.append(contentsOf: found)
            goneCount += answer.candidates.count - found.count
            guard answer.moreCandidates, let last = answer.candidates.last else {
                return (present, goneCount, inFlightCount, false)
            }
            if let limit, present.count >= limit {
                return (present, goneCount, inFlightCount, true)
            }
            cursor = last.itemID
        }
        return (present, goneCount, inFlightCount, true)
    }

    /// Asks the core to transcribe these notes again from their retained audio, and asks the job runner to drain so the
    /// jobs run now. At most `RetranscriptionAnswer.maximumBatch` items go in one call; pass only items whose recording
    /// is present (`redoableItemIDs()` decides that). The items stay readable and searchable while the jobs run.
    @discardableResult
    public func retranscribe(itemIDs: [String]) async throws -> RetranscriptionReceipt {
        guard session.currentAuthorization() != nil else { throw RetentionControlError.notAuthenticated }
        guard !itemIDs.isEmpty, !itemIDs.contains(where: \.isEmpty) else { throw RetentionControlError.invalidRequest }
        let sent = Array(itemIDs.prefix(RetranscriptionAnswer.maximumBatch))
        let answer = try await send(.retranscribe(itemIDs: sent))
        guard answer.outcome == "queued", let queued = answer.queued else { throw RetentionControlError.unreadable }
        if queued > 0 { requestDrain() }
        return RetranscriptionReceipt(
            queued: queued, deferred: (answer.deferred ?? 0) + (itemIDs.count - sent.count),
            skipped: answer.skipped ?? [:])
    }

    /// Removes the item's audio file, keeping the item and its text. Refused when the audio is the item's only source.
    public func deleteAudio(itemID: String) async throws -> AudioDeletionReceipt {
        guard session.currentAuthorization() != nil else { throw RetentionControlError.notAuthenticated }
        guard !itemID.isEmpty else { throw RetentionControlError.invalidRequest }
        let audio = try await readAudio(itemID: itemID)
        guard let audioReference = audio.audioReference else { throw RetentionControlError.noAudio }
        guard !audio.soleSource else { throw RetentionControlError.soleSourceUseItemDeletion }
        guard let remover = removerProvider() else { throw RetentionControlError.audioRemovalUnavailable }
        // The read above may have taken a while; a session that relocked since must not remove anything.
        guard session.currentAuthorization() != nil else { throw RetentionControlError.notAuthenticated }

        switch await remover.removeAudioForUser(itemID: itemID, audioReference: audioReference) {
        case .removed:
            return .removed
        case .alreadyRemoved:
            return .alreadyRemoved
        case .denied(.soleSource):
            throw RetentionControlError.soleSourceUseItemDeletion
        case .denied(let denial):
            throw RetentionControlError.removalDenied(denial)
        case .retryLater(let reason):
            throw RetentionControlError.audioRemovalIncomplete(reason)
        case .failedPermanently:
            throw RetentionControlError.audioReferenceUnresolvable
        }
    }

    // MARK: private

    private func readStatus() async throws -> RetentionStatusAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<RetentionStatusAnswer, CoreFailure>, Never>) in
            core.readStatus { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    private func readRetranscriptionStatus(after cursor: String?) async throws -> RetranscriptionStatusAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<RetranscriptionStatusAnswer, CoreFailure>, Never>) in
            core.readRetranscriptionStatus(after: cursor) { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    private func readAudio(itemID: String) async throws -> RetentionAudioAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<RetentionAudioAnswer, CoreFailure>, Never>) in
            core.readAudio(itemID: itemID) { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    private func send(_ command: RetentionControlCommand) async throws -> RetentionControlAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<RetentionControlAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    private static func value<Answer>(of result: Result<Answer, CoreFailure>) throws -> Answer {
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw RetentionControlError.itemNotFound
            case "invalid_request": throw RetentionControlError.invalidRequest
            case "read_not_authenticated", "read_relocked": throw RetentionControlError.notAuthenticated
            default: throw RetentionControlError.core(failure)
            }
        }
    }
}
