import Foundation

/// What a status surface may say about background processing as a whole, most urgent first. Mirrors the core's
/// `HealthSummary`.
public enum ProcessingHealthSummary: String, Decodable, Equatable, Sendable {
    /// Work is overdue, or a lease lapsed, and nothing picked it up.
    case stalled
    /// A failure that retrying cannot clear, or a destination that needs the user.
    case needsAttention = "needs_attention"
    /// Everything pending is deliberately waiting: backoff, offline, or a missing capability.
    case waiting
    /// Work is queued or running within its normal window.
    case working
    /// Nothing is pending and nothing has failed.
    case idle
}

public enum ProcessingStallReason: String, Decodable, Equatable, Sendable {
    case overdue
    case leaseNotRecovered = "lease_not_recovered"
}

public enum ProcessingErrorKind: String, Decodable, Equatable, Sendable {
    case retryScheduled = "retry_scheduled"
    case waitingForNetwork = "waiting_for_network"
    case capabilityUnavailable = "capability_unavailable"
    case leaseExpired = "lease_expired"
    case destinationUnavailable = "destination_unavailable"
    case retriesExhausted = "retries_exhausted"
    case permanentFailure = "permanent_failure"
}

/// How a reported error clears.
public enum ProcessingRecoveryPath: String, Decodable, Equatable, Sendable {
    /// The queue resumes the work by itself.
    case automatic
    /// The user must choose or re-authorize a destination.
    case userAction = "user_action"
    /// The job has ended; the capture and its source are untouched.
    case terminal
}

/// Jobs sharing a kind and a machine reason label. Never carries captured content or identifiers.
public struct ProcessingError: Decodable, Equatable, Sendable {
    public let kind: ProcessingErrorKind
    public let reason: String
    public let recovery: ProcessingRecoveryPath
    public let jobCount: Int
    public let oldestAgeSeconds: Int
    public let nextAttemptAt: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case reason
        case recovery
        case jobCount = "job_count"
        case oldestAgeSeconds = "oldest_age_seconds"
        case nextAttemptAt = "next_attempt_at"
    }
}

/// The core's content-free account of processing health. Timestamps stay RFC 3339 text exactly as the core wrote
/// them.
public struct ProcessingHealth: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let generatedAt: String
    public let summary: ProcessingHealthSummary
    public let stalled: Bool
    public let stallReasons: [ProcessingStallReason]
    public let pendingJobs: Int
    public let runningJobs: Int
    public let overdueJobs: Int
    public let oldestPendingAgeSeconds: Int?
    public let oldestOverdueSeconds: Int?
    /// The newest recorded interpretation result. Job types that leave no timestamped result, such as
    /// transcription, are not reflected in it.
    public let lastInterpretationSuccessAt: String?
    public let errors: [ProcessingError]

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case generatedAt = "generated_at"
        case summary
        case stalled
        case stallReasons = "stall_reasons"
        case pendingJobs = "pending_jobs"
        case runningJobs = "running_jobs"
        case overdueJobs = "overdue_jobs"
        case oldestPendingAgeSeconds = "oldest_pending_age_seconds"
        case oldestOverdueSeconds = "oldest_overdue_seconds"
        case lastInterpretationSuccessAt = "last_interpretation_success_at"
        case errors
    }
}
