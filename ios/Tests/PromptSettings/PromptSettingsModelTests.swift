import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedPromptController: PromptSettingsControlling, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: PromptScheduleOutcome
    private(set) var configured: [PromptScheduleSetting] = []
    var configureError: Error?
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

    func configure(_ setting: PromptScheduleSetting) async throws -> PromptScheduleOutcome {
        lock.lock(); defer { lock.unlock() }
        if let configureError, !(configureError is PromptScheduleConfigurationError) { throw configureError }
        configured.append(setting)
        switch setting {
        case .off: stored = Self.outcome(enabled: false)
        case .daily(let hour, let minute): stored = Self.outcome(enabled: true, hour: hour, minute: minute)
        }
        if let configureError { throw configureError }
        return stored
    }
}

private struct CallFailed: Error {}

@MainActor
final class PromptSettingsModelTests: XCTestCase {
    private var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    private func report(permission: NotificationPermissionState) -> NotificationPermissionReport {
        NotificationPermissionReport(
            permission: permission, transition: .unchanged, trigger: .explicit,
            readiness: NotificationReadinessReport(permission: permission, reminders: []),
            promptsGranted: 0, promptsDeferred: 0, promptsWithheld: 0, reconciliationFailure: nil, converged: false)
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
            stored: ScriptedPromptController.outcome(enabled: true, fireAt: "2026-10-17T09:00:00.000Z"))
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
        controller.configureError = PromptScheduleConfigurationError.installIncomplete(
            ScriptedPromptController.outcome(enabled: true), report(permission: .denied))
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled, "the choice is stored")
        XCTAssertEqual(model.notice, .savedNotificationsOff)
        XCTAssertEqual(controller.configured.count, 1, "no catch-up attempt")
    }

    func testAnIncompleteInstallWithPermissionIsSavedForTheNextOpen() async {
        let controller = ScriptedPromptController()
        controller.configureError = PromptScheduleConfigurationError.installIncomplete(
            ScriptedPromptController.outcome(enabled: true), report(permission: .usable))
        let model = PromptSettingsModel(controller: controller, calendar: calendar)
        await model.load()
        await model.setEnabled(true)
        XCTAssertEqual(model.notice, .savedNotYetApplied)
    }

    func testTheWordsExplainHorizonRefillAndNeverNameStoredContent() {
        let words = [PromptSettingsView.introText, PromptSettingsView.horizonText, PromptSettingsView.privacyText,
                     PromptSettingsView.remindersText] + [
            PromptSettingsNotice.saved, .savedNotificationsOff, .savedNotYetApplied, .notSaved, .unreadable,
        ].map(PromptSettingsView.text(for:))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("3 days"))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("each time you open the app"))
        XCTAssertTrue(PromptSettingsView.horizonText.contains("never sent together"))
        XCTAssertTrue(PromptSettingsView.remindersText.contains("separate"))
        XCTAssertEqual(PromptSettingsModel.horizonDays, 3)
        XCTAssertFalse(words.isEmpty)
    }
}
