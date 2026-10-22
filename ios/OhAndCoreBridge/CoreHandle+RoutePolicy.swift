import Foundation
import OhandCoreC

/// The result of a grant or revoke. Decodes the `{"operation_id","disposition"}` success event.
public struct RoutePolicyChangeOutcome: Decodable, Equatable, Sendable {
    public enum Disposition: String, Decodable, Equatable, Sendable {
        /// A grant was added.
        case granted
        /// A grant was removed.
        case revoked
        /// The policy already matched: nothing was written.
        case unchanged
    }

    public let operationID: UInt64
    public let disposition: Disposition

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case disposition
    }
}

/// One granted capability of a route and the classes of the destinations it may use.
public struct RoutePolicyGrant: Decodable, Equatable, Sendable {
    public let capability: String
    /// `cloud`, `private_server` or `unknown`.
    public let destinationClasses: [String]
    /// Unrevoked profile versions whose every destination this grant allows. A grant for one provider does not cover
    /// another's version. Older core output without the field decodes as empty: nothing is covered.
    public let coveredProfileVersions: [String]

    public init(capability: String, destinationClasses: [String], coveredProfileVersions: [String] = []) {
        self.capability = capability
        self.destinationClasses = destinationClasses
        self.coveredProfileVersions = coveredProfileVersions
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        capability = try container.decode(String.self, forKey: .capability)
        destinationClasses = try container.decode([String].self, forKey: .destinationClasses)
        coveredProfileVersions = try container.decodeIfPresent([String].self, forKey: .coveredProfileVersions) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case capability
        case destinationClasses = "destination_classes"
        case coveredProfileVersions = "covered_profile_versions"
    }
}

public struct RoutePolicyRoute: Decodable, Equatable, Sendable {
    public let routeID: String
    public let routeName: String
    public let scope: String
    public let grantedCapabilities: [RoutePolicyGrant]

    enum CodingKeys: String, CodingKey {
        case routeID = "route_id"
        case routeName = "route_name"
        case scope
        case grantedCapabilities = "granted_capabilities"
    }
}

/// The success event of a policy listing.
public struct RoutePolicyListing: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let routes: [RoutePolicyRoute]

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case routes
    }
}

private struct RouteGrantRequest: Encodable {
    let routeID: String
    let capability: String
    let profileVersion: String

    enum CodingKeys: String, CodingKey {
        case routeID = "route_id"
        case capability
        case profileVersion = "profile_version"
    }
}

private struct RouteRevokeRequest: Encodable {
    let routeID: String
    let capability: String

    enum CodingKeys: String, CodingKey {
        case routeID = "route_id"
        case capability
    }
}

extension CoreHandle {
    /// Queues a deliberate grant of `capability` (`text_interpretation` or `review`) on `routeID` for the destinations of
    /// the stored profile version `profileVersion`. The caller never names destinations; the core derives them from the
    /// profile. The outcome event for `operationID` decodes as `RoutePolicyChangeOutcome`, or is a failure whose `code` is
    /// `profile_unavailable`, `profile_revoked`, `profile_malformed`, `route_unknown`, `capability_not_grantable` (an
    /// on-device capability such as `transcription`) or `route_policy_malformed`, with nothing changed. Throws when the
    /// request could not be queued, in which case nothing ran.
    public func startGrantRouteCapability(
        operationID: UInt64,
        routeID: String,
        capability: String,
        profileVersion: String
    ) throws {
        let request = RouteGrantRequest(routeID: routeID, capability: capability, profileVersion: profileVersion)
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_grant_route_capability(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues the revocation of `capability` on `routeID`. Jobs already queued for it then fail authorization at dispatch,
    /// and destinations no remaining grant uses leave the route's ceiling. The outcome event decodes as
    /// `RoutePolicyChangeOutcome` (`revoked`, or `unchanged` when the capability had no grant), or is a failure whose
    /// `code` is `route_unknown`.
    public func startRevokeRouteCapability(
        operationID: UInt64,
        routeID: String,
        capability: String
    ) throws {
        let request = RouteRevokeRequest(routeID: routeID, capability: capability)
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_revoke_route_capability(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues a read of every route's name, scope, granted capabilities and destination classes. The outcome event for
    /// `operationID` decodes as `RoutePolicyListing`.
    public func startListRoutePolicy(operationID: UInt64) throws {
        _ = try consumeCoreResult(ohand_core_start_list_route_policy(handleIdentifier, operationID))
    }
}
