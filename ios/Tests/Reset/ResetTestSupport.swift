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

/// A process kill. The fakes call this at a point a test armed as `crash:<point>`: the awaiting task never resumes, so no
/// error path, cleanup or later step of that call ever runs, while everything already written stays as it was. The test
/// then builds a new service over the same durable state, which is the relaunch.
final class ResetCrashSignal: @unchecked Sendable {
    private let lock = NSLock()
    private var reachedPoints: [String] = []
    private var onReach: (() -> Void)?

    var reached: [String] {
        lock.lock()
        defer { lock.unlock() }
        return reachedPoints
    }

    /// Calls `handler` once, the next time a crash point is reached.
    func notifyOnNextCrash(_ handler: @escaping () -> Void) {
        lock.lock()
        onReach = handler
        lock.unlock()
    }

    func crashIfArmed(_ point: String, faults: FaultPlan) async {
        guard faults.fires("crash:" + point) else { return }
        lock.lock()
        reachedPoints.append(point)
        let handler = onReach
        onReach = nil
        lock.unlock()
        handler?()
        await withUnsafeContinuation { (_: UnsafeContinuation<Void, Never>) in }
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
    let crash: ResetCrashSignal
    private var liveItems: [String] = []
    private var boundary: [String] = []
    private var generation: Int64 = 0
    private var phase: ResetPhase?
    private var profiles: [ResetProfileCredential] = []
    private var revokedVersions: [String] = []
    private var starts = 0

    init(
        deletion: DurableDeletionCore, faults: FaultPlan, log: ResetEventLog = ResetEventLog(),
        crash: ResetCrashSignal = ResetCrashSignal()
    ) {
        self.deletion = deletion
        self.faults = faults
        self.log = log
        self.crash = crash
    }

    // MARK: Test controls

    /// An item exists now. It belongs to the generation current at this moment.
    func register(_ itemID: String) {
        locked { liveItems.append(itemID) }
    }

    func setProfiles(_ newProfiles: [ResetProfileCredential]) {
        locked { profiles = newProfiles }
    }

    /// The user configures a profile now, whatever generation is current.
    func addProfile(_ profile: ResetProfileCredential) {
        locked { profiles.append(profile) }
    }

    func isProfileRevoked(_ profileVersion: String) -> Bool? {
        locked { profiles.first { $0.profileVersion == profileVersion }?.isRevoked }
    }

    var currentGeneration: Int64 { locked { generation } }
    var startCalls: Int { locked { starts } }
    var revokedProfileVersions: [String] { locked { revokedVersions } }

    // MARK: ResetCore

    func start() async throws -> ResetProgress {
        try faults.check("reset.start")
        await crash.crashIfArmed("before.start", faults: faults)
        log.append("core.start")
        let progress = locked { () -> ResetProgress in
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
        await crash.crashIfArmed("after.start", faults: faults)
        return progress
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

/// Native ingress staging scripted by capture ID. `importable` inputs that the snapshot names import (and become items)
/// when `importSnapshot` runs; every other staged input stays unimported until it is purged or, when it was staged after
/// the snapshot, left alone.
final class FakeResetStaging: ResetIngressStaging {
    private let faults: FaultPlan
    private let log: ResetEventLog
    private let crash: ResetCrashSignal
    private(set) var staged: Set<String> = []
    private(set) var importedCaptureIDs: [String] = []
    private(set) var purgedCaptureIDs: [String] = []
    private(set) var snapshotCalls = 0
    var importable: Set<String> = []
    /// Called with the capture ID when an input imports, so the test can create the item the core would hold.
    var onImport: ((String) -> Void)?
    /// Called right after the snapshot is taken: the moment a capture can land after the tap.
    var onSnapshot: (() -> Void)?
    /// Called at the start of `importSnapshot`, on the task that runs the reset.
    var onImportSnapshot: (() -> Void)?
    /// True from a successful snapshot (or `holdBoundary`) until `endBoundary`: live captures wait, as at the production
    /// ingress gate.
    private(set) var isHolding = false
    private(set) var heldSnapshots: [ResetStagingSnapshot] = []
    private(set) var boundaryReleases = 0
    private(set) var heldCaptureIDs: [String] = []
    private(set) var liveImportedCaptureIDs: [String] = []
    /// Called with the capture ID when a live capture imports, so the test can create the item the core would hold.
    var onLiveImport: ((String) -> Void)?

    init(faults: FaultPlan, log: ResetEventLog, crash: ResetCrashSignal) {
        self.faults = faults
        self.log = log
        self.crash = crash
    }

    func stage(_ captureID: String) {
        staged.insert(captureID)
    }

    /// A capture the foreground app makes now: staged durably, imported at once unless the boundary is held.
    func liveCapture(_ captureID: String) {
        staged.insert(captureID)
        if isHolding {
            heldCaptureIDs.append(captureID)
        } else {
            importLive(captureID)
        }
    }

    private func importLive(_ captureID: String) {
        staged.remove(captureID)
        liveImportedCaptureIDs.append(captureID)
        log.append("staging.live.import:\(captureID)")
        onLiveImport?(captureID)
    }

    func snapshot() async throws -> ResetStagingSnapshot {
        snapshotCalls += 1
        log.append("staging.snapshot")
        try faults.check("staging.snapshot")
        let taken = ResetStagingSnapshot(captureIDs: staged)
        isHolding = true
        forwardHeld(namedBy: taken)
        onSnapshot?()
        return taken
    }

    /// As at the production gate: a waiting capture the new list names is pre-tap input of that tap, so it proceeds now.
    private func forwardHeld(namedBy snapshot: ResetStagingSnapshot) {
        let listed = heldCaptureIDs.filter { snapshot.captureIDs.contains($0) }
        heldCaptureIDs.removeAll { snapshot.captureIDs.contains($0) }
        for captureID in listed { importLive(captureID) }
    }

    /// A process start, as the production ingress gate is created: a capture that was waiting is still staged and is
    /// submitted again by ingress recovery, and the gate holds exactly when a tap is recorded.
    func relaunch(recordedRequest: ResetRequest?) {
        isHolding = recordedRequest != nil
        if !isHolding {
            let waiting = heldCaptureIDs
            heldCaptureIDs = []
            for captureID in waiting { importLive(captureID) }
        }
    }

    func holdBoundary(for snapshot: ResetStagingSnapshot) async {
        log.append("staging.hold")
        heldSnapshots.append(snapshot)
        isHolding = true
        forwardHeld(namedBy: snapshot)
    }

    func endBoundary() {
        log.append("staging.release")
        isHolding = false
        boundaryReleases += 1
        let waiting = heldCaptureIDs
        heldCaptureIDs = []
        for captureID in waiting { importLive(captureID) }
    }

    func importSnapshot(_ snapshot: ResetStagingSnapshot) async {
        log.append("staging.import")
        onImportSnapshot?()
        await crash.crashIfArmed("staging.import", faults: faults)
        if faults.fires("staging.import") { return }
        for captureID in staged.intersection(snapshot.captureIDs).intersection(importable).sorted() {
            staged.remove(captureID)
            importedCaptureIDs.append(captureID)
            onImport?(captureID)
        }
    }

    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge {
        log.append("staging.purge")
        await crash.crashIfArmed("staging.purge", faults: faults)
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
    private(set) var removal: ResetCredentialRemoval?
    private(set) var isOutstanding = false
    private(set) var recordedRequest: ResetRequest?

    /// Whether a removal is owed.
    var isRequested: Bool { removal != nil }
    /// The credential choice of the recorded tap.
    var requested: ResetCredentialChoice? { recordedRequest?.credentialChoice }

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func request() throws -> ResetRequest? {
        if faults.fires("intent.read") { throw WriteFailed() }
        return recordedRequest
    }

    func recordRequest(_ request: ResetRequest) throws {
        if faults.fires("intent.record") { throw WriteFailed() }
        recordedRequest = request
    }

    func clearRequest() throws {
        if faults.fires("intent.clear") { throw WriteFailed() }
        recordedRequest = nil
    }

    func removalRequest() throws -> ResetCredentialRemoval? { removal }

    func recordRemovalRequested(_ newRemoval: ResetCredentialRemoval) throws {
        if faults.fires("choice.record") { throw WriteFailed() }
        if removal == nil { removal = newRemoval }
    }

    func clearRemovalRequest() throws {
        if faults.fires("choice.clear") { throw WriteFailed() }
        removal = nil
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
    let crash = ResetCrashSignal()
    let core: FakeResetCore
    let staging: FakeResetStaging
    let exports: FakeResetExportTemporaries
    let credentials: FakeResetCredentials
    let choice: MemoryResetMarkerStore

    var faults: FaultPlan { deletion.faults }

    init(deletion: DeletionWorld) {
        self.deletion = deletion
        core = FakeResetCore(deletion: deletion.core, faults: deletion.faults, log: log, crash: crash)
        staging = FakeResetStaging(faults: deletion.faults, log: log, crash: crash)
        exports = FakeResetExportTemporaries(faults: deletion.faults)
        credentials = FakeResetCredentials(faults: deletion.faults, log: log)
        choice = MemoryResetMarkerStore(faults: deletion.faults)
    }

    /// A process start after a kill: the ingress gate is created from the recorded tap, and the next `makeService()` is
    /// the new process's reset service.
    func relaunch() {
        staging.relaunch(recordedRequest: choice.recordedRequest)
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

/// Wraps the real file system and fails selected operations once, by point name `fs.<operation>:<file name>` or
/// `fs.entryNames`. A failed removal after a write leaves exactly the files a kill at that moment would leave.
final class FaultingResetMarkerFileSystem: IngressFileSystem {
    private let wrapped = FileManagerIngressFileSystem()
    private let faults: FaultPlan

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func fileExists(at url: URL) -> Bool { wrapped.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try wrapped.fileSize(at: url) }

    func entryNames(in directory: URL) throws -> [String] {
        try faults.check("fs.entryNames")
        return try wrapped.entryNames(in: directory)
    }

    func read(from url: URL) throws -> Data {
        try faults.check("fs.read:\(url.lastPathComponent)")
        return try wrapped.read(from: url)
    }

    func writeDurably(_ data: Data, to url: URL) throws {
        try faults.check("fs.write:\(url.lastPathComponent)")
        try wrapped.writeDurably(data, to: url)
    }

    func move(from source: URL, to destination: URL) throws { try wrapped.move(from: source, to: destination) }

    func remove(at url: URL) throws {
        try faults.check("fs.remove:\(url.lastPathComponent)")
        try wrapped.remove(at: url)
    }
}

class ResetTestCase: DeletionTestCase {
    func makeResetWorld() throws -> ResetWorld {
        ResetWorld(deletion: try makeWorld())
    }
}
