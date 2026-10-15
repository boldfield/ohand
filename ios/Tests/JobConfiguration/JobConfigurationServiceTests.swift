import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each command with the next scripted result and records what the service asked. A hook runs between the
/// core's answer and its delivery, so a test can relock the session while a request is in flight.
private final class ScriptedJobConfigurationCore: JobConfigurationCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<JobConfigurationAnswer, CoreFailure>]
    private var received: [JobConfigurationCommand] = []
    var beforeAnswer: (@Sendable () -> Void)?

    init(_ script: [Result<JobConfigurationAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [JobConfigurationCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: JobConfigurationCommand,
        completion: @escaping @Sendable (Result<JobConfigurationAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        beforeAnswer?()
        completion(next ?? .failure(CoreJobConfigurationClient.unreadableAnswer))
    }
}

final class JobConfigurationServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func failure(_ code: String) -> CoreFailure {
        CoreFailure(errorClass: .permanent, code: code, message: "fixed message")
    }

    func testEachControlSendsItsCommandAndReturnsTheTypedAnswer() async throws {
        let pinned = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued")], total: 1,
            truncated: false)
        let requeued = JobConfigurationRequeuedJob(
            jobID: "job-2", itemID: "item-1", status: "queued", profileVersion: "profile-b")
        let record = JobConfigurationRequeueRecord(
            oldJobID: "job-1", newJobID: "job-2", fromProfileVersion: "profile-a", toProfileVersion: "profile-b",
            requeuedAt: "2026-10-14T10:30:00Z")
        let core = ScriptedJobConfigurationCore([
            .success(.pinnedJobs(pinned)), .success(.requeued(requeued)), .success(.requeueRecord(record)),
            .success(.requeueRecord(nil)), .success(.revoked(profileVersion: "profile-a")),
            .success(.repinned(moved: 3)),
        ])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))

        let listing = try await service.jobsPinned(toProfile: "profile-a", limit: 5)
        let replacement = try await service.requeue(jobID: "job-1", newJobID: "job-2", toProfileVersion: "profile-b")
        let found = try await service.requeueRecord(forJob: "job-2")
        let missing = try await service.requeueRecord(forJob: "job-9")
        try await service.revokeProfile("profile-a")
        let moved = try await service.repinWaitingWork(routeID: "route-1")

        XCTAssertEqual(listing, pinned)
        XCTAssertEqual(replacement, requeued)
        XCTAssertEqual(found, record)
        XCTAssertNil(missing)
        XCTAssertEqual(moved, 3)
        XCTAssertEqual(
            core.commands,
            [
                .jobsPinnedToProfile(profileVersion: "profile-a", limit: 5),
                .requeueJob(jobID: "job-1", newJobID: "job-2", newProfileVersion: "profile-b"),
                .requeueRecord(jobID: "job-2"), .requeueRecord(jobID: "job-9"),
                .revokeProfile(profileVersion: "profile-a"), .repinWaitingWork(routeID: "route-1"),
            ])
    }

    func testACaptureOnlySessionIsRefusedByEveryControlAndTheCoreIsNotAsked() async {
        let core = ScriptedJobConfigurationCore([.success(.repinned(moved: 1))])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: false))
        let attempts: [() async throws -> Void] = [
            { _ = try await service.jobsPinned(toProfile: "p") },
            { _ = try await service.requeue(jobID: "j", newJobID: "n", toProfileVersion: "p") },
            { _ = try await service.requeueRecord(forJob: "j") },
            { try await service.revokeProfile("p") },
            { _ = try await service.repinWaitingWork(routeID: "r") },
        ]
        for attempt in attempts {
            do {
                try await attempt()
                XCTFail("a capture-only session must not use the controls")
            } catch {
                XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testARelockedSessionIsRefusedAgain() async {
        let core = ScriptedJobConfigurationCore([.success(.repinned(moved: 1))])
        let session = makeSession(authenticated: true)
        let service = JobConfigurationService(core: core, session: session)
        session.relock(.explicit)
        do {
            _ = try await service.repinWaitingWork(routeID: "r")
            XCTFail("a relocked session must not re-pin")
        } catch {
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testAReadAnsweredAfterARelockIsDroppedButAConfirmedChangeIsReported() async throws {
        let session = makeSession(authenticated: true)
        let record = JobConfigurationRequeueRecord(
            oldJobID: "job-1", newJobID: "job-2", fromProfileVersion: nil, toProfileVersion: "profile-b",
            requeuedAt: "2026-10-14T10:30:00Z")
        let core = ScriptedJobConfigurationCore([.success(.requeueRecord(record)), .success(.repinned(moved: 2))])
        core.beforeAnswer = { session.relock(.explicit) }
        let service = JobConfigurationService(core: core, session: session)

        do {
            _ = try await service.requeueRecord(forJob: "job-1")
            XCTFail("an answer that arrives after a relock must be dropped")
        } catch {
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }

        let reauthenticated = makeSession(authenticated: true)
        let writer = JobConfigurationService(core: core, session: reauthenticated)
        core.beforeAnswer = { reauthenticated.relock(.explicit) }
        let moved = try await writer.repinWaitingWork(routeID: "r")
        XCTAssertEqual(moved, 2, "a change the core confirmed is reported truthfully")
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let core = ScriptedJobConfigurationCore([])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))
        let longIdentifier = String(repeating: "x", count: 513)
        let attempts: [() async throws -> Void] = [
            { _ = try await service.jobsPinned(toProfile: "") },
            { _ = try await service.jobsPinned(toProfile: "p", limit: 0) },
            { _ = try await service.requeue(jobID: "", newJobID: "n", toProfileVersion: "p") },
            { _ = try await service.requeue(jobID: "j", newJobID: "", toProfileVersion: "p") },
            { _ = try await service.requeue(jobID: "j", newJobID: "n", toProfileVersion: longIdentifier) },
            { _ = try await service.requeueRecord(forJob: "") },
            { try await service.revokeProfile("") },
            { _ = try await service.repinWaitingWork(routeID: "") },
        ]
        for attempt in attempts {
            do {
                try await attempt()
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? JobConfigurationError, .invalidRequest)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testTheCoresFailuresBecomeTypedErrors() async {
        let busy = CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")
        let core = ScriptedJobConfigurationCore([
            .failure(failure("not_found")), .failure(failure("job_configuration_refused")),
            .failure(failure("invalid_request")), .failure(busy), .success(.revoked(profileVersion: "p")),
        ])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))
        var seen: [JobConfigurationError] = []
        for index in 0..<5 {
            do {
                _ = try await service.requeue(jobID: "j-\(index)", newJobID: "n", toProfileVersion: "p")
                XCTFail("expected a refusal")
            } catch {
                seen.append(error as! JobConfigurationError)
            }
        }
        XCTAssertEqual(seen, [.notFound, .refused, .invalidRequest, .core(busy), .unreadable])
    }

    func testTheClientDecodesEachAnswerByCommandAndFailsUnreadableOnes() throws {
        let pinned = try JobConfigurationAnswer.decode(
            Data(#"{"operation_id":1,"jobs":[{"job_id":"j","item_id":"i","status":"queued"}],"total":4,"truncated":true}"#.utf8),
            for: .jobsPinnedToProfile(profileVersion: "p", limit: nil))
        XCTAssertEqual(
            pinned,
            .pinnedJobs(
                JobConfigurationPinnedJobs(
                    jobs: [JobConfigurationPinnedJob(jobID: "j", itemID: "i", status: "queued")], total: 4,
                    truncated: true)))
        XCTAssertEqual(
            try JobConfigurationAnswer.decode(
                Data(#"{"operation_id":2,"record":null}"#.utf8), for: .requeueRecord(jobID: "j")),
            .requeueRecord(nil))
        XCTAssertEqual(
            try JobConfigurationAnswer.decode(
                Data(#"{"operation_id":3,"moved":7}"#.utf8), for: .repinWaitingWork(routeID: "r")),
            .repinned(moved: 7))
        XCTAssertEqual(
            try JobConfigurationAnswer.decode(
                Data(#"{"operation_id":4,"profile_version":"p"}"#.utf8), for: .revokeProfile(profileVersion: "p")),
            .revoked(profileVersion: "p"))
        XCTAssertThrowsError(
            try JobConfigurationAnswer.decode(Data(#"{"operation_id":5}"#.utf8), for: .repinWaitingWork(routeID: "r")))
    }
}
