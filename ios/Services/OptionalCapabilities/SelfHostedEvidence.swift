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

/// One model the self-hosted provider is verified for, with the adapter semantic smoke artifact that says so. Another
/// model's artifact does not verify it.
public struct SelfHostedVerifiedModel: Equatable, Sendable {
    public let model: String
    public let adapterSmoke: SelfHostedEvidenceReference

    public init(model: String, adapterSmoke: SelfHostedEvidenceReference) {
        self.model = model
        self.adapterSmoke = adapterSmoke
    }
}

/// What the V08/V09 evidence establishes for the self-hosted provider: one worker-probe reference and one
/// phone-reachability reference shared by every model, and per model its own adapter smoke reference. The values mirror
/// `SMOKE_EVIDENCE` and the constants of `core/src/providers/self_hosted/mod.rs`; a test compares them with that file so
/// the two cannot drift apart.
public struct SelfHostedVerifiedBoundary: Equatable, Sendable {
    public let workerProbe: SelfHostedEvidenceReference
    public let phoneReachability: SelfHostedEvidenceReference
    /// Ordered as `SMOKE_EVIDENCE`. The first is the default verified model.
    public let models: [SelfHostedVerifiedModel]

    /// The default verified model.
    public var model: String { models[0].model }

    /// The references a registration for `model` must cite: the shared ones and, when `model` is verified, its own
    /// adapter smoke reference.
    public func evidence(forModel model: String) -> [SelfHostedEvidenceReference] {
        var references = [workerProbe]
        if let verified = models.first(where: { $0.model == model }) {
            references.append(verified.adapterSmoke)
        }
        references.append(phoneReachability)
        return references
    }

    private static let smokeDirectory = "docs/validation/evidence/spark-adapter-smoke/"

    public static let current = SelfHostedVerifiedBoundary(
        workerProbe: SelfHostedEvidenceReference(
            kind: .workerProbe,
            evidenceID: "spark-probe-20261008T093909Z-22c47772",
            artifactPath: "docs/validation/evidence/spark-probe/spark-probe-20261008T093909Z-22c47772.json",
            collectedAt: "2026-10-08T09:39:09Z"),
        phoneReachability: SelfHostedEvidenceReference(
            kind: .phoneReachability,
            evidenceID: "spark-phone-2026-10-08",
            artifactPath: "docs/validation/evidence/spark-phone/2026-10-08-phone-reachability.json",
            collectedAt: "2026-10-08T16:39:00Z"),
        models: [
            SelfHostedVerifiedModel(
                model: "deepseek-flash-iq3:latest",
                adapterSmoke: SelfHostedEvidenceReference(
                    kind: .adapterSmoke,
                    evidenceID: "self-hosted-adapter-smoke-20261018T235846Z-511d119f",
                    artifactPath: smokeDirectory + "self-hosted-adapter-smoke-20261018T235846Z-511d119f.json",
                    collectedAt: "2026-10-18T23:58:46Z")),
            SelfHostedVerifiedModel(
                model: "qwen3.8:27b",
                adapterSmoke: SelfHostedEvidenceReference(
                    kind: .adapterSmoke,
                    evidenceID: "self-hosted-adapter-smoke-20261019T000049Z-511d119f",
                    artifactPath: smokeDirectory + "self-hosted-adapter-smoke-20261019T000049Z-511d119f.json",
                    collectedAt: "2026-10-19T00:00:49Z")),
        ])
}
