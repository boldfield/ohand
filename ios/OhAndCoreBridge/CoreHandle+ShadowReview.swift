import Foundation
import OhandCoreC

extension CoreHandle {
    /// Offers one case for shadow sampling. `request` is the JSON the core documents for
    /// `ohand_core_start_shadow_selection`. One event answers; nothing is sent to a provider.
    public func startShadowSelection(operationID: UInt64, request: Data) throws {
        let requestBytes = Array(request)
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_shadow_selection(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Runs the selected case named in `request` through the registered provider transport: a failure or a
    /// `completed` event with a denial (nothing was sent), or `dispatched` followed by `completed`. Cancel it with
    /// `cancelProviderExchange`; answer its native sends with the provider exchange answers.
    public func startShadowReview(operationID: UInt64, request: Data) throws {
        let requestBytes = Array(request)
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_shadow_review(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Reads one stored shadow case. `request` is `{"job_id": ...}`.
    public func startShadowRecord(operationID: UInt64, request: Data) throws {
        let requestBytes = Array(request)
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_shadow_record(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
