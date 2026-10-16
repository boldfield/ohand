import Foundation
import XCTest
@testable import OhAndCoreBridge

/// Configures, refreshes and reads the daily prompt schedule through `CoreHandle` against the real Rust core on the
/// simulator. The core owns the schedule and its clock; native supplies only the IANA zone.
final class PromptScheduleBridgeTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-prompt-schedule-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: storeDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { storeDirectory.appendingPathComponent("core.sqlite").path }

    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in self.events.append(event) }
        }

        deinit { core.close() }

        func close() { core.close() }

        func run(_ start: (UInt64) throws -> Void, timeout: TimeInterval = 20) throws -> CoreEvent {
            let operationID = nextOperationID
            nextOperationID += 1
            try start(operationID)
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if let event = events.first(where: { $0.operationID == operationID }) { return event }
                RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
            }
            throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no event for the operation")
        }
    }

    func testAnUnconfiguredStoreReadsAsOff() throws {
        let session = try Session(path: storePath)
        defer { session.close() }

        let outcome = try session.run { try session.core.startReadPromptSchedule(operationID: $0) }
            .decode(PromptScheduleOutcome.self)

        XCTAssertFalse(outcome.enabled)
        XCTAssertNil(outcome.time)
        XCTAssertNil(outcome.next)
    }

    func testConfiguredScheduleSurvivesReopenAndCanBeTurnedOff() throws {
        let first = try Session(path: storePath)
        let configured = try first.run {
            try first.core.startConfigurePromptSchedule(
                operationID: $0, setting: .daily(hour: 7, minute: 45), timeZoneIdentifier: "Europe/Paris")
        }.decode(PromptScheduleOutcome.self)
        XCTAssertTrue(configured.enabled)
        XCTAssertEqual(configured.time, PromptScheduleOutcome.Time(hour: 7, minute: 45))
        let next = try XCTUnwrap(configured.next)
        XCTAssertEqual(next.localDate.count, 10)
        XCTAssertTrue(next.fireAt.hasSuffix("Z"))
        first.close()

        let second = try Session(path: storePath)
        defer { second.close() }
        let reread = try second.run { try second.core.startReadPromptSchedule(operationID: $0) }
            .decode(PromptScheduleOutcome.self)
        XCTAssertEqual(reread.time, configured.time)
        XCTAssertEqual(reread.next, configured.next)

        for trigger in [PromptScheduleRefreshTrigger.launch, .foreground, .timeOrZoneChange] {
            let refreshed = try second.run {
                try second.core.startRefreshPromptSchedule(
                    operationID: $0, trigger: trigger, timeZoneIdentifier: "Europe/Paris")
            }.decode(PromptScheduleOutcome.self)
            XCTAssertTrue(refreshed.enabled, "\(trigger) keeps the schedule on")
            XCTAssertEqual(refreshed.time, configured.time)
        }

        let off = try second.run {
            try second.core.startConfigurePromptSchedule(
                operationID: $0, setting: .off, timeZoneIdentifier: "Europe/Paris")
        }.decode(PromptScheduleOutcome.self)
        XCTAssertFalse(off.enabled)
        XCTAssertNil(off.time)
        XCTAssertNil(off.next)
    }

    func testAnUnknownZoneOrAnImpossibleTimeIsRefusedAndChangesNothing() throws {
        let session = try Session(path: storePath)
        defer { session.close() }

        XCTAssertThrowsError(
            try session.run {
                try session.core.startConfigurePromptSchedule(
                    operationID: $0, setting: .daily(hour: 9, minute: 0), timeZoneIdentifier: "Not/AZone")
            }
        ) { error in XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request") }
        XCTAssertThrowsError(
            try session.run {
                try session.core.startConfigurePromptSchedule(
                    operationID: $0, setting: .daily(hour: 24, minute: 0), timeZoneIdentifier: "UTC")
            }
        ) { error in XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request") }

        let outcome = try session.run { try session.core.startReadPromptSchedule(operationID: $0) }
            .decode(PromptScheduleOutcome.self)
        XCTAssertFalse(outcome.enabled)
    }
}
