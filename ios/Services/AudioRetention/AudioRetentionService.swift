import Foundation
import OhAndCoreBridge

/// Reports whether the audio volume is short of space. Pressure only adds visible notices to the core's answer; it never
/// makes audio eligible for removal.
protocol AudioStoragePressureProbing: Sendable {
    func currentPressure() -> CoreRetentionPressure
}

/// Low means less than `lowThresholdBytes` is available for important usage on the volume holding `volumeURL`. When the
/// platform cannot say, the answer is `normal`: no claim is made and nothing is freed either way.
struct VolumeStoragePressureProbe: AudioStoragePressureProbing {
    static let defaultLowThresholdBytes: Int64 = 500 * 1024 * 1024

    let volumeURL: URL
    let lowThresholdBytes: Int64

    init(volumeURL: URL, lowThresholdBytes: Int64 = VolumeStoragePressureProbe.defaultLowThresholdBytes) {
        self.volumeURL = volumeURL
        self.lowThresholdBytes = lowThresholdBytes
    }

    func currentPressure() -> CoreRetentionPressure {
        let values = try? volumeURL.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        guard let available = values?.volumeAvailableCapacityForImportantUsage else { return .normal }
        return available < lowThresholdBytes ? .low : .normal
    }
}

/// Removes audio the core's retention policy has approved, and records each completed removal durably.
///
/// The core decides and holds the fence; this service performs the file effect and records. Per item the order is
/// fixed: ask the core to remove the finalized file the exact audio reference names. The core re-decides for that
/// reference (never trusting the sweep that listed it, which may be stale) and, only if it approves, holds its
/// store-wide write lock while this service unlinks the file and verifies it absent, so audio cannot become protected,
/// deleted or changed between the approval and the unlink, whichever handle or process writes. Then the completion is
/// recorded in the ledger. Every step is idempotent, so an interruption anywhere converges on the next sweep:
///
/// * before the core acts, the removal is repeated, or refused if the audio has since become protected, deleted or
///   changed;
/// * after the file is removed but before the ledger write, the file is already absent, the ledger write is made and
///   the removal is reported then, never earlier;
/// * if the lease expired before the effect could be pinned under it, nothing is removed, and if the core cannot vouch
///   for the fence when it is released, the removal is not recorded; either way the next sweep decides it again.
///
/// A removal is reported as `removed` or `alreadyRemoved` only when the file is verified absent and the ledger holds the
/// completion.
/// Failures carry a fixed code and are counted per audio: after `maximumAttempts` failed attempts, or at once when the
/// reference can never resolve, the removal is `failedPermanently` and is neither attempted again nor hidden until
/// `resetFailure` re-arms it. Sole-source, pending and otherwise unresolved audio is never offered by the core, so it is
/// never touched here. Retention only ever removes the one finalized file the approved reference names, and never an
/// ingress staging record or in-progress recording; deleted items belong to deletion cleanup.
///
/// Sweeps are serialized (a second call waits for the running one) and bounded to `maximumRemovalsPerSweep`; the rest
/// are reported as deferred.
actor AudioRetentionService {
    static let defaultMaximumAttempts = 5
    static let defaultMaximumRemovalsPerSweep = 50

    private let core: AudioRetentionCore
    private let staging: IngressStagingStore
    private let fileSystem: AudioRetentionFileSystem
    private let ledger: AudioRetentionLedger
    private let pressureProbe: AudioStoragePressureProbing
    private let retentionPeriodSeconds: UInt64
    private let maximumAttempts: Int
    private let maximumRemovalsPerSweep: Int
    private var isRunning = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    /// `retentionPeriodSeconds` 0 selects the core's default of seven days after successful transcription.
    init(
        core: AudioRetentionCore,
        layout: ProtectedStorageLayout,
        fileSystem: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem(),
        pressureProbe: AudioStoragePressureProbing? = nil,
        retentionPeriodSeconds: UInt64 = 0,
        maximumAttempts: Int = AudioRetentionService.defaultMaximumAttempts,
        maximumRemovalsPerSweep: Int = AudioRetentionService.defaultMaximumRemovalsPerSweep
    ) {
        self.core = core
        self.fileSystem = fileSystem
        staging = IngressStagingStore(layout: layout, fileSystem: fileSystem)
        ledger = AudioRetentionLedger(layout: layout, fileSystem: fileSystem)
        self.pressureProbe = pressureProbe ?? VolumeStoragePressureProbe(volumeURL: layout.rootDirectory)
        self.retentionPeriodSeconds = retentionPeriodSeconds
        self.maximumAttempts = max(1, maximumAttempts)
        self.maximumRemovalsPerSweep = max(1, maximumRemovalsPerSweep)
    }

    /// Lists every live item's retention status and removes the audio the core approves. Safe to call on every launch
    /// and foreground, repeatedly.
    func sweep() async -> AudioRetentionSweepReport {
        await acquire()
        defer { release() }

        let pressure = pressureProbe.currentPressure()
        let items: [AudioRetentionItem]
        do {
            items = try await core.sweep(pressure: pressure, periodSeconds: retentionPeriodSeconds)
        } catch {
            return AudioRetentionSweepReport(
                listingFailure: Self.coreReason(error), pressure: pressure, items: [])
        }

        // One read of the ledger per sweep. Unreadable is "unknown": nothing is removed without a place to record it.
        let ledgerReadable: Bool
        do {
            try ledger.refresh()
            ledgerReadable = true
        } catch {
            ledgerReadable = false
        }

        var reports: [AudioRetentionItemReport] = []
        var attempted = 0
        var deferred = 0
        for item in items {
            guard case .removeAudio(let audioReference, _) = item.cleanup else {
                reports.append(AudioRetentionItemReport(item: item, removal: nil))
                continue
            }
            if Task.isCancelled {
                reports.append(AudioRetentionItemReport(item: item, removal: .retryLater(.cancelled)))
                continue
            }
            guard ledgerReadable else {
                reports.append(AudioRetentionItemReport(item: item, removal: .retryLater(.ledgerUnavailable)))
                continue
            }
            // The core keeps offering audio it already approved, so finished removals must not use up the limit.
            do {
                if try isRecordedRemoved(itemID: item.itemID, audioReference: audioReference) {
                    reports.append(AudioRetentionItemReport(item: item, removal: .alreadyRemoved))
                    continue
                }
            } catch {
                reports.append(AudioRetentionItemReport(item: item, removal: .retryLater(.ledgerUnavailable)))
                continue
            }
            guard attempted < maximumRemovalsPerSweep else {
                deferred += 1
                reports.append(AudioRetentionItemReport(item: item, removal: nil))
                continue
            }
            attempted += 1
            let outcome = await remove(itemID: item.itemID, audioReference: audioReference)
            reports.append(AudioRetentionItemReport(item: item, removal: outcome))
        }
        if ledgerReadable {
            // Entries for audio the core no longer lists are done with. Failing to forget them costs only space.
            let listed = Set(items.map { AudioRetentionLedger.Key(itemID: $0.itemID, audioReference: $0.audioReference) })
            try? ledger.prune(keeping: listed)
        }
        return AudioRetentionSweepReport(
            listingFailure: nil, pressure: pressure, items: reports, deferredRemovals: deferred)
    }

    /// Re-arms a removal that failed for good so the next sweep attempts it again.
    func resetFailure(itemID: String, audioReference: String) async throws {
        await acquire()
        defer { release() }
        try ledger.refresh()
        try ledger.resetFailure(itemID: itemID, audioReference: audioReference)
    }

    // MARK: Removal

    /// True only when the ledger holds the completion and the file is verified absent. Throws when the ledger cannot be
    /// read, which is "unknown", not "not removed".
    private func isRecordedRemoved(itemID: String, audioReference: String) throws -> Bool {
        guard let recorded = try ledger.entry(itemID: itemID, audioReference: audioReference),
              recorded.state == .removed,
              let url = staging.finalizedAudioURL(forReference: audioReference) else { return false }
        return !fileSystem.fileExists(at: url)
    }

    private func remove(itemID: String, audioReference: String) async -> AudioRemovalOutcome {
        let recorded: AudioRetentionLedger.Entry?
        do {
            recorded = try ledger.entry(itemID: itemID, audioReference: audioReference)
        } catch {
            return .retryLater(.ledgerUnavailable)
        }
        if let recorded, recorded.state == .failed {
            // The failure stays visible only while the core still offers this audio for removal.
            if let outcome = await outcomeUnlessStillApproved(itemID: itemID, audioReference: audioReference) {
                return outcome
            }
            return .failedPermanently(Self.permanentFailure(from: recorded.lastFailure))
        }

        guard let url = staging.finalizedAudioURL(forReference: audioReference) else {
            if let outcome = await outcomeUnlessStillApproved(itemID: itemID, audioReference: audioReference) {
                return outcome
            }
            return recordFailure(
                itemID: itemID, audioReference: audioReference,
                code: AudioRetentionPermanentFailure.audioReferenceUnresolvable.rawValue, permanent: true)
        }

        // The core re-decides for this exact audio and, only if it approves, holds its store-wide fence while the closure
        // removes the file here: no write can make the audio protected between the approval and the removal.
        let fileSystem = self.fileSystem
        let removeFile: () -> AudioRemovalEffect = {
            let wasPresent = fileSystem.fileExists(at: url)
            if wasPresent {
                do { try fileSystem.remove(at: url) } catch { return .failed }
            }
            if fileSystem.fileExists(at: url) { return .failed }
            return wasPresent ? .removed : .alreadyAbsent
        }
        let result: AudioRemovalResult
        do {
            result = try await core.removeAudio(
                itemID: itemID, audioReference: audioReference, periodSeconds: retentionPeriodSeconds,
                effect: removeFile)
        } catch {
            return .retryLater(Self.coreReason(error))
        }
        let effect: AudioRemovalEffect
        switch result {
        case .denied(let denial):
            return .denied(denial)
        case .approved(let approvedEffect):
            effect = approvedEffect
        }
        // The result is checked: a file that is still there is never reported gone.
        guard effect != .failed, !fileSystem.fileExists(at: url) else {
            return recordFailure(
                itemID: itemID, audioReference: audioReference,
                code: AudioRetentionRetryReason.storageUnavailable.rawValue, permanent: false)
        }

        if recorded?.state != .removed {
            do {
                try ledger.recordRemoved(itemID: itemID, audioReference: audioReference)
            } catch {
                return .retryLater(.ledgerUnavailable)
            }
        }
        return effect == .removed ? .removed : .alreadyRemoved
    }

    /// Nil when the core still approves removing this audio; otherwise the outcome to report instead of a failure.
    private func outcomeUnlessStillApproved(itemID: String, audioReference: String) async -> AudioRemovalOutcome? {
        do {
            let decision = try await core.authorizeRemoval(
                itemID: itemID, audioReference: audioReference, periodSeconds: retentionPeriodSeconds)
            if case .denied(let denial) = decision { return .denied(denial) }
            return nil
        } catch {
            return .retryLater(Self.coreReason(error))
        }
    }

    private func recordFailure(itemID: String, audioReference: String, code: String, permanent: Bool)
        -> AudioRemovalOutcome
    {
        let entry: AudioRetentionLedger.Entry
        do {
            entry = try ledger.recordFailure(
                itemID: itemID, audioReference: audioReference, code: code, permanent: permanent,
                limit: maximumAttempts)
        } catch {
            return permanent
                ? .failedPermanently(.audioReferenceUnresolvable) : .retryLater(.storageUnavailable)
        }
        switch entry.state {
        case .failed: return .failedPermanently(Self.permanentFailure(from: entry.lastFailure))
        case .failing, .removed: return .retryLater(.storageUnavailable)
        }
    }

    private static func permanentFailure(from code: String?) -> AudioRetentionPermanentFailure {
        code == AudioRetentionPermanentFailure.audioReferenceUnresolvable.rawValue
            ? .audioReferenceUnresolvable : .attemptsExhausted
    }

    private static func coreReason(_ error: Error) -> AudioRetentionRetryReason {
        (error as? CoreFailure)?.errorClass == .cancelled ? .cancelled : .coreUnavailable
    }

    // MARK: Serialization

    private func acquire() async {
        if isRunning {
            await withCheckedContinuation { (waiter: CheckedContinuation<Void, Never>) in
                waiters.append(waiter)
            }
        } else {
            isRunning = true
        }
    }

    private func release() {
        if waiters.isEmpty {
            isRunning = false
        } else {
            waiters.removeFirst().resume()
        }
    }
}
