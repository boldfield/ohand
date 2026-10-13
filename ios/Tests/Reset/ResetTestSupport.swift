import Foundation
import XCTest
@testable import OhAndServices

/// An ordered log shared by the fakes, so a test can prove which step ran before which.
final class ResetEventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [String] = []

    func append(_ event: String) {
        lock.lock()
        recorded.append(event)
        lock.unlock()
    }

    var events: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func index(of event: String) -> Int? { events.firstIndex(of: event) }
}

extension FaultPlan {
    /// True when `point` was armed (and disarms it). For fakes whose protocol methods cannot throw.
    func fires(_ point: String) -> Bool {
        do {
            try check(point)
            return false
        } catch {
            return true
        }
    }
}

/// The durable reset as the L05a contract defines it, over the deletion fake: starting records a generation whose
/// boundary is every item registered so far, tombstones those items (queuing their cleanup rows) and completes only when
/// all of their cleanup is acknowledged. Items registered afterwards belong to the next generation and are never touched.
final class FakeResetCore: ResetCore, @unchecked Sendable {
    private let lock = NSLock()
    private let deletion: DurableDeletionCore
    private let faults: FaultPlan
    private let log: ResetEventLog
    private var liveItems: [String] = []
    private var boundary: [String] = []
    private var generation: Int64 = 0
    private var phase: ResetPhase?
    private var profiles: [ResetProfileCredential] = []
    private var revokedVersions: [String] = []
    private var starts = 0

    init(deletion: DurableDeletionCore, faults: FaultPlan, log: ResetEventLog = ResetEventLog()) {
        self.deletion = deletion
        self.faults = faults
        self.log = log
    }

    // MARK: Test controls

    /// An item exists now. It belongs to the generation current at this moment.
    func register(_ itemID: String) {
        locked { liveItems.append(itemID) }
    }

    func setProfiles(_ newProfiles: [ResetProfileCredential]) {
        locked { profiles = newProfiles }
    }

    var currentGeneration: Int64 { locked { generation } }
    var startCalls: Int { locked { starts } }
    var revokedProfileVersions: [String] { locked { revokedVersions } }

    // MARK: ResetCore

    func start() async throws -> ResetProgress {
        try faults.check("reset.start")
        log.append("core.start")
        return locked {
            starts += 1
            if phase != .pending {
                generation += 1
                phase = .pending
                boundary = liveItems
                liveItems = []
                for itemID in boundary { deletion.recordDeletion(itemID) }
            }
            return evaluate(promote: true)
        }
    }

    func advance() async throws -> ResetProgress? {
        try faults.check("reset.advance")
        log.append("core.advance")
        return locked { phase == nil ? nil : evaluate(promote: true) }
    }

    func status() async throws -> ResetProgress? {
        try faults.check("reset.status")
        return locked { phase == nil ? nil : evaluate(promote: false) }
    }

    func profileCredentials() async throws -> [ResetProfileCredential] {
        try faults.check("reset.profiles")
        return locked { profiles }
    }

    func revokeProfile(profileVersion: String) async throws {
        try faults.check("reset.revoke")
        log.append("core.revoke:\(profileVersion)")
        locked {
            revokedVersions.append(profileVersion)
            profiles = profiles.map {
                $0.profileVersion == profileVersion
                    ? ResetProfileCredential(profileVersion: $0.profileVersion, credentialReference: $0.credentialReference, isRevoked: true)
                    : $0
            }
        }
    }

    private func evaluate(promote: Bool) -> ResetProgress {
        let incomplete = boundary.filter { !deletion.isDeletionComplete($0) }.count
        let failed = boundary.reduce(0) { total, itemID in
            total + DeletionCleanupKind.allCases.filter { deletion.status(itemID, $0) == .failed }.count
        }
        if promote, phase == .pending, incomplete == 0, failed == 0 { phase = .completed }
        return ResetProgress(
            generation: generation, phase: phase ?? .pending, boundaryRowID: Int64(boundary.count),
            itemsAwaitingTombstone: 0, cleanupPending: incomplete, cleanupFailed: failed)
    }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
}

/// Native ingress staging scripted by capture ID. `importable` inputs import (and become items) when `importStaged`
/// runs; every other staged input stays unimported until it is purged.
final class FakeResetStaging: ResetIngressStaging {
    private let faults: FaultPlan
    private let log: ResetEventLog
    private(set) var staged: Set<String> = []
    private(set) var importedCaptureIDs: [String] = []
    private(set) var purgedCaptureIDs: [String] = []
    private(set) var snapshotCalls = 0
    var importable: Set<String> = []
    /// Called with the capture ID when an input imports, so the test can create the item the core would hold.
    var onImport: ((String) -> Void)?
    /// Called right after the snapshot is taken: the moment a capture can land after the tap.
    var onSnapshot: (() -> Void)?

    init(faults: FaultPlan, log: ResetEventLog) {
        self.faults = faults
        self.log = log
    }

    func stage(_ captureID: String) {
        staged.insert(captureID)
    }

    func snapshot() throws -> ResetStagingSnapshot {
        snapshotCalls += 1
        log.append("staging.snapshot")
        try faults.check("staging.snapshot")
        let taken = ResetStagingSnapshot(captureIDs: staged)
        onSnapshot?()
        return taken
    }

    func importStaged() async -> Bool {
        log.append("staging.import")
        if faults.fires("staging.import") { return false }
        for captureID in staged.intersection(importable).sorted() {
            staged.remove(captureID)
            importedCaptureIDs.append(captureID)
            onImport?(captureID)
        }
        return true
    }

    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge {
        log.append("staging.purge")
        if faults.fires("staging.purge.unreadable") { return .unknown }
        let targets = snapshot.captureIDs.intersection(staged)
        if faults.fires("staging.purge") { return .incomplete(remaining: targets.count) }
        for captureID in targets.sorted() {
            staged.remove(captureID)
            purgedCaptureIDs.append(captureID)
        }
        return .purged(count: targets.count)
    }
}

final class FakeResetExportTemporaries: ResetExportTemporaries {
    private let faults: FaultPlan
    private(set) var removeCalls = 0

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func removeAll() -> Bool {
        removeCalls += 1
        return !faults.fires("exports.remove")
    }
}

final class FakeResetCredentials: ResetCredentialRemoving {
    struct DeleteFailed: Error {}

    private let faults: FaultPlan
    private let log: ResetEventLog
    private(set) var removedReferences: [String] = []

    init(faults: FaultPlan, log: ResetEventLog) {
        self.faults = faults
        self.log = log
    }

    func deleteCredential(reference: String) throws {
        if faults.fires("credentials.delete") { throw DeleteFailed() }
        log.append("credentials.delete:\(reference)")
        removedReferences.append(reference)
    }
}

final class MemoryResetMarkerStore: ResetMarkerStore {
    struct WriteFailed: Error {}

    private let faults: FaultPlan
    private(set) var isRequested = false
    private(set) var isOutstanding = false

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func isRemovalRequested() throws -> Bool { isRequested }

    func recordRemovalRequested() throws {
        if faults.fires("choice.record") { throw WriteFailed() }
        isRequested = true
    }

    func clearRemovalRequest() throws {
        if faults.fires("choice.clear") { throw WriteFailed() }
        isRequested = false
    }

    func isCleanupOutstanding() throws -> Bool { isOutstanding }

    func recordCleanupOutstanding() throws {
        if faults.fires("outstanding.record") { throw WriteFailed() }
        isOutstanding = true
    }

    func clearCleanupOutstanding() throws {
        if faults.fires("outstanding.clear") { throw WriteFailed() }
        isOutstanding = false
    }
}

/// Everything a reset service test touches. Native storage, notifications and the cleanup queue come from
/// `DeletionWorld`; the reset core, staging, temporaries and credential fakes share its fault plan. The fakes hold the
/// durable state, so `makeService()` again is a relaunch: a new service over the same disk, notification center, core,
/// staging and choice.
final class ResetWorld {
    let deletion: DeletionWorld
    let log = ResetEventLog()
    let core: FakeResetCore
    let staging: FakeResetStaging
    let exports: FakeResetExportTemporaries
    let credentials: FakeResetCredentials
    let choice: MemoryResetMarkerStore

    var faults: FaultPlan { deletion.faults }

    init(deletion: DeletionWorld) {
        self.deletion = deletion
        core = FakeResetCore(deletion: deletion.core, faults: deletion.faults, log: log)
        staging = FakeResetStaging(faults: deletion.faults, log: log)
        exports = FakeResetExportTemporaries(faults: deletion.faults)
        credentials = FakeResetCredentials(faults: deletion.faults, log: log)
        choice = MemoryResetMarkerStore(faults: deletion.faults)
    }

    func makeService() -> ResetService {
        ResetService(
            core: core,
            staging: staging,
            itemCleanup: deletion.makeService(),
            exportTemporaries: exports,
            credentials: credentials,
            markers: choice)
    }

    /// An item with its files and notifications, registered with the reset core as captured before any reset now.
    func seedItem(
        _ itemID: String,
        captureID: String,
        finalizedAudio: Bool = true,
        stagedRecord: StagedRecordKind = .none,
        interruptedRecordWrite: Bool = false,
        reminders: [String: [Int64]] = [:]
    ) async throws {
        try await deletion.seedItem(
            itemID, captureID: captureID, finalizedAudio: finalizedAudio, stagedRecord: stagedRecord,
            interruptedRecordWrite: interruptedRecordWrite, reminders: reminders)
        core.register(itemID)
    }

    /// A staged input whose import creates an item with no files of its own.
    func makeImportedItemOnImport() {
        staging.onImport = { [deletion, core] captureID in
            let itemID = "item-\(captureID)"
            deletion.core.addItem(itemID, captureID: captureID, audioReference: nil)
            core.register(itemID)
        }
    }
}

class ResetTestCase: DeletionTestCase {
    func makeResetWorld() throws -> ResetWorld {
        ResetWorld(deletion: try makeWorld())
    }
}
