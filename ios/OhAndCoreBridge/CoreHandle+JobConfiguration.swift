import Foundation
import OhandCoreC

/// One job-configuration request, as `ohand_core_start_job_configuration` takes it. Profile versions and job IDs are the
/// core's own identifiers; no request names a destination, a credential or content.
public enum JobConfigurationCommand: Equatable, Sendable {
    /// The queued or running jobs pinned to `profileVersion`: job, item and status only. `limit` is capped by the core.
    case jobsPinnedToProfile(profileVersion: String, limit: Int?)
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

    fileprivate var request: Request {
        switch self {
        case .jobsPinnedToProfile(let profileVersion, let limit):
            return Request(command: "jobs_pinned_to_profile", profileVersion: profileVersion, limit: limit)
        case .requeueJob(let jobID, let newJobID, let newProfileVersion):
            return Request(
                command: "requeue_job", jobID: jobID, newJobID: newJobID, newProfileVersion: newProfileVersion)
        case .requeueRecord(let jobID):
            return Request(command: "requeue_record", jobID: jobID)
        case .revokeProfile(let profileVersion):
            return Request(command: "revoke_profile", profileVersion: profileVersion)
        case .repinWaitingWork(let routeID):
            return Request(command: "repin_waiting_work", routeID: routeID)
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

        enum CodingKeys: String, CodingKey {
            case command
            case profileVersion = "profile_version"
            case limit
            case jobID = "job_id"
            case newJobID = "new_job_id"
            case newProfileVersion = "new_profile_version"
            case routeID = "route_id"
        }
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
    case requeued(JobConfigurationRequeuedJob)
    case requeueRecord(JobConfigurationRequeueRecord?)
    case revoked(profileVersion: String)
    case repinned(moved: Int)

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
        case .requeueJob:
            return .requeued(try decoder.decode(JobConfigurationRequeuedJob.self, from: data))
        case .requeueRecord:
            return .requeueRecord(try decoder.decode(RecordEnvelope.self, from: data).record)
        case .revokeProfile:
            return .revoked(profileVersion: try decoder.decode(RevokedEnvelope.self, from: data).profileVersion)
        case .repinWaitingWork:
            return .repinned(moved: try decoder.decode(RepinEnvelope.self, from: data).moved)
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
