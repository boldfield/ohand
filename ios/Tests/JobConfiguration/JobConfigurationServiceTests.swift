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

    func testJobsRetiredByRevocationSendsTheCommandAndValidatesTheRequest() async throws {
        let page = JobConfigurationRetiredJobs(
            jobs: [JobConfigurationRetiredJob(jobID: "job-1", itemID: "item-1", retiredAt: "t", requeuedTo: "job-2")],
            total: 1, truncated: false, nextCursor: nil)
        let core = ScriptedJobConfigurationCore([.success(.retiredJobs(page))])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))

        let result = try await service.jobsRetired(byRevocationOf: "profile-a", limit: 5, after: "job-0")

        XCTAssertEqual(result, page)
        XCTAssertEqual(
            core.commands, [.jobsRetiredByRevocation(profileVersion: "profile-a", limit: 5, after: "job-0")])
        for invalid in [("", nil, nil), ("p", 0, nil), ("p", nil, "")] as [(String, Int?, String?)] {
            do {
                _ = try await service.jobsRetired(byRevocationOf: invalid.0, limit: invalid.1, after: invalid.2)
                XCTFail("expected invalidRequest")
            } catch {
                XCTAssertEqual(error as? JobConfigurationError, .invalidRequest)
            }
        }
        XCTAssertEqual(core.commands.count, 1)
    }

    func testAnUnauthenticatedRetiredJobsReadNeverReachesTheCore() async {
        let core = ScriptedJobConfigurationCore([])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: false))
        do {
            _ = try await service.jobsRetired(byRevocationOf: "profile-a")
            XCTFail("expected notAuthenticated")
        } catch {
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
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

/// A read source that records which job-configuration reads the gate sent to the core.
private final class RecordingJobConfigurationReadSource: PrivateReadSource {
    private(set) var reads: [(operationID: UInt64, command: JobConfigurationCommand)] = []

    func startStoreCheck(operationID: UInt64) throws {}
    func startGetCapture(operationID: UInt64, captureID: String) throws {}
    func startItemStatus(operationID: UInt64, itemID: String) throws {}
    func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {}
    func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {}

    func startJobConfigurationRead(operationID: UInt64, command: JobConfigurationCommand) throws {
        reads.append((operationID, command))
    }
}

private final class ClientResults: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [Result<JobConfigurationAnswer, CoreFailure>] = []

    func append(_ value: Result<JobConfigurationAnswer, CoreFailure>) {
        lock.lock()
        stored.append(value)
        lock.unlock()
    }

    var values: [Result<JobConfigurationAnswer, CoreFailure>] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }
}

/// The stored-state reads reach the core only through the private read gate.
final class CoreJobConfigurationClientGateTests: XCTestCase {
    private var directory: URL!
    private var core: CoreHandle!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        core = try CoreHandle(path: directory.appendingPathComponent("store.sqlite").path)
    }

    override func tearDown() {
        core?.close()
        try? FileManager.default.removeItem(at: directory)
    }

    private func makeClient(
        authenticated: Bool, source: RecordingJobConfigurationReadSource
    ) -> (CoreJobConfigurationClient, ReadAuthenticationSession) {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        let gate = PrivateReadGate(session: session, source: source)
        return (CoreJobConfigurationClient(core: core, gate: gate), session)
    }

    func testACaptureOnlySessionNeverReachesTheCoreForARead() {
        let source = RecordingJobConfigurationReadSource()
        let (client, _) = makeClient(authenticated: false, source: source)
        let results = ClientResults()

        client.submit(.jobsPinnedToProfile(profileVersion: "p", limit: nil)) { results.append($0) }
        client.submit(.requeueRecord(jobID: "j")) { results.append($0) }
        client.submit(.jobsRetiredByRevocation(profileVersion: "p", limit: nil, after: nil)) { results.append($0) }

        XCTAssertTrue(source.reads.isEmpty)
        XCTAssertEqual(results.values.count, 3)
        for result in results.values {
            guard case .failure(let refusal) = result else { return XCTFail("expected a refusal") }
            XCTAssertEqual(refusal.code, "read_not_authenticated")
        }
    }

    func testAReadAnswerAfterARelockIsDroppedUndecoded() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, session) = makeClient(authenticated: true, source: source)
        let results = ClientResults()
        client.submit(.requeueRecord(jobID: "j")) { results.append($0) }
        client.submit(.jobsPinnedToProfile(profileVersion: "p", limit: 5)) { results.append($0) }
        XCTAssertEqual(
            source.reads.map(\.command),
            [.requeueRecord(jobID: "j"), .jobsPinnedToProfile(profileVersion: "p", limit: 5)])

        session.relock(.explicit)
        let payload = Data(#"{"operation_id":1,"record":null}"#.utf8)
        for read in source.reads {
            XCTAssertTrue(client.handle(CoreEvent(operationID: read.operationID, outcome: .success(payload))))
        }

        XCTAssertEqual(results.values.count, 2)
        for result in results.values {
            guard case .failure(let refusal) = result else { return XCTFail("a relocked answer must be dropped") }
            XCTAssertEqual(refusal.code, "read_relocked")
        }
    }

    func testAReadAnswerWhileAuthenticatedIsDelivered() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, _) = makeClient(authenticated: true, source: source)
        let results = ClientResults()
        client.submit(.requeueRecord(jobID: "j")) { results.append($0) }
        let operationID = try XCTUnwrap(source.reads.first?.operationID)

        XCTAssertTrue(
            client.handle(
                CoreEvent(operationID: operationID, outcome: .success(Data(#"{"operation_id":1,"record":null}"#.utf8)))))

        XCTAssertEqual(results.values.count, 1)
        guard case .success(let answer) = results.values[0] else { return XCTFail("expected the answer") }
        XCTAssertEqual(answer, .requeueRecord(nil))
    }

    func testARetiredJobsReadGoesThroughTheGateAndDecodes() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, _) = makeClient(authenticated: true, source: source)
        let results = ClientResults()
        let command = JobConfigurationCommand.jobsRetiredByRevocation(profileVersion: "p", limit: 2, after: "job-1")
        client.submit(command) { results.append($0) }
        XCTAssertEqual(source.reads.map(\.command), [command])
        let operationID = try XCTUnwrap(source.reads.first?.operationID)
        let payload = Data(
            #"{"operation_id":1,"jobs":[{"job_id":"job-2","item_id":"item-1","retired_at":"2026-10-20T10:00:00Z","requeued_to":null}],"total":2,"truncated":false,"next_cursor":null}"#
                .utf8)

        XCTAssertTrue(client.handle(CoreEvent(operationID: operationID, outcome: .success(payload))))

        guard case .success(let answer) = try XCTUnwrap(results.values.first) else { return XCTFail("expected the answer") }
        XCTAssertEqual(
            answer,
            .retiredJobs(
                JobConfigurationRetiredJobs(
                    jobs: [
                        JobConfigurationRetiredJob(
                            jobID: "job-2", itemID: "item-1", retiredAt: "2026-10-20T10:00:00Z", requeuedTo: nil)
                    ], total: 2, truncated: false, nextCursor: nil)))
    }

    func testARetiredJobsAnswerAfterARelockIsDropped() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, session) = makeClient(authenticated: true, source: source)
        let results = ClientResults()
        client.submit(.jobsRetiredByRevocation(profileVersion: "p", limit: nil, after: nil)) { results.append($0) }
        let operationID = try XCTUnwrap(source.reads.first?.operationID)

        session.relock(.explicit)
        let payload = Data(#"{"operation_id":1,"jobs":[],"total":0,"truncated":false,"next_cursor":null}"#.utf8)
        XCTAssertTrue(client.handle(CoreEvent(operationID: operationID, outcome: .success(payload))))

        guard case .failure(let refusal) = try XCTUnwrap(results.values.first) else { return XCTFail("must be dropped") }
        XCTAssertEqual(refusal.code, "read_relocked")
    }
}

/// Failed interpretation work: the read and the explicit rerun (F43).
final class FailedInterpretationControlsTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private let failedEntry = JobConfigurationFailedInterpretation(
        itemID: "item-1", capturedAt: "2026-10-22T09:00:00Z", failureReason: "rejected", pinned: true,
        profileVersion: "profile-a", recovery: .runAgain)

    func testTheServiceReadsFailedWorkAndRerunsItWithTypedAnswers() async throws {
        let page = JobConfigurationFailedInterpretations(failed: [failedEntry], nextCursor: "cursor-1")
        let result = JobConfigurationRerunResult(
            moved: 1, skipped: 1, skippedItems: [JobConfigurationSkippedItem(itemID: "item-2", reason: .notFailed)])
        let core = ScriptedJobConfigurationCore([
            .success(.failedInterpretations(page)), .success(.rerun(result)), .success(.rerun(result)),
        ])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))

        let listed = try await service.failedInterpretations(limit: 10, cursor: "cursor-0")
        let someRerun = try await service.rerunFailedInterpretations(itemIDs: ["item-1", "item-2"])
        let allRerun = try await service.rerunAllFailedInterpretations()

        XCTAssertEqual(listed, page)
        XCTAssertEqual(someRerun, result)
        XCTAssertEqual(allRerun, result)
        XCTAssertEqual(
            core.commands,
            [
                .failedInterpretations(readScopes: [], limit: 10, cursor: "cursor-0"),
                .rerunFailedInterpretations(readScopes: [], selection: .items(["item-1", "item-2"])),
                .rerunFailedInterpretations(readScopes: [], selection: .allListed),
            ])
    }

    func testMalformedFailedWorkRequestsNeverReachTheCore() async {
        let core = ScriptedJobConfigurationCore([])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: true))
        let tooMany = (0...100).map { "item-\($0)" }
        let attempts: [() async throws -> Void] = [
            { _ = try await service.failedInterpretations(limit: 0) },
            { _ = try await service.failedInterpretations(cursor: "") },
            { _ = try await service.rerunFailedInterpretations(itemIDs: []) },
            { _ = try await service.rerunFailedInterpretations(itemIDs: [""]) },
            { _ = try await service.rerunFailedInterpretations(itemIDs: tooMany) },
        ]
        for attempt in attempts {
            do {
                try await attempt()
                XCTFail("expected invalidRequest")
            } catch {
                XCTAssertEqual(error as? JobConfigurationError, .invalidRequest)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testACaptureOnlySessionIsRefusedAndTheCoreIsNotAsked() async {
        let core = ScriptedJobConfigurationCore([])
        let service = JobConfigurationService(core: core, session: makeSession(authenticated: false))
        do {
            _ = try await service.failedInterpretations()
            XCTFail("expected notAuthenticated")
        } catch {
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        do {
            _ = try await service.rerunAllFailedInterpretations()
            XCTFail("expected notAuthenticated")
        } catch {
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testTheAnswersDecodeFromTheCoresJSON() throws {
        let listed = try JobConfigurationAnswer.decode(
            Data(
                #"{"operation_id":1,"failed":[{"item_id":"item-1","captured_at":"2026-10-22T09:00:00Z","failure_reason":"rejected","pinned":true,"profile_version":"profile-a","recovery":"run_again"},{"item_id":"item-2","captured_at":"2026-10-21T09:00:00Z","failure_reason":"unsupported","pinned":false,"profile_version":null,"recovery":"change_settings"}],"next_cursor":null}"#
                    .utf8),
            for: .failedInterpretations(readScopes: [.personal], limit: nil, cursor: nil))
        XCTAssertEqual(
            listed,
            .failedInterpretations(
                JobConfigurationFailedInterpretations(
                    failed: [
                        failedEntry,
                        JobConfigurationFailedInterpretation(
                            itemID: "item-2", capturedAt: "2026-10-21T09:00:00Z", failureReason: "unsupported",
                            pinned: false, profileVersion: nil, recovery: .changeSettings),
                    ], nextCursor: nil)))
        let rerun = try JobConfigurationAnswer.decode(
            Data(
                #"{"operation_id":2,"moved":1,"skipped":1,"skipped_items":[{"item_id":"item-3","reason":"no_provider_allowed"}]}"#
                    .utf8),
            for: .rerunFailedInterpretations(readScopes: [.personal], selection: .allListed))
        XCTAssertEqual(
            rerun,
            .rerun(
                JobConfigurationRerunResult(
                    moved: 1, skipped: 1,
                    skippedItems: [JobConfigurationSkippedItem(itemID: "item-3", reason: .noProviderAllowed)])))
        XCTAssertThrowsError(
            try JobConfigurationAnswer.decode(
                Data(#"{"operation_id":3,"failed":[{"item_id":"i","captured_at":"t","failure_reason":"x","pinned":false,"profile_version":null,"recovery":"surprise"}],"next_cursor":null}"#.utf8),
                for: .failedInterpretations(readScopes: [.personal], limit: nil, cursor: nil)))
    }

    func testTheCommandsAreEncodedForTheCoreAndScopesAreReplaceable() {
        XCTAssertEqual(
            JobConfigurationCommand.rerunFailedInterpretations(readScopes: [], selection: .allListed)
                .withReadScopes([.personal, .work]),
            .rerunFailedInterpretations(readScopes: [.personal, .work], selection: .allListed))
        XCTAssertEqual(
            JobConfigurationCommand.repinWaitingWork(routeID: "r").withReadScopes([.work]),
            .repinWaitingWork(routeID: "r"))
    }
}

/// The failed-work read reaches the core only through the private read gate, which supplies the session's scopes.
final class FailedInterpretationGateTests: XCTestCase {
    private var directory: URL!
    private var core: CoreHandle!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        core = try CoreHandle(path: directory.appendingPathComponent("store.sqlite").path)
    }

    override func tearDown() {
        core?.close()
        try? FileManager.default.removeItem(at: directory)
    }

    private func makeClient(
        authenticated: Bool, source: RecordingJobConfigurationReadSource
    ) -> (CoreJobConfigurationClient, ReadAuthenticationSession) {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        let gate = PrivateReadGate(session: session, source: source)
        return (CoreJobConfigurationClient(core: core, gate: gate), session)
    }

    func testALockedSessionNeverReachesTheCoreForTheFailedWorkRead() {
        let source = RecordingJobConfigurationReadSource()
        let (client, _) = makeClient(authenticated: false, source: source)
        let results = ClientResults()

        client.submit(.failedInterpretations(readScopes: [.personal, .work], limit: nil, cursor: nil)) {
            results.append($0)
        }

        XCTAssertTrue(source.reads.isEmpty)
        guard case .failure(let refusal) = results.values.first else { return XCTFail("expected a refusal") }
        XCTAssertEqual(refusal.code, "read_not_authenticated")
    }

    func testTheGateStatesTheScopesAndAnAnswerAfterARelockIsDropped() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, session) = makeClient(authenticated: true, source: source)
        let results = ClientResults()

        client.submit(.failedInterpretations(readScopes: [], limit: 5, cursor: "c")) { results.append($0) }

        XCTAssertEqual(
            source.reads.map(\.command),
            [.failedInterpretations(readScopes: [.personal, .work], limit: 5, cursor: "c")])
        let operationID = try XCTUnwrap(source.reads.first?.operationID)
        session.relock(.explicit)
        let payload = Data(#"{"operation_id":1,"failed":[],"next_cursor":null}"#.utf8)
        XCTAssertTrue(client.handle(CoreEvent(operationID: operationID, outcome: .success(payload))))

        guard case .failure(let refusal) = try XCTUnwrap(results.values.first) else { return XCTFail("must be dropped") }
        XCTAssertEqual(refusal.code, "read_relocked")
    }

    func testAnAuthenticatedFailedWorkAnswerIsDelivered() throws {
        let source = RecordingJobConfigurationReadSource()
        let (client, _) = makeClient(authenticated: true, source: source)
        let results = ClientResults()
        client.submit(.failedInterpretations(readScopes: [], limit: nil, cursor: nil)) { results.append($0) }
        let operationID = try XCTUnwrap(source.reads.first?.operationID)

        let payload = Data(#"{"operation_id":1,"failed":[],"next_cursor":null}"#.utf8)
        XCTAssertTrue(client.handle(CoreEvent(operationID: operationID, outcome: .success(payload))))

        guard case .success(let answer) = try XCTUnwrap(results.values.first) else { return XCTFail("expected the answer") }
        XCTAssertEqual(answer, .failedInterpretations(JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)))
    }
}
