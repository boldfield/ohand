import Foundation
import OhAndCoreBridge

/// Why an explicit metrics export did not produce a report.
enum MetricsExportFailure: Error, Equatable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The session relocked while the core was answering, so the report is not handed over.
    case interrupted
    case core(CoreFailure)
    case unreadable
}

/// A report ready to hand to the user's chosen destination.
struct MetricsExport: Equatable {
    let report: CoreMetricsReport
    /// The report as stable, pretty-printed JSON: aggregates and machine labels only.
    let json: Data
}

/// Measures a span on a monotonic clock, so wall-clock changes cannot distort a latency.
struct MetricsStopwatch {
    private let startedAt: UInt64
    private let clock: @Sendable () -> UInt64

    init(clock: @escaping @Sendable () -> UInt64) {
        self.clock = clock
        self.startedAt = clock()
    }

    /// Time since the stopwatch started, in seconds.
    func elapsed() -> TimeInterval {
        let now = clock()
        return now >= startedAt ? TimeInterval(now - startedAt) / 1_000_000_000 : 0
    }
}

/// Records content-free latency and reliability metrics in the core and exports them on an explicit request.
///
/// * **Two clocks.** Trigger-to-ready (user action until the capture surface can take input) and end-of-input-to-save
///   (user finishes input until the durable acknowledgment is shown) are separate series; neither includes the other,
///   network time or processing.
/// * **Failures are not attention.** `recordProcessingFailure` and `recordSchedulingFailure` count what the system
///   observed. They never claim the user saw, read or handled anything, and this service posts no notification.
/// * **Never in the way of capture.** Recording is fire and forget: it does not wait for the core, throws nothing at
///   the caller and is never retried. A record the core cannot take (store trouble, an implausible duration, a client
///   that was shut down) is counted in `droppedRecordCount` and otherwise forgotten.
/// * **Local only.** This type has no destination. The only way data leaves is `exportReport()`, which needs an
///   authenticated read session, and the caller decides where the returned JSON goes.
/// * **Names the build.** An export carries the build identity the app was given (`CoreMetricsBuild.current()`), so a
///   report says which trial build produced it. Nothing else enters the report.
/// * **Switchable, authenticated.** `setRecordingEnabled` (on or off) and `clearAll()` need an authenticated read
///   session, like the export. A capture-only session gets `false` back and the core is not asked. While recording is
///   off the core stores nothing; `clearAll()` removes what was stored.
final class MetricsService: MetricsRecording, @unchecked Sendable {
    private let core: MetricsCore
    private let session: ReadAuthenticationSession
    private let build: CoreMetricsBuild?
    private let monotonicNanoseconds: @Sendable () -> UInt64
    private let lock = NSLock()
    private var dropped = 0

    init(
        core: MetricsCore,
        session: ReadAuthenticationSession,
        build: CoreMetricsBuild? = nil,
        monotonicNanoseconds: @escaping @Sendable () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }
    ) {
        self.core = core
        self.session = session
        self.build = build
        self.monotonicNanoseconds = monotonicNanoseconds
    }

    /// Records the core did not take or that could not be expressed as a duration.
    var droppedRecordCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return dropped
    }

    func startStopwatch() -> MetricsStopwatch {
        MetricsStopwatch(clock: monotonicNanoseconds)
    }

    // MARK: Recording

    func recordTriggerToReady(_ launch: CoreMetricsLaunchKind, elapsed: TimeInterval) {
        recordTiming(.triggerToReady(launch), elapsed: elapsed)
    }

    func recordEndOfInputToSave(_ capture: CoreMetricsCaptureKind, elapsed: TimeInterval) {
        recordTiming(.endOfInputToSave(capture), elapsed: elapsed)
    }

    func recordProcessingFailure(_ reason: CoreMetricsProcessingFailure) {
        submit(.recordFailure(.processing(reason)))
    }

    func recordSchedulingFailure(_ reason: CoreMetricsSchedulingFailure) {
        submit(.recordFailure(.scheduling(reason)))
    }

    private func recordTiming(_ series: CoreMetricsTimingSeries, elapsed: TimeInterval) {
        guard elapsed.isFinite, elapsed >= 0 else {
            noteDropped()
            return
        }
        let milliseconds = (elapsed * 1000).rounded()
        guard milliseconds < Double(UInt64.max) else {
            noteDropped()
            return
        }
        submit(.recordTiming(series, durationMilliseconds: UInt64(milliseconds)))
    }

    private func submit(_ request: CoreMetricsRequest) {
        core.submit(request) { [weak self] result in
            if case .failure = result { self?.noteDropped() }
        }
    }

    private func noteDropped() {
        lock.lock()
        dropped += 1
        lock.unlock()
    }

    // MARK: Control

    /// Turns recording on or off. Stored figures are kept; use `clearAll()` to remove them. Needs an authenticated
    /// read session. Returns whether the core applied the change; `false` for a capture-only session (the core is not
    /// asked).
    @discardableResult
    func setRecordingEnabled(_ enabled: Bool) async -> Bool {
        await authenticated { .setEnabled(enabled, readAuthentication: .authenticated) }
    }

    /// Removes every stored sample and counter. The enabled setting is unchanged. Needs an authenticated read
    /// session. Returns whether the core applied it, with the same refusal as `setRecordingEnabled`.
    @discardableResult
    func clearAll() async -> Bool {
        await authenticated { .clear(readAuthentication: .authenticated) }
    }

    private func authenticated(_ request: () -> CoreMetricsRequest) async -> Bool {
        guard session.currentAuthorization() != nil else { return false }
        let submission = request()
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<CoreMetricsAnswer, CoreFailure>, Never>) in
            core.submit(submission) { continuation.resume(returning: $0) }
        }
        if case .success = result { return true }
        return false
    }

    // MARK: Export

    /// The explicit, authenticated export. Requires an authenticated read session now; the core re-checks that, and a
    /// relock before the answer arrives discards it.
    func exportReport() async throws -> MetricsExport {
        guard let authorization = session.currentAuthorization() else { throw MetricsExportFailure.notAuthenticated }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<CoreMetricsAnswer, CoreFailure>, Never>) in
            core.submit(.export(readAuthentication: .authenticated, build: build)) { continuation.resume(returning: $0) }
        }
        guard session.isCurrent(authorization) else { throw MetricsExportFailure.interrupted }
        let answer: CoreMetricsAnswer
        switch result {
        case .success(let value): answer = value
        case .failure(let failure): throw MetricsExportFailure.core(failure)
        }
        guard let report = answer.report else { throw MetricsExportFailure.unreadable }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let json = try? encoder.encode(report) else { throw MetricsExportFailure.unreadable }
        return MetricsExport(report: report, json: json)
    }
}
