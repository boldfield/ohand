import Foundation
import OhAndCoreBridge

/// Why a job-configuration control did not take effect. Every case except `core` and `unreadable` means the core decided
/// (or never was asked) and nothing changed.
public enum JobConfigurationError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty identifier, a limit below one).
    case invalidRequest
    /// No stored job or profile version has that identifier.
    case notFound
    /// The core's rules refuse the change, for example a revoked or identical target profile, or a job in a state that
    /// cannot move.
    case refused
    case core(CoreFailure)
    case unreadable
}

/// The controls for work whose provider configuration changed after it was queued. Every call goes to the core, which
/// decides; this service adds the authentication requirement and a typed result.
///
/// * **Explicit.** Changing a default, creating a profile or granting a route never moves queued or waiting work.
///   `repinWaitingWork` is the only call that moves work that was never pinned, and it runs only when called.
/// * **Never reroutes silently.** A job pinned to a profile keeps that pin. `requeue` retires the old job and records the
///   link; `repinWaitingWork` leaves every existing job untouched and queues new ones.
/// * **Authorized targets only.** A re-pin moves work only to the profile the core selects now and only if that profile
///   is authorized for the route; otherwise it moves nothing and says so with a zero count.
/// * **Authenticated.** The session must be authenticated when a call is made; a capture-only session gets
///   `notAuthenticated` and the core is not asked. A read whose answer arrives after a relock is dropped. A change the
///   core confirmed is reported truthfully even if the session relocked while it ran.
public final class JobConfigurationService: @unchecked Sendable {
    private let core: JobConfigurationCore
    private let session: ReadAuthenticationSession

    init(core: JobConfigurationCore, session: ReadAuthenticationSession) {
        self.core = core
        self.session = session
    }

    /// The queued or running jobs pinned to `profileVersion`, item and status only. `limit` (at least 1) narrows the
    /// list; the core caps it at 100 and reports the full count in `total`.
    public func jobsPinned(toProfile profileVersion: String, limit: Int? = nil) async throws
        -> JobConfigurationPinnedJobs
    {
        guard Self.isIdentifier(profileVersion), limit.map({ $0 >= 1 }) ?? true else {
            throw JobConfigurationError.invalidRequest
        }
        let answer = try await perform(
            .jobsPinnedToProfile(profileVersion: profileVersion, limit: limit), isRead: true)
        guard case .pinnedJobs(let jobs) = answer else { throw JobConfigurationError.unreadable }
        return jobs
    }

    /// Retires `jobID` and queues `newJobID` pinned to `newProfileVersion`. Keep `newJobID` across retries of the same
    /// user action: repeating it answers the existing replacement instead of queueing another.
    public func requeue(jobID: String, newJobID: String, toProfileVersion newProfileVersion: String) async throws
        -> JobConfigurationRequeuedJob
    {
        guard Self.isIdentifier(jobID), Self.isIdentifier(newJobID), Self.isIdentifier(newProfileVersion) else {
            throw JobConfigurationError.invalidRequest
        }
        let answer = try await perform(
            .requeueJob(jobID: jobID, newJobID: newJobID, newProfileVersion: newProfileVersion), isRead: false)
        guard case .requeued(let job) = answer else { throw JobConfigurationError.unreadable }
        return job
    }

    /// The requeue link in which `jobID` is either side, or `nil` when it was never requeued.
    public func requeueRecord(forJob jobID: String) async throws -> JobConfigurationRequeueRecord? {
        guard Self.isIdentifier(jobID) else { throw JobConfigurationError.invalidRequest }
        let answer = try await perform(.requeueRecord(jobID: jobID), isRead: true)
        guard case .requeueRecord(let record) = answer else { throw JobConfigurationError.unreadable }
        return record
    }

    /// Revokes a stored profile version and retires the jobs pinned to it. Allowed while a reset holds a credential claim,
    /// because revoking changes no credential reference. Revoking again keeps the first revocation.
    public func revokeProfile(_ profileVersion: String) async throws {
        guard Self.isIdentifier(profileVersion) else { throw JobConfigurationError.invalidRequest }
        let answer = try await perform(.revokeProfile(profileVersion: profileVersion), isRead: false)
        guard case .revoked = answer else { throw JobConfigurationError.unreadable }
    }

    /// Moves the route's waiting unpinned interpretation work onto the profile the core selects now and returns how many
    /// items moved (at most 100 per call; call again for more). Zero when nothing is configured, the selected profile is
    /// not authorized for the route, or nothing is waiting.
    @discardableResult
    public func repinWaitingWork(routeID: String) async throws -> Int {
        guard Self.isIdentifier(routeID) else { throw JobConfigurationError.invalidRequest }
        let answer = try await perform(.repinWaitingWork(routeID: routeID), isRead: false)
        guard case .repinned(let moved) = answer else { throw JobConfigurationError.unreadable }
        return moved
    }

    private func perform(_ command: JobConfigurationCommand, isRead: Bool) async throws -> JobConfigurationAnswer {
        guard let authorization = session.currentAuthorization() else { throw JobConfigurationError.notAuthenticated }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<JobConfigurationAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        if isRead, !session.isCurrent(authorization) { throw JobConfigurationError.notAuthenticated }
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw JobConfigurationError.notFound
            case "invalid_request": throw JobConfigurationError.invalidRequest
            case "job_configuration_refused": throw JobConfigurationError.refused
            default: throw JobConfigurationError.core(failure)
            }
        }
    }

    private static func isIdentifier(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 512
    }
}
