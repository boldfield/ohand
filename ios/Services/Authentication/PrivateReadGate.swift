import Foundation
import OhAndCoreBridge

/// Why a private read was refused. Content-free by construction: it never reports whether the requested record exists.
enum PrivateReadDenied: Error, Equatable {
    /// The session is capture-only, so the read was never sent to the core.
    case sessionNotAuthenticated
    /// The session relocked after the read was sent; the core's answer was dropped undelivered.
    case relockedBeforeDelivery
    /// The event does not belong to a read this gate issued.
    case notAGatedRead
}

/// The core operations that return stored history. Saving a capture is deliberately absent: capture never needs, and
/// never grants, read access.
protocol PrivateReadSource: AnyObject {
    func startGetCapture(operationID: UInt64, captureID: String) throws
    func startItemStatus(operationID: UInt64, itemID: String) throws
}

extension CoreHandle: PrivateReadSource {}

/// Gates private reads on the session read scope, both when a read starts and when its answer arrives.
///
/// The core answers asynchronously, so checking the scope only at request time would let a read that began while
/// authenticated deliver stored content after a relock. Callers therefore pass every event for a gated operation
/// through `admit`, which hands the event back only while the authorization that issued the read is still current.
final class PrivateReadGate {
    private let session: ReadAuthenticationSession
    private let source: PrivateReadSource
    private let lock = NSLock()
    private var pendingReads: [UInt64: ReadAuthorization] = [:]

    init(session: ReadAuthenticationSession, source: PrivateReadSource) {
        self.session = session
        self.source = source
    }

    func startGetCapture(operationID: UInt64, captureID: String) throws {
        try issue(operationID: operationID) {
            try source.startGetCapture(operationID: operationID, captureID: captureID)
        }
    }

    func startItemStatus(operationID: UInt64, itemID: String) throws {
        try issue(operationID: operationID) {
            try source.startItemStatus(operationID: operationID, itemID: itemID)
        }
    }

    /// Returns `event` only if the read it answers is still authorized; otherwise throws and the payload, success or
    /// failure, is not exposed to the caller.
    func admit(_ event: CoreEvent) throws -> CoreEvent {
        lock.lock()
        let authorization = pendingReads.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let authorization else { throw PrivateReadDenied.notAGatedRead }
        guard session.isCurrent(authorization) else { throw PrivateReadDenied.relockedBeforeDelivery }
        return event
    }

    private func issue(operationID: UInt64, _ start: () throws -> Void) throws {
        guard let authorization = session.currentAuthorization() else {
            throw PrivateReadDenied.sessionNotAuthenticated
        }
        lock.lock()
        pendingReads = pendingReads.filter { session.isCurrent($0.value) }
        pendingReads[operationID] = authorization
        lock.unlock()
        do {
            try start()
        } catch {
            lock.lock()
            pendingReads.removeValue(forKey: operationID)
            lock.unlock()
            throw error
        }
    }
}
