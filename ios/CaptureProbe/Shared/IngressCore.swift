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

    /// Re-labels how an already committed entry was reached. The capture ID and every other field are unchanged.
    func updateSource(captureId: String, to source: IngressSource) throws {
        guard let existing = loadRecord(captureId: captureId) else {
            throw NSError(domain: "IngressStore", code: 404)
        }
        guard existing.source != source else { return }
        let relabeled = IngressRecord(
            captureId: existing.captureId,
            source: source,
            launchKind: existing.launchKind,
            protectedDataAvailable: existing.protectedDataAvailable,
            committedAt: existing.committedAt,
            syntheticText: existing.syntheticText
        )
        try writeData(try IngressStore.makeEncoder().encode(relabeled), recordURL(captureId: captureId))
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

    func withSource(_ newSource: IngressSource) -> IngressOutcome {
        IngressOutcome(
            captureId: captureId,
            source: newSource,
            launchKind: launchKind,
            protectedDataAvailable: protectedDataAvailable,
            status: status
        )
    }

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
///
/// It also remembers the entry committed during the current foreground session. A control handoff that arrives
/// after the scene already committed a direct-launch entry claims that entry (relabelling its source) instead of
/// minting a second ID, so one control activation yields one record whichever callback runs first.
final class IngressFlow {
    static let handoffRegisteredNotification = Notification.Name("com.boldfield.ohand.probes.capture.handoff")
    static var live = IngressFlow(store: IngressStore(rootDirectory: IngressStore.defaultRootDirectory()))

    let store: IngressStore
    let notificationCenter: NotificationCenter
    private let now: () -> Date
    private let makeCaptureId: () -> String
    private let lock = NSLock()
    private var inMemoryPending: PendingEntry?
    private var sessionEntry: IngressOutcome?
    private var sessionEntryClaimed = false

    init(
        store: IngressStore,
        notificationCenter: NotificationCenter = .default,
        now: @escaping () -> Date = Date.init,
        makeCaptureId: @escaping () -> String = { UUID().uuidString }
    ) {
        self.store = store
        self.notificationCenter = notificationCenter
        self.now = now
        self.makeCaptureId = makeCaptureId
    }

    /// The entry committed during the current foreground session, with its latest source label.
    var currentSessionEntry: IngressOutcome? {
        lock.lock()
        defer { lock.unlock() }
        return sessionEntry
    }

    func endForegroundSession() {
        lock.lock()
        defer { lock.unlock() }
        sessionEntry = nil
        sessionEntryClaimed = false
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
        let hasPending = (inMemoryPending ?? store.loadPending()) != nil
        if source == .controlIntent, !hasPending, let entry = sessionEntry, !sessionEntryClaimed {
            sessionEntryClaimed = true
            if entry.source != .controlIntent, (try? store.updateSource(captureId: entry.captureId, to: .controlIntent)) != nil {
                sessionEntry = entry.withSource(.controlIntent)
            }
            return entry.captureId
        }
        return resolvePending(source: source).captureId
    }

    /// What the control's intent runs: register the handoff, then tell a foreground scene about it.
    @discardableResult
    func registerControlHandoffAndAnnounce() -> String {
        let captureId = registerHandoff(source: .controlIntent)
        notificationCenter.post(
            name: IngressFlow.handoffRegisteredNotification,
            object: nil,
            userInfo: ["captureId": captureId]
        )
        return captureId
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
            let committed = outcome(result == .created ? .saved : .replayed)
            sessionEntry = committed
            sessionEntryClaimed = record.source == .controlIntent
            return committed
        } catch {
            sessionEntry = nil
            sessionEntryClaimed = false
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

/// UIKit-free per-scene coordination: foreground state, cold/warm classification and the decision whether a
/// control handoff needs a new commit or belongs to the entry the scene already committed.
final class IngressSession {
    private let flow: IngressFlow
    private var hasEnteredForeground = false
    private var handoffObserver: NSObjectProtocol?
    private(set) var isForeground = false
    private(set) var launchKind: IngressLaunchKind = .cold

    init(flow: IngressFlow) {
        self.flow = flow
    }

    deinit {
        if let handoffObserver = handoffObserver {
            flow.notificationCenter.removeObserver(handoffObserver)
        }
    }

    /// Fires for the cold launch and for every later return from the background (warm launch).
    func willEnterForeground(protectedDataAvailable: Bool) -> IngressOutcome {
        isForeground = true
        launchKind = hasEnteredForeground ? .warm : .cold
        hasEnteredForeground = true
        return flow.enter(source: .directLaunch, launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
    }

    func didEnterBackground() {
        isForeground = false
        flow.endForegroundSession()
    }

    /// Returns the outcome to render, or nil when the app is in the background; a handoff that arrives in the
    /// background stays pending and is consumed by the next `willEnterForeground`.
    func handoffRegistered(captureId: String, protectedDataAvailable: Bool) -> IngressOutcome? {
        guard isForeground else { return nil }
        if let entry = flow.currentSessionEntry, entry.captureId == captureId {
            return entry
        }
        return flow.enter(source: .controlIntent, launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
    }

    func observeHandoffs(
        protectedDataAvailable: @escaping () -> Bool,
        onOutcome: @escaping (IngressOutcome) -> Void
    ) {
        handoffObserver = flow.notificationCenter.addObserver(
            forName: IngressFlow.handoffRegisteredNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self = self, let captureId = notification.userInfo?["captureId"] as? String else { return }
            if let outcome = self.handoffRegistered(captureId: captureId, protectedDataAvailable: protectedDataAvailable()) {
                onOutcome(outcome)
            }
        }
    }
}
