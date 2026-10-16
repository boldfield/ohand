import Foundation
import OhAndCoreBridge

/// The operations one client has in flight, keyed by the operation identifier the client issued.
final class InFlightOperations<Waiting>: @unchecked Sendable {
    private let lock = NSLock()
    private var nextOperationID: UInt64
    private var waiting: [UInt64: Waiting] = [:]
    private var isShutDown = false

    init(firstOperationID: UInt64) {
        nextOperationID = firstOperationID
    }

    /// Records `entry` under a new identifier, or returns `nil` after `shutDown()`.
    func begin(_ entry: Waiting) -> UInt64? {
        lock.lock()
        defer { lock.unlock() }
        if isShutDown { return nil }
        let operationID = nextOperationID
        nextOperationID += 1
        waiting[operationID] = entry
        return operationID
    }

    func contains(_ operationID: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return waiting[operationID] != nil
    }

    func finish(_ operationID: UInt64) -> Waiting? {
        lock.lock()
        defer { lock.unlock() }
        return waiting.removeValue(forKey: operationID)
    }

    /// Refuses later operations and returns the ones still waiting. Idempotent.
    func shutDown() -> [Waiting] {
        lock.lock()
        defer { lock.unlock() }
        isShutDown = true
        let remaining = Array(waiting.values)
        waiting = [:]
        return remaining
    }
}

/// The core's answer to a provider-profile request, by command.
enum ProviderProfileAnswer: Equatable, Sendable {
    case created(ProviderProfileCreatedAnswer)
    case defaultSet(ProviderProfileDefaultSetAnswer)
    case defaultCleared(ProviderProfileDefaultClearedAnswer)
    case listing(ProviderProfileListAnswer)

    static func decode(_ data: Data, for command: ProviderProfileCommand) throws -> ProviderProfileAnswer {
        let decoder = JSONDecoder()
        switch command {
        case .createProfile: return .created(try decoder.decode(ProviderProfileCreatedAnswer.self, from: data))
        case .setDefault: return .defaultSet(try decoder.decode(ProviderProfileDefaultSetAnswer.self, from: data))
        case .clearDefault:
            return .defaultCleared(try decoder.decode(ProviderProfileDefaultClearedAnswer.self, from: data))
        case .list: return .listing(try decoder.decode(ProviderProfileListAnswer.self, from: data))
        }
    }
}

/// A route-policy request: a deliberate grant, its revocation, or the readable listing.
enum RoutePolicyCommand: Equatable, Sendable {
    case grant(routeID: String, capability: String, profileVersion: String)
    case revoke(routeID: String, capability: String)
    case list
}

/// The core's answer to a route-policy request, by command.
enum RoutePolicyAnswer: Equatable, Sendable {
    case changed(RoutePolicyChangeOutcome)
    case listing(RoutePolicyListing)

    static func decode(_ data: Data, for command: RoutePolicyCommand) throws -> RoutePolicyAnswer {
        let decoder = JSONDecoder()
        switch command {
        case .grant, .revoke: return .changed(try decoder.decode(RoutePolicyChangeOutcome.self, from: data))
        case .list: return .listing(try decoder.decode(RoutePolicyListing.self, from: data))
        }
    }
}

/// Fixed failures every configuration client reports itself, never from the core.
enum ProviderConfigurationClientFailure {
    static let shutDown = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the provider configuration client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")
}

/// The core's provider-profile requests as native code uses them (`ohand_core_start_provider_profiles`). The core owns
/// every rule: validation, versioning, which versions may be a default.
protocol ProviderProfileCore: Sendable {
    func submit(
        _ command: ProviderProfileCommand,
        completion: @escaping @Sendable (Result<ProviderProfileAnswer, CoreFailure>) -> Void)
}

/// The core's connection check as native code uses it (`ohand_core_start_provider_connection_check`).
protocol ProviderConnectionCheckCore: Sendable {
    /// Sends the check for one stored profile version and calls `completion` once with its redacted outcome.
    func check(
        profileVersion: String,
        completion: @escaping @Sendable (Result<ProviderConnectionOutcome, CoreFailure>) -> Void)
}

/// The core's route-policy requests as native code uses them (the grant, revoke and list route policy exports).
protocol RoutePolicyCore: Sendable {
    func submit(
        _ command: RoutePolicyCommand,
        completion: @escaping @Sendable (Result<RoutePolicyAnswer, CoreFailure>) -> Void)
}

/// Runs provider-profile requests over a `CoreHandle`. The core answers on its single event handler, so the owner of
/// that handler must forward every event to `handle(_:)`; it returns `true` when the event answered one of this
/// client's operations. Call `shutDown()` before closing the core so no completion is left waiting.
final class CoreProviderProfileClient: ProviderProfileCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 41

    private struct Waiting {
        let command: ProviderProfileCommand
        let completion: @Sendable (Result<ProviderProfileAnswer, CoreFailure>) -> Void
    }

    private let core: CoreHandle
    private let operations = InFlightOperations<Waiting>(firstOperationID: CoreProviderProfileClient.firstOperationID)

    init(core: CoreHandle) {
        self.core = core
    }

    func submit(
        _ command: ProviderProfileCommand,
        completion: @escaping @Sendable (Result<ProviderProfileAnswer, CoreFailure>) -> Void
    ) {
        guard let operationID = operations.begin(Waiting(command: command, completion: completion)) else {
            completion(.failure(ProviderConfigurationClientFailure.shutDown))
            return
        }
        do {
            try core.startProviderProfiles(operationID: operationID, command: command)
        } catch {
            let failure = (error as? CoreFailure) ?? ProviderConfigurationClientFailure.unreadableAnswer
            operations.finish(operationID)?.completion(.failure(failure))
        }
    }

    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        guard let waiting = operations.finish(event.operationID) else { return false }
        switch event.outcome {
        case .failure(let failure):
            waiting.completion(.failure(failure))
        case .success(let data):
            if let answer = try? ProviderProfileAnswer.decode(data, for: waiting.command) {
                waiting.completion(.success(answer))
            } else {
                waiting.completion(.failure(ProviderConfigurationClientFailure.unreadableAnswer))
            }
        }
        return true
    }

    func shutDown() {
        for waiting in operations.shutDown() {
            waiting.completion(.failure(ProviderConfigurationClientFailure.shutDown))
        }
    }
}

/// Runs connection checks over a `CoreHandle`. A check answers with a `dispatched` event once the request was handed to
/// the native transport and a `completed` event with the outcome; only the second finishes the operation. A failure
/// event finishes it too. Forward every core event to `handle(_:)`, and call `shutDown()` before closing the core.
final class CoreProviderConnectionCheckClient: ProviderConnectionCheckCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 43

    typealias Completion = @Sendable (Result<ProviderConnectionOutcome, CoreFailure>) -> Void

    private let core: CoreHandle
    private let operations = InFlightOperations<Completion>(
        firstOperationID: CoreProviderConnectionCheckClient.firstOperationID)

    init(core: CoreHandle) {
        self.core = core
    }

    func check(profileVersion: String, completion: @escaping Completion) {
        guard let operationID = operations.begin(completion) else {
            completion(.failure(ProviderConfigurationClientFailure.shutDown))
            return
        }
        do {
            try core.startProviderConnectionCheck(operationID: operationID, profileVersion: profileVersion)
        } catch {
            let failure = (error as? CoreFailure) ?? ProviderConfigurationClientFailure.unreadableAnswer
            operations.finish(operationID)?(.failure(failure))
        }
    }

    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        guard operations.contains(event.operationID) else { return false }
        switch event.outcome {
        case .failure(let failure):
            operations.finish(event.operationID)?(.failure(failure))
        case .success(let data):
            guard let step = try? JSONDecoder().decode(ProviderConnectionCheckEvent.self, from: data) else {
                operations.finish(event.operationID)?(.failure(ProviderConfigurationClientFailure.unreadableAnswer))
                return true
            }
            switch step.phase {
            case .dispatched:
                break
            case .completed:
                if let outcome = step.outcome {
                    operations.finish(event.operationID)?(.success(outcome))
                } else {
                    operations.finish(event.operationID)?(
                        .failure(ProviderConfigurationClientFailure.unreadableAnswer))
                }
            }
        }
        return true
    }

    func shutDown() {
        for completion in operations.shutDown() {
            completion(.failure(ProviderConfigurationClientFailure.shutDown))
        }
    }
}

/// Runs route-policy requests over a `CoreHandle`. Forward every core event to `handle(_:)`, and call `shutDown()`
/// before closing the core.
final class CoreRoutePolicyClient: RoutePolicyCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 45

    private struct Waiting {
        let command: RoutePolicyCommand
        let completion: @Sendable (Result<RoutePolicyAnswer, CoreFailure>) -> Void
    }

    private let core: CoreHandle
    private let operations = InFlightOperations<Waiting>(firstOperationID: CoreRoutePolicyClient.firstOperationID)

    init(core: CoreHandle) {
        self.core = core
    }

    func submit(
        _ command: RoutePolicyCommand,
        completion: @escaping @Sendable (Result<RoutePolicyAnswer, CoreFailure>) -> Void
    ) {
        guard let operationID = operations.begin(Waiting(command: command, completion: completion)) else {
            completion(.failure(ProviderConfigurationClientFailure.shutDown))
            return
        }
        do {
            switch command {
            case .grant(let routeID, let capability, let profileVersion):
                try core.startGrantRouteCapability(
                    operationID: operationID, routeID: routeID, capability: capability,
                    profileVersion: profileVersion)
            case .revoke(let routeID, let capability):
                try core.startRevokeRouteCapability(
                    operationID: operationID, routeID: routeID, capability: capability)
            case .list:
                try core.startListRoutePolicy(operationID: operationID)
            }
        } catch {
            let failure = (error as? CoreFailure) ?? ProviderConfigurationClientFailure.unreadableAnswer
            operations.finish(operationID)?.completion(.failure(failure))
        }
    }

    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        guard let waiting = operations.finish(event.operationID) else { return false }
        switch event.outcome {
        case .failure(let failure):
            waiting.completion(.failure(failure))
        case .success(let data):
            if let answer = try? RoutePolicyAnswer.decode(data, for: waiting.command) {
                waiting.completion(.success(answer))
            } else {
                waiting.completion(.failure(ProviderConfigurationClientFailure.unreadableAnswer))
            }
        }
        return true
    }

    func shutDown() {
        for waiting in operations.shutDown() {
            waiting.completion(.failure(ProviderConfigurationClientFailure.shutDown))
        }
    }
}
