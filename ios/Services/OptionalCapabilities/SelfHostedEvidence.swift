import Foundation

/// The kinds of artifact that bound what the self-hosted capability may claim.
public enum SelfHostedEvidenceKind: String, CaseIterable, Sendable {
    /// V08a: the endpoint probed from the worker context.
    case workerProbe = "worker_probe"
    /// V09: the adapter's own request answered by the endpoint.
    case adapterSmoke = "adapter_smoke"
    /// V08b: reachability from the phone context.
    case phoneReachability = "phone_reachability"
}

/// A named, sanitized evidence artifact. `artifactPath` is relative to the repository root.
public struct SelfHostedEvidenceReference: Equatable, Sendable {
    public let kind: SelfHostedEvidenceKind
    public let evidenceID: String
    public let artifactPath: String
    public let collectedAt: String

    public init(kind: SelfHostedEvidenceKind, evidenceID: String, artifactPath: String, collectedAt: String) {
        self.kind = kind
        self.evidenceID = evidenceID
        self.artifactPath = artifactPath
        self.collectedAt = collectedAt
    }
}

/// What the V08/V09 evidence establishes for the self-hosted provider: one model, with the artifacts that say so. The
/// values mirror the constants of `core/src/providers/self_hosted/mod.rs`; a test compares them with that file so the
/// two cannot drift apart.
public struct SelfHostedVerifiedBoundary: Equatable, Sendable {
    public let model: String
    public let evidence: [SelfHostedEvidenceReference]

    public static let current = SelfHostedVerifiedBoundary(
        model: "deepseek-flash-iq3:latest",
        evidence: [
            SelfHostedEvidenceReference(
                kind: .workerProbe,
                evidenceID: "spark-probe-20261008T093909Z-22c47772",
                artifactPath: "docs/validation/evidence/spark-probe/spark-probe-20261008T093909Z-22c47772.json",
                collectedAt: "2026-10-08T09:39:09Z"),
            SelfHostedEvidenceReference(
                kind: .adapterSmoke,
                evidenceID: "self-hosted-adapter-smoke-20261009T145738Z-e45f02b4",
                artifactPath:
                    "docs/validation/evidence/spark-adapter-smoke/"
                    + "self-hosted-adapter-smoke-20261009T145738Z-e45f02b4.json",
                collectedAt: "2026-10-09T14:57:38Z"),
            SelfHostedEvidenceReference(
                kind: .phoneReachability,
                evidenceID: "spark-phone-2026-10-08",
                artifactPath: "docs/validation/evidence/spark-phone/2026-10-08-phone-reachability.json",
                collectedAt: "2026-10-08T16:39:00Z"),
        ])
}
