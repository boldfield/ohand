import Foundation
import OhandCoreC

/// The result of provisioning the local-only capture route.
public struct RouteProvisioningOutcome: Decodable, Equatable, Sendable {
    public enum Disposition: String, Decodable, Equatable, Sendable {
        /// The route did not exist and was created with no processing destinations.
        case created
        /// The route already existed for the same scope and was left exactly as configured.
        case unchanged
    }

    public let operationID: UInt64
    public let disposition: Disposition

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case disposition
    }
}

private struct RouteProvisioningRequest: Encodable {
    let routeID: String
    let routeName: String
    let scope: String

    enum CodingKeys: String, CodingKey {
        case routeID = "route_id"
        case routeName = "route_name"
        case scope
    }
}

extension CoreHandle {
    /// Queues the idempotent provisioning of the local-only capture route, so a fresh install can save before any
    /// processing policy exists. A new route has no processing destinations and no authorizations, so creating it never
    /// authorizes upload; an existing route with the same scope is left untouched. The outcome event for `operationID`
    /// decodes as `RouteProvisioningOutcome`, or is a failure whose `code` is `route_scope_mismatch` when the route
    /// exists for a different scope (nothing changed). Throws when the request could not be queued, in which case
    /// nothing ran.
    public func startEnsureLocalCaptureRoute(
        operationID: UInt64,
        routeID: String,
        routeName: String,
        scope: String
    ) throws {
        let request = RouteProvisioningRequest(routeID: routeID, routeName: routeName, scope: scope)
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_ensure_local_capture_route(
                handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
