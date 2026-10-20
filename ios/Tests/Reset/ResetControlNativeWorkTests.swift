import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// `ResetControl.hasUnfinishedNativeWork()`, `ResetControl.finishPending()` and `ResetService.hasUnfinishedNativeWork()`
/// over the production `ResetService`, the reset fixture's real marker files and the real Rust core. Everything is
/// synthetic. The tests are synchronous because the fixture pumps the main run loop for the core's answers.
final class ResetControlNativeWorkTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreResetWorld!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreResetWorld()
    }

    override func tearDown() {
        world?.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func control(_ runtime: RealCoreResetRuntime, authenticated: Bool) throws -> ResetControl {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated { session.authenticate(reason: "test") { _ in } }
        XCTAssertEqual(session.scope, authenticated ? .authenticated : .captureOnly)
        return ResetControl(service: try XCTUnwrap(runtime.service), core: runtime.session.resetClient, session: session)
    }

    private func requestMarkerURL(_ number: Int) -> URL {
        world.layout.directory(for: .temporaryFiles)
            .appendingPathComponent(FileResetMarkerStore.requestFilePrefix + String(number))
    }

    private func writeMarker(_ contents: String, to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(contents.utf8).write(to: url)
    }

    private func nativeWorkOwed(_ runtime: RealCoreResetRuntime) throws -> Bool {
        let service = try XCTUnwrap(runtime.service)
        return try runtime.session.run { await service.hasUnfinishedNativeWork() }
    }

    // MARK: Authentication

    func testBothControlMembersThrowWithoutAnAuthenticatedSessionAndChangeNothing() throws {
        let runtime = try world.launch()
        try runtime.recordTap(.keep)
        let control = try control(runtime, authenticated: false)

        XCTAssertThrowsError(try runtime.session.run { try await control.hasUnfinishedNativeWork() }) { error in
            XCTAssertEqual(error as? ResetControlError, .notAuthenticated)
        }
        XCTAssertThrowsError(try runtime.session.run { try await control.finishPending() }) { error in
            XCTAssertEqual(error as? ResetControlError, .notAuthenticated)
        }

        XCTAssertNotNil(try runtime.markers.request(), "the recorded tap is left for an authenticated retry or relaunch")
        XCTAssertEqual(try runtime.currentGeneration(), 0, "nothing was asked of the core")
        runtime.terminate()
    }

    // MARK: Owed native work

    func testAPristineStoreOwesNothing() throws {
        let runtime = try world.launch()

        XCTAssertFalse(try nativeWorkOwed(runtime))
        let control = try control(runtime, authenticated: true)
        XCTAssertFalse(try runtime.session.run { try await control.hasUnfinishedNativeWork() })
        runtime.terminate()
    }

    func testARecordedTapAndAnOutstandingCleanupMarkerAreOwed() throws {
        let runtime = try world.launch()
        try runtime.recordTap(.keep)
        XCTAssertTrue(try nativeWorkOwed(runtime), "a recorded tap the core never started is owed")
        try runtime.markers.clearRequest()
        XCTAssertFalse(try nativeWorkOwed(runtime))

        try runtime.markers.recordCleanupOutstanding()
        XCTAssertTrue(try nativeWorkOwed(runtime), "outstanding native cleanup is owed")
        runtime.terminate()
    }

    func testAnUnreadableRequestMarkerCountsAsOwed() throws {
        let runtime = try world.launch()
        try writeMarker("not a reset request", to: requestMarkerURL(1))
        XCTAssertThrowsError(try runtime.markers.request(), "the marker really is unreadable")

        XCTAssertTrue(try nativeWorkOwed(runtime))
        let control = try control(runtime, authenticated: true)
        XCTAssertTrue(try runtime.session.run { try await control.hasUnfinishedNativeWork() })
        runtime.terminate()
    }

    func testARemoveRequestMarkerWithoutItsBoundListsCountsAsOwed() throws {
        let runtime = try world.launch()
        try writeMarker(#"{"credentialChoice":"remove","stagedBeforeTheTap":[]}"#, to: requestMarkerURL(1))
        XCTAssertThrowsError(try runtime.markers.request(), "the marker really is unreadable")

        XCTAssertTrue(try nativeWorkOwed(runtime))
        runtime.terminate()
    }

    func testCheckingOwedWorkStartsAndChangesNothing() throws {
        let runtime = try world.launch()
        try runtime.recordTap(.keep)
        let before = try runtime.markers.request()

        XCTAssertTrue(try nativeWorkOwed(runtime))
        XCTAssertTrue(try nativeWorkOwed(runtime))

        XCTAssertEqual(try runtime.markers.request(), before, "the recorded tap is unchanged")
        XCTAssertEqual(try runtime.currentGeneration(), 0, "no reset was started")
        runtime.terminate()
    }

    // MARK: Finishing a pending reset

    func testFinishPendingWithNothingRecordedStartsNoReset() throws {
        let runtime = try world.launch()
        let control = try control(runtime, authenticated: true)

        let outcome = try runtime.session.run { try await control.finishPending() }

        XCTAssertEqual(outcome.state, .notStarted)
        XCTAssertEqual(try runtime.currentGeneration(), 0, "no reset was started")
        XCTAssertFalse(try nativeWorkOwed(runtime))
        runtime.terminate()
    }

    func testFinishPendingFinishesTheRecordedResetOnceAndStartsNoFurtherOne() throws {
        // A process killed right after the tap was recorded; the relaunched screen finishes it.
        let killed = try world.launch()
        try killed.recordTap(.keep)
        killed.terminate()
        let runtime = try world.launch()
        XCTAssertTrue(try nativeWorkOwed(runtime))
        let control = try control(runtime, authenticated: true)

        let finished = try runtime.session.run { try await control.finishPending() }
        XCTAssertEqual(finished.state, .completed, "blockers: \(finished.blockers)")
        XCTAssertEqual(try runtime.currentGeneration(), 1, "only the recorded reset ran")
        XCTAssertFalse(try nativeWorkOwed(runtime))

        _ = try runtime.session.run { try await control.finishPending() }
        XCTAssertEqual(try runtime.currentGeneration(), 1, "a second call starts no further reset")
        runtime.terminate()
    }
}
