import Foundation
import OhAndCoreBridge

/// Carries configure, refresh and read requests for the daily prompt schedule to the real core and hands each answer
/// back to the awaiting caller. The core owns the schedule, its ledger and its clock; this client supplies nothing but
/// the IANA zone, and refuses any answer it cannot read exactly.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Call `shutDown()` before closing the
/// core so no caller is left waiting.
public final class CorePromptScheduleClient: @unchecked Sendable {
    /// The first operation identifier this client submits. Every client sharing a handle owns a distinct range or one
    /// takes the other's event: shadow review starts at `1 << 48`, transcription at `1 << 50`, deletion cleanup at
    /// `1 << 52`, export at `1 << 54`, suggestion responses at `1 << 55`, reminder reconciliation at `1 << 56`, the
    /// item status reader at `1 << 57`, audio retention at `1 << 58`, the prompt schedule at `1 << 59`, notification
    /// actions at `1 << 60`, processing health at `1 << 61` and the job runner at `1 << 62`.
    public static let firstOperationID: UInt64 = 1 << 59

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the prompt schedule client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CorePromptScheduleClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    /// Applies the user's choice. The schedule is persisted before this returns. It installs nothing: prompts are
    /// installed by the reminder pass, which takes them only from the persisted schedule.
    public func configure(_ setting: PromptScheduleSetting, timeZoneIdentifier: String) async throws
        -> PromptScheduleOutcome
    {
        try await ask { operationID in
            try self.core.startConfigurePromptSchedule(
                operationID: operationID, setting: setting, timeZoneIdentifier: timeZoneIdentifier)
        }
    }

    public func refresh(trigger: PromptScheduleRefreshTrigger, timeZoneIdentifier: String) async throws
        -> PromptScheduleOutcome
    {
        try await ask { operationID in
            try self.core.startRefreshPromptSchedule(
                operationID: operationID, trigger: trigger, timeZoneIdentifier: timeZoneIdentifier)
        }
    }

    public func read() async throws -> PromptScheduleOutcome {
        try await ask { operationID in
            try self.core.startReadPromptSchedule(operationID: operationID)
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

    private func ask(_ start: @escaping (UInt64) throws -> Void) async throws -> PromptScheduleOutcome {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            submit(start, continuation)
        }
        let data = try outcome.get()
        do {
            return try JSONDecoder().decode(PromptScheduleOutcome.self, from: data)
        } catch {
            throw Self.unreadableAnswer
        }
    }

    private func submit(
        _ start: (UInt64) throws -> Void,
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
            try start(operationID)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
