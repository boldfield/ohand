import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The runner's single drain-idle slot is shared: the owner keeps its own observer in the runner and the completion
/// service's observer rides on the adapter, so both run after a real drain and neither replaces the other.
final class JobRunnerCompletionDrainTests: XCTestCase {
    private final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        var count: Int {
            lock.lock()
            defer { lock.unlock() }
            return value
        }
        func increment() {
            lock.lock()
            value += 1
            lock.unlock()
        }
    }

    private final class UnusedSender: ProviderRequestSender, @unchecked Sendable {
        func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
            throw URLError(.notConnectedToInternet)
        }
    }

    private var storeDirectory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-completion-drain-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: storeDirectory)
        super.tearDown()
    }

    func testTheOwnersObserverAndTheCompletionObserverBothRunAfterADrain() throws {
        let storePath = storeDirectory.appendingPathComponent("store.sqlite").path
        let session = try BackgroundCompletionSession(storePath: storePath, sender: UnusedSender())
        defer { session.close() }
        let drain = JobRunnerCompletionDrain(runner: session.service)
        let ownerRan = Counter()
        let completionRan = Counter()
        session.service.setDrainIdleObserver {
            ownerRan.increment()
            drain.drainWentIdle()
        }
        drain.setDrainIdleObserver { completionRan.increment() }

        session.service.activate()
        let deadline = Date().addingTimeInterval(15)
        while (ownerRan.count == 0 || completionRan.count == 0) && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }

        XCTAssertGreaterThan(ownerRan.count, 0, "the owner's observer ran")
        XCTAssertGreaterThan(completionRan.count, 0, "the completion observer ran too")

        let before = completionRan.count
        drain.setDrainIdleObserver(nil)
        drain.drainWentIdle()
        XCTAssertEqual(completionRan.count, before, "a removed completion observer is not called")
    }
}
