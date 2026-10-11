import Foundation
import OhandCoreC

/// What the core knows about a running transcription job: the audio to recognize and the locale to recognize it in.
/// It is read under the job's lease and carries no transcript text.
public struct TranscriptionJobSource: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let jobID: String
    public let itemID: String
    public let captureID: String
    public let leaseAttempt: Int
    /// The audio reference stored with the capture, relative to the protected storage root; nil without audio.
    public let audioReference: String?
    /// The locale the capture was made in, such as `en_US`.
    public let locale: String
    public let lifecycleState: String
    public let transcriptionState: String

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case jobID = "job_id"
        case itemID = "item_id"
        case captureID = "capture_id"
        case leaseAttempt = "lease_attempt"
        case audioReference = "audio_reference"
        case locale
        case lifecycleState = "lifecycle_state"
        case transcriptionState = "transcription_state"
    }
}

/// A transcript the on-device recognizer produced from one audio file.
public struct RecognizedTranscriptRequest: Encodable, Equatable, Sendable {
    public let text: String
    public let confidence: Double?
    public let detectedLanguage: String?
    /// Name and version of the recognizer, at most 128 bytes.
    public let recognizer: String
    /// Lowercase hex SHA-256 of the audio file the recognizer read.
    public let audioSHA256: String

    public init(text: String, confidence: Double?, detectedLanguage: String?, recognizer: String, audioSHA256: String) {
        self.text = text
        self.confidence = confidence
        self.detectedLanguage = detectedLanguage
        self.recognizer = recognizer
        self.audioSHA256 = audioSHA256
    }

    enum CodingKeys: String, CodingKey {
        case text
        case confidence
        case detectedLanguage = "detected_language"
        case recognizer
        case audioSHA256 = "audio_sha256"
    }
}

/// Interpretation work to queue when the transcript is attached, if the core authorizes it.
public struct InterpretationJobRequest: Encodable, Equatable, Sendable {
    public let jobID: String
    public let profileVersion: String?
    public let requestVersion: String

    public init(jobID: String, profileVersion: String?, requestVersion: String) {
        self.jobID = jobID
        self.profileVersion = profileVersion
        self.requestVersion = requestVersion
    }

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case profileVersion = "profile_version"
        case requestVersion = "request_version"
    }
}

/// What the recognizer concluded for one claimed job.
public enum TranscriptionResultRequest: Equatable, Sendable {
    case transcript(RecognizedTranscriptRequest)
    /// The language or on-device model is not available. The item stays pending and the audio is kept.
    case unsupported
    /// The audio could not be recognized. The item reports the failure and the audio is kept.
    case failed
}

/// One recognizer outcome for the job leased as `leaseAttempt`.
public struct TranscriptionOutcomeRequest: Encodable, Equatable, Sendable {
    public let jobID: String
    public let leaseAttempt: Int
    public let audioReference: String
    public let result: TranscriptionResultRequest
    public let interpretation: InterpretationJobRequest?

    public init(
        jobID: String,
        leaseAttempt: Int,
        audioReference: String,
        result: TranscriptionResultRequest,
        interpretation: InterpretationJobRequest? = nil
    ) {
        self.jobID = jobID
        self.leaseAttempt = leaseAttempt
        self.audioReference = audioReference
        self.result = result
        self.interpretation = interpretation
    }

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case leaseAttempt = "lease_attempt"
        case audioReference = "audio_reference"
        case result
        case transcript
        case interpretation
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(jobID, forKey: .jobID)
        try container.encode(leaseAttempt, forKey: .leaseAttempt)
        try container.encode(audioReference, forKey: .audioReference)
        switch result {
        case .transcript(let transcript):
            try container.encode("transcript", forKey: .result)
            try container.encode(transcript, forKey: .transcript)
        case .unsupported:
            try container.encode("unsupported", forKey: .result)
        case .failed:
            try container.encode("failed", forKey: .result)
        }
        try container.encodeIfPresent(interpretation, forKey: .interpretation)
    }
}

/// The core's account of an applied outcome. It is delivered only after the core's transaction committed (or an
/// earlier identical delivery had), and carries no transcript text.
public struct TranscriptionApplyAcknowledgment: Decodable, Equatable, Sendable {
    public enum Disposition: String, Decodable, Equatable, Sendable {
        /// The transcript is now the item's text.
        case attached
        /// An earlier delivery of the same job already attached the identical transcript.
        case alreadyAttached = "already_attached"
        /// No text was attached; the item reports `transcriptionState` and the audio is kept.
        case pending
        /// The outcome was refused against current durable state and the job retired; nothing about the item changed.
        case discarded
    }

    public struct Interpretation: Decodable, Equatable, Sendable {
        /// `not_requested`, `queued` or `not_authorized`.
        public let status: String
        public let jobID: String?
        public let denial: String?

        enum CodingKeys: String, CodingKey {
            case status
            case jobID = "job_id"
            case denial
        }
    }

    public let operationID: UInt64
    public let itemID: String
    public let jobID: String
    public let disposition: Disposition
    public let revision: Int?
    public let transcriptionState: String?
    public let discardReason: String?
    public let interpretation: Interpretation?

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case itemID = "item_id"
        case jobID = "job_id"
        case disposition
        case revision
        case transcriptionState = "transcription_state"
        case discardReason = "discard_reason"
        case interpretation
    }
}

extension CoreHandle {
    /// Queues a read of the running transcription job leased as `leaseAttempt`. The outcome event decodes as
    /// `TranscriptionJobSource`, or is a failure: `transcription_lease_not_held` when the job is unknown or not
    /// running under that lease, `transcription_wrong_job_type` for another kind of job. Nothing is written.
    /// Throws when the request could not be queued, in which case nothing ran.
    public func startReadTranscriptionJob(operationID: UInt64, jobID: String, leaseAttempt: Int) throws {
        struct Request: Encodable {
            let jobID: String
            let leaseAttempt: Int
            enum CodingKeys: String, CodingKey {
                case jobID = "job_id"
                case leaseAttempt = "lease_attempt"
            }
        }
        let requestBytes = Array(try JSONEncoder().encode(Request(jobID: jobID, leaseAttempt: leaseAttempt)))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_read_transcription_job(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues the guarded attachment of one recognizer outcome. The outcome event decodes as
    /// `TranscriptionApplyAcknowledgment`, or is a failure whose `code` says what happened:
    /// `transcription_rejected` (malformed; nothing written), `transcription_lease_not_held` (stale or duplicate
    /// delivery; nothing written), `transcription_not_authorized`, `transcription_conflicting_result` (the job
    /// already attached a different transcript), `transcription_not_committed` (nothing written; retry is safe) and
    /// `transcription_commit_unknown` (retry with the same outcome, which converges). Throws when the request could
    /// not be queued, in which case nothing ran.
    public func startApplyTranscriptionOutcome(operationID: UInt64, outcome: TranscriptionOutcomeRequest) throws {
        let requestBytes = Array(try JSONEncoder().encode(outcome))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_apply_transcription_outcome(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
