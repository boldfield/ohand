import Foundation
import OhandCoreC

extension CoreHandle {
    /// Queues a read-only snapshot of background-processing health. The outcome event for `operationID` carries the
    /// core's content-free health JSON (counts, ages, timestamps and machine labels only). Reading changes no job and
    /// never blocks a capture save. `stallAfterSeconds` is how long due work may wait before it counts as a stall and
    /// `leaseGraceSeconds` how long a lapsed lease may stay unrecovered; 0 selects the core's default for either.
    public func startProcessingHealth(
        operationID: UInt64,
        stallAfterSeconds: UInt32 = 0,
        leaseGraceSeconds: UInt32 = 0
    ) throws {
        _ = try consumeCoreResult(
            ohand_core_start_processing_health(handleIdentifier, operationID, stallAfterSeconds, leaseGraceSeconds))
    }
}
