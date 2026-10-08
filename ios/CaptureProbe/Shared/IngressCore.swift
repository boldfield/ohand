import Foundation

// Compiled into CaptureProbe, CaptureProbeControl and CaptureProbeTests. Foundation only: it must not use
// UIApplication (unavailable in extensions) so that protected-data state is passed in by the caller.

/// The handoff that created a capture entry. A plain app launch is not a handoff and creates no entry.
enum IngressSource: String, Codable {
    case controlIntent
    case shortcutURL
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

    /// Records what the capture screen shows: the current entry, or the idle screen when `outcome` is nil.
    func writePresented(_ outcome: IngressOutcome?) throws {
        try prepareDirectories()
        let presented = IngressPresented(
            captureId: outcome?.captureId,
            statusText: outcome?.statusText ?? IngressOutcome.idleStatusText,
            lines: outcome?.displayLines ?? IngressOutcome.idleDisplayLines
        )
        try writeData(try IngressStore.makeEncoder().encode(presented), presentedURL)
    }
}

struct IngressPresented: Codable, Equatable {
    let captureId: String?
    let statusText: String
    let lines: [String]
}

/// What the capture screen shows. It carries only the current entry; it never lists earlier captures.
struct IngressOutcome: Equatable {
    enum Status: Equatable {
        case saved
        case replayed
        case failed(String)

        var isFailure: Bool {
            if case .failed = self { return true }
            return false
        }
    }

    static let idleStatusText = "Ready"
    static let idleDisplayLines = ["No capture entry. Open the probe from its control or shortcut to start one."]

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

/// Owns the per-entry capture ID. Only a handoff (the control's intent or the shortcut URL) creates an entry: it
/// mints the ID once, keeps it in a durable pending-entry file (and in memory if that write fails), and every
/// retry and relaunch commits that same ID. The ID is released only after its record is committed.
///
/// A plain launch never creates or commits an entry, so the order and latency of the handoff callback relative to
/// the scene's foreground callbacks cannot produce a second ID or relabel another entry's record.
final class IngressFlow {
    static let handoffRegisteredNotification = Notification.Name("com.boldfield.ohand.probes.capture.handoff")
    static var live = IngressFlow(store: IngressStore(rootDirectory: IngressStore.defaultRootDirectory()))

    let store: IngressStore
    let notificationCenter: NotificationCenter
    private let now: () -> Date
    private let makeCaptureId: () -> String
    private let lock = NSLock()
    private var inMemoryPending: PendingEntry?

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

    private func timestamp() -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: now())
    }

    private func currentPending() -> PendingEntry? {
        inMemoryPending ?? store.loadPending()
    }

    /// Registers a handoff and returns the capture ID it will commit. An entry still pending because its commit
    /// failed is not abandoned: the handoff adopts it (same ID, original source and registration time).
    @discardableResult
    func registerHandoff(source: IngressSource) -> String {
        lock.lock()
        defer { lock.unlock() }
        if let existing = currentPending() {
            inMemoryPending = existing
            return existing.captureId
        }
        let created = PendingEntry(captureId: makeCaptureId(), source: source, registeredAt: timestamp())
        inMemoryPending = created
        try? store.savePending(created)
        return created.captureId
    }

    /// Registers a handoff, then tells a foreground scene which capture ID it carries.
    @discardableResult
    func registerHandoffAndAnnounce(source: IngressSource) -> String {
        let captureId = registerHandoff(source: source)
        notificationCenter.post(
            name: IngressFlow.handoffRegisteredNotification,
            object: nil,
            userInfo: ["captureId": captureId, "source": source.rawValue]
        )
        return captureId
    }

    /// Commits the pending entry, or returns nil when no handoff is pending.
    func commitPending(launchKind: IngressLaunchKind, protectedDataAvailable: Bool) -> IngressOutcome? {
        lock.lock()
        defer { lock.unlock() }
        guard let pending = currentPending() else { return nil }
        return commitLocked(pending, launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
    }

    /// Commits the entry a handoff announced. If that ID was already committed (for example by the foreground
    /// callback that raced the announcement), it reports the existing record as a replay instead of minting anything.
    func commitHandoff(
        captureId: String,
        source: IngressSource,
        launchKind: IngressLaunchKind,
        protectedDataAvailable: Bool
    ) -> IngressOutcome {
        lock.lock()
        defer { lock.unlock() }
        if let pending = currentPending(), pending.captureId == captureId {
            return commitLocked(pending, launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
        }
        if let existing = store.loadRecord(captureId: captureId) {
            return IngressOutcome(
                captureId: existing.captureId,
                source: existing.source,
                launchKind: existing.launchKind,
                protectedDataAvailable: existing.protectedDataAvailable,
                status: .replayed
            )
        }
        let announced = PendingEntry(captureId: captureId, source: source, registeredAt: timestamp())
        return commitLocked(announced, launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
    }

    /// A failed commit leaves the pending entry in place so a retry reuses the same capture ID.
    private func commitLocked(
        _ pending: PendingEntry,
        launchKind: IngressLaunchKind,
        protectedDataAvailable: Bool
    ) -> IngressOutcome {
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
            if inMemoryPending?.captureId == record.captureId {
                inMemoryPending = nil
            }
            return outcome(result == .created ? .saved : .replayed)
        } catch {
            inMemoryPending = pending
            let nsError = error as NSError
            return outcome(.failed("write error \(nsError.domain) \(nsError.code)"))
        }
    }

    func recordPresentation(_ outcome: IngressOutcome?) {
        lock.lock()
        defer { lock.unlock() }
        try? store.writePresented(outcome)
    }
}

/// UIKit-free per-scene coordination: foreground state, cold/warm classification, and committing a handoff either
/// immediately (scene in the foreground) or on the next foreground entry.
final class IngressSession {
    static let shortcutURLScheme = "ohand-captureprobe"
    static let shortcutURLHost = "capture"

    private let flow: IngressFlow
    private var hasEnteredForeground = false
    private var handoffObserver: NSObjectProtocol?
    private(set) var isForeground = false
    private(set) var launchKind: IngressLaunchKind = .cold
    private(set) var currentOutcome: IngressOutcome?

    init(flow: IngressFlow) {
        self.flow = flow
    }

    deinit {
        if let handoffObserver = handoffObserver {
            flow.notificationCenter.removeObserver(handoffObserver)
        }
    }

    /// Fires for the cold launch and for every later return from the background (warm launch). Commits a pending
    /// handoff if there is one; otherwise returns nil and the screen stays idle. A plain launch creates no entry.
    func willEnterForeground(protectedDataAvailable: Bool) -> IngressOutcome? {
        isForeground = true
        launchKind = hasEnteredForeground ? .warm : .cold
        hasEnteredForeground = true
        currentOutcome = flow.commitPending(launchKind: launchKind, protectedDataAvailable: protectedDataAvailable)
        return currentOutcome
    }

    func didEnterBackground() {
        isForeground = false
        currentOutcome = nil
    }

    /// Returns the outcome to render, or nil when the app is in the background; a handoff that arrives in the
    /// background stays pending and is committed by the next `willEnterForeground`.
    func handoffRegistered(captureId: String, source: IngressSource, protectedDataAvailable: Bool) -> IngressOutcome? {
        guard isForeground else { return nil }
        if let current = currentOutcome, current.captureId == captureId, !current.status.isFailure {
            return current
        }
        currentOutcome = flow.commitHandoff(
            captureId: captureId,
            source: source,
            launchKind: launchKind,
            protectedDataAvailable: protectedDataAvailable
        )
        return currentOutcome
    }

    static func isShortcutURL(_ url: URL) -> Bool {
        url.scheme?.lowercased() == shortcutURLScheme && url.host?.lowercased() == shortcutURLHost
    }

    /// Handles a URL the scene received. The URL carries no data; it only marks a shortcut handoff.
    func receive(url: URL, protectedDataAvailable: Bool) -> IngressOutcome? {
        guard IngressSession.isShortcutURL(url) else { return nil }
        let captureId = flow.registerHandoff(source: .shortcutURL)
        return handoffRegistered(captureId: captureId, source: .shortcutURL, protectedDataAvailable: protectedDataAvailable)
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
            guard
                let self = self,
                let captureId = notification.userInfo?["captureId"] as? String,
                let sourceName = notification.userInfo?["source"] as? String,
                let source = IngressSource(rawValue: sourceName)
            else { return }
            if let outcome = self.handoffRegistered(
                captureId: captureId,
                source: source,
                protectedDataAvailable: protectedDataAvailable()
            ) {
                onOutcome(outcome)
            }
        }
    }
}
