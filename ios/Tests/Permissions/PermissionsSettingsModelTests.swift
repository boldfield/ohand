import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedOverview: PermissionsReading, @unchecked Sendable {
    private let lock = NSLock()
    private var next: [PermissionEntry]

    init(_ entries: [PermissionEntry]) { next = entries }

    func set(_ entries: [PermissionEntry]) {
        lock.lock(); defer { lock.unlock() }
        next = entries
    }

    func refresh() async -> [PermissionEntry] {
        lock.lock(); defer { lock.unlock() }
        return next
    }
}

private final class ScriptedPromptController: PromptSettingsControlling, @unchecked Sendable {
    private let lock = NSLock()
    private var stored = ScriptedPromptController.outcome(enabled: false)
    private(set) var configured: [PromptScheduleSetting] = []
    var permissionAtPass: NotificationPermissionState = .usable

    static func outcome(enabled: Bool) -> PromptScheduleOutcome {
        let time = enabled ? "{\"hour\":9,\"minute\":0}" : "null"
        let json = "{\"operation_id\":1,\"enabled\":\(enabled),\"time\":\(time),\"next\":null}"
        return try! JSONDecoder().decode(PromptScheduleOutcome.self, from: Data(json.utf8))
    }

    func read() async throws -> PromptScheduleOutcome {
        lock.lock(); defer { lock.unlock() }
        return stored
    }

    func configureReportingPass(_ setting: PromptScheduleSetting) async throws -> PromptScheduleChange {
        lock.lock(); defer { lock.unlock() }
        configured.append(setting)
        switch setting {
        case .off: stored = Self.outcome(enabled: false)
        case .daily: stored = Self.outcome(enabled: true)
        }
        let report = NotificationPermissionReport(
            permission: permissionAtPass, transition: .unchanged, trigger: .explicit,
            readiness: NotificationReadinessReport(permission: permissionAtPass, reminders: []),
            promptsGranted: 0, promptsDeferred: 0, promptsWithheld: 0, reconciliationFailure: nil, converged: true)
        return PromptScheduleChange(outcome: stored, report: report)
    }
}

private final class RecordingAsker: NotificationPermissionAsking, @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    private let controller: ScriptedPromptController?
    private let answer: NotificationPermissionState

    init(answer: NotificationPermissionState, controller: ScriptedPromptController? = nil) {
        self.answer = answer
        self.controller = controller
    }

    var askCount: Int {
        lock.lock(); defer { lock.unlock() }
        return count
    }

    func askIfNeverRequested() async -> NotificationPermissionAskOutcome {
        lock.lock()
        count += 1
        lock.unlock()
        controller?.permissionAtPass = answer
        return .asked(answer)
    }
}

@MainActor
final class PermissionsSettingsModelTests: XCTestCase {
    private func entries(_ status: PermissionStatus, change: PermissionChange = .unchanged) -> [PermissionEntry] {
        PermissionKind.allCases.map { PermissionEntry(kind: $0, status: status, change: change) }
    }

    func testRowsShowGuidanceForEveryPermissionAndRefreshFollowsSettingsReturn() async {
        let overview = ScriptedOverview(entries(.denied))
        let model = PermissionsSettingsModel(overview: overview, openSystemSettings: {})
        XCTAssertFalse(model.hasLoaded)
        await model.refresh()
        XCTAssertEqual(model.rows.map(\.kind), PermissionKind.allCases)
        XCTAssertTrue(model.rows.allSatisfy { $0.guidance.recovery == .openSettings })

        overview.set(entries(.granted, change: .restored))
        await model.refresh()
        XCTAssertTrue(model.rows.allSatisfy { $0.status == .granted && $0.change == .restored })
        XCTAssertTrue(model.rows.allSatisfy { $0.guidance.recovery == PermissionRecovery.none })
    }

    func testRevocationAndRestrictionAreShownPerPermission() async {
        let overview = ScriptedOverview([
            PermissionEntry(kind: .microphone, status: .denied, change: .revoked),
            PermissionEntry(kind: .speech, status: .restricted, change: .unchanged),
            PermissionEntry(kind: .notifications, status: .notRequested, change: .unchanged),
        ])
        let model = PermissionsSettingsModel(overview: overview, openSystemSettings: {})
        await model.refresh()
        XCTAssertEqual(model.rows[0].guidance.headline, "Microphone was turned off")
        XCTAssertEqual(model.rows[1].guidance.recovery, .managedByDevice)
        XCTAssertEqual(model.rows[2].guidance.recovery, PermissionRecovery.none)
    }

    func testOpeningSettingsCallsTheSystemHook() {
        var opened = 0
        let model = PermissionsSettingsModel(overview: ScriptedOverview([]), openSystemSettings: { opened += 1 })
        model.openSettings()
        XCTAssertEqual(opened, 1)
    }

    // MARK: daily prompt boundary

    func testTurningThePromptOnAsksOnceThenSavesAndShowsTheRealOutcome() async {
        let controller = ScriptedPromptController()
        let asker = RecordingAsker(answer: .denied, controller: controller)
        let model = PromptSettingsModel(controller: controller, notificationPermission: asker, calendar: .current)
        await model.load()
        XCTAssertEqual(asker.askCount, 0, "opening the screen asks for nothing")

        await model.setEnabled(true)
        XCTAssertEqual(asker.askCount, 1)
        XCTAssertEqual(controller.configured.count, 1, "the choice is saved even though notifications were denied")
        XCTAssertTrue(model.isEnabled)
        XCTAssertEqual(model.notice, .savedNotificationsOff)
    }

    func testTurningThePromptOffOrChangingItsTimeNeverAsks() async {
        let controller = ScriptedPromptController()
        let asker = RecordingAsker(answer: .usable, controller: controller)
        let model = PromptSettingsModel(controller: controller, notificationPermission: asker, calendar: .current)
        await model.load()
        await model.setEnabled(true)
        XCTAssertEqual(asker.askCount, 1)
        await model.setTime(Date())
        await model.setEnabled(false)
        XCTAssertEqual(asker.askCount, 1)
        XCTAssertFalse(model.isEnabled)
    }
}
