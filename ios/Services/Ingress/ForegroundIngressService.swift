import Foundation

/// The foreground app's single ingress writer. It stages every input durably first, finalizes recorded audio, asks the
/// core to import in one transaction, and removes the staging record only after the core confirmed the import. The
/// "saved" outcome therefore never precedes the durable core commit, and an interruption at any step leaves the
/// record, and any audio it owns, for the next attempt.
///
/// Confined to the main queue: call every method there, and completions arrive there.
final class ForegroundIngressService {
    let layout: ProtectedStorageLayout
    private let staging: IngressStagingStore
    private let importer: ForegroundIngressImporting
    private let protection: ProtectedStorageService?
    private let writerLock: IngressWriterLock
    private var waitersByCaptureID: [String: [(IngressOutcome) -> Void]] = [:]

    /// Throws `IngressWriterLock.AcquisitionError.anotherWriterActive` when another instance already writes ingress.
    /// The store directories must exist (`ProtectedStorageService.prepare()` creates them).
    init(
        layout: ProtectedStorageLayout,
        importer: ForegroundIngressImporting,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem(),
        protection: ProtectedStorageService? = nil
    ) throws {
        self.layout = layout
        self.importer = importer
        self.protection = protection
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        writerLock = try IngressWriterLock(
            lockFileURL: layout.directory(for: .temporaryFiles).appendingPathComponent(IngressWriterLock.lockFileName))
    }

    /// Stages `record` and imports it. A repeated submission of the same record joins the attempt already running or
    /// converges on the same item; a different record under the same capture ID is refused.
    func submit(_ record: IngressRecord, completion: @escaping (IngressOutcome) -> Void) {
        do {
            _ = try staging.stage(record)
        } catch {
            completion(.notStaged(captureID: record.captureID, failure: Self.stagingFailure(error)))
            return
        }
        process(record, completion: completion)
    }

    /// Imports every staged record left by an earlier run, one at a time, and reports what was found. Safe to call on
    /// every launch and repeatedly.
    func recover(completion: @escaping (IngressRecoveryReport) -> Void) {
        let listing: IngressStagingStore.Listing
        do {
            listing = try staging.listRecords()
        } catch {
            var report = IngressRecoveryReport()
            report.listingFailure = Self.stagingFailure(error)
            completion(report)
            return
        }
        var report = IngressRecoveryReport()
        report.incompleteWrites = listing.incompleteWrites
        var claimedInProgressNames = Set<String>()
        var everyRecordReadable = true

        func finish() {
            if !everyRecordReadable {
                report.unclaimedAudioNotEvaluated = true
            } else {
                do {
                    report.unclaimedInProgressAudio = try staging.unclaimedInProgressAudio(
                        claimedFileNames: claimedInProgressNames)
                } catch {
                    report.unclaimedAudioListingFailure = Self.stagingFailure(error)
                }
            }
            completion(report)
        }

        func recoverRecord(at index: Int) {
            guard index < listing.recordCaptureIDs.count else {
                finish()
                return
            }
            let captureID = listing.recordCaptureIDs[index]
            let loaded: IngressRecord?
            do {
                loaded = try staging.load(captureID: captureID)
            } catch {
                everyRecordReadable = false
                report.entries.append(IngressRecoveryEntry(
                    captureID: captureID,
                    outcome: .keptForRetry(captureID: captureID, problem: .recordUnreadable(Self.stagingFailure(error)))))
                recoverRecord(at: index + 1)
                return
            }
            guard let record = loaded else {
                recoverRecord(at: index + 1)
                return
            }
            if let audio = record.audio { claimedInProgressNames.insert(audio.inProgressFileName) }
            process(record) { outcome in
                report.entries.append(IngressRecoveryEntry(captureID: captureID, outcome: outcome))
                recoverRecord(at: index + 1)
            }
        }
        recoverRecord(at: 0)
    }

    private func process(_ record: IngressRecord, completion: @escaping (IngressOutcome) -> Void) {
        let captureID = record.captureID
        if waitersByCaptureID[captureID] != nil {
            waitersByCaptureID[captureID]?.append(completion)
            return
        }
        waitersByCaptureID[captureID] = [completion]

        var protectionFailures: [StorageSetupFailure] = []
        if let audio = record.audio {
            do {
                _ = try staging.finalizeAudio(audio)
            } catch {
                finish(captureID, .keptForRetry(
                    captureID: captureID, problem: .sourceUnavailable(Self.stagingFailure(error))))
                return
            }
            protectionFailures = protection?.applyPolicy(to: .finalizedAudio).failures ?? []
        }

        importer.importRecord(record) { [self] result in
            switch result {
            case .confirmed(let confirmation):
                guard confirmation.captureID == captureID else {
                    finish(captureID, .keptForRetry(captureID: captureID, problem: .rejected(code: "confirmation_mismatch")))
                    return
                }
                let cleanedUp = (try? staging.removeRecord(captureID: captureID)) != nil
                finish(captureID, .saved(IngressSaveAcknowledgment(
                    captureID: captureID,
                    itemID: confirmation.itemID,
                    savedAt: confirmation.savedAt,
                    alreadyImported: confirmation.alreadyImported,
                    stagingCleanedUp: cleanedUp,
                    audioProtectionFailures: protectionFailures)))
            case .failed(.itemDeleted):
                _ = try? staging.removeRecord(captureID: captureID)
                finish(captureID, .itemDeleted(captureID: captureID))
            case .failed(let failure):
                finish(captureID, .keptForRetry(captureID: captureID, problem: Self.pendingProblem(failure)))
            }
        }
    }

    private func finish(_ captureID: String, _ outcome: IngressOutcome) {
        let waiters = waitersByCaptureID.removeValue(forKey: captureID) ?? []
        for waiter in waiters { waiter(outcome) }
    }

    private static func stagingFailure(_ error: Error) -> IngressStagingFailure {
        (error as? IngressStagingFailure) ?? .storage(IngressStorageError(error))
    }

    private static func pendingProblem(_ failure: IngressImportFailure) -> IngressPendingProblem {
        switch failure {
        case .notCommitted: return .notCommitted
        case .commitUnknown: return .commitUnknown
        case .coreUnavailable: return .coreUnavailable
        case .rejected(let code): return .rejected(code: code)
        case .conflictingReuse: return .conflictingReuse
        case .itemDeleted: return .rejected(code: "ingress_item_deleted")
        }
    }
}
