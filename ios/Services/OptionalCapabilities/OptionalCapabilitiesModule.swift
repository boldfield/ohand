import Foundation
import OhAndCoreBridge

/// Joins the self-hosted provider and shadow review to the production root through the extension seam, without any edit
/// to the root.
///
/// * **Off by default.** With `.none`, or with a registration that fails verification, the module takes no core client
///   and contributes nothing; the base app builds and captures exactly as without it, and the shadow review service does
///   not exist.
/// * **Same rules.** The module has no `CoreHandle`, provider transport, endpoint or credential. Self-hosted jobs run in
///   the core's job runner and shadow review through the core-built, core-authorized exchange, so both follow the same
///   route grant, revocation and credential-reference checks as every other provider. A registration never widens them.
/// * **Diagnostics only.** A shadow verdict is stored by the core as fixed difference codes and cannot change items,
///   reminders or routing.
public final class OptionalCapabilitiesModule: CompositionModule {
    public static let moduleIdentifier = "optional.capabilities"
    /// The module's own operation range; its clients use their fixed ranges.
    public static let moduleFirstOperationID: UInt64 = 1 << 44

    public let identifier = OptionalCapabilitiesModule.moduleIdentifier
    public let firstOperationID = OptionalCapabilitiesModule.moduleFirstOperationID

    public let configuration: OptionalCapabilitiesConfiguration
    public let selfHostedState: OptionalCapabilityState
    public let shadowReviewState: OptionalCapabilityState

    /// Present only while shadow review is enabled and the module is attached to a root.
    public private(set) var shadowReview: ShadowReviewService?

    public init(configuration: OptionalCapabilitiesConfiguration = .none) {
        self.configuration = configuration
        selfHostedState = OptionalCapabilityVerification.selfHostedState(configuration.selfHosted)
        shadowReviewState = OptionalCapabilityVerification.shadowReviewState(
            configuration.shadowReview, selfHosted: configuration.selfHosted)
    }

    /// The verified self-hosted claim, or `nil` while that capability is off.
    public var selfHosted: SelfHostedRegistration? {
        selfHostedState.isEnabled ? configuration.selfHosted : nil
    }

    public func attach(to context: CompositionContext) throws {
        guard shadowReviewState.isEnabled, let shadowConfiguration = configuration.shadowReview else { return }
        shadowReview = try context.clients.shadowReview(configuration: shadowConfiguration)
    }

    public func handle(_ event: CoreEvent) -> Bool {
        shadowReview?.handle(event) ?? false
    }

    public func shutDown() {
        shadowReview = nil
    }
}
