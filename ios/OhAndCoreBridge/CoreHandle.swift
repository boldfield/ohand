import Foundation
import OhandCoreC

/// Normalized error classes shared with the Rust core.
public enum CoreErrorClass: String, Decodable, Sendable {
    case transient
    case permanent
    case unauthorized
    case cancelled
    case unsupported
}

/// A normalized failure. Rust-originated failures carry fixed, content-free messages.
public struct CoreFailure: Error, Equatable, Decodable, Sendable {
    public let errorClass: CoreErrorClass
    public let code: String
    public let message: String

    enum CodingKeys: String, CodingKey {
        case errorClass = "class"
        case code
        case message
    }

    public init(errorClass: CoreErrorClass, code: String, message: String) {
        self.errorClass = errorClass
        self.code = code
        self.message = message
    }

    public static let handleClosed = CoreFailure(
        errorClass: .permanent,
        code: "handle_closed",
        message: "the core handle was already closed"
    )
}

/// The outcome of one background operation, delivered on the main queue.
public struct CoreEvent: Equatable, Sendable {
    public let operationID: UInt64
    public let outcome: Result<Data, CoreFailure>

    public func decode<Payload: Decodable>(_ type: Payload.Type) throws -> Payload {
        try JSONDecoder().decode(type, from: outcome.get())
    }
}

/// Result of the smallest real background operation: a read of the core store.
public struct StoreCheckReport: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let schemaVersion: Int
    public let captureCount: Int

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case schemaVersion = "schema_version"
        case captureCount = "capture_count"
    }
}

func decodeCoreFailure(status: UInt32, bytes: Data) -> CoreFailure {
    if let failure = try? JSONDecoder().decode(CoreFailure.self, from: bytes) {
        return failure
    }
    return CoreFailure(
        errorClass: .permanent,
        code: "unreadable_failure",
        message: "status \(status) without a readable error payload"
    )
}

/// Takes ownership of a Rust-allocated result, copies its bytes and releases the Rust buffer
/// exactly once. Returns the response bytes or throws the normalized failure.
func consumeCoreResult(_ rawResult: OhandCoreResult) throws -> Data {
    var result = rawResult
    defer { ohand_core_result_free(&result) }
    var bytes = Data()
    if let pointer = result.data, result.len > 0 {
        bytes = Data(bytes: pointer, count: result.len)
    }
    if result.status == UInt32(OHAND_CORE_STATUS_OK) {
        return bytes
    }
    throw decodeCoreFailure(status: result.status, bytes: bytes)
}

/// Hands events from the Rust worker thread to a handler on the main queue. Once `retire()`
/// returns, the handler is never started again: a delivery that already claimed the handler
/// is waited for, so there is no window between claiming and invoking it. A `retire()` made on
/// the main queue (for example from inside a handler) cannot wait for itself, but it also
/// cannot race with a handler because handlers only run on the main queue. Callers must not
/// hold a lock the handler may need while retiring from another thread.
private final class EventDelivery: @unchecked Sendable {
    private let condition = NSCondition()
    private var handler: ((CoreEvent) -> Void)?
    private var claimedInvocationCount = 0

    init(handler: @escaping (CoreEvent) -> Void) {
        self.handler = handler
    }

    func enqueue(_ event: CoreEvent) {
        DispatchQueue.main.async { self.deliver(event) }
    }

    private func deliver(_ event: CoreEvent) {
        condition.lock()
        guard let activeHandler = handler else {
            condition.unlock()
            return
        }
        claimedInvocationCount += 1
        condition.unlock()
        defer {
            condition.lock()
            claimedInvocationCount -= 1
            condition.broadcast()
            condition.unlock()
        }
        CoreHandle.deliveryClaimedHookForTesting?()
        activeHandler(event)
    }

    func retire() {
        condition.lock()
        handler = nil
        if !Thread.isMainThread {
            while claimedInvocationCount > 0 {
                condition.wait()
            }
        }
        condition.unlock()
    }
}

/// Runs on the Rust worker thread. It captures nothing: the context is an unretained
/// `EventDelivery` that `CoreHandle` keeps alive until Rust can no longer call back.
private let coreEventTrampoline: OhandCoreEventCallback = { context, operationID, status, data, length in
    guard let context else { return }
    let delivery = Unmanaged<EventDelivery>.fromOpaque(context).takeUnretainedValue()
    var bytes = Data()
    if let data, length > 0 {
        bytes = Data(bytes: data, count: length)
    }
    let outcome: Result<Data, CoreFailure> = status == UInt32(OHAND_CORE_STATUS_OK)
        ? .success(bytes)
        : .failure(decodeCoreFailure(status: status, bytes: bytes))
    delivery.enqueue(CoreEvent(operationID: operationID, outcome: outcome))
}

/// One open Rust core. Every call is safe from any thread. Events are delivered to the handler
/// on the main queue, in submission order. After `cancel()`, `close()` or replacing the handler
/// returns, the old handler is never invoked again, and `close()`/`deinit` release the core.
///
/// A handler may call any method of its own handle. When `cancel()`, `close()` or
/// `setEventHandler` is called from another thread, it waits for a handler invocation that
/// has already been claimed, so the handler has finished or never starts. The handler must
/// therefore not block on work that depends on that other thread making progress.
public final class CoreHandle: @unchecked Sendable {
    public static var liveHandleCount: Int { Int(ohand_core_live_handles()) }
    public static var liveResultBufferCount: Int { Int(ohand_core_live_result_buffers()) }

    /// Runs on the main queue after a delivery claims its handler and before the handler is
    /// invoked. Tests use it to hold that window open.
    static var deliveryClaimedHookForTesting: (() -> Void)?

    private let stateLock = NSLock()
    let handleIdentifier: OhandCoreHandle
    private var isClosed = false
    private var delivery: EventDelivery?
    private var retainedContext: UnsafeMutableRawPointer?

    public init(path: String = ":memory:") throws {
        let pathBytes = Array(path.utf8)
        var openedHandle: OhandCoreHandle = 0
        let rawResult = pathBytes.withUnsafeBufferPointer { buffer in
            ohand_core_open(buffer.baseAddress, buffer.count, &openedHandle)
        }
        _ = try consumeCoreResult(rawResult)
        handleIdentifier = openedHandle
    }

    deinit {
        close()
    }

    public func setEventHandler(_ handler: ((CoreEvent) -> Void)?) throws {
        stateLock.lock()
        guard !isClosed else {
            stateLock.unlock()
            throw CoreFailure.handleClosed
        }

        let newDelivery = handler.map { EventDelivery(handler: $0) }
        let newContext = newDelivery.map { Unmanaged.passRetained($0).toOpaque() }
        let rawResult = ohand_core_set_event_callback(
            handleIdentifier,
            newDelivery == nil ? nil : coreEventTrampoline,
            newContext
        )
        do {
            _ = try consumeCoreResult(rawResult)
        } catch {
            stateLock.unlock()
            newDelivery?.retire()
            if let newContext {
                Unmanaged<EventDelivery>.fromOpaque(newContext).release()
            }
            throw error
        }
        let replacedDelivery = delivery
        let replacedContext = retainedContext
        delivery = newDelivery
        retainedContext = newContext
        stateLock.unlock()

        // Retire without the state lock: a handler that is being waited for may call this handle.
        replacedDelivery?.retire()
        releaseContext(replacedContext)
    }

    public func startStoreCheck(operationID: UInt64) throws {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !isClosed else { throw CoreFailure.handleClosed }
        _ = try consumeCoreResult(ohand_core_start_store_check(handleIdentifier, operationID))
    }

    /// Discards pending work and refuses later work. Idempotent; the handle stays open until
    /// `close()`.
    public func cancel() throws {
        stateLock.lock()
        guard !isClosed else {
            stateLock.unlock()
            throw CoreFailure.handleClosed
        }
        let activeDelivery = delivery
        let rawResult = ohand_core_cancel(handleIdentifier)
        stateLock.unlock()

        activeDelivery?.retire()
        _ = try consumeCoreResult(rawResult)
    }

    /// Cancels, stops the worker and releases the core. Idempotent.
    public func close() {
        stateLock.lock()
        guard !isClosed else {
            stateLock.unlock()
            return
        }
        isClosed = true
        let closedDelivery = delivery
        let closedContext = retainedContext
        delivery = nil
        retainedContext = nil
        _ = try? consumeCoreResult(ohand_core_close(handleIdentifier))
        stateLock.unlock()

        closedDelivery?.retire()
        releaseContext(closedContext)
    }

    private func releaseContext(_ context: UnsafeMutableRawPointer?) {
        if let context {
            Unmanaged<EventDelivery>.fromOpaque(context).release()
        }
    }
}
