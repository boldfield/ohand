import Foundation
import XCTest
@testable import OhAndServices

struct StorageSetupFailed: Error {}

struct InjectedDeletionFault: Error, Equatable {
    let point: String
}

/// One-shot faults named by point. A point throws the first time it is checked and then disarms, so a test proves both
/// that the interruption happened and that the next pass converges.
final class FaultPlan: @unchecked Sendable {
    private let lock = NSLock()
    private var armed: Set<String> = []
    private var fired: [String] = []

    func arm(_ point: String) {
        locked { _ = armed.insert(point) }
    }

    func disarmAll() {
        locked { armed.removeAll() }
    }

    func check(_ point: String) throws {
        let shouldFail = locked { () -> Bool in
            guard armed.remove(point) != nil else { return false }
            fired.append(point)
            return true
        }
        if shouldFail { throw InjectedDeletionFault(point: point) }
    }

    var firedPoints: [String] { locked { fired } }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
}

/// A core whose deletion queue survives "reopen": the test keeps the one instance and builds new services against it.
/// It follows the L01 contract: a tombstone removes searchability and readable text and queues one row per effect; a
/// notification row cannot complete while a cancel of the item's reminder is unrecorded; completing the audio row
/// releases the audio reference.
final class DurableDeletionCore: DeletionCleanupCore, @unchecked Sendable {
    enum WorkStatus: Equatable { case pending, completed, failed }

    struct Item {
        var captureID: String
        var audioReference: String?
        var searchable = true
        var readableText: String? = "private synthetic text"
        var isDeleted = false
    }

    struct Work {
        let id: String
        let itemID: String
        let kind: DeletionCleanupKind
        var status: WorkStatus
    }

    struct ReminderOperation {
        let identifier: NotificationIdentifier
        var cancelled: Bool
    }

    struct RefusedCompletion: Error {}

    private let lock = NSLock()
    private let faults: FaultPlan
    private var items: [String: Item] = [:]
    private var works: [Work] = []
    private var reminders: [String: [ReminderOperation]] = [:]
    private(set) var mutationCount = 0

    init(faults: FaultPlan) {
        self.faults = faults
    }

    // MARK: Seeding and inspection

    func addItem(_ itemID: String, captureID: String, audioReference: String?) {
        locked { items[itemID] = Item(captureID: captureID, audioReference: audioReference) }
    }

    func addReminderNotification(_ itemID: String, _ identifier: NotificationIdentifier) {
        locked { reminders[itemID, default: []].append(ReminderOperation(identifier: identifier, cancelled: false)) }
    }

    func recordDeletion(_ itemID: String) {
        locked {
            guard var item = items[itemID] else { return }
            item.searchable = false
            item.readableText = nil
            item.isDeleted = true
            items[itemID] = item
            var kinds: [DeletionCleanupKind] = [.cancelNotifications, .clearIngress]
            if item.audioReference != nil { kinds.append(.removeAudio) }
            for kind in kinds {
                works.append(Work(id: "work:\(itemID):\(kind.rawValue)", itemID: itemID, kind: kind, status: .pending))
            }
        }
    }

    func isSearchable(_ itemID: String) -> Bool { locked { items[itemID]?.searchable ?? false } }
    func readableText(_ itemID: String) -> String? { locked { items[itemID]?.readableText } }
    func audioReference(_ itemID: String) -> String? { locked { items[itemID]?.audioReference } }

    func status(_ itemID: String, _ kind: DeletionCleanupKind) -> WorkStatus? {
        locked { works.first { $0.itemID == itemID && $0.kind == kind }?.status }
    }

    func isDeletionComplete(_ itemID: String) -> Bool {
        locked {
            let rows = works.filter { $0.itemID == itemID }
            return !rows.isEmpty && rows.allSatisfy { $0.status == .completed }
        }
    }

    // MARK: DeletionCleanupCore

    func itemsWithPendingCleanup() async throws -> [String] {
        try faults.check("core.itemsWithPendingCleanup")
        return locked { Array(Set(works.filter { $0.status == .pending }.map { $0.itemID })) }
    }

    func pendingCleanup(itemID: String) async throws -> [DeletionCleanupWork] {
        try faults.check("core.pendingCleanup")
        return locked {
            works.filter { $0.itemID == itemID && $0.status == .pending }
                .map { DeletionCleanupWork(workID: $0.id, itemID: $0.itemID, kind: $0.kind) }
        }
    }

    func terminallyFailedCleanup() async throws -> [DeletionCleanupWork] {
        try faults.check("core.terminalFailures")
        return locked {
            works.filter { $0.status == .failed }
                .sorted { ($0.itemID, $0.id) < ($1.itemID, $1.id) }
                .map { DeletionCleanupWork(workID: $0.id, itemID: $0.itemID, kind: $0.kind) }
        }
    }

    func cleanupTarget(itemID: String) async throws -> DeletionCleanupTarget? {
        try faults.check("core.cleanupTarget")
        return locked {
            items[itemID].map { DeletionCleanupTarget(captureID: $0.captureID, audioReference: $0.audioReference) }
        }
    }

    func reminderNotificationIdentifiers(itemID: String) async throws -> [NotificationIdentifier] {
        try faults.check("core.reminderIdentifiers")
        return locked { reminders[itemID, default: []].map { $0.identifier } }
    }

    func recordNotificationCancelled(_ identifier: NotificationIdentifier) async throws {
        try faults.check("core.recordCancelled")
        locked {
            mutationCount += 1
            for (itemID, operations) in reminders {
                var updated = operations
                for index in updated.indices where updated[index].identifier == identifier {
                    updated[index].cancelled = true
                }
                reminders[itemID] = updated
            }
        }
    }

    func completeCleanup(workID: String) async throws {
        guard let kind = locked({ works.first { $0.id == workID }?.kind }) else { throw RefusedCompletion() }
        try faults.check("core.complete.\(kind.rawValue)")
        try locked {
            guard let index = works.firstIndex(where: { $0.id == workID }) else { throw RefusedCompletion() }
            guard works[index].status != .failed else { throw RefusedCompletion() }
            let itemID = works[index].itemID
            if kind == .cancelNotifications, reminders[itemID, default: []].contains(where: { !$0.cancelled }) {
                throw RefusedCompletion()
            }
            if kind == .removeAudio { items[itemID]?.audioReference = nil }
            if works[index].status != .completed { mutationCount += 1 }
            works[index].status = .completed
        }
    }

    func failCleanup(workID: String) async throws {
        try faults.check("core.fail")
        try locked {
            guard let index = works.firstIndex(where: { $0.id == workID }), works[index].status != .completed else {
                throw RefusedCompletion()
            }
            if works[index].status != .failed { mutationCount += 1 }
            works[index].status = .failed
        }
    }

    private func locked<Value>(_ body: () throws -> Value) rethrows -> Value {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }
}

/// Wraps the real file system and fails selected operations once, by point name `fs.<operation>:<file name>` (or
/// `fs.entryNames`), to reproduce an interruption at any file step.
final class FaultingDeletionFileSystem: IngressFileSystem {
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

    func writeDurably(_ data: Data, to url: URL) throws { try wrapped.writeDurably(data, to: url) }
    func move(from source: URL, to destination: URL) throws { try wrapped.move(from: source, to: destination) }

    func remove(at url: URL) throws {
        try faults.check("fs.remove:\(url.lastPathComponent)")
        try wrapped.remove(at: url)
    }
}

/// The production bridge behind fault points before and after the real cancel, so an interruption can land with the
/// request removed and the core not yet told.
final class FaultingNotificationCanceller: DeletionNotificationCancelling, @unchecked Sendable {
    private let bridge: NotificationBridge
    private let faults: FaultPlan

    init(bridge: NotificationBridge, faults: FaultPlan) {
        self.bridge = bridge
        self.faults = faults
    }

    func cancel(_ identifier: NotificationIdentifier) async throws {
        try faults.check("notifier.cancel")
        try await bridge.cancel(identifier)
        try faults.check("notifier.cancel.afterEffect")
    }

    func pendingNotifications() async throws -> [PendingNotification] {
        try faults.check("notifier.pending")
        return try await bridge.pendingNotifications()
    }
}

enum StagedRecordKind {
    case none
    case text
    case audio
}

/// Everything a deletion test touches: protected storage on disk, the fake OS notification center, and the durable core.
/// `makeService()` builds a fresh service and notification bridge over the same state, which is what reopening the app
/// after an interruption does.
final class DeletionWorld {
    let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    let layout: ProtectedStorageLayout
    let center = FakeNotificationCenter()
    let faults: FaultPlan
    let core: DurableDeletionCore

    init() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-deletion-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        let plan = FaultPlan()
        faults = plan
        core = DurableDeletionCore(faults: plan)
        guard ProtectedStorageService(layout: layout).prepare().isFullyConfigured else {
            throw StorageSetupFailed()
        }
    }

    func removeStorage() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
    }

    func makeBridge() -> NotificationBridge {
        let now = currentTime
        return NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { _ in }),
            now: { now },
            removalPollInterval: 0,
            maximumRemovalPolls: 5
        )
    }

    func makeService() -> DeletionCleanupService {
        DeletionCleanupService(
            core: core,
            notifications: FaultingNotificationCanceller(bridge: makeBridge(), faults: faults),
            localCleanup: DeletionLocalCleanup(layout: layout, fileSystem: FaultingDeletionFileSystem(faults: faults))
        )
    }

    // MARK: Files

    func url(_ store: ProtectedStore, _ name: String) -> URL {
        layout.directory(for: store).appendingPathComponent(name)
    }

    func exists(_ store: ProtectedStore, _ name: String) -> Bool {
        FileManager.default.fileExists(atPath: url(store, name).path)
    }

    /// Paths anywhere under the storage root whose name mentions `captureID`.
    func remnants(of captureID: String) -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: layout.rootDirectory.path)) ?? []
        return subpaths.filter { ($0 as NSString).lastPathComponent.contains(captureID) }.sorted()
    }

    var context: IngressCaptureContext {
        IngressCaptureContext(
            captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: "route-personal", entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z", sessionTopic: nil)
    }

    // MARK: Seeding

    /// Creates an item with its capture's files and notifications. `reminders` maps a reminder identifier to the
    /// schedule generations installed in the fake OS. The item is not deleted until `core.recordDeletion` runs.
    func seedItem(
        _ itemID: String,
        captureID: String,
        finalizedAudio: Bool = true,
        stagedRecord: StagedRecordKind = .none,
        interruptedRecordWrite: Bool = false,
        reminders: [String: [Int64]] = [:]
    ) async throws {
        var audioReference: String?
        if finalizedAudio {
            let name = "\(captureID).m4a"
            try Data("synthetic audio".utf8).write(to: url(.finalizedAudio, name))
            audioReference = IngressRecord.audioReference(finalizedFileName: name)
        }
        let staging = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        switch stagedRecord {
        case .none:
            break
        case .text:
            _ = try staging.stage(IngressRecord(captureID: captureID, text: "buy oat milk", audio: nil, context: context))
        case .audio:
            let handoff = IngressAudioHandoff(
                inProgressFileName: "\(captureID)-recording.m4a", finalizedFileName: "\(captureID).m4a")
            try Data("synthetic audio".utf8).write(to: url(.ingressInProgressAudio, handoff.inProgressFileName))
            _ = try staging.stage(IngressRecord(captureID: captureID, text: nil, audio: handoff, context: context))
        }
        if interruptedRecordWrite {
            try Data("{\"text\":\"buy oat".utf8).write(to: url(.ingressStagingRecords, "\(captureID).json.incomplete"))
        }

        core.addItem(itemID, captureID: captureID, audioReference: audioReference)
        let bridge = makeBridge()
        let target = try XCTUnwrap(OpaqueIdentifier(itemID))
        for (reminderID, generations) in reminders {
            for generation in generations {
                let identifier = try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: generation))
                _ = try await bridge.schedule(.generic(
                    identifier: identifier, dueInstant: currentTime.addingTimeInterval(3600), opaqueTargetID: target))
                core.addReminderNotification(itemID, identifier)
            }
        }
    }

    func pendingNotificationIdentifiers(ofReminder reminderID: String) -> [String] {
        center.pendingIdentifiers.filter { $0.hasPrefix(reminderID + "#") }
    }
}

class DeletionTestCase: XCTestCase {
    private var worlds: [DeletionWorld] = []

    func makeWorld() throws -> DeletionWorld {
        let world = try DeletionWorld()
        worlds.append(world)
        return world
    }

    override func tearDown() {
        worlds.forEach { $0.removeStorage() }
        worlds = []
        super.tearDown()
    }
}
