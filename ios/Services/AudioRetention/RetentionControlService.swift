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

    init(
        core: RetentionControlCore,
        session: ReadAuthenticationSession,
        pressureProbe: AudioStoragePressureProbing,
        removerProvider: @escaping () -> UserAudioRemoving?
    ) {
        self.core = core
        self.session = session
        self.pressureProbe = pressureProbe
        self.removerProvider = removerProvider
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
