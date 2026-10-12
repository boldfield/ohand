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
final class ScriptedRetentionCore: AudioRetentionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var listing: [AudioRetentionItem] = []
    private var decisions: [String: AudioRemovalDecision] = [:]
    private var sweepFailure: Error?
    private var decisionFailure: Error?
    private(set) var authorizedRequests: [String] = []
    private(set) var sweepPressures: [CoreRetentionPressure] = []

    func setListing(_ items: [AudioRetentionItem]) { locked { listing = items } }
    var currentListing: [AudioRetentionItem] { locked { listing } }
    func setDecision(_ decision: AudioRemovalDecision, itemID: String) { locked { decisions[itemID] = decision } }
    func failSweep(_ error: Error?) { locked { sweepFailure = error } }
    func failDecisions(_ error: Error?) { locked { decisionFailure = error } }

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

/// Wraps the real file system and fails selected operations once, by point name: `fs.remove:<file name>`,
/// `fs.read:<file name>` and `fs.replace`. Removal can also be made to "succeed" without removing, to prove a surviving file is not reported gone.
final class FaultingRetentionFileSystem: AudioRetentionFileSystem, @unchecked Sendable {
    private let wrapped = FileManagerAudioRetentionFileSystem()
    private let faults: FaultPlan
    private let lock = NSLock()
    private var silentlyKept: Set<String> = []
    private(set) var removedFileNames: [String] = []

    init(faults: FaultPlan) {
        self.faults = faults
    }

    func keepSilently(_ fileName: String) {
        lock.lock()
        silentlyKept.insert(fileName)
        lock.unlock()
    }

    func fileExists(at url: URL) -> Bool { wrapped.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try wrapped.fileSize(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try wrapped.entryNames(in: directory) }
    func read(from url: URL) throws -> Data {
        try faults.check("fs.read:\(url.lastPathComponent)")
        return try wrapped.read(from: url)
    }
    func writeDurably(_ data: Data, to url: URL) throws { try wrapped.writeDurably(data, to: url) }
    func move(from source: URL, to destination: URL) throws { try wrapped.move(from: source, to: destination) }

    func remove(at url: URL) throws {
        try faults.check("fs.remove:\(url.lastPathComponent)")
        lock.lock()
        let keep = silentlyKept.contains(url.lastPathComponent)
        if !keep { removedFileNames.append(url.lastPathComponent) }
        lock.unlock()
        if keep { return }
        try wrapped.remove(at: url)
    }

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
