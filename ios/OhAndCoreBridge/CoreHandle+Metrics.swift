import Foundation
import OhandCoreC

/// How the capture surface was reached for a trigger-to-ready measurement. Raw values are the core's labels.
public enum CoreMetricsLaunchKind: String, Codable, Equatable, Sendable {
    case cold
    case warm
    case lockedHandoff = "locked_handoff"
}

/// How a capture was entered, for an end-of-input-to-save measurement. Raw values are the core's labels.
public enum CoreMetricsCaptureKind: String, Codable, Equatable, Sendable {
    case voice
    case text
}

/// Why background processing failed, as a machine label only. Raw values are the core's labels.
public enum CoreMetricsProcessingFailure: String, Codable, Equatable, Sendable {
    case timeout
    case unavailable
    case rateLimited = "rate_limited"
    case unauthorized
    case invalidOutput = "invalid_output"
    case capabilityUnavailable = "capability_unavailable"
    case unsupportedLanguage = "unsupported_language"
    case interrupted
    case retriesExhausted = "retries_exhausted"
    case other
}

/// Why a reminder could not be scheduled, as a machine label only. Raw values are the core's labels.
public enum CoreMetricsSchedulingFailure: String, Codable, Equatable, Sendable {
    case permissionDenied = "permission_denied"
    case capacityExceeded = "capacity_exceeded"
    case unschedulable
    case nativeFailure = "native_failure"
    case other
}

/// A latency series. The two cases are separate clocks and are never combined.
public enum CoreMetricsTimingSeries: Equatable, Sendable {
    /// From the user's trigger until the capture surface can take input.
    case triggerToReady(CoreMetricsLaunchKind)
    /// From the user finishing input until the durable acknowledgment is shown.
    case endOfInputToSave(CoreMetricsCaptureKind)

    var wireSeries: String {
        switch self {
        case .triggerToReady: return "trigger_to_ready"
        case .endOfInputToSave: return "end_of_input_to_save"
        }
    }

    var wireDimension: String {
        switch self {
        case .triggerToReady(let launch): return launch.rawValue
        case .endOfInputToSave(let capture): return capture.rawValue
        }
    }
}

/// A failure counter. It counts what the system observed; it says nothing about whether anyone saw it.
public enum CoreMetricsFailureSeries: Equatable, Sendable {
    case processing(CoreMetricsProcessingFailure)
    case scheduling(CoreMetricsSchedulingFailure)

    var wireSeries: String {
        switch self {
        case .processing: return "processing_failure"
        case .scheduling: return "scheduling_failure"
        }
    }

    var wireDimension: String {
        switch self {
        case .processing(let reason): return reason.rawValue
        case .scheduling(let reason): return reason.rawValue
        }
    }
}

/// One metrics operation for `ohand_core_start_metrics`. There is no free-text case: every request is built from the
/// closed vocabulary above, so no caller can put captured content into a metric.
public enum CoreMetricsRequest: Equatable, Sendable {
    case recordTiming(CoreMetricsTimingSeries, durationMilliseconds: UInt64)
    case recordFailure(CoreMetricsFailureSeries)
    case setEnabled(Bool)
    case clear
    case export(readAuthentication: CoreExportReadAuthentication)

    private struct Wire: Encodable {
        var op: String
        var series: String?
        var dimension: String?
        var durationMilliseconds: UInt64?
        var enabled: Bool?
        var readAuthentication: String?

        enum CodingKeys: String, CodingKey {
            case op
            case series
            case dimension
            case durationMilliseconds = "duration_ms"
            case enabled
            case readAuthentication = "read_authentication"
        }
    }

    func encoded() throws -> Data {
        var wire: Wire
        switch self {
        case .recordTiming(let series, let durationMilliseconds):
            wire = Wire(op: "record_timing")
            wire.series = series.wireSeries
            wire.dimension = series.wireDimension
            wire.durationMilliseconds = durationMilliseconds
        case .recordFailure(let series):
            wire = Wire(op: "record_failure")
            wire.series = series.wireSeries
            wire.dimension = series.wireDimension
        case .setEnabled(let enabled):
            wire = Wire(op: "set_enabled")
            wire.enabled = enabled
        case .clear:
            wire = Wire(op: "clear")
        case .export(let readAuthentication):
            wire = Wire(op: "export")
            wire.readAuthentication = readAuthentication.rawValue
        }
        return try JSONEncoder().encode(wire)
    }
}

/// Distribution of the retained samples of one timing series.
public struct CoreMetricsTimingSummary: Codable, Equatable, Sendable {
    public let series: String
    public let dimension: String
    public let retainedSamples: Int
    public let minMilliseconds: Int
    public let medianMilliseconds: Int
    public let p95Milliseconds: Int
    public let maxMilliseconds: Int

    public init(
        series: String, dimension: String, retainedSamples: Int, minMilliseconds: Int, medianMilliseconds: Int,
        p95Milliseconds: Int, maxMilliseconds: Int
    ) {
        self.series = series
        self.dimension = dimension
        self.retainedSamples = retainedSamples
        self.minMilliseconds = minMilliseconds
        self.medianMilliseconds = medianMilliseconds
        self.p95Milliseconds = p95Milliseconds
        self.maxMilliseconds = maxMilliseconds
    }

    enum CodingKeys: String, CodingKey {
        case series
        case dimension
        case retainedSamples = "retained_samples"
        case minMilliseconds = "min_ms"
        case medianMilliseconds = "median_ms"
        case p95Milliseconds = "p95_ms"
        case maxMilliseconds = "max_ms"
    }
}

/// How often the system observed one kind of failure. Not a count of failures anyone saw or handled.
public struct CoreMetricsFailureSummary: Codable, Equatable, Sendable {
    public let series: String
    public let dimension: String
    public let occurrences: Int

    public init(series: String, dimension: String, occurrences: Int) {
        self.series = series
        self.dimension = dimension
        self.occurrences = occurrences
    }
}

/// The core's aggregate report: labels, counts and millisecond figures only.
public struct CoreMetricsReport: Codable, Equatable, Sendable {
    public let schema: String
    public let generatedAt: String
    public let recordingEnabled: Bool
    public let sampleLimitPerSeries: Int
    public let timings: [CoreMetricsTimingSummary]
    public let failures: [CoreMetricsFailureSummary]

    public init(
        schema: String, generatedAt: String, recordingEnabled: Bool, sampleLimitPerSeries: Int,
        timings: [CoreMetricsTimingSummary], failures: [CoreMetricsFailureSummary]
    ) {
        self.schema = schema
        self.generatedAt = generatedAt
        self.recordingEnabled = recordingEnabled
        self.sampleLimitPerSeries = sampleLimitPerSeries
        self.timings = timings
        self.failures = failures
    }

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedAt = "generated_at"
        case recordingEnabled = "recording_enabled"
        case sampleLimitPerSeries = "sample_limit_per_series"
        case timings
        case failures
    }
}

/// The outcome event of a metrics operation.
public struct CoreMetricsAnswer: Decodable, Equatable, Sendable {
    /// `recorded`, `disabled` (recording is off and nothing was stored), `ok` or `report`.
    public let outcome: String
    /// Present only for an export.
    public let report: CoreMetricsReport?

    public init(outcome: String, report: CoreMetricsReport? = nil) {
        self.outcome = outcome
        self.report = report
    }
}

extension CoreHandle {
    /// Queues one metrics operation. The outcome event decodes as `CoreMetricsAnswer`, or is a failure:
    /// `metrics_export_not_authorized` for a capture-only export (nothing was read) and `invalid_request` for an
    /// implausible duration. Throws when the request could not be queued, in which case nothing ran.
    public func startMetrics(operationID: UInt64, request: CoreMetricsRequest) throws {
        let requestBytes = Array(try request.encoded())
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_metrics(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
