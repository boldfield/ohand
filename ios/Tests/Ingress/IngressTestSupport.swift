import Foundation
import XCTest
@testable import OhAndServices

/// Scripted importer: records every request and answers from a queue, so service behavior can be tested without a core.
final class FakeForegroundIngressImporter: ForegroundIngressImporting {
    private(set) var importedRecords: [IngressRecord] = []
    var results: [IngressImportResult] = []
    /// When true, completions are held until `release()` so in-flight behavior can be observed.
    var holdsCompletions = false
    private var held: [() -> Void] = []
    /// Called with the record when an import starts; lets a test observe disk state at that moment.
    var onImport: ((IngressRecord) -> Void)?

    func importRecord(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void) {
        importedRecords.append(record)
        onImport?(record)
        let result = results.isEmpty ? .failed(.notCommitted) : results.removeFirst()
        if holdsCompletions {
            held.append { completion(result) }
        } else {
            completion(result)
        }
    }

    func release() {
        let pending = held
        held = []
        pending.forEach { $0() }
    }

    static func confirmation(_ captureID: String, itemID: String = "item-1", already: Bool = false) -> IngressImportResult {
        .confirmed(IngressImportConfirmation(
            captureID: captureID, itemID: itemID, savedAt: "2026-10-08T09:30:02Z", alreadyImported: already))
    }
}

/// Wraps the real file system and fails selected operations, to prove an interrupted step loses nothing.
final class FailingIngressFileSystem: IngressFileSystem {
    enum Operation: Equatable { case write, move, remove, read }

    struct InjectedFailure: Error {}

    private let wrapped = FileManagerIngressFileSystem()
    var failing: Set<Operation> = []

    func fileExists(at url: URL) -> Bool { wrapped.fileExists(at: url) }
    func fileSize(at url: URL) throws -> Int { try wrapped.fileSize(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try wrapped.entryNames(in: directory) }

    func read(from url: URL) throws -> Data {
        if failing.contains(.read) { throw InjectedFailure() }
        return try wrapped.read(from: url)
    }

    func writeDurably(_ data: Data, to url: URL) throws {
        if failing.contains(.write) { throw InjectedFailure() }
        try wrapped.writeDurably(data, to: url)
    }

    func move(from source: URL, to destination: URL) throws {
        if failing.contains(.move) { throw InjectedFailure() }
        try wrapped.move(from: source, to: destination)
    }

    func remove(at url: URL) throws {
        if failing.contains(.remove) { throw InjectedFailure() }
        try wrapped.remove(at: url)
    }
}

/// A prepared protected storage tree under a unique temporary root.
class IngressStorageTestCase: XCTestCase {
    var layout: ProtectedStorageLayout!

    override func setUpWithError() throws {
        try super.setUpWithError()
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-ingress-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        XCTAssertTrue(ProtectedStorageService(layout: layout).prepare().isFullyConfigured)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
        super.tearDown()
    }

    var context: IngressCaptureContext {
        IngressCaptureContext(
            captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: "route-personal", entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z", sessionTopic: nil)
    }

    func textRecord(_ captureID: String, text: String = "buy oat milk") -> IngressRecord {
        IngressRecord(captureID: captureID, text: text, audio: nil, context: context)
    }

    /// Places synthetic recorded bytes in the in-progress audio store and returns the record that hands them off.
    func audioRecord(_ captureID: String, bytes: Data = Data("synthetic audio".utf8)) throws -> IngressRecord {
        let handoff = IngressAudioHandoff(inProgressFileName: "\(captureID).m4a", finalizedFileName: "\(captureID).m4a")
        try bytes.write(to: layout.directory(for: .ingressInProgressAudio).appendingPathComponent(handoff.inProgressFileName))
        return IngressRecord(captureID: captureID, text: nil, audio: handoff, context: context)
    }

    func recordURL(_ captureID: String) -> URL {
        layout.directory(for: .ingressStagingRecords).appendingPathComponent("\(captureID).json")
    }

    func inProgressAudioURL(_ name: String) -> URL { layout.directory(for: .ingressInProgressAudio).appendingPathComponent(name) }
    func finalizedAudioURL(_ name: String) -> URL { layout.directory(for: .finalizedAudio).appendingPathComponent(name) }
    func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    func makeService(
        importer: ForegroundIngressImporting,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) throws -> ForegroundIngressService {
        try ForegroundIngressService(layout: layout, importer: importer, fileSystem: fileSystem)
    }

    func submit(_ service: ForegroundIngressService, _ record: IngressRecord) -> IngressOutcome? {
        var outcome: IngressOutcome?
        service.submit(record) { outcome = $0 }
        return outcome
    }

    func recover(_ service: ForegroundIngressService) -> IngressRecoveryReport? {
        var report: IngressRecoveryReport?
        service.recover { report = $0 }
        return report
    }
}
