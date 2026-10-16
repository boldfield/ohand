import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The prompt schedule operations this screen needs. `PromptScheduleService` is the production implementation.
protocol PromptSettingsControlling: AnyObject, Sendable {
    func read() async throws -> PromptScheduleOutcome
    func configureReportingPass(_ setting: PromptScheduleSetting) async throws -> PromptScheduleChange
}

extension PromptScheduleService: PromptSettingsControlling {}

/// What the screen says after a change or a failed read. Wording never includes stored items: prompts are generic.
enum PromptSettingsNotice: Equatable {
    /// The choice is saved and the reminder pass converged.
    case saved
    /// The choice is saved, but the system will not show prompts until notifications are allowed.
    case savedNotificationsOff
    /// The choice is saved, but explicit reminders hold every notification slot the system allows, so prompts wait.
    case savedWaitingForRoom
    /// The choice is saved; installing or cancelling the prompts did not finish and the next app open finishes it.
    case savedNotYetApplied
    /// The choice was not saved and the screen shows what is stored.
    case notSaved
    /// The stored setting could not be read.
    case unreadable
}

/// The state of the optional daily prompt settings.
///
/// Rules this type keeps:
/// * Prompts are off until the user turns them on; an unread or failed read shows them off and changes nothing stored.
/// * Every change is one configure call. Turning prompts off is the same call, so cancellation persists and is applied
///   by the core's reminder pass; this type never touches reminders, so explicit reminders are unaffected.
/// * A change that could not be saved leaves the stored choice on screen. A change that was saved but not yet applied
///   (permission denied, no room or an install failure) is shown as saved with the reason, read from the pass report; nothing is retried in a burst here, because
///   the core refills only the bounded horizon on launch.
/// * Nothing shown comes from stored items.
/// Confined to the main queue; the entry points that change state run on the main actor.
final class PromptSettingsModel: ObservableObject {
    /// The planning horizon the core keeps, in days; it matches `PROMPT_HORIZON_DAYS` in the core's prompt schedule.
    static let horizonDays = 3
    /// The time offered when prompts are turned on.
    static let defaultTime = (hour: 9, minute: 0)

    @Published private(set) var isEnabled = false
    @Published private(set) var time: Date
    @Published private(set) var nextPrompt: Date?
    @Published private(set) var notice: PromptSettingsNotice?
    @Published private(set) var isBusy = false
    @Published private(set) var hasLoaded = false

    private let controller: PromptSettingsControlling
    private let currentCalendar: () -> Calendar
    private var calendar: Calendar { currentCalendar() }
    private let nextPromptFormatter: ISO8601DateFormatter

    /// `currentCalendar` is read every time a local time is converted, so a time zone change while the app is running
    /// is followed; the default tracks the system zone, the same one the time picker renders in.
    init(controller: PromptSettingsControlling, currentCalendar: @escaping () -> Calendar = { .autoupdatingCurrent }) {
        self.controller = controller
        self.currentCalendar = currentCalendar
        nextPromptFormatter = ISO8601DateFormatter()
        nextPromptFormatter.formatOptions = [.withInternetDateTime]
        time = Self.date(hour: Self.defaultTime.hour, minute: Self.defaultTime.minute, calendar: currentCalendar())
    }

    convenience init(controller: PromptSettingsControlling, calendar: Calendar) {
        self.init(controller: controller, currentCalendar: { calendar })
    }

    /// Reads the stored schedule. Reading changes nothing.
    @MainActor func load() async {
        do {
            adopt(try await controller.read())
            notice = nil
        } catch {
            notice = .unreadable
        }
        hasLoaded = true
    }

    @MainActor func setEnabled(_ enabled: Bool) async {
        guard !isBusy, enabled != isEnabled else { return }
        await apply(enabled ? timeSetting(for: time) : .off)
    }

    @MainActor func setTime(_ chosen: Date) async {
        guard !isBusy else { return }
        guard isEnabled else {
            time = chosen
            return
        }
        await apply(timeSetting(for: chosen))
    }

    // MARK: private

    private func timeSetting(for date: Date) -> PromptScheduleSetting {
        let components = calendar.dateComponents([.hour, .minute], from: date)
        return .daily(hour: components.hour ?? Self.defaultTime.hour, minute: components.minute ?? 0)
    }

    @MainActor private func apply(_ setting: PromptScheduleSetting) async {
        isBusy = true
        defer { isBusy = false }
        do {
            let change = try await controller.configureReportingPass(setting)
            adopt(change.outcome)
            notice = Self.notice(after: setting, report: change.report)
        } catch {
            // The service saves before it runs the reminder pass, so a thrown error may mean the pass failed after the
            // change was saved. Only the stored setting tells the two apart.
            if let stored = try? await controller.read() {
                adopt(stored)
                notice = Self.matches(stored, setting) ? .savedNotYetApplied : .notSaved
            } else {
                notice = .notSaved
            }
        }
    }

    private static func matches(_ outcome: PromptScheduleOutcome, _ setting: PromptScheduleSetting) -> Bool {
        switch setting {
        case .off: return !outcome.enabled
        case .daily(let hour, let minute):
            return outcome.enabled && outcome.time?.hour == hour && outcome.time?.minute == minute
        }
    }

    /// Classifies the pass that followed a saved change from its report. Turning prompts off only needs the pass to
    /// have finished; turning them on also needs permission and room.
    private static func notice(after setting: PromptScheduleSetting, report: NotificationPermissionReport) -> PromptSettingsNotice {
        if case .daily = setting, !report.readiness.optionalPromptsAllowed { return .savedNotificationsOff }
        if report.reconciliationFailure != nil || !report.converged { return .savedNotYetApplied }
        guard case .daily = setting else { return .saved }
        if report.promptsWithheld > 0 { return .savedNotYetApplied }
        if report.promptsDeferred > 0 { return .savedWaitingForRoom }
        return .saved
    }

    @MainActor private func adopt(_ outcome: PromptScheduleOutcome) {
        isEnabled = outcome.enabled
        if outcome.enabled, let stored = outcome.time {
            time = Self.date(hour: stored.hour, minute: stored.minute, calendar: calendar)
        }
        nextPrompt = outcome.enabled ? outcome.next.flatMap { nextPromptFormatter.date(from: $0.fireAt) } : nil
    }

    private static func date(hour: Int, minute: Int, calendar: Calendar) -> Date {
        calendar.date(bySettingHour: hour, minute: minute, second: 0, of: Date()) ?? Date()
    }
}
