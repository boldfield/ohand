import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// TEMPORARY diagnostics for F27; never merged.
final class F27DiagTests: XCTestCase {
    private var storageRoot: URL!
    private var created: [AppServices] = []

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("f27diag-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        created.forEach { $0.shutDown() }
        created = []
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func makeServices(notificationCenter: NotificationCenter, timeSources: TimeSources) throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        let services = AppServices(
            root: root, lifecycle: lifecycle,
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), voice: nil,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: notificationCenter,
            timeSources: timeSources,
            isProtectedDataAvailable: { true })
        created.append(services)
        return services
    }

    /// The original test on main, real steps, timing the whole pass (per-step times come from the F27DIAG log lines).
    func testRealStepsTiming() async throws {
        let callbacks = NotificationCenter()
        let time = ManualTime(wall: Date(), zone: TimeZone(identifier: "America/New_York")!)
        let services = try makeServices(notificationCenter: callbacks, timeSources: time.sources)
        services.start(isActive: false)
        time.travel(to: TimeZone(identifier: "America/Los_Angeles")!)
        for _ in 0..<3 { callbacks.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        let held = await timeChangeWaitUntil { time.sleeperCount == 3 }
        XCTAssertTrue(held)
        let began = DispatchTime.now().uptimeNanoseconds
        time.releaseTimers()
        let reconciled = await timeChangeWaitUntil { services.lifecycle.finishedPassCount == 1 }
        NSLog("F27DIAG pass_ms=%.1f", Double(DispatchTime.now().uptimeNanoseconds - began) / 1e6)
        XCTAssertTrue(reconciled)
    }

    /// One injected effectful step slower than the 10 s wait: the recorded signature must appear.
    func testASlowStepReproducesTheRecordedSignature() async throws {
        let callbacks = NotificationCenter()
        let time = ManualTime(wall: Date(), zone: TimeZone(identifier: "America/New_York")!)
        let services = try makeServices(notificationCenter: callbacks, timeSources: time.sources)
        services.start(isActive: false)
        services.lifecycle.setAction({ _ in
            try? await Task.sleep(nanoseconds: 15_000_000_000)
            return .completed
        }, for: .reminders)
        time.travel(to: TimeZone(identifier: "America/Los_Angeles")!)
        for _ in 0..<3 { callbacks.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        let held = await timeChangeWaitUntil { time.sleeperCount == 3 }
        XCTAssertTrue(held)
        time.releaseTimers()
        let reconciled = await timeChangeWaitUntil { services.lifecycle.finishedPassCount == 1 }
        NSLog(
            "F27DIAG slow reconciled=%d finishedPassCount=%d lastReportNil=%d",
            reconciled ? 1 : 0, services.lifecycle.finishedPassCount, services.lifecycle.lastReport == nil ? 1 : 0)
        XCTAssertFalse(reconciled, "the wait at the reconciled line fails")
        XCTAssertEqual(services.lifecycle.finishedPassCount, 0)
        XCTAssertNil(services.lifecycle.lastReport)
    }
}
