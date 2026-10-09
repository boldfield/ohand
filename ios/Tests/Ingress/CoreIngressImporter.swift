import Foundation
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Connects `ForegroundIngressService` to the real Rust core's transactional import. It lives with the tests until the
/// Services target links the bridge's C module (V05b); it contains no ingress policy, only the mapping from the core's
/// events to `IngressImportResult`. A result is `confirmed` only for a success event, which the core sends after the
/// import transaction committed.
final class CoreIngressImporter: ForegroundIngressImporting {
    private let core: CoreHandle
    private var pendingByOperationID: [UInt64: (IngressImportResult) -> Void] = [:]
    private var nextOperationID: UInt64 = 1

    init(core: CoreHandle) throws {
        self.core = core
        try core.setEventHandler { [weak self] event in self?.deliver(event) }
    }

    func importRecord(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void) {
        let operationID = nextOperationID
        nextOperationID += 1
        pendingByOperationID[operationID] = completion
        let capture = CaptureRecord(
            captureID: record.captureID,
            text: record.text,
            audioReference: record.coreAudioReference,
            captureInstant: record.context.captureInstant,
            timezoneID: record.context.timezoneID,
            utcOffsetMinutes: record.context.utcOffsetMinutes,
            locale: record.context.locale,
            calendar: record.context.calendar,
            itemScope: record.context.itemScope,
            routeID: record.context.routeID,
            entryLocked: record.context.entryLocked,
            createdAt: record.context.createdAt,
            sessionTopic: record.context.sessionTopic)
        do {
            try core.startImportForegroundIngress(operationID: operationID, record: capture)
        } catch {
            pendingByOperationID[operationID] = nil
            completion(.failed(.coreUnavailable))
        }
    }

    private func deliver(_ event: CoreEvent) {
        guard let completion = pendingByOperationID.removeValue(forKey: event.operationID) else { return }
        completion(Self.result(for: event))
    }

    static func result(for event: CoreEvent) -> IngressImportResult {
        switch event.outcome {
        case .success:
            guard let acknowledgment = try? event.decode(IngressImportAcknowledgment.self) else {
                return .failed(.commitUnknown)
            }
            return .confirmed(IngressImportConfirmation(
                captureID: acknowledgment.captureID,
                itemID: acknowledgment.itemID,
                savedAt: acknowledgment.savedAt,
                alreadyImported: acknowledgment.disposition == .alreadyImported))
        case .failure(let failure):
            switch failure.code {
            case "ingress_not_committed": return .failed(.notCommitted)
            case "ingress_commit_unknown": return .failed(.commitUnknown)
            case "ingress_conflicting_reuse": return .failed(.conflictingReuse)
            case "ingress_item_deleted": return .failed(.itemDeleted)
            default:
                if failure.code.hasPrefix("ingress_") { return .failed(.rejected(code: failure.code)) }
                if failure.errorClass == .cancelled || failure.code == CoreFailure.handleClosed.code {
                    return .failed(.coreUnavailable)
                }
                return .failed(.commitUnknown)
            }
        }
    }
}
