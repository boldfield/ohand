import Foundation

/// The importer `ForegroundIngressService` is given when reset is in the app. It is the single place that sits between a
/// staged capture and the core's import, so it can do two things the service itself must not know about:
///
/// 1. **Hold the reset boundary.** While a reset tap is recorded and not yet consumed (from the tap until the core has
///    recorded the reset and the request is cleared), imports of captures that were not staged before the tap wait here. The core's boundary is `max(items.rowid)` at start, so an item created
///    in that window would sit below it and be tombstoned, although a capture made after the tap is fresh-generation
///    content. A held capture stays staged and durable; its completion simply arrives after the boundary is released.
///    The gate is created holding when the markers show a recorded tap, at the list recorded with it, so a capture made
///    after a tap whose process died is held from launch, before ingress recovery or `ResetService.resume` can run.
/// 2. **Discard the audio of a rejected capture.** When the core says the capture's item was deleted (by the user or by a
///    reset fence), the service drops the staging record but would leave the audio it finalized just before asking. The
///    audio is removed first, and the rejection is reported as retryable if that cannot be confirmed, so the record keeps
///    naming what is left.
///
/// Confined to the main queue, like `ForegroundIngressService`: call every method there.
final class ResetIngressGate: ForegroundIngressImporting {
    static let audioDiscardIncompleteCode = "ingress_audio_discard_incomplete"

    private struct ParkedImport {
        let record: IngressRecord
        let completion: (IngressImportResult) -> Void
    }

    private let wrapped: ForegroundIngressImporting
    private let discardOwnedFiles: (String) throws -> Void
    private var isHolding = false
    private var capturesStagedBeforeTheTap: Set<String> = []
    private var parked: [ParkedImport] = []

    /// `markers` are read once, here: a recorded tap starts the gate holding at its list. When they cannot be read, a
    /// recorded tap cannot be ruled out, so every import waits until reset resolves the request.
    init(
        wrapping wrapped: ForegroundIngressImporting,
        markers: ResetMarkerStore,
        discardOwnedFiles: @escaping (String) throws -> Void
    ) {
        self.wrapped = wrapped
        self.discardOwnedFiles = discardOwnedFiles
        do {
            if let request = try markers.request() {
                hold(stagedBeforeTheTap: request.stagedBeforeTheTap.captureIDs)
            }
        } catch {
            hold(stagedBeforeTheTap: [])
        }
    }

    convenience init(
        wrapping wrapped: ForegroundIngressImporting,
        layout: ProtectedStorageLayout,
        markers: ResetMarkerStore,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) {
        let localCleanup = DeletionLocalCleanup(layout: layout, fileSystem: fileSystem)
        self.init(wrapping: wrapped, markers: markers, discardOwnedFiles: { captureID in
            // Finalized audio first and the staging record last, so an interruption leaves the record naming what remains.
            try localCleanup.removeAudio(captureID: captureID, audioReference: nil)
            try localCleanup.clearIngress(captureID: captureID)
        })
    }

    /// Starts holding the boundary, or moves it to a later tap. `stagedBeforeTheTap` are the captures the reset itself
    /// imports or purges; they pass through, every other import waits for `release`. An import already waiting here for a
    /// capture the new list names is pre-tap input of this tap, so it is started now: the reset's own import of that
    /// capture joins the attempt in flight and must not wait for a release that only follows the reset.
    func hold(stagedBeforeTheTap: Set<String>) {
        isHolding = true
        capturesStagedBeforeTheTap = stagedBeforeTheTap
        let nowListed = parked.filter { stagedBeforeTheTap.contains($0.record.captureID) }
        guard !nowListed.isEmpty else { return }
        parked.removeAll { stagedBeforeTheTap.contains($0.record.captureID) }
        for pending in nowListed {
            forward(pending.record, completion: pending.completion)
        }
    }

    /// Ends the hold and starts every parked import, in the order they arrived. Safe to call when nothing is held.
    func release() {
        isHolding = false
        capturesStagedBeforeTheTap = []
        let waiting = parked
        parked = []
        for pending in waiting {
            forward(pending.record, completion: pending.completion)
        }
    }

    var heldImportCount: Int { parked.count }

    func importRecord(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void) {
        if isHolding, !capturesStagedBeforeTheTap.contains(record.captureID) {
            parked.append(ParkedImport(record: record, completion: completion))
            return
        }
        forward(record, completion: completion)
    }

    private func forward(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void) {
        wrapped.importRecord(record) { [discardOwnedFiles] result in
            guard case .failed(.itemDeleted) = result else {
                completion(result)
                return
            }
            do {
                try discardOwnedFiles(record.captureID)
                completion(result)
            } catch {
                completion(.failed(.rejected(code: Self.audioDiscardIncompleteCode)))
            }
        }
    }
}
