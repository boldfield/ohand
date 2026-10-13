import Foundation
import OhAndCoreBridge
import UIKit

// MARK: - Vocabulary

/// What asked for a reconciliation pass.
public enum LifecycleTrigger: Hashable, Sendable {
    case launch
    case foreground
    /// The system clock or time zone changed. Raised by the separately owned time-change handling through
    /// `LifecycleCoordinator.timeChangeTrigger`; only the steps that depend on local time run.
    case systemTimeChange

    var steps: [LifecycleStep] {
        switch self {
        case .launch, .foreground:
            return LifecycleStep.allCases
        case .systemTimeChange:
            return [.reminders, .promptEligibility]
        }
    }
}

/// One unit of recovery or foreground work. The declaration order is the order a pass runs them in: captures first,
/// so everything after them sees every durable capture; then queued work, deletion that must finish, the reminder
/// desired state, space-freeing retention, optional prompting, and finally the read-only health snapshot.
public enum LifecycleStep: String, CaseIterable, Sendable {
    case ingress
    case jobs
    case deletion
    case reminders
    case retention
    case promptEligibility
    case health
}

/// How one step ended. Reasons are fixed labels and never carry identifiers, paths or content.
public enum LifecycleStepOutcome: Equatable, Sendable {
    /// The step finished and what it reconciled has converged.
    case completed
    /// The step handed its work to a runner that finishes it on its own time (a job drain, a health read).
    case requested
    /// Nothing is connected for this step in this build.
    case notConfigured
    /// The step could not finish; the next pass repeats it.
    case deferred(String)
    case failed(String)
    /// The step did not answer within its bound. The pass went on without it.
    case timedOut
    /// The coordinator was shut down (the process is going away) before or during the step.
    case interrupted
}

public struct LifecycleStepResult: Equatable, Sendable {
    public let step: LifecycleStep
    public let outcome: LifecycleStepOutcome
}

public struct LifecyclePassReport: Equatable, Sendable {
    /// Every trigger this pass answers: requests that arrived during the previous pass share one pass.
    public let triggers: Set<LifecycleTrigger>
    public let results: [LifecycleStepResult]

    public func outcome(of step: LifecycleStep) -> LifecycleStepOutcome? {
        results.first(where: { $0.step == step })?.outcome
    }
}

/// Work one step performs. It receives the triggers its pass answers. It must be idempotent: any step may be repeated
/// after an interruption, and a repeat converges on the same state.
public typealias LifecycleAction = @Sendable (Set<LifecycleTrigger>) async -> LifecycleStepOutcome

public enum LifecycleCoordinatorError: Error, Equatable {
    /// `connect(to:)` needs the coordinator to have been attached to a root as one of its modules first.
    case notAttached
}

// MARK: - Coordinator

/// Orders the recovery and foreground work that keeps the app's services converged, without ever making capture wait.
///
/// A pass runs the steps of `LifecycleStep` in order. Every step is idempotent and bounded, so there is no recovery
/// state to persist: a process that dies anywhere between or inside steps simply repeats the whole pass on the next
/// launch, and each step finds and finishes what the interruption left undone.
///
/// * **Capture never waits.** A pass runs on its own task. `request(_:)` and the lifecycle hooks return at once, and
///   the capture path (`ServiceCompositionRoot.saveText`) shares nothing with a pass beyond the services' own locks.
/// * **Serialized.** At most one pass runs. Triggers that arrive while one runs coalesce into exactly one further pass,
///   so repeated activation, an overlapping launch and a foreground event never run steps concurrently or pile up.
/// * **Bounded.** Each step runs once per pass and is abandoned after `stepTimeoutSeconds`; a hung effect reports
///   `timedOut` and the pass goes on. A step that only starts work (a job drain, a health read) returns `requested`.
/// * **Failures are local.** One step failing or deferring never stops the steps after it.
///
/// It joins the root as a `CompositionModule` so it can own the deletion, retention and health clients and receive
/// their events. Build it, pass it in `modules`, then call `connect(to:)` with the root and `start(isActive:)`.
/// The clock/zone-change handling owned elsewhere raises `timeChangeTrigger`. No prompt-eligibility reconciliation
/// exists in the native layer yet, so that step reports `notConfigured` until an action is set for it.
public final class LifecycleCoordinator: CompositionModule, @unchecked Sendable {
    public static let defaultStepTimeoutSeconds: TimeInterval = 30

    public let identifier = "lifecycle"
    /// The coordinator issues no operations of its own; this is the first range a module may use.
    public let firstOperationID: UInt64 = ServiceCompositionRoot.minimumModuleOperationID

    private let stepTimeoutSeconds: TimeInterval
    private let lock = NSLock()
    private var notificationCenter: NotificationCenter
    private var actions: [LifecycleStep: LifecycleAction]
    private var isRunning = false
    private var queuedTriggers = Set<LifecycleTrigger>()
    private var queuedWaiters: [CheckedContinuation<LifecyclePassReport, Never>] = []
    private var activeRace: StepRace?
    private var isStopped = false
    private var foregroundObserver: NSObjectProtocol?
    private var latest: LifecyclePassReport?
    private var passCount = 0
    private var deletionClient: CoreDeletionCleanupClient?
    private var retentionClient: CoreAudioRetentionClient?
    private var healthService: ProcessingHealthService?

    public init(
        stepTimeoutSeconds: TimeInterval = LifecycleCoordinator.defaultStepTimeoutSeconds,
        notificationCenter: NotificationCenter = .default
    ) {
        self.stepTimeoutSeconds = stepTimeoutSeconds
        self.notificationCenter = notificationCenter
        actions = [:]
    }

    // MARK: Triggers

    /// Asks for a pass and returns at once. The pass runs on its own task.
    public func request(_ trigger: LifecycleTrigger) {
        Task(priority: .utility) { [self] in
            _ = await reconcile(trigger)
        }
    }

    /// The app launched and is active. Call it once, when the first scene is active.
    public func launch() {
        request(.launch)
    }

    public func foregroundActivated() {
        request(.foreground)
    }

    /// What the separately owned clock and time-zone handling calls when the system time or zone changed. Callbacks
    /// that arrive together share one pass, and only the reminder and prompt steps run.
    public var timeChangeTrigger: @Sendable () -> Void {
        return { [weak self] in self?.request(.systemTimeChange) }
    }

    /// Runs one pass for `trigger` and returns its report. A call made while a pass runs waits for the single pass
    /// that follows it, shared with every other call made in the meantime. After `shutDown()` nothing runs and every
    /// step reports `interrupted`.
    public func reconcile(_ trigger: LifecycleTrigger) async -> LifecyclePassReport {
        if stopped() {
            return stoppedReport(for: [trigger])
        }
        return await withCheckedContinuation { (continuation: CheckedContinuation<LifecyclePassReport, Never>) in
            lock.lock()
            if isRunning {
                queuedTriggers.insert(trigger)
                queuedWaiters.append(continuation)
                lock.unlock()
                return
            }
            isRunning = true
            lock.unlock()
            Task(priority: .utility) { [self] in
                await drain(first: trigger, firstWaiter: continuation)
            }
        }
    }

    /// Registers the foreground hook and, when `isActive`, requests the launch pass. Idempotent.
    public func start(isActive: Bool) {
        lock.lock()
        guard !isStopped, foregroundObserver == nil else {
            lock.unlock()
            return
        }
        foregroundObserver = notificationCenter.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.foregroundActivated()
        }
        lock.unlock()
        if isActive {
            launch()
        }
    }

    // MARK: Observation

    /// The report of the most recent finished pass.
    public var lastReport: LifecyclePassReport? {
        lock.lock()
        defer { lock.unlock() }
        return latest
    }

    /// How many passes have finished.
    public var finishedPassCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return passCount
    }

    /// Calls that are waiting for the pass that follows the running one.
    var queuedRequestCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return queuedWaiters.count
    }

    // MARK: Actions

    /// Sets what a step does. Steps without an action report `notConfigured`.
    public func setAction(_ action: LifecycleAction?, for step: LifecycleStep) {
        lock.lock()
        actions[step] = action
        lock.unlock()
    }

    // MARK: CompositionModule

    public func attach(to context: CompositionContext) throws {
        let deletion = try context.clients.deletionCleanup()
        let retention = try context.clients.audioRetention()
        let health = try context.clients.processingHealth()
        lock.lock()
        deletionClient = deletion
        retentionClient = retention
        healthService = health
        lock.unlock()
    }

    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let health = healthService
        let deletion = deletionClient
        let retention = retentionClient
        lock.unlock()
        return health?.handle(event) ?? false
            || deletion?.handle(event) ?? false
            || retention?.handle(event) ?? false
    }

    /// Stops accepting passes, releases the step in flight and every waiting caller, and ends the clients' waits. The
    /// root calls it before closing the core. Idempotent.
    public func shutDown() {
        lock.lock()
        guard !isStopped else {
            lock.unlock()
            return
        }
        isStopped = true
        let observer = foregroundObserver
        foregroundObserver = nil
        let race = activeRace
        let deletion = deletionClient
        let retention = retentionClient
        let center = notificationCenter
        lock.unlock()

        if let observer {
            center.removeObserver(observer)
        }
        race?.finish(.interrupted)
        deletion?.shutDown()
        retention?.shutDown()
    }

    // MARK: Pass

    private func stopped() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return isStopped
    }

    private func stoppedReport(for triggers: Set<LifecycleTrigger>) -> LifecyclePassReport {
        let wanted = Set(triggers.flatMap { $0.steps })
        return LifecyclePassReport(
            triggers: triggers,
            results: LifecycleStep.allCases.filter { wanted.contains($0) }
                .map { LifecycleStepResult(step: $0, outcome: .interrupted) })
    }

    private func drain(first: LifecycleTrigger, firstWaiter: CheckedContinuation<LifecyclePassReport, Never>) async {
        var triggers: Set<LifecycleTrigger> = [first]
        var waiters = [firstWaiter]
        while true {
            let report = await runPass(triggers: triggers)
            recordFinished(report)
            for waiter in waiters {
                waiter.resume(returning: report)
            }
            guard let next = takeQueued() else { return }
            triggers = next.triggers
            waiters = next.waiters
        }
    }

    /// The triggers and callers that arrived during the pass, or `nil` (with the running flag cleared) when none did.
    private func takeQueued() -> (triggers: Set<LifecycleTrigger>, waiters: [CheckedContinuation<LifecyclePassReport, Never>])? {
        lock.lock()
        defer { lock.unlock() }
        guard !queuedWaiters.isEmpty else {
            isRunning = false
            return nil
        }
        let taken = (queuedTriggers, queuedWaiters)
        queuedTriggers = []
        queuedWaiters = []
        return taken
    }

    private func recordFinished(_ report: LifecyclePassReport) {
        lock.lock()
        latest = report
        passCount += 1
        lock.unlock()
    }

    private func action(for step: LifecycleStep) -> LifecycleAction? {
        lock.lock()
        defer { lock.unlock() }
        return actions[step]
    }

    private func runPass(triggers: Set<LifecycleTrigger>) async -> LifecyclePassReport {
        let wanted = Set(triggers.flatMap { $0.steps })
        var results: [LifecycleStepResult] = []
        for step in LifecycleStep.allCases where wanted.contains(step) {
            if stopped() {
                results.append(LifecycleStepResult(step: step, outcome: .interrupted))
                continue
            }
            guard let action = action(for: step) else {
                results.append(LifecycleStepResult(step: step, outcome: .notConfigured))
                continue
            }
            let outcome = await runBounded(action, triggers: triggers)
            results.append(LifecycleStepResult(step: step, outcome: outcome))
        }
        return LifecyclePassReport(triggers: triggers, results: results)
    }

    /// Runs `action` and gives up on it after the step bound or on shutdown, whichever comes first. An action that
    /// ignores cancellation keeps running, but the pass no longer waits for it.
    private func runBounded(_ action: @escaping LifecycleAction, triggers: Set<LifecycleTrigger>) async
        -> LifecycleStepOutcome
    {
        let timeoutNanoseconds = UInt64(max(0, stepTimeoutSeconds) * 1_000_000_000)
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<LifecycleStepOutcome, Never>) in
            let race = StepRace(continuation: continuation)
            guard beginRace(race) else {
                race.finish(.interrupted)
                return
            }
            race.track(
                Task {
                    race.finish(await action(triggers))
                })
            race.track(
                Task {
                    try? await Task.sleep(nanoseconds: timeoutNanoseconds)
                    race.finish(.timedOut)
                })
        }
        endRace()
        return outcome
    }

    private func beginRace(_ race: StepRace) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !isStopped else { return false }
        activeRace = race
        return true
    }

    private func endRace() {
        lock.lock()
        activeRace = nil
        lock.unlock()
    }
}

/// Resolves a step with whichever comes first: its result, its time bound or a shutdown.
private final class StepRace: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<LifecycleStepOutcome, Never>?
    private var tasks: [Task<Void, Never>] = []
    private var isFinished = false

    init(continuation: CheckedContinuation<LifecycleStepOutcome, Never>) {
        self.continuation = continuation
    }

    func track(_ task: Task<Void, Never>) {
        lock.lock()
        if isFinished {
            lock.unlock()
            task.cancel()
            return
        }
        tasks.append(task)
        lock.unlock()
    }

    func finish(_ outcome: LifecycleStepOutcome) {
        lock.lock()
        guard !isFinished, let pending = continuation else {
            lock.unlock()
            return
        }
        isFinished = true
        continuation = nil
        let toCancel = tasks
        tasks = []
        lock.unlock()
        for task in toCancel {
            task.cancel()
        }
        pending.resume(returning: outcome)
    }
}

// MARK: - Production wiring

extension LifecycleCoordinator {
    /// Connects the steps to the services of a built root: staged-capture recovery, the job runner, deletion cleanup,
    /// the reminder pass, audio retention and the health read. The coordinator must already be one of the root's
    /// modules. Call it before `start(isActive:)`, which then listens on the root's notification center.
    public func connect(to root: ServiceCompositionRoot) throws {
        lock.lock()
        let deletionClient = self.deletionClient
        let retentionClient = self.retentionClient
        let health = self.healthService
        lock.unlock()
        guard let deletionClient, let retentionClient, let health else {
            throw LifecycleCoordinatorError.notAttached
        }

        let layout = ProtectedStorageLayout(
            rootDirectory: URL(fileURLWithPath: root.configuration.storageRootPath, isDirectory: true))
        let notifications = NotificationBridge(
            center: root.adapters.notificationCenter, ingestor: NotificationEventIngestor(handler: { _ in }))
        let deletion = DeletionCleanupService(
            core: deletionClient, notifications: notifications, localCleanup: DeletionLocalCleanup(layout: layout))
        let retention = AudioRetentionService(core: retentionClient, layout: layout)

        lock.lock()
        notificationCenter = root.adapters.lifecycleNotifications
        lock.unlock()

        setAction(
            { [weak root] _ in
                guard let root else { return .deferred("root_unavailable") }
                return await withCheckedContinuation { continuation in
                    DispatchQueue.main.async {
                        root.recoverStagedIngress { report in
                            continuation.resume(returning: Self.outcome(ofIngress: report))
                        }
                    }
                }
            }, for: .ingress)
        setAction(
            { [weak root] _ in
                guard let root else { return .deferred("root_unavailable") }
                root.jobRunner.activate()
                return .requested
            }, for: .jobs)
        setAction({ _ in Self.outcome(ofDeletion: await deletion.reconcile()) }, for: .deletion)
        setAction(
            { [weak root] triggers in
                guard let root else { return .deferred("root_unavailable") }
                let trigger: NotificationReconcileTrigger =
                    triggers.contains(.launch) ? .launch : (triggers.contains(.foreground) ? .foreground : .explicit)
                do {
                    return Self.outcome(ofReminders: try await root.reconcileReminders(trigger: trigger))
                } catch is CancellationError {
                    return .deferred("reminders_cancelled")
                } catch {
                    return .failed("reminders_pass_failed")
                }
            }, for: .reminders)
        setAction({ _ in Self.outcome(ofRetention: await retention.sweep()) }, for: .retention)
        setAction(
            { _ in
                health.refresh()
                return .requested
            }, for: .health)
    }

    static func outcome(ofIngress report: IngressRecoveryReport?) -> LifecycleStepOutcome {
        guard let report else { return .deferred("ingress_unavailable") }
        if report.listingFailure != nil { return .deferred("ingress_listing_failed") }
        let keptForRetry = report.entries.contains { entry in
            if case .keptForRetry = entry.outcome { return true }
            return false
        }
        return keptForRetry ? .deferred("ingress_kept_for_retry") : .completed
    }

    static func outcome(ofDeletion report: DeletionReconcileReport) -> LifecycleStepOutcome {
        if report.listingFailure != nil { return .deferred("deletion_listing_failed") }
        if !report.terminalFailures.isEmpty { return .failed("deletion_terminal_failure") }
        return report.converged ? .completed : .deferred("deletion_pending")
    }

    static func outcome(ofReminders report: NotificationPermissionReport) -> LifecycleStepOutcome {
        if report.reconciliationFailure != nil { return .deferred("reminders_pass_incomplete") }
        return report.converged ? .completed : .deferred("reminders_not_converged")
    }

    static func outcome(ofRetention report: AudioRetentionSweepReport) -> LifecycleStepOutcome {
        if report.listingFailure != nil { return .deferred("retention_listing_failed") }
        return report.converged ? .completed : .deferred("retention_unfinished")
    }
}
