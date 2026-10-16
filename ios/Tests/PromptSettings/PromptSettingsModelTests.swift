import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedPromptController: PromptSettingsControlling, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: PromptScheduleOutcome
    private(set) var configured: [PromptScheduleSetting] = []
    var configureError: Error?
    var passReport: NotificationPermissionReport?
    var readError: Error?

    init(stored: PromptScheduleOutcome = ScriptedPromptController.outcome(enabled: false)) {
        self.stored = stored
    }

    static func outcome(enabled: Bool, hour: Int = 9, minute: Int = 0, fireAt: String? = nil) -> PromptScheduleOutcome {
        let next = fireAt.map { "{\"local_date\":\"2026-10-17\",\"fire_at\":\"\($0)\"}" } ?? "null"
        let time = enabled ? "{\"hour\":\(hour),\"minute\":\(minute)}" : "null"
        let json = "{\"operation_id\":1,\"enabled\":\(enabled),\"time\":\(time),\"next\":\(next)}"
        return try! JSONDecoder().decode(PromptScheduleOutcome.self, from: Data(json.utf8))
    }

    func read() async throws -> PromptScheduleOutcome {
        lock.lock(); defer { lock.unlock() }
        if let readError { throw readError }
        return stored
    }

    func configureReportingPass(_ setting: PromptScheduleSetting) async throws -> PromptScheduleChange {
        lock.lock(); defer { lock.unlock() }
        if let configureError { throw configureError }
        configured.append(setting)
        switch setting {
        case .off: stored = Self.outcome(enabled: false)
        case .daily(let hour, let minute): stored = Self.outcome(enabled: true, hour: hour, minute: minute)
        }
        let report = passReport ?? ScriptedPromptController.cleanReport
        return PromptScheduleChange(outcome: stored, report: report)
    }

    static let cleanReport = NotificationPermissionReport(
        permission: .usable, transition: .unchanged, trigger: .explicit,
        readiness: NotificationReadinessReport(permission: .usable, reminders: []),
        promptsGranted: 3, promptsDeferred: 0, promptsWithheld: 0, reconciliationFailure: nil, converged: true)
}

private struct CallFailed: Error {}

@MainActor
final class PromptSettingsModelTests: XCTestCase {
    private var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    /// A pass that converged, the way the real service reports a denied permission or a full budget.
    private func report(
        permission: NotificationPermissionState, deferred: Int = 0, withheld: Int = 0, converged: Bool = true
    ) -> NotificationPermissionReport {
        NotificationPermissionReport(
            permission: permission, transition: .unchanged, trigger: .explicit,
            readiness: NotificationReadinessReport(permission: permission, reminders: []),
            promptsGranted: 0, promptsDeferred: deferred, promptsWithheld: withheld, reconciliationFailure: nil,
            converged: converged)
    }

    func testPromptsAreOffByDefault() async {
        let controller = ScriptedPromptController()
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        XCTAssertFalse(model.isEnabled)
        XCTAssertNil(model.nextPrompt)
        XCTAssertNil(model.notice)
        XCTAssertTrue(controller.configured.isEmpty, "opening the screen configures nothing")
    }

    func testAnUnreadableSettingShowsOffAndChangesNothing() async {
        let controller = ScriptedPromptController()
        controller.readError = CallFailed()
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        XCTAssertFalse(model.isEnabled)
        XCTAssertEqual(model.notice, .unreadable)
        XCTAssertTrue(controller.configured.isEmpty)
    }

    func testTurningOnConfiguresTheSelectedTimeAndPersists() async {
        let controller = ScriptedPromptController()
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setTime(calendar.date(bySettingHour: 7, minute: 45, second: 0, of: Date())!)
        XCTAssertTrue(controller.configured.isEmpty, "choosing a time while off does not turn prompts on")
        await model.setEnabled(true)
        XCTAssertEqual(controller.configured, [.daily(hour: 7, minute: 45)])
        XCTAssertTrue(model.isEnabled)
        XCTAssertEqual(model.notice, .saved)

        let relaunched = PromptSettingsModel(controller: controller, calendar: calendar)
        await relaunched.load()
        XCTAssertTrue(relaunched.isEnabled)
        XCTAssertEqual(calendar.dateComponents([.hour, .minute], from: relaunched.time).hour, 7)
    }

    func testChangingTheTimeWhileOnReconfigures() async {
        let controller = ScriptedPromptController(stored: ScriptedPromptController.outcome(enabled: true))
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setTime(calendar.date(bySettingHour: 20, minute: 30, second: 0, of: Date())!)
        XCTAssertEqual(controller.configured, [.daily(hour: 20, minute: 30)])
    }

    func testTurningOffPersistsAndClearsTheNextPrompt() async {
        let controller = ScriptedPromptController(
            stored: ScriptedPromptController.outcome(enabled: true, fireAt: "2026-10-17T09:00:00Z"))
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        XCTAssertNotNil(model.nextPrompt)
        await model.setEnabled(false)
        XCTAssertEqual(controller.configured, [.off])
        XCTAssertFalse(model.isEnabled)
        XCTAssertNil(model.nextPrompt)
        let relaunched = PromptSettingsModel(controller: controller, calendar: calendar)
        await relaunched.load()
        XCTAssertFalse(relaunched.isEnabled)
    }

    func testAFailedSaveKeepsTheStoredChoice() async {
        let controller = ScriptedPromptController()
        controller.configureError = CallFailed()
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertFalse(model.isEnabled)
        XCTAssertEqual(model.notice, .notSaved)
    }

    func testADeniedPermissionIsSavedAndExplainedWithoutRetrying() async {
        let controller = ScriptedPromptController()
        controller.passReport = report(permission: .denied, withheld: 3)
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled, "the choice is stored")
        XCTAssertEqual(model.notice, .savedNotificationsOff)
        XCTAssertEqual(controller.configured.count, 1, "no catch-up attempt")
    }

    func testAFullBudgetIsSavedAndExplainedAsWaiting() async {
        let controller = ScriptedPromptController()
        controller.passReport = report(permission: .usable, deferred: 3)
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled)
        XCTAssertEqual(model.notice, .savedWaitingForRoom)
        XCTAssertEqual(controller.configured.count, 1, "no catch-up attempt")
    }

    func testTurningOffUnderADeniedPermissionIsJustSaved() async {
        let controller = ScriptedPromptController(stored: ScriptedPromptController.outcome(enabled: true))
        controller.passReport = report(permission: .denied)
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(false)
        XCTAssertEqual(model.notice, .saved)
    }

    func testAnUnconvergedPassWithPermissionIsSavedForTheNextOpen() async {
        let controller = ScriptedPromptController()
        controller.passReport = report(permission: .usable, converged: false)
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertEqual(model.notice, .savedNotYetApplied)
    }

    func testTheNextPromptParsesTheCoresWholeSecondTimestamp() async {
        let controller = ScriptedPromptController(
            stored: ScriptedPromptController.outcome(enabled: true, fireAt: "2026-10-17T09:00:00Z"))
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        XCTAssertEqual(model.nextPrompt, Date(timeIntervalSince1970: 1_792_227_600))
    }

    func testTheWordsExplainHorizonAndRefill() {
        let words = [PromptSettingsView.introText, PromptSettingsView.horizonText, PromptSettingsView.privacyText,
                     PromptSettingsView.remindersText] + [
            PromptSettingsNotice.saved, .savedNotificationsOff, .savedWaitingForRoom, .savedNotYetApplied, .notSaved, .unreadable,
        ].map(PromptSettingsView.text(for:))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("3 days"))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("each time you open the app"))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("never sent together"))
        XCTAssertTrue(PromptSettingsView.remindersText.contains("separate"))
        XCTAssertEqual(PromptSettingsModel.horizonDays, 3)
        XCTAssertTrue(words.allSatisfy { !$0.isEmpty })
    }
}
