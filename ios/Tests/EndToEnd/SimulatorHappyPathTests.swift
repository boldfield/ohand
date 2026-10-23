import SQLite3
import UIKit
import UserNotifications
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class CountingOfflineSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var attempts = 0

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        attempts += 1
        lock.unlock()
        throw URLError(.notConnectedToInternet)
    }
}

private final class SearchBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<RetrievalPage, RetrievalSearchFailure>?

    var value: Result<RetrievalPage, RetrievalSearchFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

private final class ReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Int, Error>?

    var value: Result<Int, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<Int, Error>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

/// The happy-path simulator loop through the app's production wiring: `AppServices` over the real composition root,
/// Rust core, ingress, job runner, reminder reconciler, notification action handler, correction, lifecycle and
/// retrieval services, on an on-disk store. Only the provider transport (unreachable, counted), the system
/// notification center, the Face ID prompt and the microphone are deterministic stand-ins. Failure journeys belong to
/// `FailureJourneys`.
final class SimulatorHappyPathTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var services: AppServices!
    private var sender: CountingOfflineSender!
    private var center: FakeNotificationCenter!
    private var retrieval: RetrievalSearchModule!
    private let engine = SyntheticVoiceEngine()
    private let permission = FakeMicrophonePermission()
    private let poller = ManualPoller()

    private var utcCalendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("end-to-end-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        sender = CountingOfflineSender()
        center = FakeNotificationCenter()
        retrieval = RetrievalSearchModule(calendar: utcCalendar)
        let lifecycle = LifecycleCoordinator()
        let lifecycleCenter = NotificationCenter()
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-end-to-end", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender,
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: false),
                lifecycleNotifications: lifecycleCenter),
            modules: [lifecycle, retrieval])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let manualPoller = poller
        environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: permission, environment: environment,
            notificationCenter: NotificationCenter())
        services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, retrieval: retrieval,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter, isProtectedDataAvailable: { true })
        services.start(isActive: true)
    }

    override func tearDownWithError() throws {
        settleBackgroundWork()
        services.shutDown()
        services = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: the loop

    func testSilentCaptureThroughReminderSchedulingAndNotificationActionCompletion() throws {
        unlock()
        let words = "remind me 2099-01-01 09:00:00 to call mom"

        let itemID = try captureSilently(words)

        // Classification: the offline grammar reads the explicit reminder with no provider.
        XCTAssertTrue(
            pump { self.scalar("SELECT item_type FROM items WHERE item_id = '\(itemID)'") == "action" },
            "the saved capture is classified as an action")

        // Explicit reminder scheduling reaches the (fake) OS center.
        XCTAssertTrue(pump { self.reminderRequest() != nil }, "the explicit reminder was handed to the OS")
        let request = try XCTUnwrap(reminderRequest())
        let notification = try XCTUnwrap(NotificationIdentifier(rawValue: request.identifier))
        XCTAssertEqual(
            scalar("SELECT schedule_state FROM reminders WHERE item_id = '\(itemID)'"), "scheduled")
        XCTAssertEqual(
            scalar("SELECT reminder_id FROM reminders WHERE item_id = '\(itemID)'"), notification.reminderID.rawValue)

        // Saved-source retrieval returns the original words.
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "call mom")).get())
        let row = try XCTUnwrap(page.rows.first { $0.reference.value == itemID })
        XCTAssertEqual(row.text, words)
        XCTAssertEqual(row.basis, .original)

        // Notification action: the Done action completes the item through the real handler and core.
        services.root.notificationIngestor.ingestResponse(
            requestIdentifier: request.identifier, actionIdentifier: NotificationActionIdentifier.complete,
            userInfo: [NotificationContent.targetKey: itemID])
        runAsync { await self.services.root.reconcileNotificationActions() }
        XCTAssertTrue(
            pump { self.scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'") == "completed" },
            "the notification action completed the item")
        XCTAssertEqual(sender.attemptCount, 0, "no provider was contacted anywhere in the loop")
    }

    func testCorrectionAndCompletionThroughTheProductionServices() throws {
        unlock()
        let itemID = try captureSilently("synthetic garage shelf idea")

        let typeRevision = try revision(of: itemID)
        _ = try correct { corrections in
            try await corrections.correctType(
                commandID: "e2e-type", itemID: itemID, expectedRevision: typeRevision, itemType: .idea).revision
        }
        XCTAssertEqual(scalar("SELECT item_type FROM items WHERE item_id = '\(itemID)'"), "idea")

        let completeRevision = try revision(of: itemID)
        let controls = services.root.itemLifecycleControls
        let finished = expectation(description: "complete")
        Task {
            _ = try? await controls.complete(commandID: "e2e-complete", itemID: itemID, expectedRevision: completeRevision)
            finished.fulfill()
        }
        wait(for: [finished], timeout: 30)
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
        XCTAssertEqual(sender.attemptCount, 0)
    }

    func testWithNoProviderASessionTopicAndFacetCorrectionAreRetrievedBySinceDateWithoutNetwork() throws {
        unlock()
        let garageID = try captureSilently("synthetic garage shelf")
        let kitchenID = try captureSilently("synthetic kitchen tap")

        for (index, pair) in [(garageID, "Garage"), (kitchenID, "Kitchen")].enumerated() {
            let start = try revision(of: pair.0)
            _ = try correct { corrections in
                try await corrections.correctSessionTopic(
                    commandID: "e2e-topic-\(index)", itemID: pair.0, expectedRevision: start, sessionTopic: pair.1
                ).revision
            }
        }
        let facetRevision = try revision(of: garageID)
        _ = try correct { corrections in
            try await corrections.correctType(
                commandID: "e2e-facet", itemID: garageID, expectedRevision: facetRevision, itemType: .idea
            ).revision
        }
        let yesterday = Date().addingTimeInterval(-86_400)

        let page = try XCTUnwrap(
            search(RetrievalSearchRequest(sessionTopic: "garage", since: yesterday)).get())

        XCTAssertEqual(page.rows.map(\.reference.value), [garageID], "the topic matches whatever its case")
        let row = try XCTUnwrap(page.rows.first)
        XCTAssertEqual(row.itemType, .idea, "the corrected facet is shown")
        XCTAssertEqual(row.text, "synthetic garage shelf", "the original words come back")
        let tomorrow = Date().addingTimeInterval(86_400)
        XCTAssertEqual(
            try XCTUnwrap(search(RetrievalSearchRequest(sessionTopic: "garage", since: tomorrow)).get())
                .totalAccessible, 0, "the since date drops items captured before it")
        XCTAssertEqual(sender.attemptCount, 0, "no provider configured, no network request")
        XCTAssertTrue(center.addedRequests.allSatisfy { $0.identifier == PendingProcessingAlert.identifier })
    }

    // MARK: helpers

    private func unlock() {
        services.root.authenticateReads(reason: "end-to-end") { _ in }
        XCTAssertTrue(pump { self.services.root.readScope == .authenticated }, "the read session authenticated")
    }

    private func captureSilently(_ text: String) throws -> String {
        let previousItemID = services.captureEntry.acknowledgment?.savedItemID
        services.captureEntry.textModel.updateText(text)
        services.captureEntry.saveText()
        XCTAssertTrue(
            pump {
                guard let savedItemID = self.services.captureEntry.acknowledgment?.savedItemID else { return false }
                return savedItemID != previousItemID
            }, "the core confirmed the save")
        return try XCTUnwrap(services.captureEntry.acknowledgment?.savedItemID)
    }

    private func reminderRequest() -> NotificationCenterRequest? {
        center.addedRequests.first { $0.identifier != PendingProcessingAlert.identifier }
    }

    private func search(
        _ request: RetrievalSearchRequest
    ) -> Result<RetrievalPage, RetrievalSearchFailure> {
        let box = SearchBox()
        retrieval.search(request) { box.set($0) }
        XCTAssertTrue(pump { box.value != nil }, "the search never completed")
        return box.value ?? .failure(.unavailable)
    }

    private func correct(
        _ operation: @escaping @Sendable (ItemCorrectionService) async throws -> Int
    ) throws -> Int {
        let corrections = services.root.itemCorrections
        let box = ReceiptBox()
        Task {
            do { box.set(.success(try await operation(corrections))) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump { box.value != nil }, "the correction never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func runAsync(_ operation: @escaping @Sendable () async -> Void) {
        let finished = expectation(description: "async")
        Task {
            await operation()
            finished.fulfill()
        }
        wait(for: [finished], timeout: 30)
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
    }

    private func settleBackgroundWork() {
        _ = pump(timeout: 30) { !self.services.root.jobRunner.isDraining }
        let settled = Date().addingTimeInterval(1.5)
        _ = pump(timeout: 2) { Date() >= settled }
    }

    @discardableResult
    private func pump(timeout: TimeInterval = 30, until condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        guard sqlite3_open(path, &database) == SQLITE_OK else { return "" }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return "" }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
