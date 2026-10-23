import Foundation
import OhandCoreC

/// One job-configuration request, as `ohand_core_start_job_configuration` takes it. Profile versions and job IDs are the
/// core's own identifiers; no request names a destination, a credential or content.
public enum JobConfigurationCommand: Equatable, Sendable {
    /// The queued or running jobs pinned to `profileVersion`: job, item and status only. `limit` is capped by the core.
    case jobsPinnedToProfile(profileVersion: String, limit: Int?)
    /// The jobs a revocation of `profileVersion` retired (failure reason `profile_revoked`), in stable order, one page at a
    /// time. `limit` is capped by the core; `after` is the previous page's `nextCursor`. Read it after the revoke.
    case jobsRetiredByRevocation(profileVersion: String, limit: Int?, after: String?)
    /// Retires `jobID` and queues `newJobID` pinned to `newProfileVersion`. `newJobID` is the identity of the user's
    /// action and is reused unchanged by a retry, which then answers the existing replacement.
    case requeueJob(jobID: String, newJobID: String, newProfileVersion: String)
    /// The requeue link in which `jobID` is either side, if any.
    case requeueRecord(jobID: String)
    /// Revokes a stored profile version and retires the jobs pinned to it.
    case revokeProfile(profileVersion: String)
    /// Moves the route's waiting unpinned interpretation work onto the profile the core selects now, when that profile
    /// is authorized for the route. Runs only when asked; the core moves a bounded batch per call.
    case repinWaitingWork(routeID: String)
    /// The active items whose newest `interpret` job ended `failed` and was not since replaced, newest first, one page at
    /// a time. `readScopes` is what the session may read; the client states it, the core refuses an empty list.
    case failedInterpretations(readScopes: [CoreExportScope], limit: Int?, cursor: String?)
    /// Runs the selected failed items again: a new `interpret` job each, pinned to what the core selects now.
    case rerunFailedInterpretations(readScopes: [CoreExportScope], selection: JobConfigurationRerunSelection)

    /// The same command with `readScopes` replaced, for the commands that carry them; any other command is unchanged.
    func withReadScopes(_ scopes: [CoreExportScope]) -> JobConfigurationCommand {
        switch self {
        case .failedInterpretations(_, let limit, let cursor):
            return .failedInterpretations(readScopes: scopes, limit: limit, cursor: cursor)
        case .rerunFailedInterpretations(_, let selection):
            return .rerunFailedInterpretations(readScopes: scopes, selection: selection)
        default:
            return self
        }
    }

    fileprivate var request: Request {
        switch self {
        case .jobsPinnedToProfile(let profileVersion, let limit):
            return Request(command: "jobs_pinned_to_profile", profileVersion: profileVersion, limit: limit)
        case .jobsRetiredByRevocation(let profileVersion, let limit, let after):
            return Request(
                command: "jobs_retired_by_revocation", profileVersion: profileVersion, limit: limit, after: after)
        case .requeueJob(let jobID, let newJobID, let newProfileVersion):
            return Request(
                command: "requeue_job", jobID: jobID, newJobID: newJobID, newProfileVersion: newProfileVersion)
        case .requeueRecord(let jobID):
            return Request(command: "requeue_record", jobID: jobID)
        case .revokeProfile(let profileVersion):
            return Request(command: "revoke_profile", profileVersion: profileVersion)
        case .repinWaitingWork(let routeID):
            return Request(command: "repin_waiting_work", routeID: routeID)
        case .failedInterpretations(let readScopes, let limit, let cursor):
            return Request(command: "failed_interpretations", limit: limit, cursor: cursor, readScopes: readScopes)
        case .rerunFailedInterpretations(let readScopes, let selection):
            switch selection {
            case .items(let itemIDs):
                return Request(command: "rerun_failed_interpretations", readScopes: readScopes, itemIDs: itemIDs)
            case .allListed:
                return Request(command: "rerun_failed_interpretations", readScopes: readScopes, allListed: true)
            }
        }
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        let command: String
        var profileVersion: String?
        var limit: Int?
        var jobID: String?
        var newJobID: String?
        var newProfileVersion: String?
        var routeID: String?
        var after: String?
        var cursor: String?
        var readScopes: [CoreExportScope]?
        var itemIDs: [String]?
        var allListed: Bool?

        enum CodingKeys: String, CodingKey {
            case command
            case profileVersion = "profile_version"
            case limit
            case jobID = "job_id"
            case newJobID = "new_job_id"
            case newProfileVersion = "new_profile_version"
            case routeID = "route_id"
            case after
            case cursor
            case readScopes = "read_scopes"
            case itemIDs = "item_ids"
            case allListed = "all_listed"
        }
    }
}

/// Which failed items a rerun covers.
public enum JobConfigurationRerunSelection: Equatable, Sendable {
    /// These items; the core takes at most 100.
    case items([String])
    /// Every item the failed-work read lists, newest first, at most 100 per call.
    case allListed
}

/// Whether running failed work again could succeed. Raw values are the core's.
public enum JobConfigurationRecovery: String, Decodable, Equatable, Sendable {
    /// A provider is allowed now, so a new job can be pinned and sent.
    case runAgain = "run_again"
    /// Nothing can run until the person changes a setting.
    case changeSettings = "change_settings"
}

/// One item whose interpretation failed. Carries no item text.
public struct JobConfigurationFailedInterpretation: Decodable, Equatable, Sendable {
    public let itemID: String
    /// RFC 3339: when the note was captured.
    public let capturedAt: String
    /// The job's failure reason as a fixed machine label.
    public let failureReason: String
    /// Whether the failed job was pinned to a provider profile.
    public let pinned: Bool
    public let profileVersion: String?
    public let recovery: JobConfigurationRecovery

    public init(
        itemID: String, capturedAt: String, failureReason: String, pinned: Bool, profileVersion: String?,
        recovery: JobConfigurationRecovery
    ) {
        self.itemID = itemID
        self.capturedAt = capturedAt
        self.failureReason = failureReason
        self.pinned = pinned
        self.profileVersion = profileVersion
        self.recovery = recovery
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case capturedAt = "captured_at"
        case failureReason = "failure_reason"
        case pinned
        case profileVersion = "profile_version"
        case recovery
    }
}

/// One page of failed interpretations and the cursor of the next page (`nil` on the last).
public struct JobConfigurationFailedInterpretations: Decodable, Equatable, Sendable {
    public let failed: [JobConfigurationFailedInterpretation]
    public let nextCursor: String?

    public init(failed: [JobConfigurationFailedInterpretation], nextCursor: String?) {
        self.failed = failed
        self.nextCursor = nextCursor
    }

    enum CodingKeys: String, CodingKey {
        case failed
        case nextCursor = "next_cursor"
    }
}

/// Why an item was not run again. Raw values are the core's.
public enum JobConfigurationSkipReason: String, Decodable, Equatable, Sendable {
    /// No failed, unreplaced interpretation the session may read: unknown, deleted, not failed or already run again.
    case notFailed = "not_failed"
    /// No provider is allowed for the item now.
    case noProviderAllowed = "no_provider_allowed"
}

public struct JobConfigurationSkippedItem: Decodable, Equatable, Sendable {
    public let itemID: String
    public let reason: JobConfigurationSkipReason

    public init(itemID: String, reason: JobConfigurationSkipReason) {
        self.itemID = itemID
        self.reason = reason
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case reason
    }
}

/// The answer to `rerunFailedInterpretations`: how many items got a new job and which were skipped, with reasons.
public struct JobConfigurationRerunResult: Decodable, Equatable, Sendable {
    public let moved: Int
    public let skipped: Int
    public let skippedItems: [JobConfigurationSkippedItem]

    public init(moved: Int, skipped: Int, skippedItems: [JobConfigurationSkippedItem]) {
        self.moved = moved
        self.skipped = skipped
        self.skippedItems = skippedItems
    }

    enum CodingKeys: String, CodingKey {
        case moved, skipped
        case skippedItems = "skipped_items"
    }
}

/// One job pinned to a profile. Carries no item content.
public struct JobConfigurationPinnedJob: Decodable, Equatable, Sendable {
    public let jobID: String
    public let itemID: String
    public let status: String

    public init(jobID: String, itemID: String, status: String) {
        self.jobID = jobID
        self.itemID = itemID
        self.status = status
    }

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case itemID = "item_id"
        case status
    }
}

/// The answer to `jobsPinnedToProfile`: at most the requested number of jobs, the full count and whether the list was cut.
public struct JobConfigurationPinnedJobs: Decodable, Equatable, Sendable {
    public let jobs: [JobConfigurationPinnedJob]
    public let total: Int
    public let truncated: Bool

    public init(jobs: [JobConfigurationPinnedJob], total: Int, truncated: Bool) {
        self.jobs = jobs
        self.total = total
        self.truncated = truncated
    }
}

/// One job a revocation retired. Carries no item content. `requeuedTo` is the replacement job, once requeued.
public struct JobConfigurationRetiredJob: Decodable, Equatable, Sendable {
    public let jobID: String
    public let itemID: String
    /// RFC 3339: when the profile was revoked.
    public let retiredAt: String
    public let requeuedTo: String?

    public init(jobID: String, itemID: String, retiredAt: String, requeuedTo: String?) {
        self.jobID = jobID
        self.itemID = itemID
        self.retiredAt = retiredAt
        self.requeuedTo = requeuedTo
    }

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case itemID = "item_id"
        case retiredAt = "retired_at"
        case requeuedTo = "requeued_to"
    }
}

/// The answer to `jobsRetiredByRevocation`: one page, the full count, and the cursor of the next page (`nil` on the last).
public struct JobConfigurationRetiredJobs: Decodable, Equatable, Sendable {
    public let jobs: [JobConfigurationRetiredJob]
    public let total: Int
    public let truncated: Bool
    public let nextCursor: String?

    public init(jobs: [JobConfigurationRetiredJob], total: Int, truncated: Bool, nextCursor: String?) {
        self.jobs = jobs
        self.total = total
        self.truncated = truncated
        self.nextCursor = nextCursor
    }

    enum CodingKeys: String, CodingKey {
        case jobs, total, truncated
        case nextCursor = "next_cursor"
    }
}

/// The answer to `requeueJob`: the replacement job.
public struct JobConfigurationRequeuedJob: Decodable, Equatable, Sendable {
    public let jobID: String
    public let itemID: String
    public let status: String
    public let profileVersion: String?

    public init(jobID: String, itemID: String, status: String, profileVersion: String?) {
        self.jobID = jobID
        self.itemID = itemID
        self.status = status
        self.profileVersion = profileVersion
    }

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case itemID = "item_id"
        case status
        case profileVersion = "profile_version"
    }
}

/// A requeue link. `fromProfileVersion` is `nil` when the old job was waiting unpinned work that a re-pin replaced.
public struct JobConfigurationRequeueRecord: Decodable, Equatable, Sendable {
    public let oldJobID: String
    public let newJobID: String
    public let fromProfileVersion: String?
    public let toProfileVersion: String
    /// RFC 3339.
    public let requeuedAt: String

    public init(
        oldJobID: String, newJobID: String, fromProfileVersion: String?, toProfileVersion: String, requeuedAt: String
    ) {
        self.oldJobID = oldJobID
        self.newJobID = newJobID
        self.fromProfileVersion = fromProfileVersion
        self.toProfileVersion = toProfileVersion
        self.requeuedAt = requeuedAt
    }

    enum CodingKeys: String, CodingKey {
        case oldJobID = "old_job_id"
        case newJobID = "new_job_id"
        case fromProfileVersion = "from_profile_version"
        case toProfileVersion = "to_profile_version"
        case requeuedAt = "requeued_at"
    }
}

/// The core's answer to a job-configuration request, by command. An unknown job or profile is a failure with code
/// `not_found`; a change the core refuses is a failure with code `job_configuration_refused`.
public enum JobConfigurationAnswer: Equatable, Sendable {
    case pinnedJobs(JobConfigurationPinnedJobs)
    case retiredJobs(JobConfigurationRetiredJobs)
    case requeued(JobConfigurationRequeuedJob)
    case requeueRecord(JobConfigurationRequeueRecord?)
    case revoked(profileVersion: String)
    case repinned(moved: Int)
    case failedInterpretations(JobConfigurationFailedInterpretations)
    case rerun(JobConfigurationRerunResult)

    private struct RecordEnvelope: Decodable {
        let record: JobConfigurationRequeueRecord?
    }

    private struct RevokedEnvelope: Decodable {
        let profileVersion: String

        enum CodingKeys: String, CodingKey { case profileVersion = "profile_version" }
    }

    private struct RepinEnvelope: Decodable {
        let moved: Int
    }

    /// Decodes the core's event payload for `command`.
    public static func decode(_ data: Data, for command: JobConfigurationCommand) throws -> JobConfigurationAnswer {
        let decoder = JSONDecoder()
        switch command {
        case .jobsPinnedToProfile:
            return .pinnedJobs(try decoder.decode(JobConfigurationPinnedJobs.self, from: data))
        case .jobsRetiredByRevocation:
            return .retiredJobs(try decoder.decode(JobConfigurationRetiredJobs.self, from: data))
        case .requeueJob:
            return .requeued(try decoder.decode(JobConfigurationRequeuedJob.self, from: data))
        case .requeueRecord:
            return .requeueRecord(try decoder.decode(RecordEnvelope.self, from: data).record)
        case .revokeProfile:
            return .revoked(profileVersion: try decoder.decode(RevokedEnvelope.self, from: data).profileVersion)
        case .repinWaitingWork:
            return .repinned(moved: try decoder.decode(RepinEnvelope.self, from: data).moved)
        case .failedInterpretations:
            return .failedInterpretations(try decoder.decode(JobConfigurationFailedInterpretations.self, from: data))
        case .rerunFailedInterpretations:
            return .rerun(try decoder.decode(JobConfigurationRerunResult.self, from: data))
        }
    }
}

extension CoreHandle {
    /// Queues one job-configuration request. The outcome event decodes with `JobConfigurationAnswer.decode(_:for:)`, or
    /// is a failure. Throws when the request could not be queued, in which case nothing ran.
    public func startJobConfiguration(operationID: UInt64, command: JobConfigurationCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_job_configuration(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
