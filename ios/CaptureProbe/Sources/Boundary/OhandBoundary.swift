import Foundation

/// Normalized error classes shared with the Rust core (see docs/architecture/m1-contracts.md).
enum BoundaryErrorClass: String, Decodable {
    case transient
    case permanent
    case unauthorized
    case cancelled
    case unsupported
}

/// A normalized failure. Rust-originated failures carry fixed, content-free messages.
struct BoundaryFailure: Error, Equatable, Decodable {
    let errorClass: BoundaryErrorClass
    let code: String
    let message: String

    enum CodingKeys: String, CodingKey {
        case errorClass = "class"
        case code
        case message
    }

    static let storeClosed = BoundaryFailure(
        errorClass: .permanent,
        code: "store_closed",
        message: "the store handle was already released"
    )
    static let storeUnavailable = BoundaryFailure(
        errorClass: .permanent,
        code: "store_unavailable",
        message: "the core store could not be opened"
    )
}

struct CaptureRecord: Codable, Equatable {
    var captureId: String
    var text: String?
    var audioReference: String?
    var captureInstant: String
    var timezoneId: String
    var utcOffsetMinutes: Int32
    var locale: String
    var calendar: String
    var itemScope: String
    var routeId: String
    var entryLocked: Bool
    var createdAt: String
    var sessionTopic: String?

    enum CodingKeys: String, CodingKey {
        case captureId = "capture_id"
        case text
        case audioReference = "audio_reference"
        case captureInstant = "capture_instant"
        case timezoneId = "timezone_id"
        case utcOffsetMinutes = "utc_offset_minutes"
        case locale
        case calendar
        case itemScope = "item_scope"
        case routeId = "route_id"
        case entryLocked = "entry_locked"
        case createdAt = "created_at"
        case sessionTopic = "session_topic"
    }

    static func sample(captureId: String, text: String) -> CaptureRecord {
        CaptureRecord(
            captureId: captureId,
            text: text,
            audioReference: nil,
            captureInstant: "2026-03-01T09:30:00Z",
            timezoneId: "Europe/Berlin",
            utcOffsetMinutes: 60,
            locale: "de_DE",
            calendar: "gregorian",
            itemScope: "personal",
            routeId: "route-default",
            entryLocked: false,
            createdAt: "2026-03-01T09:30:00Z",
            sessionTopic: nil
        )
    }
}

struct SavedCapture: Decodable, Equatable {
    let capture: CaptureRecord
    let idempotentReplay: Bool

    enum CodingKeys: String, CodingKey {
        case capture
        case idempotentReplay = "idempotent_replay"
    }
}

/// Takes ownership of a Rust-allocated result, copies its bytes and always releases the
/// Rust buffer exactly once. Returns the response bytes or throws the normalized failure.
func consumeResult(_ rawResult: OhandResult) throws -> Data {
    var result = rawResult
    defer { ohand_result_free(&result) }
    var bytes = Data()
    if let pointer = result.data, result.len > 0 {
        bytes = Data(bytes: pointer, count: Int(result.len))
    }
    if result.status == UInt32(OHAND_STATUS_OK) {
        return bytes
    }
    if let failure = try? JSONDecoder().decode(BoundaryFailure.self, from: bytes) {
        throw failure
    }
    throw BoundaryFailure(
        errorClass: .permanent,
        code: "unreadable_failure",
        message: "status \(result.status) without a readable error payload"
    )
}

func boundaryLiveAllocations() -> Int {
    Int(ohand_bindings_live_allocations())
}

func boundaryMaxRequestBytes() -> Int {
    Int(ohand_bindings_max_request_bytes())
}

/// Cooperative cancellation flag owned by Swift; the Rust side only borrows it during a call.
final class CancelToken: @unchecked Sendable {
    fileprivate let handle: OpaquePointer

    init() {
        handle = ohand_cancel_token_new()
    }

    func cancel() {
        ohand_cancel_token_cancel(handle)
    }

    deinit {
        ohand_cancel_token_free(handle)
    }
}

/// A file-backed core capture store. The lock makes `close()` wait for in-flight calls, so a
/// handle is never freed while Rust is using it.
final class ProbeStore: @unchecked Sendable {
    private var handle: OpaquePointer?
    private let lock = NSLock()

    init() throws {
        let appSupport = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        let storePath = appSupport.appendingPathComponent("captures.db").path
        let pathBytes = [UInt8](storePath.utf8)
        guard let opened = pathBytes.withUnsafeBufferPointer({ buffer in
            ohand_probe_store_open_at_path(buffer.baseAddress, buffer.count)
        }) else {
            throw BoundaryFailure.storeUnavailable
        }
        handle = opened
    }

    func close() {
        lock.lock()
        defer { lock.unlock() }
        if let openHandle = handle {
            ohand_probe_store_free(openHandle)
            handle = nil
        }
    }

    deinit {
        close()
    }

    func withHandle<Output>(_ body: (OpaquePointer) throws -> Output) throws -> Output {
        lock.lock()
        defer { lock.unlock() }
        guard let openHandle = handle else {
            throw BoundaryFailure.storeClosed
        }
        return try body(openHandle)
    }

    func saveRaw(_ bytes: [UInt8], cancelToken: CancelToken? = nil) throws -> Data {
        try withExtendedLifetime(cancelToken) {
            try withHandle { openHandle in
                try bytes.withUnsafeBufferPointer { buffer in
                    try consumeResult(
                        ohand_probe_save_capture(openHandle, cancelToken?.handle, buffer.baseAddress, buffer.count)
                    )
                }
            }
        }
    }

    func save(_ record: CaptureRecord, cancelToken: CancelToken? = nil) throws -> SavedCapture {
        let request = try JSONEncoder().encode(record)
        let response = try saveRaw([UInt8](request), cancelToken: cancelToken)
        return try JSONDecoder().decode(SavedCapture.self, from: response)
    }

    func capture(id captureId: String, cancelToken: CancelToken? = nil) throws -> CaptureRecord {
        let idBytes = [UInt8](captureId.utf8)
        let response: Data = try withExtendedLifetime(cancelToken) {
            try withHandle { openHandle in
                try idBytes.withUnsafeBufferPointer { buffer in
                    try consumeResult(
                        ohand_probe_get_capture(openHandle, cancelToken?.handle, buffer.baseAddress, buffer.count)
                    )
                }
            }
        }
        return try JSONDecoder().decode(CaptureRecord.self, from: response)
    }
}
