import Foundation
import OhAndCoreBridge

/// The budget and sampling limits of shadow review, passed to the core with every call. The core validates them,
/// owns the accounting, and treats a disabled policy as a hard stop. The default is off.
public struct ShadowReviewPolicy: Equatable, Sendable {
    public var isEnabled: Bool
    public var samplePerMille: Int
    public var windowSeconds: Int
    public var maxRequestsPerWindow: Int
    public var maxAttemptsPerSample: Int

    public init(
        isEnabled: Bool,
        samplePerMille: Int,
        windowSeconds: Int,
        maxRequestsPerWindow: Int,
        maxAttemptsPerSample: Int
    ) {
        self.isEnabled = isEnabled
        self.samplePerMille = samplePerMille
        self.windowSeconds = windowSeconds
        self.maxRequestsPerWindow = maxRequestsPerWindow
        self.maxAttemptsPerSample = maxAttemptsPerSample
    }

    public static let disabled = ShadowReviewPolicy(
        isEnabled: false, samplePerMille: 0, windowSeconds: 3600, maxRequestsPerWindow: 0, maxAttemptsPerSample: 1)

    var wire: WirePolicy {
        WirePolicy(
            enabled: isEnabled, samplePerMille: samplePerMille, windowSeconds: windowSeconds,
            maxRequestsPerWindow: maxRequestsPerWindow, maxAttemptsPerSample: maxAttemptsPerSample)
    }
}

struct WirePolicy: Encodable {
    let enabled: Bool
    let samplePerMille: Int
    let windowSeconds: Int
    let maxRequestsPerWindow: Int
    let maxAttemptsPerSample: Int

    enum CodingKeys: String, CodingKey {
        case enabled
        case samplePerMille = "sample_per_mille"
        case windowSeconds = "window_seconds"
        case maxRequestsPerWindow = "max_requests_per_window"
        case maxAttemptsPerSample = "max_attempts_per_sample"
    }
}

/// A fixed-vocabulary signal about shadow review. It carries job identifiers and core-defined codes only: never
/// capture text, provider output, routes, credential references or destinations.
public enum ShadowReviewInstrumentationEvent: Equatable, Sendable {
    case skipped(code: String)
    case selected(jobID: String)
    case dispatched(jobID: String)
    case denied(jobID: String, code: String)
    case reviewed(jobID: String, verdict: ShadowReviewVerdict?, differences: [String])
    case unreviewed(jobID: String, reason: String?)
    case failed(code: String)
}

/// Optional observer of shadow review. Shadow review works identically without one.
public protocol ShadowReviewInstrumentation: AnyObject {
    func shadowReview(_ event: ShadowReviewInstrumentationEvent)
}

/// How shadow review is set up. `reviewProfileVersion` names the separately approved review profile; with none,
/// or with the default disabled policy, no case is ever selected and nothing is sent. The core still decides
/// every individual case: it requires the route's `review` grant for the profile's destinations, enforces the
/// budget, and never lets the verdict change authoritative state.
public struct ShadowReviewConfiguration {
    public var reviewProfileVersion: String?
    public var policy: ShadowReviewPolicy
    public weak var instrumentation: ShadowReviewInstrumentation?

    public init(
        reviewProfileVersion: String? = nil,
        policy: ShadowReviewPolicy = .disabled,
        instrumentation: ShadowReviewInstrumentation? = nil
    ) {
        self.reviewProfileVersion = reviewProfileVersion
        self.policy = policy
        self.instrumentation = instrumentation
    }

    public static let off = ShadowReviewConfiguration()
}

public enum ShadowReviewOutcome: String, Decodable, Equatable, Sendable {
    case pending
    case reviewed
    case unreviewed
    case error
}

public enum ShadowReviewVerdict: String, Decodable, Equatable, Sendable {
    case agreement
    case disagreement
}

/// The stored, redacted state of one shadow case. `differences` names the facets (`item_type`,
/// `reminder_presence`, `reminder_time`, `session_topic`, `abstention`) on which the review disagreed.
public struct ShadowReviewRecord: Decodable, Equatable, Sendable {
    public let jobID: String
    public let itemID: String
    public let sourceRevision: Int
    public let requestVersion: String
    public let reviewProfileVersion: String
    public let attemptsUsed: Int
    public let outcome: ShadowReviewOutcome
    public let reason: String?
    public let lastFailure: String?
    public let verdict: ShadowReviewVerdict?
    public let differences: [String]

    enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case itemID = "item_id"
        case sourceRevision = "source_revision"
        case requestVersion = "request_version"
        case reviewProfileVersion = "review_profile_version"
        case attemptsUsed = "attempts_used"
        case outcome
        case reason
        case lastFailure = "last_failure"
        case verdict
        case differences
    }
}

public struct ShadowReviewSkip: Equatable, Sendable {
    public let code: String
    public let denial: String?

    public static let notConfigured = ShadowReviewSkip(code: "not_configured", denial: nil)
}

public enum ShadowReviewSelectionResult: Equatable, Sendable {
    case selected(ShadowReviewRecord)
    case alreadySelected(ShadowReviewRecord)
    case skipped(ShadowReviewSkip)
    case failed(code: String)
}

public enum ShadowReviewRunResult: Equatable, Sendable {
    /// The case ended this run in `record`. `denial` is set when the core refused to send anything.
    case completed(ShadowReviewRecord, denial: String?)
    /// The core refused the run before touching the case (unknown case, not leasable, no transport).
    case failed(code: String)
}

public enum ShadowReviewReadResult: Equatable, Sendable {
    case found(ShadowReviewRecord)
    case failed(code: String)
}

struct ShadowSelectionPayload: Decodable {
    let selection: String
    let skip: String?
    let denial: String?
    let record: ShadowReviewRecord?
}

struct ShadowRunPayload: Decodable {
    let phase: ProviderExchangePhase
    let denial: String?
    let record: ShadowReviewRecord?
}

struct ShadowRecordPayload: Decodable {
    let record: ShadowReviewRecord
}
