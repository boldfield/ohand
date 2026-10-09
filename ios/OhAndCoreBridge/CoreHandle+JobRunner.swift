import Foundation
import OhandCoreC

/// One job the core asks native code to run itself, for a job type the host declared at registration.
public struct JobCapabilityRun: Decodable, Equatable, Sendable {
    public let jobID: String
    public let jobType: String
    /// The job's lease token. A result applied through the core must name it, so a stale result is rejected.
    public let attempt: Int

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case jobType = "job_type"
        case attempt
    }

    public init(jobID: String, jobType: String, attempt: Int) {
        self.jobID = jobID
        self.jobType = jobType
        self.attempt = attempt
    }
}

/// A command from a running job drain to native code, delivered on the drain's own thread.
public enum JobHostCommand: Equatable, Sendable {
    /// Perform the provider HTTP request and answer exactly once with `completeJobExchange` or `failJobExchange`.
    case send(ProviderSendCommand)
    /// Abandon the named exchange; the drain no longer waits for it.
    case cancelExchange
    /// Run the job and answer exactly once with `finishJobCapability`.
    case runCapability(JobCapabilityRun)
    /// Stop the named capability run; the drain no longer waits for it.
    case cancelCapability
}

/// How a native capability ended. `settled` means the host already settled the job through the core.
public enum JobCapabilityOutcome: UInt32, Sendable {
    case settled = 0
    case transient = 1
    case permanent = 2
    case interrupted = 3
}

/// The content-free account of one job in a drain.
public struct JobDrainJob: Decodable, Equatable, Sendable {
    public let jobID: String
    public let jobType: String
    public let result: String
    public let settlement: String?

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case jobType = "job_type"
        case result
        case settlement
    }
}

/// The final event of a drain, decoded from the outcome of the drain's operation.
public struct JobDrainSummary: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    /// `idle`, `job_limit`, `time_budget`, `cancelled`, `interrupted` or `claim_failed`.
    public let stop: String
    public let jobs: [JobDrainJob]

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case stop
        case jobs
    }
}

/// Keeps one native job host registered with a core. The handler runs on a drain thread and must return promptly
/// (start the work and return). `invalidate()` waits for a running handler invocation, so it must not be called
/// from inside the handler.
public final class JobHostRegistration: @unchecked Sendable {
    private let handleIdentifier: OhandCoreHandle
    private let handler: @Sendable (UInt64, JobHostCommand) -> Void
    private let stateLock = NSLock()
    private var isInvalidated = false

    fileprivate init(handleIdentifier: OhandCoreHandle, handler: @escaping @Sendable (UInt64, JobHostCommand) -> Void) {
        self.handleIdentifier = handleIdentifier
        self.handler = handler
    }

    fileprivate func receive(requestID: UInt64, command: UInt32, data: Data) {
        switch command {
        case UInt32(OHAND_JOB_HOST_COMMAND_SEND):
            guard let send = try? JSONDecoder().decode(ProviderSendCommand.self, from: data) else {
                let name = Array("rejected".utf8)
                _ = try? consumeCoreResult(
                    name.withUnsafeBufferPointer {
                        ohand_core_fail_job_exchange(handleIdentifier, requestID, $0.baseAddress, $0.count)
                    })
                return
            }
            handler(requestID, .send(send))
        case UInt32(OHAND_JOB_HOST_COMMAND_CANCEL_EXCHANGE):
            handler(requestID, .cancelExchange)
        case UInt32(OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY):
            guard let run = try? JSONDecoder().decode(JobCapabilityRun.self, from: data) else {
                _ = try? consumeCoreResult(
                    ohand_core_finish_job_capability(
                        handleIdentifier, requestID, JobCapabilityOutcome.interrupted.rawValue, nil, 0))
                return
            }
            handler(requestID, .runCapability(run))
        case UInt32(OHAND_JOB_HOST_COMMAND_CANCEL_CAPABILITY):
            handler(requestID, .cancelCapability)
        default:
            break
        }
    }

    /// Unregisters the host, cancelling the handle's running drain, and releases the registration's retained
    /// context. Idempotent. After it returns the handler is never invoked again. A registration that a later
    /// `registerJobHost` replaced only releases itself and leaves the replacement active.
    public func invalidate() {
        stateLock.lock()
        guard !isInvalidated else {
            stateLock.unlock()
            return
        }
        isInvalidated = true
        stateLock.unlock()

        let ownContext = Unmanaged.passUnretained(self).toOpaque()
        _ = try? consumeCoreResult(ohand_core_set_job_host(handleIdentifier, nil, ownContext, nil, 0))
        Unmanaged.passUnretained(self).release()
    }
}

/// Runs on a drain thread. The context is a `JobHostRegistration` retained once on creation and released by
/// `invalidate()`, which first waits for any running invocation.
private let jobHostTrampoline: OhandJobHostCallback = { context, requestID, command, data, length in
    guard let context else { return }
    let registration = Unmanaged<JobHostRegistration>.fromOpaque(context).takeUnretainedValue()
    var bytes = Data()
    if let data, length > 0 {
        bytes = Data(bytes: data, count: length)
    }
    registration.receive(requestID: requestID, command: command, data: bytes)
}

extension CoreHandle {
    /// Registers the native host that job drains use for provider requests and for the job types in
    /// `nativeJobTypes` (lowercase names such as `transcribe`; `interpret` belongs to the core), replacing any
    /// earlier host. Keep the returned registration until the host is no longer needed, then call `invalidate()`
    /// before closing.
    public func registerJobHost(
        nativeJobTypes: [String],
        handler: @escaping @Sendable (UInt64, JobHostCommand) -> Void
    ) throws -> JobHostRegistration {
        let registration = JobHostRegistration(handleIdentifier: handleIdentifier, handler: handler)
        let context = Unmanaged.passRetained(registration).toOpaque()
        let typeBytes = nativeJobTypes.isEmpty ? [] : Array(try JSONEncoder().encode(nativeJobTypes))
        let rawResult = typeBytes.withUnsafeBufferPointer { buffer in
            ohand_core_set_job_host(handleIdentifier, jobHostTrampoline, context, buffer.baseAddress, buffer.count)
        }
        do {
            _ = try consumeCoreResult(rawResult)
        } catch {
            Unmanaged<JobHostRegistration>.fromOpaque(context).release()
            throw error
        }
        return registration
    }

    /// Starts one drain of the ready jobs on the core's own thread and returns at once. `storePath` is the path
    /// this core was opened with (the core does not verify it): the drain uses its own connection, so capture
    /// saves are never blocked. The final
    /// event for `operationID` decodes as `JobDrainSummary`. Refused with `drain_in_progress` while a drain of
    /// this handle runs, and with `host_not_registered` before `registerJobHost`.
    public func startJobDrain(operationID: UInt64, storePath: String) throws {
        let pathBytes = Array(storePath.utf8)
        let rawResult = pathBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_job_drain(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Tells the core whether provider requests can reach the network. While they cannot, the core defers jobs
    /// that would call a provider, without spending retry budget and without ending the drain, so on-device
    /// capabilities and local jobs keep running. A drain that starts with a network releases the deferred jobs.
    /// Applies to the current registration (`host_not_registered` without one), which starts out reachable.
    public func setJobNetworkReachable(_ reachable: Bool) throws {
        _ = try consumeCoreResult(ohand_core_set_job_network_reachable(handleIdentifier, reachable ? 1 : 0))
    }

    /// Stops the running drain at a checkpoint: a request in flight is abandoned and its job put back without
    /// spending retry budget. Idempotent; with no drain running it does nothing.
    public func cancelJobDrain() throws {
        _ = try consumeCoreResult(ohand_core_cancel_job_drain(handleIdentifier))
    }

    /// Answers a send with a completed HTTP exchange of any status. `not_found` means the request already ended.
    public func completeJobExchange(requestID: UInt64, status: Int, headers: [String: String], body: Data) throws {
        let headerPairs = headers.sorted { $0.key < $1.key }.map { [$0.key, $0.value] }
        let headerBytes = Array(try JSONEncoder().encode(headerPairs))
        let bodyBytes = Array(body)
        let rawResult = headerBytes.withUnsafeBufferPointer { headerBuffer in
            bodyBytes.withUnsafeBufferPointer { bodyBuffer in
                ohand_core_complete_job_exchange(
                    handleIdentifier,
                    requestID,
                    UInt32(clamping: status),
                    headerBuffer.baseAddress,
                    headerBuffer.count,
                    bodyBuffer.baseAddress,
                    bodyBuffer.count
                )
            }
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Answers a send with a native failure named by the core transport error (`timeout`, `cancelled`,
    /// `unavailable`, `unauthorized`, `rejected`, `invalid_output`).
    public func failJobExchange(requestID: UInt64, error: String) throws {
        let errorBytes = Array(error.utf8)
        let rawResult = errorBytes.withUnsafeBufferPointer { buffer in
            ohand_core_fail_job_exchange(handleIdentifier, requestID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Answers a capability run. `reason` is a short lowercase label (`[a-z0-9_]`) the core stores on the job for
    /// a failure; it must never carry captured content.
    public func finishJobCapability(requestID: UInt64, outcome: JobCapabilityOutcome, reason: String = "") throws {
        let reasonBytes = Array(reason.utf8)
        let rawResult = reasonBytes.withUnsafeBufferPointer { buffer in
            ohand_core_finish_job_capability(handleIdentifier, requestID, outcome.rawValue, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
