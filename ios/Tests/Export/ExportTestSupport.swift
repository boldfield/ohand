import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Holds a fake core call open until the test releases it, so a relock or cancellation can land mid-export.
final class ExportGate: @unchecked Sendable {
    private let lock = NSLock()
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            if isOpen {
                lock.unlock()
                continuation.resume()
            } else {
                waiters.append(continuation)
                lock.unlock()
            }
        }
    }

    func open() {
        lock.lock()
        isOpen = true
        let released = waiters
        waiters = []
        lock.unlock()
        released.forEach { $0.resume() }
    }
}

/// A core that follows the L04b contract without a database: it writes a synthetic export file to the requested path and
/// answers with the configured audio references (only when the request asked for them, as the real core does).
final class FakeExportCore: ExportCore, @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [CoreExportRequest] = []
    var audioReferences: [CoreExportAudioReference] = []
    var failure: Error?
    var gate: ExportGate?
    var writesExportFile = true
    var createsParentDirectory = false
    var itemCount = 2

    var requests: [CoreExportRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func export(_ request: CoreExportRequest) async throws -> CoreExportAnswer {
        lock.lock()
        recorded.append(request)
        lock.unlock()
        if let gate { await gate.wait() }
        if let failure { throw failure }
        let file = URL(fileURLWithPath: request.outputPath)
        if writesExportFile {
            if createsParentDirectory {
                try FileManager.default.createDirectory(
                    at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            }
            try Data("synthetic export".utf8).write(to: file)
        }
        return CoreExportAnswer(
            itemCount: itemCount,
            sha256: String(repeating: "0", count: 64),
            audioReferences: request.audio == .references ? audioReferences : [])
    }
}

/// The real file system with one-shot style switches for the failures an export must survive.
final class FaultingExportFileSystem: ExportFileSystem, @unchecked Sendable {
    struct Injected: Error {}

    private let real = FileManagerExportFileSystem()
    var failsCreateDirectory = false
    var failsProtectFile = false
    var failsRemove = false
    var failingCopyNames: Set<String> = []

    func createPrivateDirectory(at url: URL) throws {
        if failsCreateDirectory { throw Injected() }
        try real.createPrivateDirectory(at: url)
    }

    func protectFile(at url: URL) throws {
        if failsProtectFile { throw Injected() }
        try real.protectFile(at: url)
    }

    func copyFile(from source: URL, to destination: URL) throws {
        if failingCopyNames.contains(source.lastPathComponent) {
            try Data("partial".utf8).write(to: destination)
            throw Injected()
        }
        try real.copyFile(from: source, to: destination)
    }

    func fileExists(at url: URL) -> Bool { real.fileExists(at: url) }
    func entryNames(in directory: URL) throws -> [String] { try real.entryNames(in: directory) }

    func remove(at url: URL) throws {
        if failsRemove { throw Injected() }
        try real.remove(at: url)
    }
}

/// Protected storage under a temporary root, an authenticated-on-demand read session and a fake core.
final class ExportWorld {
    let layout: ProtectedStorageLayout
    let boundary = FakeReadAuthenticationBoundary()
    let session: ReadAuthenticationSession
    let core = FakeExportCore()
    let fileSystem = FaultingExportFileSystem()

    init() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-export-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        guard ProtectedStorageService(layout: layout).prepare().isFullyConfigured else { throw StorageSetupFailed() }
        session = ReadAuthenticationSession(boundary: boundary)
    }

    func tearDown() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
    }

    /// The boundary completes immediately, so the scope is already authenticated when this returns.
    func authenticate(file: StaticString = #filePath, line: UInt = #line) {
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated, file: file, line: line)
    }

    func makeService(identifiers: [String]? = nil, core overrideCore: ExportCore? = nil) -> ExportService {
        var remaining = identifiers ?? []
        let usesFixedIdentifiers = identifiers != nil
        let lock = NSLock()
        return ExportService(
            core: overrideCore ?? core,
            session: session,
            layout: layout,
            fileSystem: fileSystem,
            makeIdentifier: {
                guard usesFixedIdentifiers else { return UUID().uuidString.lowercased() }
                lock.lock()
                defer { lock.unlock() }
                return remaining.isEmpty ? UUID().uuidString.lowercased() : remaining.removeFirst()
            })
    }

    var exportsDirectory: URL {
        layout.directory(for: .temporaryFiles).appendingPathComponent(ExportService.exportsDirectoryName)
    }

    /// Names directly inside the exports directory.
    func exportDirectoryNames() -> [String] {
        ((try? FileManager.default.contentsOfDirectory(atPath: exportsDirectory.path)) ?? []).sorted()
    }

    /// Every path anywhere under the storage root whose name mentions the export file or audio directory names, i.e.
    /// everything an export could leave readable.
    func exportRemnants() -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: layout.rootDirectory.path)) ?? []
        return subpaths.filter { $0.hasPrefix("Temporary/" + ExportService.exportsDirectoryName + "/") }.sorted()
    }

    func writeFinalizedAudio(_ fileName: String, contents: String = "synthetic audio") throws {
        try Data(contents.utf8).write(to: layout.directory(for: .finalizedAudio).appendingPathComponent(fileName))
    }

    func finalizedAudioExists(_ fileName: String) -> Bool {
        FileManager.default.fileExists(
            atPath: layout.directory(for: .finalizedAudio).appendingPathComponent(fileName).path)
    }

    static func reference(item: String, capture: String, file: String? = nil) -> CoreExportAudioReference {
        CoreExportAudioReference(
            itemID: item, captureID: capture, reference: "FinalizedAudio/" + (file ?? "\(capture).m4a"))
    }
}

/// Polls until `condition` holds, so a test can act while an async call is parked at its gate.
func waitUntil(timeout: TimeInterval = 10, _ condition: @escaping () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}
