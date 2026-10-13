import Foundation
import OhAndCoreBridge

/// Runs the durable reset over a `CoreHandle`: the production `ResetCore`. The core owns every rule; this client only
/// carries each request and hands the answer back to the awaiting caller.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Operation IDs start at
/// `firstOperationID`, far from the ranges the other services use. Call `shutDown()` before closing the core so no
/// caller is left waiting.
final class CoreResetClient: ResetCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 60

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the reset client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreResetClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    init(core: CoreHandle) {
        self.core = core
    }

    // MARK: ResetCore

    func start() async throws -> ResetProgress {
        guard let answer = try await ask(.start, as: ResetStatusAnswer.self).status else { throw Self.unreadableAnswer }
        return try Self.progress(from: answer)
    }

    func advance() async throws -> ResetProgress? {
        try await ask(.advance, as: ResetStatusAnswer.self).status.map(Self.progress(from:))
    }

    func status() async throws -> ResetProgress? {
        try await ask(.status, as: ResetStatusAnswer.self).status.map(Self.progress(from:))
    }

    func profileCredentials() async throws -> [ResetProfileCredential] {
        try await ask(.profileCredentials, as: ResetProfileCredentialsAnswer.self).profiles.map {
            ResetProfileCredential(
                profileVersion: $0.profileVersion, credentialReference: $0.credentialReference, isRevoked: $0.revoked)
        }
    }

    func revokeProfile(profileVersion: String) async throws {
        _ = try await ask(.revokeProfile(profileVersion: profileVersion), as: ResetProfileRevoked.self)
    }

    /// The generation new captures belong to right now: the latest started reset, or 0.
    func currentGeneration() async throws -> Int64 {
        guard let generation = try await ask(.currentGeneration, as: ResetGenerationAnswer.self).generation else {
            throw Self.unreadableAnswer
        }
        return generation
    }

    /// The generation a capture belongs to, or `nil` for an unknown item.
    func generation(ofItem itemID: String) async throws -> Int64? {
        try await ask(.itemGeneration(itemID: itemID), as: ResetGenerationAnswer.self).generation
    }

    private static func progress(from answer: ResetProgressAnswer) throws -> ResetProgress {
        guard let phase = ResetPhase(rawValue: answer.phase) else { throw unreadableAnswer }
        return ResetProgress(
            generation: answer.generation,
            phase: phase,
            boundaryRowID: answer.boundaryRowID,
            itemsAwaitingTombstone: answer.itemsAwaitingTombstone,
            cleanupPending: answer.cleanupPending,
            cleanupFailed: answer.cleanupFailed)
    }

    // MARK: Event routing

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let continuation = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let continuation else { return false }
        continuation.resume(returning: event.outcome)
        return true
    }

    /// Fails every operation still waiting and refuses later ones. Idempotent.
    func shutDown() {
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

    private func ask<Answer: Decodable>(_ command: ResetCommand, as type: Answer.Type) async throws -> Answer {
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
        _ command: ResetCommand,
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
            try core.startReset(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
