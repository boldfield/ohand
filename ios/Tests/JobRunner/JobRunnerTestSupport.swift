import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// One job row as the job queue stores it, read straight from the store file.
struct StoredJob: Equatable {
    let status: String
    let attemptCount: Int
    let failureReason: String?
}

enum JobStoreInspector {
    static let itemID = "5a1c0000-0000-4000-8000-0000000000e1"

    static func job(at path: String, id: String) -> StoredJob? {
        var database: OpaquePointer?
        guard sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { return nil }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 2000)
        var statement: OpaquePointer?
        let sql = "SELECT status, attempt_count, failure_reason FROM jobs WHERE job_id = ?"
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return nil }
        defer { sqlite3_finalize(statement) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        sqlite3_bind_text(statement, 1, id, -1, transient)
        guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
        let reason = sqlite3_column_text(statement, 2).map { String(cString: $0) }
        return StoredJob(
            status: sqlite3_column_text(statement, 0).map { String(cString: $0) } ?? "",
            attemptCount: Int(sqlite3_column_int(statement, 1)),
            failureReason: reason)
    }

    /// Adds a queued job of a type that runs on the device, the way a foreground import would.
    static func insertQueuedJob(at path: String, id: String, jobType: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        let statement = """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES ('\(id)', 1, '\(itemID)', '\(jobType)', 0, \
            NULL, NULL, 'queued', 0, '2026-10-08T09:30:03Z')
            """
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, statement, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
    }
}

/// A reachability source the test flips by hand.
final class FakeReachability: NetworkReachability, @unchecked Sendable {
    private let lock = NSLock()
    private var reachable: Bool
    private var onChange: (@Sendable (Bool) -> Void)?
    private(set) var isMonitoring = false

    init(reachable: Bool) {
        self.reachable = reachable
    }

    var isReachable: Bool {
        lock.lock()
        defer { lock.unlock() }
        return reachable
    }

    func startMonitoring(_ onChange: @escaping @Sendable (Bool) -> Void) {
        lock.lock()
        self.onChange = onChange
        isMonitoring = true
        let current = reachable
        lock.unlock()
        onChange(current)
    }

    func stopMonitoring() {
        lock.lock()
        onChange = nil
        isMonitoring = false
        lock.unlock()
    }

    func set(_ value: Bool) {
        lock.lock()
        reachable = value
        let callback = onChange
        lock.unlock()
        callback?(value)
    }
}

/// Holds every request until opened, then forwards it. A request whose task is cancelled ends as `cancelled`,
/// which is what the real transport does.
final class GatedSender: ProviderRequestSender, @unchecked Sendable {
    private let forward: ProviderRequestSender?
    private let lock = NSLock()
    private var isOpen: Bool
    private var count = 0

    init(forwarding forward: ProviderRequestSender?, open: Bool = false) {
        self.forward = forward
        self.isOpen = open
    }

    var requestCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func open() {
        lock.lock()
        isOpen = true
        lock.unlock()
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        recordRequest()
        while true {
            if Task.isCancelled { throw ProviderTransportError.cancelled }
            if currentlyOpen { break }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        guard let forward else { throw ProviderTransportError.connectionFailed }
        return try await forward.send(request)
    }

    private func recordRequest() {
        lock.lock()
        count += 1
        lock.unlock()
    }

    private var currentlyOpen: Bool {
        lock.lock()
        defer { lock.unlock() }
        return isOpen
    }
}

/// A native job capability that records its calls and answers with a scripted result.
final class ScriptedCapability: JobCapabilityHandler, @unchecked Sendable {
    let jobType: String
    private let result: JobCapabilityResult
    private let lock = NSLock()
    private var runs: [JobCapabilityRun] = []

    init(jobType: String, result: JobCapabilityResult) {
        self.jobType = jobType
        self.result = result
    }

    var receivedRuns: [JobCapabilityRun] {
        lock.lock()
        defer { lock.unlock() }
        return runs
    }

    func run(_ job: JobCapabilityRun) async -> JobCapabilityResult {
        lock.lock()
        runs.append(job)
        lock.unlock()
        return result
    }
}

/// A real core over a seeded store with the job runner attached. Events arrive on the main queue, where XCTest
/// runs synchronous tests, so waiting pumps the run loop.
final class JobRunnerSession {
    let core: CoreHandle
    let service: JobRunnerService
    let storePath: String
    private(set) var otherEvents: [CoreEvent] = []
    private var isClosed = false

    init(
        storePath: String,
        sender: ProviderRequestSender,
        capabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = true,
        retryDelay: TimeInterval = 3600
    ) throws {
        self.storePath = storePath
        core = try CoreHandle(path: storePath)
        service = try JobRunnerService.attach(
            to: core, storePath: storePath, sender: sender, capabilities: capabilities,
            isReachable: isReachable, retryDelay: retryDelay)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            if !self.service.handle(event) {
                self.otherEvents.append(event)
            }
        }
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 15) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Pumps for a fixed time, for asserting that something does not happen.
    func pumpFor(_ seconds: TimeInterval) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    func waitForFinishedDrains(_ count: Int, timeout: TimeInterval = 15) -> Bool {
        pump(until: { self.service.finishedDrainCount >= count && !self.service.isDraining }, timeout: timeout)
    }

    func summary() -> JobDrainSummary? {
        guard case .finished(let summary)? = service.lastOutcome else { return nil }
        return summary
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        service.shutDown()
        core.close()
    }
}
