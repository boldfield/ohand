import Foundation

// Compiled into CaptureProbe, CaptureProbeControl and CaptureProbeTests. Foundation only: it must not use
// UIApplication (unavailable in extensions) so that protected-data state is passed in by the caller.

enum IngressSource: String, Codable {
    case directLaunch
    case controlIntent
}

enum IngressLaunchKind: String, Codable {
    case cold
    case warm
}

struct IngressRecord: Codable, Equatable {
    let captureId: String
    let source: IngressSource
    let launchKind: IngressLaunchKind
    let protectedDataAvailable: Bool
    let committedAt: String
    let syntheticText: String
}

struct PendingEntry: Codable, Equatable {
    let captureId: String
    let source: IngressSource
    let registeredAt: String
}

enum IngressCommitResult: Equatable {
    case created
    case replayed
}

/// File-backed ingress store. Every file is written atomically with the "complete until first user
/// authentication" protection class so a capture can be written while the device is locked after the
/// first unlock. Before first unlock the writes fail and the failure is reported, never hidden.
final class IngressStore {
    typealias DataWriter = (Data, URL) throws -> Void

    let rootDirectory: URL
    private let writeData: DataWriter
    private let fileManager = FileManager.default

    init(rootDirectory: URL, writeData: DataWriter? = nil) {
        self.rootDirectory = rootDirectory
        self.writeData = writeData ?? IngressStore.protectedAtomicWrite
    }

    static func protectedAtomicWrite(_ data: Data, to url: URL) throws {
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    static func defaultRootDirectory() -> URL {
        let applicationSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return applicationSupport.appendingPathComponent("CaptureProbe", isDirectory: true)
    }

    var recordsDirectory: URL { rootDirectory.appendingPathComponent("records", isDirectory: true) }
    var pendingURL: URL { rootDirectory.appendingPathComponent("pending-entry.json") }
    var presentedURL: URL { rootDirectory.appendingPathComponent("last-presented.json") }

    private func recordURL(captureId: String) -> URL {
        recordsDirectory.appendingPathComponent("\(captureId).json")
    }

    private static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .prettyPrinted]
        return encoder
    }

    private func prepareDirectories() throws {
        try fileManager.createDirectory(
            at: recordsDirectory,
            withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication]
        )
    }

    func loadPending() -> PendingEntry? {
        guard let data = try? Data(contentsOf: pendingURL) else { return nil }
        return try? JSONDecoder().decode(PendingEntry.self, from: data)
    }

    func savePending(_ pending: PendingEntry) throws {
        try prepareDirectories()
        try writeData(try IngressStore.makeEncoder().encode(pending), pendingURL)
    }

    func clearPending(matching captureId: String) {
        guard let pending = loadPending(), pending.captureId == captureId else { return }
        try? fileManager.removeItem(at: pendingURL)
    }

    func loadRecord(captureId: String) -> IngressRecord? {
        guard let data = try? Data(contentsOf: recordURL(captureId: captureId)) else { return nil }
        return try? JSONDecoder().decode(IngressRecord.self, from: data)
    }

    func allRecords() -> [IngressRecord] {
        let files = (try? fileManager.contentsOfDirectory(at: recordsDirectory, includingPropertiesForKeys: nil)) ?? []
        let records = files
            .filter { $0.pathExtension == "json" }
            .compactMap { try? Data(contentsOf: $0) }
            .compactMap { try? JSONDecoder().decode(IngressRecord.self, from: $0) }
        return records.sorted { ($0.committedAt, $0.captureId) < ($1.committedAt, $1.captureId) }
    }

    /// First write wins: committing an ID that already has a record is a replay and never overwrites it.
    func commit(_ record: IngressRecord) throws -> IngressCommitResult {
        if loadRecord(captureId: record.captureId) != nil {
            return .replayed
        }
        try prepareDirectories()
        try writeData(try IngressStore.makeEncoder().encode(record), recordURL(captureId: record.captureId))
        return .created
    }

    func writePresented(_ outcome: IngressOutcome) throws {
        try prepareDirectories()
        let presented = IngressPresented(
            captureId: outcome.captureId,
            statusText: outcome.statusText,
            lines: outcome.displayLines
        )
        try writeData(try IngressStore.makeEncoder().encode(presented), presentedURL)
    }
}

struct IngressPresented: Codable, Equatable {
    let captureId: String
    let statusText: String
    let lines: [String]
}

/// What the capture screen shows. It carries only the current entry; it never lists earlier captures.
struct IngressOutcome: Equatable {
    enum Status: Equatable {
        case saved
        case replayed
        case failed(String)
    }

    let captureId: String
    let source: IngressSource
    let launchKind: IngressLaunchKind
    let protectedDataAvailable: Bool
    let status: Status

    var statusText: String {
        switch status {
        case .saved: return "Saved"
        case .replayed: return "Replayed (already saved)"
        case .failed(let reason): return "Failed: \(reason)"
        }
    }

    var displayLines: [String] {
        [
            "Capture ID: \(captureId)",
            "Entry: \(source.rawValue), \(launchKind.rawValue) launch",
            "Protected data: \(protectedDataAvailable ? "available" : "unavailable")",
            "Result: \(statusText)",
        ]
    }
}

/// Owns the per-entry capture ID. The ID is created once, kept in a durable pending-entry file (and in memory
/// if that write fails), reused by every retry, and released only after the record is committed.
final class IngressFlow {
    static let handoffRegisteredNotification = Notification.Name("com.boldfield.ohand.probes.capture.handoff")
    static var live = IngressFlow(store: IngressStore(rootDirectory: IngressStore.defaultRootDirectory()))

    let store: IngressStore
    private let now: () -> Date
    private let makeCaptureId: () -> String
    private let lock = NSLock()
    private var inMemoryPending: PendingEntry?

    init(
        store: IngressStore,
        now: @escaping () -> Date = Date.init,
        makeCaptureId: @escaping () -> String = { UUID().uuidString }
    ) {
        self.store = store
        self.now = now
        self.makeCaptureId = makeCaptureId
    }

    private func timestamp() -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: now())
    }

    private func resolvePending(source: IngressSource) -> PendingEntry {
        if let existing = inMemoryPending ?? store.loadPending() {
            if source == .controlIntent && existing.source != .controlIntent {
                let upgraded = PendingEntry(captureId: existing.captureId, source: .controlIntent, registeredAt: existing.registeredAt)
                inMemoryPending = upgraded
                try? store.savePending(upgraded)
                return upgraded
            }
            inMemoryPending = existing
            return existing
        }
        let created = PendingEntry(captureId: makeCaptureId(), source: source, registeredAt: timestamp())
        inMemoryPending = created
        try? store.savePending(created)
        return created
    }

    /// Called by the control's intent. Returns the capture ID the next entry will use.
    @discardableResult
    func registerHandoff(source: IngressSource) -> String {
        lock.lock()
        defer { lock.unlock() }
        return resolvePending(source: source).captureId
    }

    /// Commits the pending entry (creating it when no handoff registered one). A failed commit leaves the
    /// pending entry in place so a retry reuses the same capture ID.
    func enter(source: IngressSource, launchKind: IngressLaunchKind, protectedDataAvailable: Bool) -> IngressOutcome {
        lock.lock()
        defer { lock.unlock() }
        let pending = resolvePending(source: source)
        let record = IngressRecord(
            captureId: pending.captureId,
            source: pending.source,
            launchKind: launchKind,
            protectedDataAvailable: protectedDataAvailable,
            committedAt: timestamp(),
            syntheticText: "Synthetic probe capture"
        )
        func outcome(_ status: IngressOutcome.Status) -> IngressOutcome {
            IngressOutcome(
                captureId: record.captureId,
                source: record.source,
                launchKind: launchKind,
                protectedDataAvailable: protectedDataAvailable,
                status: status
            )
        }
        do {
            let result = try store.commit(record)
            store.clearPending(matching: record.captureId)
            inMemoryPending = nil
            return outcome(result == .created ? .saved : .replayed)
        } catch {
            let nsError = error as NSError
            return outcome(.failed("write error \(nsError.domain) \(nsError.code)"))
        }
    }

    func recordPresentation(_ outcome: IngressOutcome) {
        lock.lock()
        defer { lock.unlock() }
        try? store.writePresented(outcome)
    }
}
