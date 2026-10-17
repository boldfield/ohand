import Foundation
import OhAndCoreBridge

/// Joins the self-hosted provider and shadow review to the production root through the extension seam, without any edit
/// to the root.
///
/// * **Off by default.** With `.none`, or with a registration that fails verification, the module takes no core client
///   and declares no provider protocol, so the core refuses to dispatch self-hosted jobs whatever the store holds; the
///   base app builds and captures exactly as without it, and the shadow review service does not exist.
/// * **Same rules.** The module has no `CoreHandle`, provider transport, endpoint or credential. Self-hosted jobs run in
///   the core's job runner and shadow review through the core-built, core-authorized exchange, so both follow the same
///   route grant, revocation and credential-reference checks as every other provider. A registration never widens them.
/// * **Registered pair only.** The root passes the verified profile version and model to the core, which sends
///   self-hosted requests only to that pair; `selfHostedAvailability` is a diagnostic read of the same pair.
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

    private let availabilityLock = NSLock()
    private var storedSelfHostedAvailability: SelfHostedProfileAvailability = .unchecked
    private var profileStatusClient: CoreProviderProfileStatusClient?

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

    /// The root enables the self-hosted protocol in the core only while the registration is verified.
    public var enabledProviderProtocols: Set<CoreProviderProtocol> {
        selfHostedState.isEnabled ? [.selfHosted] : []
    }

    /// The registered profile version and model the core may send self-hosted requests to. The core refuses every
    /// other self-hosted request (`self_hosted_not_registered`), so a stored supported profile that is not the
    /// registered one is never used, and a registered profile the store cannot validate (missing, revoked, other
    /// protocol, other model) is refused by the core's own checks on that exact pair.
    public var selfHostedRegistrations: Set<CoreSelfHostedRegistration> {
        guard let registration = selfHosted else { return [] }
        return [CoreSelfHostedRegistration(profileVersion: registration.profileVersion, model: registration.model)]
    }

    /// What the core's store says about the registered profile. `.unchecked` until the read is answered, and always
    /// while the capability is off.
    public var selfHostedAvailability: SelfHostedProfileAvailability {
        availabilityLock.withLock { storedSelfHostedAvailability }
    }

    public func attach(to context: CompositionContext) throws {
        if let registration = selfHosted {
            let client = try context.clients.providerProfileStatus()
            availabilityLock.withLock { profileStatusClient = client }
            client.status(profileVersion: registration.profileVersion, model: registration.model) { [weak self] result in
                self?.record(result)
            }
        }
        guard shadowReviewState.isEnabled, let shadowConfiguration = configuration.shadowReview else { return }
        shadowReview = try context.clients.shadowReview(configuration: shadowConfiguration)
    }

    public func handle(_ event: CoreEvent) -> Bool {
        let client = availabilityLock.withLock { profileStatusClient }
        if client?.handle(event) == true { return true }
        return shadowReview?.handle(event) ?? false
    }

    public func shutDown() {
        let client = availabilityLock.withLock { () -> CoreProviderProfileStatusClient? in
            defer { profileStatusClient = nil }
            return profileStatusClient
        }
        client?.shutDown()
        shadowReview = nil
    }

    private func record(_ result: CoreProviderProfileStatusResult) {
        let availability: SelfHostedProfileAvailability
        switch result {
        case .failure:
            availability = .unavailable(.checkFailed)
        case .success(let status):
            if !status.exists {
                availability = .unavailable(.missing)
            } else if status.providerProtocol != .selfHosted {
                availability = .unavailable(.notSelfHosted)
            } else if status.revoked {
                availability = .unavailable(.revoked)
            } else if !status.servesModel {
                availability = .unavailable(.modelNotServed)
            } else {
                availability = .available
            }
        }
        availabilityLock.withLock { storedSelfHostedAvailability = availability }
    }
}
