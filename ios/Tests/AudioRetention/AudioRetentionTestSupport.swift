import Foundation
@testable import OhAndCoreBridge
@testable import OhAndServices

struct RetentionFixtureError: Error {}

enum RetentionFixtures {
    static func audioReference(_ fileName: String) -> String {
        IngressRecord.audioReference(finalizedFileName: fileName)
    }

    static func item(
        _ itemID: String,
        audio fileName: String,
        status: AudioRetentionStatus = .expired(expiredAt: "2026-10-05T09:00:00Z"),
        soleSource: Bool = false
    ) -> AudioRetentionItem {
        let reference = audioReference(fileName)
        let cleanup: AudioCleanupIntent
        if case .expired(let expiredAt) = status, !soleSource {
            cleanup = .removeAudio(audioReference: reference, expiredAt: expiredAt)
        } else {
            cleanup = .none
        }
        return AudioRetentionItem(
            itemID: itemID, audioReference: reference, status: status, isSoleSource: soleSource,
            options: soleSource ? [.retryTranscription, .deleteAudio] : [.deleteAudio], cleanup: cleanup,
            storageNotice: nil)
    }
}

/// A core whose sweep listing and removal-time decisions a test scripts separately, so a listing can be made stale.
/// `removeAudio` behaves like the real core: it denies what the script denies, and otherwise runs the caller's effect.
final class ScriptedRetentionCore: AudioRetentionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var listing: [AudioRetentionItem] = []
    private var decisions: [String: AudioRemovalDecision] = [:]
    private var sweepFailure: Error?
    private var decisionFailure: Error?
    private var removalFailures: Set<String> = []
    private var failedEffects: Set<String> = []
    private var silentlyKept: Set<String> = []
    private var lapsedLeases: Set<String> = []
    private(set) var authorizedRequests: [String] = []
    private(set) var removalRequests: [String] = []
    private(set) var removedFileNames: [String] = []
    private(set) var sweepPressures: [CoreRetentionPressure] = []

    func setListing(_ items: [AudioRetentionItem]) { locked { listing = items } }
    var currentListing: [AudioRetentionItem] { locked { listing } }
    func setDecision(_ decision: AudioRemovalDecision, itemID: String) { locked { decisions[itemID] = decision } }
    func failSweep(_ error: Error?) { locked { sweepFailure = error } }
    func failDecisions(_ error: Error?) { locked { decisionFailure = error } }
    /// The next removal of this file never reaches the core: nothing is touched.
    func interruptRemoval(of fileName: String) { locked { _ = removalFailures.insert(fileName) } }
    /// The next removal of this file is approved but the core reports it could not remove it.
    func failEffect(of fileName: String) { locked { _ = failedEffects.insert(fileName) } }
    /// The next removal of this file runs its effect, but the core then reports that the fence lapsed before the end.
    func lapseLease(of fileName: String) { locked { _ = lapsedLeases.insert(fileName) } }
    /// Removals of this file are reported as done although the file stays.
    func keepSilently(_ fileName: String) { locked { _ = silentlyKept.insert(fileName) } }

    func sweep(pressure: CoreRetentionPressure, periodSeconds: UInt64) async throws -> [AudioRetentionItem] {
        try locked {
            sweepPressures.append(pressure)
            if let sweepFailure { throw sweepFailure }
            return listing
        }
    }

    func authorizeRemoval(itemID: String, audioReference: String, periodSeconds: UInt64) async throws
        -> AudioRemovalDecision
    {
        try locked {
            authorizedRequests.append(itemID)
            if let decisionFailure { throw decisionFailure }
            return decisions[itemID] ?? .approved
        }
    }

    func removeAudio(itemID: String, audioReference: String, periodSeconds: UInt64, effect: () -> AudioRemovalEffect)
        async throws -> AudioRemovalResult
    {
        try locked {
            removalRequests.append(itemID)
            let fileName = (audioReference as NSString).lastPathComponent
            if removalFailures.remove(fileName) != nil { throw RetentionFixtureError() }
            if let decisionFailure { throw decisionFailure }
            if case .denied(let denial)? = decisions[itemID] { return .denied(denial) }
            if failedEffects.remove(fileName) != nil { return .approved(.failed) }
            if silentlyKept.contains(fileName) { return .approved(.removed) }
            let performed = effect()
            if performed == .removed { removedFileNames.append(fileName) }
            if lapsedLeases.remove(fileName) != nil { throw CoreAudioRetentionClient.leaseTooOld }
            return .approved(performed)
        }
    }

    private func locked<Value>(_ body: () throws -> Value) rethrows -> Value {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }
}

struct FixedPressureProbe: AudioStoragePressureProbing {
    let pressure: CoreRetentionPressure
    func currentPressure() -> CoreRetentionPressure { pressure }
}

/// Wraps the real file system and fails selected operations once, by point name: `fs.read:<file name>` and
/// `fs.replace`. It also records every read, to prove how often the ledger is read.
final class FaultingRetentionFileSystem: AudioRetentionFileSystem, @unchecked Sendable {
    private let wrapped = FileManagerAudioRetentionFileSystem()
    private let faults: FaultPlan
    private let lock = NSLock()
    private var reads: [String] = []

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func readCount(of fileName: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return reads.filter { $0 == fileName }.count
    }

    func fileExists(at url: URL) -> Bool { wrapped.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try wrapped.fileSize(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try wrapped.entryNames(in: directory) }
    func read(from url: URL) throws -> Data {
        lock.lock()
        reads.append(url.lastPathComponent)
        lock.unlock()
        try faults.check("fs.read:\(url.lastPathComponent)")
        return try wrapped.read(from: url)
    }
    func writeDurably(_ data: Data, to url: URL) throws { try wrapped.writeDurably(data, to: url) }
    func move(from source: URL, to destination: URL) throws { try wrapped.move(from: source, to: destination) }
    func remove(at url: URL) throws { try wrapped.remove(at: url) }

    func replaceDurably(_ data: Data, at url: URL) throws {
        try faults.check("fs.replace")
        try wrapped.replaceDurably(data, at: url)
    }
}

/// A protected storage tree in a temporary directory with synthetic finalized audio files.
final class RetentionStorage {
    let layout: ProtectedStorageLayout

    init() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-retention-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        guard ProtectedStorageService(layout: layout).prepare().isFullyConfigured else { throw StorageSetupFailed() }
    }

    func tearDown() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
    }

    func audioURL(_ fileName: String) -> URL {
        layout.directory(for: .finalizedAudio).appendingPathComponent(fileName)
    }

    func writeAudio(_ fileName: String) throws {
        try Data("synthetic audio".utf8).write(to: audioURL(fileName))
    }

    func audioExists(_ fileName: String) -> Bool {
        FileManager.default.fileExists(atPath: audioURL(fileName).path)
    }

    var ledgerURL: URL {
        layout.directory(for: .configuration).appendingPathComponent(AudioRetentionLedger.fileName)
    }

    func ledgerEntries() throws -> [AudioRetentionLedger.Entry] {
        try AudioRetentionLedger(layout: layout, fileSystem: FileManagerAudioRetentionFileSystem()).allEntries()
    }
}
