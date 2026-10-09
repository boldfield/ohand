import Foundation
import OhAndCoreBridge

/// How one drain ended: the core's content-free summary, or the failure that ended it.
public enum JobDrainOutcome: Equatable, Sendable {
    case finished(JobDrainSummary)
    case failed(CoreFailure)
}

/// Drives the core's job loop from native lifecycle events.
///
/// The core owns every job rule: claiming, leases, retry budgets, backoff and settlement. This type only decides
/// *when* to ask for one drain and lends the core the native effects it needs: the secure provider transport and
/// any `JobCapabilityHandler` (C05 registers transcription here). It never touches the capture path: a drain runs
/// on the core's own thread over its own store connection, so saves stay responsive while jobs run.
///
/// * **One drain at a time.** Launch, foreground, reachability and retry triggers all go through one gate. A
///   trigger that arrives while a drain runs sets a flag and runs exactly one more drain after the first ends, so
///   repeated or overlapping activation cannot start competing drainers. The core refuses a second concurrent
///   drain too.
/// * **Suspension is a checkpoint.** `suspend()` and `terminate()` ask the core to stop at a checkpoint: the
///   request in flight is abandoned and its job is put back without spending retry budget, so the next launch
///   resumes it. A job whose lease is simply abandoned (the process was killed) is recovered by the core when the
///   lease expires.
/// * **Offline does not spend retries, and does not stop on-device work.** Reachability gates only provider
///   (network) work. The service tells the core when the network goes away; the core then puts jobs that would
///   call a provider back without counting a failure and carries on with on-device capabilities and local jobs.
///   A provider request already in flight is cancelled, and one that still reaches the host is answered
///   `cancelled`, which the core also puts back at no cost. Network return starts a drain, which releases the
///   deferred jobs. A native capability running when the network drops is not cancelled.
///
/// The owner of the core's single event handler must forward every event to `handle(_:)`; it returns `true` when
/// the event ended one of this service's drains.
public final class JobRunnerService: @unchecked Sendable {
    /// Operation IDs for drains start here so they cannot collide with the IDs of ordinary core operations.
    public static let firstOperationID: UInt64 = 1 << 62

    private let core: CoreHandle
    private let storePath: String
    private let sender: ProviderRequestSender
    private let capabilityHandlers: [String: JobCapabilityHandler]
    private let retryDelay: TimeInterval

    private let stateLock = NSLock()
    private var registration: JobHostRegistration?
    private var nextOperationID = JobRunnerService.firstOperationID
    private var activeOperationID: UInt64?
    private var rerunRequested = false
    private var isForeground = false
    private var isReachable: Bool
    private var isShutDown = false
    private var retryTask: Task<Void, Never>?
    private var runningSends: [UInt64: Task<Void, Never>] = [:]
    private var runningCapabilities: [UInt64: Task<Void, Never>] = [:]
    private var idleWaiters: [@Sendable () -> Void] = []
    private var startedCount = 0
    private var finishedCount = 0
    private var latestOutcome: JobDrainOutcome?

    private init(
        core: CoreHandle,
        storePath: String,
        sender: ProviderRequestSender,
        capabilityHandlers: [String: JobCapabilityHandler],
        isReachable: Bool,
        retryDelay: TimeInterval
    ) {
        self.core = core
        self.storePath = storePath
        self.sender = sender
        self.capabilityHandlers = capabilityHandlers
        self.isReachable = isReachable
        self.retryDelay = retryDelay
    }

    /// Registers the service as the core's job host. `storePath` is the path `core` was opened with. Pass a
    /// `JobCapabilityHandler` for each job type that runs on the device; with none, only the core's own job types
    /// run. `retryDelay` is how long after a drain that left retryable work the service tries again while it
    /// stays in the foreground. Call `shutDown()` before closing the core.
    public static func attach(
        to core: CoreHandle,
        storePath: String,
        sender: ProviderRequestSender,
        capabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = false,
        retryDelay: TimeInterval = 60
    ) throws -> JobRunnerService {
        var handlers: [String: JobCapabilityHandler] = [:]
        for handler in capabilities {
            handlers[handler.jobType] = handler
        }
        let service = JobRunnerService(
            core: core, storePath: storePath, sender: sender, capabilityHandlers: handlers,
            isReachable: isReachable, retryDelay: retryDelay)
        let registration = try core.registerJobHost(nativeJobTypes: handlers.keys.sorted()) {
            [weak service] requestID, command in
            service?.handleHost(requestID: requestID, command: command)
        }
        service.registration = registration
        do {
            try core.setJobNetworkReachable(isReachable)
        } catch {
            registration.invalidate()
            service.registration = nil
            throw error
        }
        return service
    }

    // MARK: observation

    public var isDraining: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return activeOperationID != nil
    }

    /// Drains this service asked the core to start.
    public var startedDrainCount: Int {
        stateLock.lock()
        defer { stateLock.unlock() }
        return startedCount
    }

    /// Drains whose final event arrived.
    public var finishedDrainCount: Int {
        stateLock.lock()
        defer { stateLock.unlock() }
        return finishedCount
    }

    public var lastOutcome: JobDrainOutcome? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return latestOutcome
    }

    /// Runs `completion` once no drain is running: immediately when none is, otherwise after the running drain's
    /// final event. Used to hold a background-execution grant open while a suspended drain reaches its checkpoint.
    public func whenIdle(_ completion: @escaping @Sendable () -> Void) {
        stateLock.lock()
        if activeOperationID == nil {
            stateLock.unlock()
            completion()
            return
        }
        idleWaiters.append(completion)
        stateLock.unlock()
    }

    // MARK: lifecycle

    /// The app launched, or became active again: drain whatever is ready.
    public func activate() {
        stateLock.lock()
        isForeground = true
        stateLock.unlock()
        requestDrain()
    }

    /// The network changed. Becoming reachable starts a drain. Becoming unreachable abandons the provider requests
    /// in flight without spending retry budget; the drain carries on with on-device capabilities and local jobs.
    public func reachabilityChanged(_ reachable: Bool) {
        stateLock.lock()
        guard !isShutDown else {
            stateLock.unlock()
            return
        }
        isReachable = reachable
        let sends = reachable ? [] : Array(runningSends.values)
        stateLock.unlock()
        try? core.setJobNetworkReachable(reachable)
        if reachable {
            requestDrain()
        } else {
            sends.forEach { $0.cancel() }
        }
    }

    /// The app is entering the background: stop at a checkpoint and start nothing until `activate()`.
    public func suspend() {
        stateLock.lock()
        isForeground = false
        rerunRequested = false
        retryTask?.cancel()
        retryTask = nil
        stateLock.unlock()
        cancelRunningDrain()
    }

    /// The app is about to be terminated: same checkpoint as `suspend()`.
    public func terminate() {
        suspend()
    }

    /// Stops the drain, ends every native request and unregisters from the core. Idempotent. Must not be called
    /// from a core callback.
    public func shutDown() {
        stateLock.lock()
        guard !isShutDown else {
            stateLock.unlock()
            return
        }
        isShutDown = true
        isForeground = false
        rerunRequested = false
        retryTask?.cancel()
        retryTask = nil
        let tasks = Array(runningSends.values) + Array(runningCapabilities.values)
        let activeRegistration = registration
        registration = nil
        stateLock.unlock()

        cancelRunningDrain()
        tasks.forEach { $0.cancel() }
        activeRegistration?.invalidate()
    }

    // MARK: events

    /// Applies a core event. Returns `true` when it ended one of this service's drains (or was a stale event of
    /// one), in which case the caller should not process it further.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        guard event.operationID >= Self.firstOperationID else { return false }

        stateLock.lock()
        guard event.operationID == activeOperationID else {
            stateLock.unlock()
            return true
        }
        activeOperationID = nil
        finishedCount += 1
        let outcome = Self.outcome(of: event)
        latestOutcome = outcome
        let rerun = wantsAnotherDrain(after: outcome) && canDrainLocked
        if !rerun, canDrainLocked, hasRetryableWork(outcome) {
            scheduleRetryLocked()
        }
        let waiters = idleWaiters
        idleWaiters = []
        stateLock.unlock()

        waiters.forEach { $0() }
        if rerun {
            requestDrain()
        }
        return true
    }

    private static func outcome(of event: CoreEvent) -> JobDrainOutcome {
        switch event.outcome {
        case .failure(let failure):
            return .failed(failure)
        case .success(let bytes):
            guard let summary = try? JSONDecoder().decode(JobDrainSummary.self, from: bytes) else {
                return .failed(
                    CoreFailure(errorClass: .permanent, code: "unreadable_summary", message: "the drain summary was unreadable"))
            }
            return .finished(summary)
        }
    }

    /// A trigger arrived during the drain, the drain stopped only because it reached its bounds, or a provider
    /// request was abandoned because the network dropped (the core ends the drain at that job; the jobs queued
    /// behind it, such as on-device ones, still need their turn and no longer wait for the network).
    private func wantsAnotherDrain(after outcome: JobDrainOutcome) -> Bool {
        if rerunRequested { return true }
        guard case .finished(let summary) = outcome else { return false }
        if summary.stop == "interrupted" {
            return !isReachable && summary.jobs.last?.jobType == Self.providerJobType
        }
        return summary.stop == "job_limit" || summary.stop == "time_budget"
    }

    /// Whether the drain left work that a timer should retry. A job the core deferred as
    /// `capability_unavailable` is left out: the core already holds it back for a long while, and nothing changes
    /// until a handler is registered. While offline, provider jobs are left out too: they wait for the network, and
    /// network return starts a drain.
    private func hasRetryableWork(_ outcome: JobDrainOutcome) -> Bool {
        guard case .finished(let summary) = outcome else { return false }
        if summary.stop == "claim_failed" { return true }
        return summary.jobs.contains { job in
            if !isReachable && job.jobType == Self.providerJobType { return false }
            return job.result == "retry_scheduled" || job.settlement == "backed_off"
        }
    }

    // MARK: draining

    /// The core's own job type for provider interpretation, the only work that needs the network.
    private static let providerJobType = "interpret"

    private var canDrainLocked: Bool {
        isForeground && !isShutDown
    }

    private func requestDrain() {
        stateLock.lock()
        guard canDrainLocked else {
            stateLock.unlock()
            return
        }
        if activeOperationID != nil {
            rerunRequested = true
            stateLock.unlock()
            return
        }
        retryTask?.cancel()
        retryTask = nil
        let operationID = nextOperationID
        nextOperationID += 1
        activeOperationID = operationID
        rerunRequested = false
        startedCount += 1
        stateLock.unlock()

        do {
            try core.startJobDrain(operationID: operationID, storePath: storePath)
            // A suspend or network loss that arrived before the core knew of this drain had nothing to cancel.
            stateLock.lock()
            let stillAllowed = canDrainLocked
            stateLock.unlock()
            if !stillAllowed {
                try? core.cancelJobDrain()
            }
        } catch {
            let failure = (error as? CoreFailure) ?? CoreFailure.handleClosed
            stateLock.lock()
            if activeOperationID == operationID {
                activeOperationID = nil
            }
            latestOutcome = .failed(failure)
            let waiters = idleWaiters
            idleWaiters = []
            stateLock.unlock()
            waiters.forEach { $0() }
        }
    }

    private func cancelRunningDrain() {
        stateLock.lock()
        let isRunning = activeOperationID != nil
        stateLock.unlock()
        if isRunning {
            try? core.cancelJobDrain()
        }
    }

    private func scheduleRetryLocked() {
        retryTask?.cancel()
        guard retryDelay >= 0 else { return }
        let delayNanoseconds = UInt64(retryDelay * 1_000_000_000)
        retryTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: delayNanoseconds)
            guard !Task.isCancelled else { return }
            self?.requestDrain()
        }
    }

    // MARK: native effects

    private func handleHost(requestID: UInt64, command: JobHostCommand) {
        switch command {
        case .send(let description):
            startSend(requestID: requestID, description: description)
        case .cancelExchange:
            stateLock.lock()
            let send = runningSends[requestID]
            stateLock.unlock()
            send?.cancel()
        case .runCapability(let run):
            startCapability(requestID: requestID, run: run)
        case .cancelCapability:
            stateLock.lock()
            let capability = runningCapabilities[requestID]
            stateLock.unlock()
            capability?.cancel()
        }
    }

    private func startSend(requestID: UInt64, description: ProviderSendCommand) {
        guard let request = Self.makeRequest(description) else {
            failExchange(requestID: requestID, error: "rejected")
            return
        }
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !isShutDown else {
            failExchange(requestID: requestID, error: "cancelled")
            return
        }
        guard isReachable else {
            failExchange(requestID: requestID, error: "cancelled")
            return
        }
        // The task's cleanup takes the lock, so it cannot run before the task is recorded.
        runningSends[requestID] = Task { [self] in
            await perform(request, requestID: requestID)
            endSend(requestID: requestID)
        }
    }

    private func endSend(requestID: UInt64) {
        stateLock.lock()
        runningSends[requestID] = nil
        stateLock.unlock()
    }

    private func perform(_ request: ProviderHTTPRequest, requestID: UInt64) async {
        do {
            let response = try await sender.send(request)
            try? core.completeJobExchange(
                requestID: requestID, status: response.status, headers: response.headers, body: response.body)
        } catch let error as ProviderTransportError {
            failExchange(requestID: requestID, error: error.coreTransportError)
        } catch {
            failExchange(requestID: requestID, error: "rejected")
        }
    }

    private func failExchange(requestID: UInt64, error: String) {
        try? core.failJobExchange(requestID: requestID, error: error)
    }

    private func startCapability(requestID: UInt64, run: JobCapabilityRun) {
        guard let handler = capabilityHandlers[run.jobType] else {
            finishCapability(requestID: requestID, result: .interrupted)
            return
        }
        stateLock.lock()
        defer { stateLock.unlock() }
        guard !isShutDown else {
            finishCapability(requestID: requestID, result: .interrupted)
            return
        }
        runningCapabilities[requestID] = Task { [self] in
            let result = await handler.run(run)
            finishCapability(requestID: requestID, result: Task.isCancelled ? .interrupted : result)
            endCapability(requestID: requestID)
        }
    }

    private func endCapability(requestID: UInt64) {
        stateLock.lock()
        runningCapabilities[requestID] = nil
        stateLock.unlock()
    }

    private func finishCapability(requestID: UInt64, result: JobCapabilityResult) {
        let outcome: JobCapabilityOutcome
        var reason = ""
        switch result {
        case .settled:
            outcome = .settled
        case .transientFailure(let label):
            outcome = .transient
            reason = label
        case .permanentFailure(let label):
            outcome = .permanent
            reason = label
        case .interrupted:
            outcome = .interrupted
        }
        try? core.finishJobCapability(requestID: requestID, outcome: outcome, reason: reason)
    }

    private static func makeRequest(_ description: ProviderSendCommand) -> ProviderHTTPRequest? {
        guard let url = URL(string: description.url),
            let method = ProviderHTTPMethod(rawValue: description.method),
            description.timeoutMilliseconds > 0
        else { return nil }
        var headers: [String: String] = [:]
        for pair in description.headers {
            guard pair.count == 2 else { return nil }
            headers[pair[0]] = pair[1]
        }
        let credential = description.credential.map {
            ProviderCredentialAttachment(reference: $0.reference, headerName: $0.header, scheme: $0.scheme)
        }
        return ProviderHTTPRequest(
            url: url,
            method: method,
            headers: headers,
            body: Data(description.body.utf8),
            timeout: TimeInterval(description.timeoutMilliseconds) / 1000,
            maxResponseBytes: description.maxResponseBytes,
            credential: credential,
            authorization: ProviderTransportAuthorization(
                jobID: String(description.operationID),
                capability: .textInterpretation,
                authorizedOrigins: description.authorizedOrigins))
    }
}
