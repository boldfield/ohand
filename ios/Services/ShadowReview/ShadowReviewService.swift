import Foundation
import OhAndCoreBridge

/// Runs shadow review through the core and the native provider transport.
///
/// Shadow review is a diagnostic comparison: the core samples a finished interpretation, sends one request to a
/// separately approved review profile, and stores only fixed difference codes. This service adds no policy of its
/// own. The core re-derives the profile, destination, credential reference and request text from stored state,
/// re-checks the route's `review` grant and the request budget immediately before each send, and records a
/// timeout, cancellation or refusal as unreviewed. A verdict never changes an item, reminder or permission.
///
/// The service is confined to the main thread, where `CoreHandle` delivers events. Route the handle's events to
/// `handle(_:)`; it returns `false` for events that belong to someone else. A provider transport must be attached
/// to the same core (`ProviderExchangeCoordinator.attach`) before a review is run, because the review's send and
/// cancel commands travel through it.
public final class ShadowReviewService {
    private enum Pending {
        case selection((ShadowReviewSelectionResult) -> Void)
        case run(jobID: String, (ShadowReviewRunResult) -> Void)
        case read((ShadowReviewReadResult) -> Void)
    }

    /// Operation identifiers owned by this service start far above the ranges other callers use.
    private static let firstOperationID: UInt64 = 1 << 48

    private let core: CoreHandle
    public private(set) var configuration: ShadowReviewConfiguration
    private var nextOperationID = ShadowReviewService.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var runningOperations: [String: UInt64] = [:]

    public init(core: CoreHandle, configuration: ShadowReviewConfiguration = .off) {
        self.core = core
        self.configuration = configuration
    }

    public func updateConfiguration(_ configuration: ShadowReviewConfiguration) {
        self.configuration = configuration
    }

    /// True when a review profile is configured and the policy is enabled. The core remains the authority.
    public var isConfigured: Bool {
        configuration.reviewProfileVersion != nil && configuration.policy.isEnabled
    }

    /// Offers a finished interpretation result for sampling. Without a configured review profile and an enabled
    /// policy, the answer is a local `notConfigured` skip and the core is not called.
    public func offer(
        itemID: String,
        sourceRevision: Int,
        requestVersion: String,
        completion: @escaping (ShadowReviewSelectionResult) -> Void
    ) {
        guard isConfigured, let reviewProfileVersion = configuration.reviewProfileVersion else {
            report(.skipped(code: ShadowReviewSkip.notConfigured.code))
            completion(.skipped(.notConfigured))
            return
        }
        let request = SelectionRequest(
            itemID: itemID, sourceRevision: sourceRevision, requestVersion: requestVersion,
            reviewProfileVersion: reviewProfileVersion, policy: configuration.policy.wire)
        submit(.selection(completion), failure: { completion(.failed(code: $0)) }) { operationID, body in
            try self.core.startShadowSelection(operationID: operationID, request: body)
        } body: {
            try JSONEncoder().encode(request)
        }
    }

    /// Runs one selected case. Cancel it with `cancel(jobID:)`. A case run while review is switched off is refused
    /// by the core (policy disabled) and stored as unreviewed rather than skipped locally.
    public func review(jobID: String, completion: @escaping (ShadowReviewRunResult) -> Void) {
        let request = RunRequest(jobID: jobID, policy: configuration.policy.wire)
        submit(.run(jobID: jobID, completion), failure: { completion(.failed(code: $0)) }) { operationID, body in
            try self.core.startShadowReview(operationID: operationID, request: body)
            self.runningOperations[jobID] = operationID
        } body: {
            try JSONEncoder().encode(request)
        }
    }

    /// Cancels a running review. The core ends it as unreviewed (cancelled). Idempotent; a no-op when none runs.
    public func cancel(jobID: String) {
        guard let operationID = runningOperations[jobID] else { return }
        try? core.cancelProviderExchange(operationID: operationID)
    }

    /// Reads the stored state of one case. Works whether or not review is currently configured.
    public func record(jobID: String, completion: @escaping (ShadowReviewReadResult) -> Void) {
        submit(.read(completion), failure: { completion(.failed(code: $0)) }) { operationID, body in
            try self.core.startShadowRecord(operationID: operationID, request: body)
        } body: {
            try JSONEncoder().encode(["job_id": jobID])
        }
    }

    /// Consumes the event if it answers one of this service's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        guard let operation = pending[event.operationID] else { return false }
        switch operation {
        case .selection(let completion):
            pending[event.operationID] = nil
            completion(selectionResult(of: event))
        case .read(let completion):
            pending[event.operationID] = nil
            completion(readResult(of: event))
        case .run(let jobID, let completion):
            handleRun(event, jobID: jobID, completion: completion)
        }
        return true
    }

    // MARK: private

    private struct SelectionRequest: Encodable {
        let itemID: String
        let sourceRevision: Int
        let requestVersion: String
        let reviewProfileVersion: String
        let policy: WirePolicy

        enum CodingKeys: String, CodingKey {
            case itemID = "item_id"
            case sourceRevision = "source_revision"
            case requestVersion = "request_version"
            case reviewProfileVersion = "review_profile_version"
            case policy
        }
    }

    private struct RunRequest: Encodable {
        let jobID: String
        let policy: WirePolicy

        enum CodingKeys: String, CodingKey {
            case jobID = "job_id"
            case policy
        }
    }

    private func submit(
        _ operation: Pending,
        failure: (String) -> Void,
        start: (UInt64, Data) throws -> Void,
        body: () throws -> Data
    ) {
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = operation
        do {
            try start(operationID, try body())
        } catch let error as CoreFailure {
            pending[operationID] = nil
            clearRunning(operationID)
            report(.failed(code: error.code))
            failure(error.code)
        } catch {
            pending[operationID] = nil
            clearRunning(operationID)
            report(.failed(code: "invalid_request"))
            failure("invalid_request")
        }
    }

    private func clearRunning(_ operationID: UInt64) {
        runningOperations = runningOperations.filter { $0.value != operationID }
    }

    private func selectionResult(of event: CoreEvent) -> ShadowReviewSelectionResult {
        guard let payload = try? event.decode(ShadowSelectionPayload.self) else {
            return failedSelection(event)
        }
        switch (payload.selection, payload.record) {
        case ("selected", let record?):
            report(.selected(jobID: record.jobID))
            return .selected(record)
        case ("already_selected", let record?):
            return .alreadySelected(record)
        case ("skipped", _):
            let code = payload.skip ?? "unknown"
            report(.skipped(code: code))
            return .skipped(ShadowReviewSkip(code: code, denial: payload.denial))
        default:
            report(.failed(code: "unreadable_event"))
            return .failed(code: "unreadable_event")
        }
    }

    private func failedSelection(_ event: CoreEvent) -> ShadowReviewSelectionResult {
        let code = failureCode(of: event)
        report(.failed(code: code))
        return .failed(code: code)
    }

    private func readResult(of event: CoreEvent) -> ShadowReviewReadResult {
        if let payload = try? event.decode(ShadowRecordPayload.self) {
            return .found(payload.record)
        }
        let code = failureCode(of: event)
        report(.failed(code: code))
        return .failed(code: code)
    }

    private func handleRun(_ event: CoreEvent, jobID: String, completion: (ShadowReviewRunResult) -> Void) {
        if case .failure(let failure) = event.outcome {
            finishRun(event.operationID, jobID: jobID)
            report(.failed(code: failure.code))
            completion(.failed(code: failure.code))
            return
        }
        guard let payload = try? event.decode(ShadowRunPayload.self) else {
            finishRun(event.operationID, jobID: jobID)
            report(.failed(code: "unreadable_event"))
            completion(.failed(code: "unreadable_event"))
            return
        }
        switch payload.phase {
        case .dispatched:
            report(.dispatched(jobID: jobID))
        case .completed:
            finishRun(event.operationID, jobID: jobID)
            guard let record = payload.record else {
                report(.failed(code: "unreadable_event"))
                completion(.failed(code: "unreadable_event"))
                return
            }
            reportCompletion(of: record, denial: payload.denial)
            completion(.completed(record, denial: payload.denial))
        }
    }

    private func finishRun(_ operationID: UInt64, jobID: String) {
        pending[operationID] = nil
        if runningOperations[jobID] == operationID { runningOperations[jobID] = nil }
    }

    private func reportCompletion(of record: ShadowReviewRecord, denial: String?) {
        if let denial {
            report(.denied(jobID: record.jobID, code: denial))
            return
        }
        switch record.outcome {
        case .reviewed:
            report(.reviewed(jobID: record.jobID, verdict: record.verdict, differences: record.differences))
        case .unreviewed:
            report(.unreviewed(jobID: record.jobID, reason: record.reason))
        case .pending, .error:
            report(.failed(code: record.lastFailure ?? record.outcome.rawValue))
        }
    }

    private func failureCode(of event: CoreEvent) -> String {
        if case .failure(let failure) = event.outcome { return failure.code }
        return "unreadable_event"
    }

    private func report(_ event: ShadowReviewInstrumentationEvent) {
        configuration.instrumentation?.shadowReview(event)
    }
}
