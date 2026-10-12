import Foundation
import OhAndCoreBridge

/// The core's export as native code uses it (`core/src/export`, `ohand_core_start_export`). The core owns the scope,
/// authentication and exclusion rules; the caller names an absolute file that does not exist yet and the core writes the
/// portable export there.
protocol ExportCore: Sendable {
    func export(_ request: CoreExportRequest) async throws -> CoreExportAnswer
}

/// Runs exports over a `CoreHandle`: the production `ExportCore`. The core answers on its single event handler, so the
/// owner of that handler must forward every event to `handle(_:)`; it returns `true` when the event answered one of this
/// client's operations. Operation IDs start at `firstOperationID`, far from the ranges the other services use. Call
/// `shutDown()` before closing the core so no caller is left waiting.
public final class CoreExportClient: ExportCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 54

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the export client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreExportClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    func export(_ request: CoreExportRequest) async throws -> CoreExportAnswer {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            submit(request, continuation)
        }
        let data = try outcome.get()
        do {
            return try JSONDecoder().decode(CoreExportAnswer.self, from: data)
        } catch {
            throw Self.unreadableAnswer
        }
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

    private func submit(
        _ request: CoreExportRequest,
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
            try core.startExport(operationID: operationID, request: request)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
