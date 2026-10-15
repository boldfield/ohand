import Foundation
import OhAndCoreBridge

/// Why `PromptScheduleService.configure(_:)` could not finish what it promised.
public enum PromptScheduleConfigurationError: Error, Equatable, Sendable {
    /// The choice was persisted, but the reminder pass that installs or cancels prompts did not converge.
    case installIncomplete(PromptScheduleOutcome, NotificationPermissionReport)
}

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
    /// off cancels what is installed at once. The choice is persisted before the pass runs.
    ///
    /// The reminder pass is single-flight: a caller that arrives while one runs joins it and gets its report. A pass
    /// already running may have read the schedule before this choice was persisted, so the first call can only return
    /// that stale pass. It always finishes before it returns, so the second call runs a pass that began after the
    /// choice was persisted. Throws `PromptScheduleConfigurationError.installIncomplete` when that pass did not
    /// converge; the choice stays persisted and the next launch, foreground or time-change pass converges.
    @discardableResult
    public func configure(_ setting: PromptScheduleSetting) async throws -> PromptScheduleOutcome {
        let outcome = try await core.configure(setting, timeZoneIdentifier: timeZone().identifier)
        _ = try await install(.explicit)
        let report = try await install(.explicit)
        if report.reconciliationFailure != nil || !report.converged {
            throw PromptScheduleConfigurationError.installIncomplete(outcome, report)
        }
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
        do {
            _ = try await core.refresh(trigger: refreshTrigger, timeZoneIdentifier: timeZone().identifier)
        } catch {
            return .deferred("prompts_refresh_failed")
        }
        // The coordinator's `.reminders` step already ran a pass against the schedule as it was before the refresh above,
        // so each lifecycle pass reads the permission and pending list twice. This pass is the one that reflects the
        // refreshed schedule; after a zone change it may cancel a date the earlier pass briefly installed.
        do {
            let report = try await install(reconcileTrigger)
            let outcome = LifecycleCoordinator.outcome(ofPrompts: report)
            if case .completed = outcome, !report.converged { return .deferred("prompts_not_converged") }
            return outcome
        } catch is CancellationError {
            return .deferred("prompts_cancelled")
        } catch {
            return .failed("prompts_pass_failed")
        }
    }
}
