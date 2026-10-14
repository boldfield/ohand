import Foundation
import OhAndCoreBridge

/// Keeps the optional daily prompt schedule current and its prompts installed.
///
/// The core owns the schedule, its ledger and the decision of which prompts exist; this service supplies the device's
/// current zone and the triggers, and asks the reminder pass (the only thing that installs or cancels a prompt) to
/// act on what the core persisted. Prompts stay off until `configure(_:)` turns them on. Notification permission is
/// applied by that same pass under N04b's rules: a denied or provisional permission withholds prompts and never
/// touches reminders. A prompt carries no content beyond the generic wording the pass installs.
public final class PromptScheduleService: @unchecked Sendable {
    /// Runs the reminder pass that installs or cancels prompts from the persisted schedule.
    public typealias Installer = @Sendable (NotificationReconcileTrigger) async throws -> NotificationPermissionReport

    private let core: CorePromptScheduleClient
    private let timeZone: @Sendable () -> TimeZone
    private let install: Installer

    public init(
        core: CorePromptScheduleClient,
        timeZone: @escaping @Sendable () -> TimeZone,
        install: @escaping Installer
    ) {
        self.core = core
        self.timeZone = timeZone
        self.install = install
    }

    /// The schedule as the core holds it. Reading changes nothing.
    public func read() async throws -> PromptScheduleOutcome {
        try await core.read()
    }

    /// Applies the user's choice with the device's current zone, then runs the pass that acts on it, so turning prompts
    /// off cancels what is installed at once. The choice is persisted before the pass runs; if the pass fails, the next
    /// launch, foreground or time-change pass converges.
    @discardableResult
    public func configure(_ setting: PromptScheduleSetting) async throws -> PromptScheduleOutcome {
        let outcome = try await core.configure(setting, timeZoneIdentifier: timeZone().identifier)
        _ = try? await install(.explicit)
        return outcome
    }

    /// The system clock or time zone changed: the core drops occurrences that are no longer valid, using the zone the
    /// device has now, and adds none.
    @discardableResult
    public func timeChanged() async throws -> PromptScheduleOutcome {
        try await core.refresh(trigger: .timeOrZoneChange, timeZoneIdentifier: timeZone().identifier)
    }

    /// The lifecycle step for `.promptEligibility`: refreshes the schedule for the pass's triggers, then runs the
    /// reminder pass so the installs and cancellations follow the refreshed schedule in the same lifecycle pass.
    public var lifecycleAction: LifecycleAction {
        return { [weak self] triggers in
            guard let self else { return .deferred("prompts_unavailable") }
            return await self.runLifecyclePass(triggers)
        }
    }

    func runLifecyclePass(_ triggers: Set<LifecycleTrigger>) async -> LifecycleStepOutcome {
        let refreshTrigger: PromptScheduleRefreshTrigger
        let reconcileTrigger: NotificationReconcileTrigger
        if triggers.contains(.launch) {
            refreshTrigger = .launch
            reconcileTrigger = .launch
        } else if triggers.contains(.foreground) {
            refreshTrigger = .foreground
            reconcileTrigger = .foreground
        } else {
            refreshTrigger = .timeOrZoneChange
            reconcileTrigger = .explicit
        }
        NSLog("DIAG prompt step begin \(triggers)")
        do {
            _ = try await core.refresh(trigger: refreshTrigger, timeZoneIdentifier: timeZone().identifier)
            NSLog("DIAG prompt refresh done")
        } catch {
            NSLog("DIAG prompt refresh failed \(error)")
            return .deferred("prompts_refresh_failed")
        }
        do {
            let report = try await install(reconcileTrigger)
            NSLog("DIAG prompt install done")
            return LifecycleCoordinator.outcome(ofPrompts: report)
        } catch is CancellationError {
            return .deferred("prompts_cancelled")
        } catch {
            return .failed("prompts_pass_failed")
        }
    }
}
