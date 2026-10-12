import Foundation
import OhAndCoreBridge

/// A reminder reconciliation that did not complete, in the shared normalized form. Its description never
/// carries identifiers, content or the text of an underlying error.
public struct ReminderReconciliationFailure: Error, Equatable, Sendable {
    public let errorClass: NotificationErrorClass
    public let code: String

    public init(errorClass: NotificationErrorClass, code: String) {
        self.errorClass = errorClass
        self.code = code
    }

    public static let coreUnavailable = ReminderReconciliationFailure(errorClass: .transient, code: "core_unavailable")
    public static let reconciliationFailed = ReminderReconciliationFailure(
        errorClass: .transient, code: "reconciliation_failed")

    static func normalized(_ error: Error) -> ReminderReconciliationFailure {
        if let failure = error as? ReminderReconciliationFailure { return failure }
        if let failure = error as? NotificationBridgeError {
            return ReminderReconciliationFailure(errorClass: failure.errorClass, code: failure.code)
        }
        if let failure = error as? CoreFailure {
            let errorClass = NotificationErrorClass(rawValue: failure.errorClass.rawValue) ?? .permanent
            return ReminderReconciliationFailure(errorClass: errorClass, code: failure.code)
        }
        if error is CancellationError { return ReminderReconciliationFailure(errorClass: .cancelled, code: "cancelled") }
        return .reconciliationFailed
    }
}

/// The production `ReminderReconciling`: it runs the real core's capacity pass and re-arm of OS-rejected
/// schedules, with the notification bridge as the core's native effects.
///
/// The core decides what to install, cancel and keep; this type only performs those effects through
/// `NotificationBridge` and reports the reminders' stored facts back. Nothing here writes delivery or
/// acknowledgment, and a permission change alone changes no durable reminder. A pass that the core could not
/// finish (for example an unreadable pending list) changed nothing it did not confirm, and the next pass repeats
/// it.
///
/// It owns the core's event handler through `claimEventHandler()`, or an assembly that needs other event
/// consumers forwards each event to `handle(_:)` and keeps the handler. Operation identifiers start at
/// `firstOperationID`, which an assembly sharing the handle sets apart from its other consumers.
public final class CoreReminderReconciler: ReminderReconciling, @unchecked Sendable {
    private let core: CoreHandle
    private let lock = NSLock()
    private var waiting: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var nextOperationID: UInt64
    private var registration: NotificationEffectsRegistration?

    /// `center` must be the center `bridge` was built on: the bridge installs and cancels reminder requests, and
    /// the same center is read and cancelled directly for optional-prompt requests, whose identifiers the
    /// bridge does not carry.
    public init(
        core: CoreHandle, bridge: NotificationBridge, center: NotificationCenterProviding,
        effectTimeout: TimeInterval = 10, removalPollInterval: TimeInterval = 0.05, maximumRemovalPolls: Int = 20,
        firstOperationID: UInt64 = 1 << 48
    ) throws {
        self.core = core
        self.nextOperationID = firstOperationID
        let effects = ReminderNativeEffects(
            bridge: bridge, center: center, effectTimeout: effectTimeout,
            removalPollInterval: removalPollInterval, maximumRemovalPolls: maximumRemovalPolls)
        registration = try core.registerNotificationEffects { [weak core] effectID, command in
            Self.perform(core: core, effects: effects, effectID: effectID, command: command)
        }
    }

    deinit { invalidate() }

    public func claimEventHandler() throws {
        try core.setEventHandler { [weak self] event in _ = self?.handle(event) }
    }

    /// Takes the event if it belongs to a pass of this reconciler.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let continuation = waiting.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let continuation else { return false }
        continuation.resume(returning: event.outcome)
        return true
    }

    /// Unregisters the native effects and ends passes still waiting for an event. Idempotent; call it before
    /// closing the core.
    public func invalidate() {
        lock.lock()
        let registration = self.registration
        self.registration = nil
        let abandoned = waiting
        waiting = [:]
        lock.unlock()
        registration?.invalidate()
        for continuation in abandoned.values {
            continuation.resume(returning: .failure(.handleClosed))
        }
    }

    public func reconcile(_ request: ReminderReconciliationRequest) async throws -> ReminderReconciliationOutcome {
        let formatter = Self.makeFormatter()
        let outcome = try await run(
            CoreReminderReconcileRequest(
                rearmRejectedSchedules: request.rearmRejectedSchedules,
                allowOptionalPrompts: request.allowOptionalPrompts,
                prompts: request.promptCandidates.map {
                    CoreReminderReconcileRequest.Prompt(
                        notificationID: $0.notificationID, fireAt: formatter.string(from: $0.fireAt))
                }
            ))
        return ReminderReconciliationOutcome(
            reminders: outcome.reminders.map(Self.facts),
            promptsGranted: outcome.promptsGranted,
            promptsDeferred: outcome.promptsDeferred,
            promptsWithheld: outcome.promptsWithheld,
            converged: outcome.converged)
    }

    public func storedFacts() async throws -> [ReminderFacts] {
        try await run(CoreReminderReconcileRequest(factsOnly: true)).reminders.map(Self.facts)
    }

    private func run(_ request: CoreReminderReconcileRequest) async throws -> CoreReminderReconcileOutcome {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            lock.lock()
            let operationID = nextOperationID
            nextOperationID += 1
            waiting[operationID] = continuation
            lock.unlock()
            do {
                try core.startReconcileReminders(operationID: operationID, request: request)
            } catch {
                lock.lock()
                let claimed = waiting.removeValue(forKey: operationID)
                lock.unlock()
                claimed?.resume(returning: .failure(Self.failure(error)))
            }
        }
        switch outcome {
        case .success(let data): return try JSONDecoder().decode(CoreReminderReconcileOutcome.self, from: data)
        case .failure(let failure): throw ReminderReconciliationFailure.normalized(failure)
        }
    }

    private static func failure(_ error: Error) -> CoreFailure {
        (error as? CoreFailure) ?? CoreFailure(
            errorClass: .transient, code: ReminderReconciliationFailure.coreUnavailable.code,
            message: "the reminder reconciliation could not start")
    }

    private static func facts(_ fact: CoreReminderFacts) -> ReminderFacts {
        ReminderFacts(
            itemID: fact.itemID, requestState: fact.requestState, scheduleState: fact.scheduleState,
            deliveryState: fact.deliveryState, acknowledgmentState: fact.acknowledgmentState,
            unschedulableReason: fact.unschedulableReason)
    }

    private static func makeFormatter() -> ISO8601DateFormatter {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }

    /// Runs on the core worker thread, which waits for the answer: start the OS work and return. An abandoned
    /// effect needs nothing here; the bridge bounds its own work, and its late answer is `not_found`.
    private static func perform(core: CoreHandle?, effects: ReminderNativeEffects, effectID: UInt64, command: NotificationEffectCommand) {
        if command == .abandon { return }
        Task {
            let reply = await answer(for: command, effects: effects)
            try? core?.completeNotificationEffect(effectID: effectID, answer: reply)
        }
    }

    private static func answer(for command: NotificationEffectCommand, effects: ReminderNativeEffects) async -> NotificationEffectAnswer {
        let invalidPayload = NotificationEffectAnswer.failed(errorClass: .permanent, code: NotificationBridgeError.invalidPayload.code)
        let formatter = makeFormatter()
        do {
            switch command {
            case .schedule(let schedule):
                guard let identifier = NotificationIdentifier(rawValue: schedule.notificationID),
                      let dueInstant = formatter.date(from: schedule.fireAt),
                      schedule.payloadKind == NotificationPayloadKind.generic.rawValue,
                      let target = OpaqueIdentifier(schedule.opaqueTargetID)
                else { return invalidPayload }
                _ = try await effects.bridge.schedule(.generic(identifier: identifier, dueInstant: dueInstant, opaqueTargetID: target))
                return .done
            case .cancel(let notificationID):
                try await effects.cancel(notificationID)
                return .done
            case .listPending:
                let pending = ReminderNativeEffects.carried(try await effects.pending())
                return .pending(pending.map { request in
                    PendingNotificationAnswer(
                        notificationID: request.identifier, fireAt: request.dueInstant.map { formatter.string(from: $0) })
                })
            case .abandon:
                return .done
            }
        } catch {
            let failure = ReminderReconciliationFailure.normalized(error)
            return .failed(errorClass: CoreErrorClass(rawValue: failure.errorClass.rawValue) ?? .permanent, code: failure.code)
        }
    }
}
