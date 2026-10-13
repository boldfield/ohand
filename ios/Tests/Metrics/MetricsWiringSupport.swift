import Foundation
@testable import OhAndCoreBridge

/// A metrics sink that keeps what the production flows reported, so a test can observe the labels and durations.
final class RecordingMetrics: MetricsRecording, @unchecked Sendable {
    enum Record: Equatable {
        case triggerToReady(CoreMetricsLaunchKind, milliseconds: Int)
        case endOfInputToSave(CoreMetricsCaptureKind, milliseconds: Int)
        case processingFailure(CoreMetricsProcessingFailure)
        case schedulingFailure(CoreMetricsSchedulingFailure)
    }

    private let lock = NSLock()
    private var stored: [Record] = []

    var records: [Record] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    private func append(_ record: Record) {
        lock.lock()
        stored.append(record)
        lock.unlock()
    }

    func recordTriggerToReady(_ launch: CoreMetricsLaunchKind, elapsed: TimeInterval) {
        append(.triggerToReady(launch, milliseconds: Int((elapsed * 1000).rounded())))
    }

    func recordEndOfInputToSave(_ capture: CoreMetricsCaptureKind, elapsed: TimeInterval) {
        append(.endOfInputToSave(capture, milliseconds: Int((elapsed * 1000).rounded())))
    }

    func recordProcessingFailure(_ reason: CoreMetricsProcessingFailure) {
        append(.processingFailure(reason))
    }

    func recordSchedulingFailure(_ reason: CoreMetricsSchedulingFailure) {
        append(.schedulingFailure(reason))
    }
}

/// A monotonic uptime clock the test advances.
final class TestUptimeClock: @unchecked Sendable {
    private let lock = NSLock()
    private var nanoseconds: UInt64 = 5_000_000_000

    func advance(milliseconds: UInt64) {
        lock.lock()
        nanoseconds += milliseconds * 1_000_000
        lock.unlock()
    }

    func now() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return nanoseconds
    }
}
