import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Drives `ItemDeletionModel` and `SourceDetailModel` over the production composition root, the real lifecycle
/// coordinator's cleanup service and the real Rust core on a file store. Everything is synthetic. The tests are
/// synchronous on purpose: core answers arrive on the main run loop, which only a synchronous test pumps.
@MainActor
final class ItemDeletionRealCoreTests: XCTestCase {
    private static let routeID = "route-item-deletion-ui"
    private static let captureText = "synthetic words that must not outlive the deletion"
    private static let reminderID = "rem-ui-1"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-deletion-ui-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    func testDeletionStaysPendingUntilCleanupIsAcknowledgedAndTheSourceIsNeverShownAgain() throws {
        let center = FakeNotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], center: center)
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudioAndReminder(itemID: itemID, captureID: captureID, center: center)

        let deletion = makeDeletionModel(root, itemID: itemID)
        let source = makeSourceModel(root, itemID: itemID, deletion: deletion)
        try drive { await source.load() }
        XCTAssertEqual(source.content?.currentText, Self.captureText, "the live item is shown")
        try drive { await deletion.prepare() }
        XCTAssertNotNil(deletion.revision)

        // The coordinator is not connected, so no cleanup can run: the deletion is recorded and stays pending.
        deletion.requestDeletion()
        try drive { await deletion.confirmDeletion() }
        guard case .pending = deletion.phase else { return XCTFail("got \(deletion.phase)") }
        XCTAssertNotEqual(deletion.phase, .complete)
        XCTAssertTrue(deletion.isRecorded)
        XCTAssertTrue(deletion.canRetry)
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path), "the recording is not claimed removed")

        try drive { await source.load() }
        assertNothingOfTheItemIsShown(source)

        // Connecting the cleanup makes the retry succeed: it replays the recorded request.
        try coordinator.connect(to: root)
        try drive { await deletion.retry() }
        XCTAssertEqual(deletion.phase, .complete)
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path))
        XCTAssertEqual(center.pendingIdentifiers, [])

        try drive { await source.load() }
        assertNothingOfTheItemIsShown(source)
    }

    func testARelaunchedScreenShowsTheEarlierDeletionsProgressChecksAgainAndNeverShowsTheSource() throws {
        let center = FakeNotificationCenter()
        let first = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let itemID: String
        do {
            let root = try makeRoot(modules: [first], center: center)
            defer { root.shutDown() }
            let saved = try saveCapture(root)
            itemID = saved.itemID
            _ = try seedAudioAndReminder(itemID: itemID, captureID: saved.captureID, center: center)
            let deletion = makeDeletionModel(root, itemID: itemID)
            try drive { await deletion.prepare() }
            deletion.requestDeletion()
            try drive { await deletion.confirmDeletion() }
            XCTAssertTrue(deletion.phase.isPending, "cleanup could not run before the process ended")
        }

        // A new process over the same store: a fresh model knows nothing of the earlier request.
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], center: center)
        defer { root.shutDown() }
        let deletion = makeDeletionModel(root, itemID: itemID)
        let source = makeSourceModel(root, itemID: itemID, deletion: deletion)

        try drive { await source.load() }
        try drive { await deletion.prepare() }
        try drive { await deletion.refreshProgress() }

        XCTAssertTrue(deletion.phase.isPending, "the earlier deletion's actual progress is shown")
        XCTAssertTrue(deletion.isRecorded)
        XCTAssertNil(deletion.revision, "a deleted item offers no new deletion")
        XCTAssertFalse(deletion.canRetry, "the earlier request's revision is not known to this screen")
        XCTAssertTrue(deletion.canCheckProgress)
        assertNothingOfTheItemIsShown(source)

        // The lifecycle pass resumes the cleanup; checking again shows it finished.
        try coordinator.connect(to: root)
        root.setAppActiveForTests(true)
        let pass = try drive { await coordinator.reconcile(.foreground) }
        XCTAssertEqual(pass.outcome(of: .deletion), .completed)
        try drive { await deletion.refreshProgress() }
        XCTAssertEqual(deletion.phase, .complete)
        try drive { await source.load() }
        assertNothingOfTheItemIsShown(source)
    }

    // MARK: helpers

    private func assertNothingOfTheItemIsShown(
        _ source: SourceDetailModel, file: StaticString = #filePath, line: UInt = #line
    ) {
        guard let content = source.content else { return }
        XCTAssertTrue(content.isDeleted, "only a deleted marker may remain", file: file, line: line)
        XCTAssertNil(content.currentText, file: file, line: line)
        XCTAssertTrue(content.sections.isEmpty, file: file, line: line)
        XCTAssertFalse(
            content.sections.contains { $0.text.contains(Self.captureText) }, "the deleted text is gone", file: file,
            line: line)
    }

    private func makeDeletionModel(_ root: ServiceCompositionRoot, itemID: String) -> ItemDeletionModel {
        ItemDeletionModel(
            itemID: itemID, controls: root.itemDeletion, reader: root.itemDetails,
            readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }

    private func makeSourceModel(
        _ root: ServiceCompositionRoot, itemID: String, deletion: ItemDeletionModel
    ) -> SourceDetailModel {
        let model = SourceDetailModel(
            itemID: itemID, reader: root.itemDetails, opener: root.playback,
            readScope: { [weak root] in root?.readScope ?? .captureOnly })
        model.deletion = deletion
        return model
    }

    private var layout: ProtectedStorageLayout { ProtectedStorageLayout(rootDirectory: storageRoot) }

    private var databasePath: String { layout.databaseURL.path }

    private func makeRoot(
        modules: [CompositionModule], center: FakeNotificationCenter = FakeNotificationCenter()
    ) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: DeletionUnreachableProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true)),
            modules: modules)
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> (itemID: String, captureID: String) {
        let captureID = UUID().uuidString.lowercased()
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: captureID, text: Self.captureText, capturedAt: Date()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return (itemID, captureID)
    }

    private func seedAudioAndReminder(
        itemID: String, captureID: String, center: FakeNotificationCenter
    ) throws -> URL {
        let finalizedName = "\(captureID).m4a"
        let audioURL = layout.directory(for: .finalizedAudio).appendingPathComponent(finalizedName)
        try FileManager.default.createDirectory(
            at: audioURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("synthetic audio".utf8).write(to: audioURL)
        let reference = IngressRecord.audioReference(finalizedFileName: finalizedName)
        try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'")
        try execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(Self.reminderID)', '\(itemID)', 'resolved', 'scheduled', 'unknown', 'not_acknowledged', "
                + "'2090-01-01T09:00:00Z', 'UTC', 1, '2026-10-10T09:31:00Z', '2026-10-10T09:32:00Z')")
        try execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                + "effect_identity, external_id, created_at) VALUES ('\(Self.reminderID)-op-0', '\(Self.reminderID)', "
                + "'schedule', 'acknowledged', 'schedule:\(Self.reminderID)#1', '\(Self.reminderID)#1', "
                + "'2026-10-10T09:31:00Z')")
        center.installForeignRequest(identifier: "\(Self.reminderID)#1", dueInstant: Date().addingTimeInterval(3600))
        return audioURL
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Runs a main-actor async operation to completion while pumping the main run loop that delivers core answers.
    @discardableResult
    private func drive<Value>(_ operation: @escaping @MainActor () async -> Value) throws -> Value {
        let box = DeletionRealCoreBox<Value>()
        Task { @MainActor in box.set(await operation()) }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value)
    }

    private func execute(_ sql: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        if code != SQLITE_OK { throw CompositionError.storageUnavailable }
    }
}

private final class DeletionUnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class DeletionRealCoreBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Value?

    var value: Value? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Value) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}
