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
/// The core decides; this service only carries out. Per item the order is fixed: ask the core to authorize removal of
/// the exact audio reference (never trusting the sweep that listed it, which may be stale), remove the finalized file,
/// verify it is absent, then record the completion in the ledger. Every step is idempotent, so an interruption anywhere
/// converges on the next sweep:
///
/// * before the file is removed, the core is asked again and the removal is repeated, or refused if the audio has since
///   become protected, deleted or changed;
/// * after the file is removed but before the ledger write, the file is already absent, the ledger write is made and
///   the removal is reported then, never earlier.
///
/// A removal is reported as `removed` only when the file is verified absent and the ledger holds the completion.
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
            // The core keeps offering audio it already approved, so finished removals must not use up the limit.
            if isRecordedRemoved(itemID: item.itemID, audioReference: audioReference) {
                reports.append(AudioRetentionItemReport(item: item, removal: .alreadyRemoved))
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
        return AudioRetentionSweepReport(
            listingFailure: nil, pressure: pressure, items: reports, deferredRemovals: deferred)
    }

    /// Re-arms a removal that failed for good so the next sweep attempts it again.
    func resetFailure(itemID: String, audioReference: String) async throws {
        await acquire()
        defer { release() }
        try ledger.resetFailure(itemID: itemID, audioReference: audioReference)
    }

    // MARK: Removal

    /// True only when the ledger holds the completion and the file is verified absent. Any doubt is "no", and the
    /// ordinary flow asks the core again.
    private func isRecordedRemoved(itemID: String, audioReference: String) -> Bool {
        guard let recorded = try? ledger.entry(itemID: itemID, audioReference: audioReference),
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
            return .failedPermanently(Self.permanentFailure(from: recorded.lastFailure))
        }

        // Asked again here, immediately before the effect: the sweep that listed this audio may be stale.
        let decision: AudioRemovalDecision
        do {
            decision = try await core.authorizeRemoval(
                itemID: itemID, audioReference: audioReference, periodSeconds: retentionPeriodSeconds)
        } catch {
            return .retryLater(Self.coreReason(error))
        }
        if case .denied(let denial) = decision { return .denied(denial) }

        guard let url = staging.finalizedAudioURL(forReference: audioReference) else {
            return recordFailure(
                itemID: itemID, audioReference: audioReference,
                code: AudioRetentionPermanentFailure.audioReferenceUnresolvable.rawValue, permanent: true)
        }

        let wasPresent = fileSystem.fileExists(at: url)
        do {
            if wasPresent { try fileSystem.remove(at: url) }
        } catch {
            return recordFailure(
                itemID: itemID, audioReference: audioReference,
                code: AudioRetentionRetryReason.storageUnavailable.rawValue, permanent: false)
        }
        guard !fileSystem.fileExists(at: url) else {
            return recordFailure(
                itemID: itemID, audioReference: audioReference,
                code: AudioRetentionRetryReason.storageUnavailable.rawValue, permanent: false)
        }

        if recorded?.state == .removed && !wasPresent { return .alreadyRemoved }
        do {
            try ledger.recordRemoved(itemID: itemID, audioReference: audioReference)
        } catch {
            return .retryLater(.ledgerUnavailable)
        }
        return .removed
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
