import Foundation
import OhandCoreC

/// A JSON value the bridge passes through without interpreting it.
public enum CoreJSONValue: Decodable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([CoreJSONValue])
    case object([String: CoreJSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([CoreJSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: CoreJSONValue].self))
        }
    }
}

/// What the core asks native code to send. The destination, headers, credential reference and origin
/// allow-list were all produced by the core at dispatch from stored policy; native code adds only the secret.
public struct ProviderSendCommand: Decodable, Equatable, Sendable {
    public struct Credential: Decodable, Equatable, Sendable {
        public let reference: String
        public let header: String
        public let scheme: String?
    }

    public let operationID: UInt64
    public let url: String
    public let method: String
    public let headers: [[String]]
    public let body: String
    public let timeoutMilliseconds: UInt64
    public let maxResponseBytes: Int
    public let credential: Credential?
    public let authorizedOrigins: [String]
    /// The capability the core authorized this request under (`text_interpretation` or `review`).
    public let capability: String

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case url
        case method
        case headers
        case body
        case timeoutMilliseconds = "timeout_ms"
        case maxResponseBytes = "max_response_bytes"
        case credential
        case authorizedOrigins = "authorized_origins"
        case capability
    }
}

/// A command from the core's provider exchange to native code, delivered on a core exchange thread.
public enum ProviderTransportCommand: Equatable, Sendable {
    /// Start the HTTP exchange and answer exactly once with `completeProviderExchange` or `failProviderExchange`.
    case send(ProviderSendCommand)
    /// The exchange was cancelled, ran past its deadline or its core closed; abandon it.
    case cancel
}

public enum ProviderExchangePhase: String, Decodable, Equatable, Sendable {
    case dispatched
    case completed
}

/// The validated interpretation the core produced from a provider response.
public struct ProviderExchangeOutput: Decodable, Equatable, Sendable {
    public let requestVersion: String
    public let proposal: CoreJSONValue

    enum CodingKeys: String, CodingKey {
        case requestVersion = "request_version"
        case proposal
    }
}

/// Success events of a provider exchange: `dispatched` once the core authorized and built the request, then
/// `completed` with the output. A failure arrives as a normal failed `CoreEvent` instead.
public struct ProviderExchangeEvent: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let phase: ProviderExchangePhase
    public let output: ProviderExchangeOutput?

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case phase
        case output
    }
}

/// Keeps one native transport handler registered with a core. The handler runs on a core exchange thread and
/// must return promptly (start the request and return). `invalidate()` waits for a running handler invocation, so
/// it must not be called from inside the handler.
public final class ProviderTransportRegistration: @unchecked Sendable {
    private let handleIdentifier: OhandCoreHandle
    private let handler: @Sendable (UInt64, ProviderTransportCommand) -> Void
    private let stateLock = NSLock()
    private var isInvalidated = false

    fileprivate init(handleIdentifier: OhandCoreHandle, handler: @escaping @Sendable (UInt64, ProviderTransportCommand) -> Void) {
        self.handleIdentifier = handleIdentifier
        self.handler = handler
    }

    fileprivate func receive(operationID: UInt64, command: UInt32, data: Data) {
        switch command {
        case UInt32(OHAND_PROVIDER_COMMAND_SEND):
            guard let send = try? JSONDecoder().decode(ProviderSendCommand.self, from: data) else {
                _ = try? consumeCoreResult(failExchange(operationID: operationID, error: "rejected"))
                return
            }
            handler(operationID, .send(send))
        case UInt32(OHAND_PROVIDER_COMMAND_CANCEL):
            handler(operationID, .cancel)
        default:
            break
        }
    }

    private func failExchange(operationID: UInt64, error: String) -> OhandCoreResult {
        let errorBytes = Array(error.utf8)
        return errorBytes.withUnsafeBufferPointer { buffer in
            ohand_core_fail_provider_exchange(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
    }

    /// Unregisters the handler, cancelling the handle's running exchanges, and releases the registration's
    /// retained context. Idempotent. After it returns the handler is never invoked again. A registration that a
    /// later `registerProviderTransport` replaced only releases itself and leaves the replacement active.
    public func invalidate() {
        stateLock.lock()
        guard !isInvalidated else {
            stateLock.unlock()
            return
        }
        isInvalidated = true
        stateLock.unlock()

        let ownContext = Unmanaged.passUnretained(self).toOpaque()
        _ = try? consumeCoreResult(ohand_core_set_provider_transport(handleIdentifier, nil, ownContext))
        Unmanaged.passUnretained(self).release()
    }
}

/// Runs on a core exchange thread. The context is a `ProviderTransportRegistration` retained once on creation and
/// released by `invalidate()`, which first waits for any running invocation.
private let providerTransportTrampoline: OhandProviderTransportCallback = { context, operationID, command, data, length in
    guard let context else { return }
    let registration = Unmanaged<ProviderTransportRegistration>.fromOpaque(context).takeUnretainedValue()
    var bytes = Data()
    if let data, length > 0 {
        bytes = Data(bytes: data, count: length)
    }
    registration.receive(operationID: operationID, command: command, data: bytes)
}

extension CoreHandle {
    /// Registers the native transport the core uses for provider exchanges, replacing any earlier one. Keep the
    /// returned registration until the transport is no longer needed, then call `invalidate()` before closing.
    public func registerProviderTransport(
        handler: @escaping @Sendable (UInt64, ProviderTransportCommand) -> Void
    ) throws -> ProviderTransportRegistration {
        let registration = ProviderTransportRegistration(handleIdentifier: handleIdentifier, handler: handler)
        let context = Unmanaged.passRetained(registration).toOpaque()
        let rawResult = ohand_core_set_provider_transport(handleIdentifier, providerTransportTrampoline, context)
        do {
            _ = try consumeCoreResult(rawResult)
        } catch {
            Unmanaged<ProviderTransportRegistration>.fromOpaque(context).release()
            throw error
        }
        return registration
    }

    /// Queues the exchange for a stored job. The core reads authorization, destination, credential reference and
    /// request text from stored state; the caller supplies only the job identifier. Events with `operationID`:
    /// a failure (nothing was sent), or a `dispatched` event followed by a final `completed` event or failure.
    public func startProviderExchange(operationID: UInt64, jobID: String) throws {
        let requestBytes = Array(try JSONEncoder().encode(["job_id": jobID]))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_provider_exchange(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Cancels a running exchange. Idempotent; the final event is a `cancelled` failure.
    public func cancelProviderExchange(operationID: UInt64) throws {
        _ = try consumeCoreResult(ohand_core_cancel_provider_exchange(handleIdentifier, operationID))
    }

    /// Answers a send with a completed HTTP exchange of any status. `not_found` means the exchange already ended.
    public func completeProviderExchange(
        operationID: UInt64,
        status: Int,
        headers: [String: String],
        body: Data
    ) throws {
        let headerPairs = headers.sorted { $0.key < $1.key }.map { [$0.key, $0.value] }
        let headerBytes = Array(try JSONEncoder().encode(headerPairs))
        let bodyBytes = Array(body)
        let rawResult = headerBytes.withUnsafeBufferPointer { headerBuffer in
            bodyBytes.withUnsafeBufferPointer { bodyBuffer in
                ohand_core_complete_provider_exchange(
                    handleIdentifier,
                    operationID,
                    UInt32(clamping: status),
                    headerBuffer.baseAddress,
                    headerBuffer.count,
                    bodyBuffer.baseAddress,
                    bodyBuffer.count
                )
            }
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Answers a send with a native failure named by the core transport error (`timeout`, `cancelled`,
    /// `unavailable`, `unauthorized`, `rejected`, `invalid_output`).
    public func failProviderExchange(operationID: UInt64, error: String) throws {
        let errorBytes = Array(error.utf8)
        let rawResult = errorBytes.withUnsafeBufferPointer { buffer in
            ohand_core_fail_provider_exchange(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
