import Foundation
import OhAndCoreBridge

/// Asks the real core for retention decisions over a `CoreHandle`: the production `AudioRetentionCore`. The core owns
/// every rule; this client carries each request, hands the answer back to the awaiting caller and refuses any answer it
/// cannot read exactly, so an unfamiliar state is never taken for an approval.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Operation IDs start at
/// `firstOperationID`, far from the ranges the other services use. Call `shutDown()` before closing the core so no
/// caller is left waiting.
public final class CoreAudioRetentionClient: AudioRetentionCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 56

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the audio retention client was shut down")
    static let leaseTooOld = CoreFailure(
        errorClass: .transient, code: "lease_expired", message: "the removal lease was too old to use")
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
        // Taken before asking, so the lease is never assumed younger than it is.
        let askedAt = Self.uptime()
        let answer = try await ask(
            .beginRemoval(itemID: itemID, audioReference: audioReference, periodSeconds: periodSeconds),
            as: CoreRetentionDecisionAnswer.self)
        if case .denied(let denial) = try Self.decision(from: answer) { return .denied(denial) }
        guard let leaseID = answer.leaseID, let leaseMillis = answer.leaseMillis, leaseMillis > 0 else {
            throw Self.unreadableAnswer
        }

        var performed: AudioRemovalEffect?
        if Self.leaseAllowsEffect(elapsed: Self.uptime() - askedAt, leaseMillis: leaseMillis) {
            performed = effect()
        }
        // The fence is always ended, and a lease that lapsed first is reported rather than ignored.
        try core.releaseAudioRemoval(leaseIdentifier: leaseID)
        guard let performed else { throw Self.leaseTooOld }
        return .approved(performed)
    }

    /// The effect may start only in the first half of the lease, which leaves the rest as margin for the effect itself
    /// and for scheduling delays between this check and the removal.
    static func leaseAllowsEffect(elapsed: TimeInterval, leaseMillis: UInt64) -> Bool {
        elapsed >= 0 && elapsed < Double(leaseMillis) / 2_000
    }

    private static func uptime() -> TimeInterval {
        ProcessInfo.processInfo.systemUptime
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
