import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The prompt schedule operations this screen needs. `PromptScheduleService` is the production implementation.
protocol PromptSettingsControlling: AnyObject, Sendable {
    func read() async throws -> PromptScheduleOutcome
    func configure(_ setting: PromptScheduleSetting) async throws -> PromptScheduleOutcome
}

extension PromptScheduleService: PromptSettingsControlling {}

/// What the screen says after a change or a failed read. Wording never includes stored items: prompts are generic.
enum PromptSettingsNotice: Equatable {
    /// The choice is saved and the reminder pass converged.
    case saved
    /// The choice is saved, but the system will not show prompts until notifications are allowed.
    case savedNotificationsOff
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
///   (permission denied, capacity or an install failure) is shown as saved; nothing is retried in a burst here, because
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
    private let calendar: Calendar
    private let nextPromptFormatter: ISO8601DateFormatter

    init(controller: PromptSettingsControlling, calendar: Calendar = .current) {
        self.controller = controller
        self.calendar = calendar
        nextPromptFormatter = ISO8601DateFormatter()
        nextPromptFormatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        time = Self.date(hour: Self.defaultTime.hour, minute: Self.defaultTime.minute, calendar: calendar)
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

    private func apply(_ setting: PromptScheduleSetting) async {
        isBusy = true
        defer { isBusy = false }
        do {
            adopt(try await controller.configure(setting))
            notice = .saved
        } catch let PromptScheduleConfigurationError.installIncomplete(outcome, report) {
            adopt(outcome)
            switch report.permission {
            case .denied, .notRequested: notice = .savedNotificationsOff
            case .usable: notice = .savedNotYetApplied
            }
        } catch {
            notice = .notSaved
            if let stored = try? await controller.read() { adopt(stored) }
        }
    }

    private func adopt(_ outcome: PromptScheduleOutcome) {
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
