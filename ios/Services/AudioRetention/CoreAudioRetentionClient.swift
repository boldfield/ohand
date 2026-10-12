import Foundation
import OhAndCoreBridge

/// Asks the real core for retention decisions over a `CoreHandle`: the production `AudioRetentionCore`. The core owns
/// every rule; this client carries each request, hands the answer back to the awaiting caller and refuses any answer it
/// cannot read exactly, so an unfamiliar state is never taken for an approval.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Call `shutDown()` before closing the core
/// so no caller is left waiting.
public final class CoreAudioRetentionClient: AudioRetentionCore, @unchecked Sendable {
    /// The first operation identifier this client submits. The core accepts a duplicate operation identifier without
    /// complaint, so every client sharing a handle owns a distinct range or one takes the other's event: shadow review
    /// starts at `1 << 48`, transcription at `1 << 50`, deletion cleanup at `1 << 52`, export at `1 << 54`, reminder
    /// reconciliation at `1 << 56`, processing health at `1 << 61` and the job runner at `1 << 62`.
    public static let firstOperationID: UInt64 = 1 << 58

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the audio retention client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreAudioRetentionClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    // MARK: AudioRetentionCore

    public func sweep(pressure: CoreRetentionPressure, periodSeconds: UInt64) async throws -> [AudioRetentionItem] {
        let answer = try await ask(.sweep(pressure: pressure, periodSeconds: periodSeconds), as: CoreRetentionSweepAnswer.self)
        return try answer.assessments.map(Self.item(from:))
    }

    public func authorizeRemoval(itemID: String, audioReference: String, periodSeconds: UInt64) async throws
        -> AudioRemovalDecision
    {
        let answer = try await ask(
            .authorizeRemoval(itemID: itemID, audioReference: audioReference, periodSeconds: periodSeconds),
            as: CoreRetentionDecisionAnswer.self)
        return try Self.decision(from: answer)
    }

    public func removeAudio(
        itemID: String, audioReference: String, periodSeconds: UInt64, effect: () -> AudioRemovalEffect
    ) async throws -> AudioRemovalResult {
        let answer = try await ask(
            .beginRemoval(itemID: itemID, audioReference: audioReference, periodSeconds: periodSeconds),
            as: CoreRetentionDecisionAnswer.self)
        if case .denied(let denial) = try Self.decision(from: answer) { return .denied(denial) }
        guard let leaseID = answer.leaseID else { throw Self.unreadableAnswer }

        // Pinned first: from then on the lease cannot expire, so the fence stays up for the whole effect however long it
        // takes, and only the release below ends it. A lease that expired before the pin is refused and nothing runs.
        let performed: AudioRemovalEffect
        do {
            try core.pinAudioRemoval(leaseIdentifier: leaseID)
            performed = effect()
        } catch {
            try? core.releaseAudioRemoval(leaseIdentifier: leaseID)
            throw error
        }
        // A release that cannot vouch for the fence is reported rather than ignored.
        try core.releaseAudioRemoval(leaseIdentifier: leaseID)
        return .approved(performed)
    }

    // MARK: Answer mapping

    static func item(from assessment: CoreRetentionAssessment) throws -> AudioRetentionItem {
        let options = try assessment.options.map { raw -> AudioRetentionOption in
            guard let option = AudioRetentionOption(rawValue: raw) else { throw unreadableAnswer }
            return option
        }
        var notice: AudioStorageNotice?
        if let rawNotice = assessment.storagePressureNotice {
            guard let parsed = AudioStorageNotice(rawValue: rawNotice) else { throw unreadableAnswer }
            notice = parsed
        }
        return AudioRetentionItem(
            itemID: assessment.itemID,
            audioReference: assessment.audioReference,
            status: try status(from: assessment.state),
            isSoleSource: assessment.soleSource,
            options: options,
            cleanup: try cleanup(from: assessment.cleanup),
            storageNotice: notice)
    }

    private static func status(from state: CoreRetentionState) throws -> AudioRetentionStatus {
        switch state.kind {
        case "retained":
            guard let expiresAt = state.expiresAt else { throw unreadableAnswer }
            return .retained(expiresAt: expiresAt)
        case "expired":
            guard let expiredAt = state.expiredAt else { throw unreadableAnswer }
            return .expired(expiredAt: expiredAt)
        case "unresolved":
            guard let raw = state.reason, let reason = AudioUnresolvedReason(rawValue: raw) else { throw unreadableAnswer }
            return .unresolved(reason)
        default:
            throw unreadableAnswer
        }
    }

    private static func cleanup(from cleanup: CoreRetentionCleanup) throws -> AudioCleanupIntent {
        switch cleanup.kind {
        case "none":
            return .none
        case "remove_audio":
            guard let reference = cleanup.audioReference, let expiredAt = cleanup.expiredAt else { throw unreadableAnswer }
            return .removeAudio(audioReference: reference, expiredAt: expiredAt)
        default:
            throw unreadableAnswer
        }
    }

    static func decision(from answer: CoreRetentionDecisionAnswer) throws -> AudioRemovalDecision {
        switch answer.decision {
        case "approved":
            return .approved
        case "denied":
            switch answer.reason {
            case "item_not_found": return .denied(.itemNotFound)
            case "audio_reference_mismatch": return .denied(.audioReferenceMismatch)
            case "deletion_owns_cleanup": return .denied(.deletionOwnsCleanup)
            case "not_yet_expired": return .denied(.notYetExpired)
            case "unresolved":
                guard let raw = answer.unresolvedReason, let reason = AudioUnresolvedReason(rawValue: raw) else {
                    throw unreadableAnswer
                }
                return .denied(.unresolved(reason))
            default:
                throw unreadableAnswer
            }
        default:
            throw unreadableAnswer
        }
    }

    // MARK: Event routing

    /// Operations submitted and not yet answered.
    var outstandingOperationCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return pending.count
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let continuation = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let continuation else { return false }
        continuation.resume(returning: event.outcome)
        return true
    }

    /// Fails every operation still waiting and refuses later ones. Idempotent.
    public func shutDown() {
        lock.lock()
        isShutDown = true
        let waiting = pending
        pending = [:]
        lock.unlock()
        for continuation in waiting.values {
            continuation.resume(returning: .failure(Self.shutDownFailure))
        }
    }

    // MARK: private

    private func ask<Answer: Decodable>(_ command: AudioRetentionCommand, as type: Answer.Type) async throws -> Answer {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            submit(command, continuation)
        }
        let data = try outcome.get()
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw Self.unreadableAnswer
        }
    }

    private func submit(
        _ command: AudioRetentionCommand,
        _ continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            continuation.resume(returning: .failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = continuation
        lock.unlock()
        do {
            try core.startAudioRetention(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
