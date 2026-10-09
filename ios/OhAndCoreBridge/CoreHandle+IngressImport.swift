import Foundation
import OhandCoreC

/// The Capture Ingestion Contract acknowledgment: capture ID, item ID and save timestamp. It is delivered only after
/// the core's import transaction committed, so it is the only success a native caller may treat as "saved". It is not
/// the pre-import write acknowledgment (`SaveCaptureAcknowledgment`), which means the capture bytes are durable and
/// nothing more.
public struct IngressImportAcknowledgment: Decodable, Equatable, Sendable {
    public enum Disposition: String, Decodable, Equatable, Sendable {
        case imported
        case alreadyImported = "already_imported"
    }

    public let operationID: UInt64
    public let captureID: String
    public let itemID: String
    public let savedAt: String
    public let disposition: Disposition

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case captureID = "capture_id"
        case itemID = "item_id"
        case savedAt = "saved_at"
        case disposition
    }
}

extension CoreHandle {
    /// Queues the foreground import of one ingress record. The outcome event decodes as
    /// `IngressImportAcknowledgment`, or is a failure whose `code` says whether the import committed:
    /// `ingress_not_committed` and the validation codes wrote nothing, `ingress_commit_unknown` may have committed
    /// (retry with the same capture ID), `ingress_conflicting_reuse` left the stored capture untouched, and
    /// `ingress_item_deleted` is terminal. Throws when the request could not be queued, in which case nothing ran.
    public func startImportForegroundIngress(operationID: UInt64, record: CaptureRecord) throws {
        let requestBytes = Array(try JSONEncoder().encode(record))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_import_foreground_ingress(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
