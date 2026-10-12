import Foundation
import OhandCoreC

/// The native session read scope as the core's `ohand_core_start_export` takes it. Raw values are the core's.
public enum CoreExportReadAuthentication: String, Encodable, Equatable, Sendable {
    case authenticated
    case captureOnly = "capture_only"
}

/// An item scope the user may select for export. Raw values are the core's.
public enum CoreExportScope: String, Encodable, Equatable, Hashable, Sendable {
    case personal
    case work
}

/// Whether the export carries audio references. Raw values are the core's.
public enum CoreExportAudio: String, Encodable, Equatable, Sendable {
    case excluded
    case references
}

/// One export request. `outputPath` must be an absolute path of a file that does not exist yet; the core creates it
/// and writes the portable export there.
public struct CoreExportRequest: Encodable, Equatable, Sendable {
    public let readAuthentication: CoreExportReadAuthentication
    public let scopes: [CoreExportScope]
    public let audio: CoreExportAudio
    public let outputPath: String

    public init(
        readAuthentication: CoreExportReadAuthentication,
        scopes: [CoreExportScope],
        audio: CoreExportAudio,
        outputPath: String
    ) {
        self.readAuthentication = readAuthentication
        self.scopes = scopes
        self.audio = audio
        self.outputPath = outputPath
    }

    enum CodingKeys: String, CodingKey {
        case readAuthentication = "read_authentication"
        case scopes
        case audio
        case outputPath = "output_path"
    }
}

/// One exported item whose audio reference is in the export file.
public struct CoreExportAudioReference: Decodable, Equatable, Sendable {
    public let itemID: String
    public let captureID: String
    public let reference: String

    public init(itemID: String, captureID: String, reference: String) {
        self.itemID = itemID
        self.captureID = captureID
        self.reference = reference
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case captureID = "capture_id"
        case reference
    }
}

/// The answer to a completed export. `audioReferences` is empty unless audio references were requested.
public struct CoreExportAnswer: Decodable, Equatable, Sendable {
    public let itemCount: Int
    public let sha256: String
    public let audioReferences: [CoreExportAudioReference]

    public init(itemCount: Int, sha256: String, audioReferences: [CoreExportAudioReference]) {
        self.itemCount = itemCount
        self.sha256 = sha256
        self.audioReferences = audioReferences
    }

    enum CodingKeys: String, CodingKey {
        case itemCount = "item_count"
        case sha256
        case audioReferences = "audio_references"
    }
}

extension CoreHandle {
    /// Queues one export. The outcome event decodes as `CoreExportAnswer`, or is a failure: `export_not_authorized`
    /// for a capture-only session (nothing was read or created) and `export_write_failed` when the destination
    /// could not be created or written (no file is left behind). Throws when the request could not be queued, in
    /// which case nothing ran.
    public func startExport(operationID: UInt64, request: CoreExportRequest) throws {
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_export(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
