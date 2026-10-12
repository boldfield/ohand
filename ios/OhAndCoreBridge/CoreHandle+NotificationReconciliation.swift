import Foundation
import OhandCoreC

/// A schedule request from the core. Identifiers and the instant are text exactly as the core wrote them.
public struct NotificationScheduleCommand: Decodable, Equatable, Sendable {
    public let notificationID: String
    public let fireAt: String
    public let payloadKind: String
    public let opaqueTargetID: String

    enum CodingKeys: String, CodingKey {
        case notificationID = "notification_id"
        case fireAt = "fire_at"
        case payloadKind = "payload_kind"
        case opaqueTargetID = "opaque_target_id"
    }
}

struct NotificationCancelCommand: Decodable {
    let notificationID: String

    enum CodingKeys: String, CodingKey {
        case notificationID = "notification_id"
    }
}

/// A command from the core's reminder reconciliation to native code, delivered on the core worker thread.
public enum NotificationEffectCommand: Equatable, Sendable {
    /// Install the request and answer once with `completeNotificationEffect`.
    case schedule(NotificationScheduleCommand)
    case cancel(notificationID: String)
    case listPending
    /// The core stopped waiting for the effect; abandon the work. No answer is expected.
    case abandon
}

public struct PendingNotificationAnswer: Equatable, Sendable {
    public let notificationID: String
    public let fireAt: String?

    public init(notificationID: String, fireAt: String?) {
        self.notificationID = notificationID
        self.fireAt = fireAt
    }
}

/// The native answer to one `NotificationEffectCommand`. `code` is lowercase letters, digits and underscores.
/// The answer must fit the command: `schedule` and `cancel` take `done` or `failed`, `listPending` takes `pending` or
/// `failed`. The core refuses any other pairing with `invalid_request` and keeps waiting for a valid answer.
public enum NotificationEffectAnswer: Equatable, Sendable {
    case done
    case failed(errorClass: CoreErrorClass, code: String)
    /// The pending requests native can name, plus how many more are pending under identifiers the core cannot
    /// carry (empty, or longer than 128 UTF-8 bytes). The core charges each of those against the budget under a
    /// placeholder no prompt or reminder can share, and never asks to cancel one.
    case pending([PendingNotificationAnswer], unattributable: Int = 0)
}

/// One pass of the core's capacity and reconciliation logic. `factsOnly` reads the reminders without a pass.
public struct CoreReminderReconcileRequest: Encodable, Equatable, Sendable {
    public struct Prompt: Encodable, Equatable, Sendable {
        public let notificationID: String
        public let fireAt: String

        public init(notificationID: String, fireAt: String) {
            self.notificationID = notificationID
            self.fireAt = fireAt
        }

        enum CodingKeys: String, CodingKey {
            case notificationID = "notification_id"
            case fireAt = "fire_at"
        }
    }

    public let rearmRejectedSchedules: Bool
    public let allowOptionalPrompts: Bool
    public let prompts: [Prompt]
    public let factsOnly: Bool

    public init(
        rearmRejectedSchedules: Bool = false,
        allowOptionalPrompts: Bool = false,
        prompts: [Prompt] = [],
        factsOnly: Bool = false
    ) {
        self.rearmRejectedSchedules = rearmRejectedSchedules
        self.allowOptionalPrompts = allowOptionalPrompts
        self.prompts = prompts
        self.factsOnly = factsOnly
    }

    enum CodingKeys: String, CodingKey {
        case rearmRejectedSchedules = "rearm_rejected_schedules"
        case allowOptionalPrompts = "allow_optional_prompts"
        case prompts
        case factsOnly = "facts_only"
    }
}

/// A reminder's stored state after a pass: machine labels only, as the core's status report states them.
public struct CoreReminderFacts: Decodable, Equatable, Sendable {
    public let itemID: String
    public let requestState: String
    public let scheduleState: String?
    public let deliveryState: String?
    public let acknowledgmentState: String?
    public let unschedulableReason: String?

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case requestState = "request_state"
        case scheduleState = "schedule_state"
        case deliveryState = "delivery_state"
        case acknowledgmentState = "acknowledgment_state"
        case unschedulableReason = "unschedulable_reason"
    }
}

/// The outcome event of a reconciliation pass. Counts and labels only.
public struct CoreReminderReconcileOutcome: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let rearmed: Int
    public let converged: Bool
    public let installed: Int
    public let alreadyInstalled: Int
    public let removed: Int
    public let withheld: Int
    public let failures: Int
    public let demoted: Int
    public let refilled: Int
    public let expired: Int
    public let promptsGranted: Int
    public let promptsDeferred: Int
    public let promptsWithheld: Int
    public let reminders: [CoreReminderFacts]

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case rearmed
        case converged
        case installed
        case alreadyInstalled = "already_installed"
        case removed
        case withheld
        case failures
        case demoted
        case refilled
        case expired
        case promptsGranted = "prompts_granted"
        case promptsDeferred = "prompts_deferred"
        case promptsWithheld = "prompts_withheld"
        case reminders
    }
}

private struct EffectAnswerBody: Encodable {
    struct ErrorBody: Encodable {
        let errorClass: String
        let code: String

        enum CodingKeys: String, CodingKey {
            case errorClass = "class"
            case code
        }
    }

    struct PendingBody: Encodable {
        let notificationID: String
        let fireAt: String?

        enum CodingKeys: String, CodingKey {
            case notificationID = "notification_id"
            case fireAt = "fire_at"
        }
    }

    let error: ErrorBody?
    let pending: [PendingBody]?
    let unattributable: Int?

    init(_ answer: NotificationEffectAnswer) {
        switch answer {
        case .done:
            error = nil
            pending = nil
            unattributable = nil
        case .failed(let errorClass, let code):
            error = ErrorBody(errorClass: errorClass.rawValue, code: code)
            pending = nil
            unattributable = nil
        case .pending(let entries, let unattributableCount):
            error = nil
            pending = entries.map { PendingBody(notificationID: $0.notificationID, fireAt: $0.fireAt) }
            unattributable = unattributableCount > 0 ? unattributableCount : nil
        }
    }
}

/// Keeps one native notification-effects handler registered with a core. The handler runs on the core worker
/// thread, which waits for the answer, so it must return promptly (start the OS work and return) and answer
/// each command exactly once with `CoreHandle.completeNotificationEffect`. `invalidate()` waits for a running
/// handler invocation, so it must not be called from inside the handler.
public final class NotificationEffectsRegistration: @unchecked Sendable {
    private let handleIdentifier: OhandCoreHandle
    private let handler: @Sendable (UInt64, NotificationEffectCommand) -> Void
    private let stateLock = NSLock()
    private var isInvalidated = false

    fileprivate init(handleIdentifier: OhandCoreHandle, handler: @escaping @Sendable (UInt64, NotificationEffectCommand) -> Void) {
        self.handleIdentifier = handleIdentifier
        self.handler = handler
    }

    fileprivate func receive(effectID: UInt64, command: UInt32, data: Data) {
        switch command {
        case UInt32(OHAND_NOTIFICATION_COMMAND_SCHEDULE):
            guard let schedule = try? JSONDecoder().decode(NotificationScheduleCommand.self, from: data) else {
                reject(effectID)
                return
            }
            handler(effectID, .schedule(schedule))
        case UInt32(OHAND_NOTIFICATION_COMMAND_CANCEL):
            guard let cancel = try? JSONDecoder().decode(NotificationCancelCommand.self, from: data) else {
                reject(effectID)
                return
            }
            handler(effectID, .cancel(notificationID: cancel.notificationID))
        case UInt32(OHAND_NOTIFICATION_COMMAND_LIST_PENDING):
            handler(effectID, .listPending)
        case UInt32(OHAND_NOTIFICATION_COMMAND_ABANDON):
            handler(effectID, .abandon)
        default:
            reject(effectID)
        }
    }

    private func reject(_ effectID: UInt64) {
        let body = Array(#"{"error":{"class":"permanent","code":"invalid_payload"}}"#.utf8)
        let rawResult = body.withUnsafeBufferPointer { buffer in
            ohand_core_complete_notification_effect(handleIdentifier, effectID, buffer.baseAddress, buffer.count)
        }
        _ = try? consumeCoreResult(rawResult)
    }

    /// Unregisters the handler and releases the registration's retained context. Idempotent. After it returns
    /// the handler is never invoked again. A registration that a later `registerNotificationEffects` replaced
    /// only releases itself and leaves the replacement active.
    public func invalidate() {
        stateLock.lock()
        guard !isInvalidated else {
            stateLock.unlock()
            return
        }
        isInvalidated = true
        stateLock.unlock()

        let ownContext = Unmanaged.passUnretained(self).toOpaque()
        _ = try? consumeCoreResult(ohand_core_set_notification_effects(handleIdentifier, nil, ownContext))
        Unmanaged.passUnretained(self).release()
    }
}

/// Runs on the core worker thread. The context is a `NotificationEffectsRegistration` retained once on creation
/// and released by `invalidate()`, which first waits for any running invocation.
private let notificationEffectsTrampoline: OhandNotificationEffectCallback = { context, effectID, command, data, length in
    guard let context else { return }
    let registration = Unmanaged<NotificationEffectsRegistration>.fromOpaque(context).takeUnretainedValue()
    var bytes = Data()
    if let data, length > 0 {
        bytes = Data(bytes: data, count: length)
    }
    registration.receive(effectID: effectID, command: command, data: bytes)
}

extension CoreHandle {
    /// Registers the native notification effects the core's reconciliation drives, replacing any earlier ones.
    /// Keep the returned registration until the effects are no longer needed, then call `invalidate()` before
    /// closing.
    public func registerNotificationEffects(
        handler: @escaping @Sendable (UInt64, NotificationEffectCommand) -> Void
    ) throws -> NotificationEffectsRegistration {
        let registration = NotificationEffectsRegistration(handleIdentifier: handleIdentifier, handler: handler)
        let context = Unmanaged.passRetained(registration).toOpaque()
        let rawResult = ohand_core_set_notification_effects(handleIdentifier, notificationEffectsTrampoline, context)
        do {
            _ = try consumeCoreResult(rawResult)
        } catch {
            Unmanaged<NotificationEffectsRegistration>.fromOpaque(context).release()
            throw error
        }
        return registration
    }

    /// Queues one reconciliation pass (or a facts-only read). The outcome event decodes as
    /// `CoreReminderReconcileOutcome`. A pass needs registered effects; a failure event means the pass did not
    /// complete, and the next pass repeats it.
    public func startReconcileReminders(operationID: UInt64, request: CoreReminderReconcileRequest) throws {
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_reconcile_reminders(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Answers a native effect command. `not_found` means the core already stopped waiting for it.
    public func completeNotificationEffect(effectID: UInt64, answer: NotificationEffectAnswer) throws {
        let answerBytes = Array(try JSONEncoder().encode(EffectAnswerBody(answer)))
        let rawResult = answerBytes.withUnsafeBufferPointer { buffer in
            ohand_core_complete_notification_effect(handleIdentifier, effectID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
