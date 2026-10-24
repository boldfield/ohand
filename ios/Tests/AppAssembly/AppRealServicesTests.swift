import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
import UIKit
import UserNotifications
@testable import OhAnd

/// The app's production wiring over a fresh store, with only the microphone, the audio engine and the system
/// notification center replaced. Everything else (the core, ingress, the recording save path, recovery) is real.
final class AppRealServicesTests: XCTestCase {
    private final class RecordingHooks {
        private(set) var events: [String] = []
        private(set) var installedDelegate: UNUserNotificationCenterDelegate?

        var hooks: NotificationHooks {
            NotificationHooks(
                installDelegate: { [unowned self] delegate in
                    events.append("install")
                    installedDelegate = delegate
                },
                reconcile: { [unowned self] _ in events.append("reconcile") })
        }
    }

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var created: [AppServices] = []
    private let engine = SyntheticVoiceEngine()
    private let permission = FakeMicrophonePermission()
    private let poller = ManualPoller()
    private let hooks = RecordingHooks()

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("app-real-services-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        created = []
    }

    override func tearDownWithError() throws {
        created.forEach { $0.shutDown() }
        created = []
        // A core read issued before the shutdown completes on its own queue; wait for its buffer to be freed.
        let bufferDeadline = Date().addingTimeInterval(5)
        while CoreHandle.liveResultBufferCount != baselineBuffers, Date() < bufferDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func makeServices() throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let manualPoller = poller
        environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: permission, environment: environment,
            notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, notificationHooks: hooks.hooks, notificationCenter: NotificationCenter(),
            isProtectedDataAvailable: { true })
        created.append(services)
        return services
    }

    /// The production assembly over a fresh store with only the platform adapters replaced: the background-time
    /// grant, the notification center, the lifecycle notification center, the recognizer and the voice engine.
    private func makeInjectedServices(
        grant: BackgroundExecutionGrant?,
        center: FakeNotificationCenter,
        lifecycleCenter: NotificationCenter,
        recognizer: OnDeviceRecognizing?
    ) throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(),
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: false),
                backgroundGrant: grant, lifecycleNotifications: lifecycleCenter, recognizer: recognizer),
            modules: [lifecycle])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let manualPoller = poller
        environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: permission, environment: environment,
            notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter, isProtectedDataAvailable: { true })
        created.append(services)
        return services
    }

    private final class LockedCounter: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        var count: Int {
            lock.lock()
            defer { lock.unlock() }
            return value
        }
        func increment() {
            lock.lock()
            value += 1
            lock.unlock()
        }
    }

    private final class ChangeBox: @unchecked Sendable {
        private let lock = NSLock()
        private var changes: [ItemChange] = []
        var all: [ItemChange] {
            lock.lock()
            defer { lock.unlock() }
            return changes
        }
        func add(_ change: ItemChange) {
            lock.lock()
            changes.append(change)
            lock.unlock()
        }
    }

    private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
        func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
            throw URLError(.notConnectedToInternet)
        }
    }

    /// Turns the run loop until the condition holds, failing the test (naming the condition) if it does not in time. The 30 s
    /// bound is the one these waits always had: they wait on the real core's database work.
    private func wait(
        _ condition: String, timeout: TimeInterval = 30, file: StaticString = #filePath, line: UInt = #line,
        until holds: () -> Bool
    ) {
        waitUntilPumping(condition, timeout: timeout, file: file, line: line, holds)
    }

    /// A fixed pause that is meant to run its whole length: it gives something that must not happen time to happen.
    private func pause(seconds: TimeInterval) {
        let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    // MARK: saving through the real core

    func testATextCaptureOnAFreshStoreIsSavedByTheRealCore() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)

        services.captureEntry.textModel.updateText("remind me to water the plants")
        services.captureEntry.saveText()
        wait("the core confirmed the text save") { services.captureEntry.acknowledgment?.savedItemID != nil }

        let itemID = try XCTUnwrap(services.captureEntry.acknowledgment?.savedItemID, "the core confirmed the import")
        XCTAssertFalse(itemID.isEmpty)
        XCTAssertEqual(services.captureEntry.textModel.text, "", "the field clears only after a confirmed save")
    }

    func testARecordingIsSavedByTheRealCoreThroughTheRootHandoff() throws {
        let services = try makeServices()

        services.captureEntry.startRecording()
        wait("the recorder is recording") { if case .recording = services.captureEntry.recorderState { return true } else { return false } }
        guard case .recording = services.captureEntry.recorderState else {
            return XCTFail("the recorder did not start: \(services.captureEntry.recorderState)")
        }
        XCTAssertEqual(engine.startCount, 1, "the real recorder, not an unavailable stand-in, drove the engine")

        services.captureEntry.stopRecording()
        wait("the recording save is acknowledged") { services.captureEntry.acknowledgment?.savedItemID != nil }

        let acknowledgment = try XCTUnwrap(services.captureEntry.acknowledgment)
        XCTAssertNotNil(acknowledgment.savedItemID, "saved only on the core's confirmation: \(acknowledgment.save)")
        XCTAssertEqual(acknowledgment.source, .audio)
    }

    func testACancelledRecordingStaysReachableThroughTheRecoveryModel() throws {
        let services = try makeServices()

        services.captureEntry.startRecording()
        wait("the recorder is recording") { if case .recording = services.captureEntry.recorderState { return true } else { return false } }
        services.captureEntry.cancelRecording()
        wait("the recording is cancelled") { services.captureEntry.acknowledgment?.save == .cancelled }
        XCTAssertEqual(services.captureEntry.acknowledgment?.save, .cancelled)

        services.voice.recoveryModel.refresh()
        wait("the cancelled audio is offered for recovery") { !services.voice.recoveryModel.rows.isEmpty }

        XCTAssertEqual(services.voice.recoveryModel.rows.count, 1, "the cancelled audio is offered, never silently dropped")
    }

    // MARK: processing starts when a capture is saved

    func testASavedTypedCaptureIsInterpretedAndItsReminderRecordedWithNoLifecycleEvent() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)
        XCTAssertEqual(services.root.jobRunner.startedDrainCount, 0, "nothing has drained before the save")

        services.captureEntry.textModel.updateText("remind me 2099-01-01 09:00:00 to call mom")
        services.captureEntry.saveText()
        wait("the reminder is recorded") { self.scalar("SELECT COUNT(*) FROM reminders") != "0" }

        XCTAssertGreaterThan(services.root.jobRunner.startedDrainCount, 0, "the save itself asked for the drain")
        XCTAssertNotEqual(scalar("SELECT COUNT(*) FROM reminders"), "0", "the offline grammar's reminder is recorded")
        settleBackgroundWork(services)
    }

    func testASavedRecordingStartsTranscriptionWithNoLifecycleEvent() throws {
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "water the plants", confidence: nil))
        let services = try makeInjectedServices(
            grant: nil, center: FakeNotificationCenter(), lifecycleCenter: NotificationCenter(), recognizer: recognizer)
        services.start(isActive: true)
        settleBackgroundWork(services)
        XCTAssertEqual(recognizer.callCount, 0)

        services.captureEntry.startRecording()
        wait("the recorder is recording") { if case .recording = services.captureEntry.recorderState { return true } else { return false } }
        services.captureEntry.stopRecording()
        wait("the recording save is acknowledged") { services.captureEntry.acknowledgment?.savedItemID != nil }
        XCTAssertNotNil(services.captureEntry.acknowledgment?.savedItemID)
        wait("the recognizer is asked to transcribe") { recognizer.callCount > 0 }

        XCTAssertGreaterThan(recognizer.callCount, 0, "the recognizer was asked to transcribe the saved audio")
        settleBackgroundWork(services)
    }

    func testASaveOpensACompletionWindowArmsTheAlertAndActivationDisarmsIt() throws {
        let grant = FakeBackgroundGrant()
        let center = FakeNotificationCenter()
        let lifecycleCenter = NotificationCenter()
        let services = try makeInjectedServices(
            grant: grant, center: center, lifecycleCenter: lifecycleCenter, recognizer: nil)
        services.start(isActive: false)
        let alertID = PendingProcessingAlert.identifier
        func alertAdds() -> Int { center.addedRequests.filter { $0.identifier == alertID }.count }
        func alertRemovals() -> Int { center.removedIdentifiers.filter { $0 == alertID }.count }

        var saved = false
        services.root.saveText(captureID: UUID().uuidString, text: "buy some milk", capturedAt: Date()) { outcome in
            if case .saved = outcome { saved = true }
        }
        wait("the recording save has finished") { saved }
        XCTAssertTrue(saved)
        wait("the completion window armed the alert") { grant.beginCount == 1 && grant.endedTokenList.count == 1 && alertAdds() > 0 }

        XCTAssertEqual(grant.beginCount, 1, "the save took background time once")
        XCTAssertEqual(grant.endedTokenList.count, 1, "and gave it back once")
        XCTAssertGreaterThan(services.root.jobRunner.startedDrainCount, 0, "the window drained the queue")
        XCTAssertGreaterThan(alertAdds(), 0, "the capture still waits on the network, so the alert is armed")

        let removalsBeforeActivation = alertRemovals()
        lifecycleCenter.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        wait("activation disarmed the alert") { alertRemovals() > removalsBeforeActivation }
        XCTAssertGreaterThan(alertRemovals(), removalsBeforeActivation, "activation disarms the alert")
        settleBackgroundWork(services)
    }

    func testBothDrainIdleObserversRunAfterADrainThroughTheProductionRoot() throws {
        let center = FakeNotificationCenter()
        let services = try makeInjectedServices(
            grant: nil, center: center, lifecycleCenter: NotificationCenter(), recognizer: nil)
        services.start(isActive: true)
        settleBackgroundWork(services)
        let completionObserverRuns = LockedCounter()
        services.root.completionDrain.setDrainIdleObserver { completionObserverRuns.increment() }
        let runsBeforeSave = completionObserverRuns.count

        var saved = false
        services.root.saveText(
            captureID: UUID().uuidString, text: "remind me 2099-01-01 09:00:00 to call mom", capturedAt: Date()
        ) { outcome in
            if case .saved = outcome { saved = true }
        }
        wait("the recording save has finished") { saved }
        func reminderScheduled() -> Bool {
            center.addedRequests.contains { $0.identifier != PendingProcessingAlert.identifier }
        }
        wait("the completion pass scheduled the reminder") { completionObserverRuns.count > runsBeforeSave && reminderScheduled() }

        XCTAssertGreaterThan(completionObserverRuns.count, runsBeforeSave, "the completion observer ran after the drain")
        XCTAssertTrue(reminderScheduled(), "the reminder pass ran after the same drain")
        settleBackgroundWork(services)
    }

    func testACompletedInterpretationDrainPublishesTheItemChangeSignalThroughTheProductionRoot() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)
        let changes = ChangeBox()
        let stopObserving = services.root.observeItemChanges { changes.add($0) }
        defer { stopObserving() }

        services.captureEntry.textModel.updateText("remind me 2099-01-01 09:00:00 to call mom")
        services.captureEntry.saveText()
        wait("the all-items change is observed") { changes.all.contains(.allItems) }

        XCTAssertTrue(changes.all.contains(.allItems), "applied interpretation asks every list to read again")
        XCTAssertNotEqual(scalar("SELECT COUNT(*) FROM reminders"), "0", "the drain really applied its result")
        settleBackgroundWork(services)
    }

    func testTheDrainEventItselfDecidesTheItemChangeWhateverTheRunnerHasMovedOnTo() {
        func drainEvent(_ jobsJSON: String) -> CoreEvent {
            CoreEvent(
                operationID: 7,
                outcome: .success(Data(#"{"operation_id":7,"stop":"job_limit","jobs":\#(jobsJSON)}"#.utf8)))
        }
        let completedInterpretation = drainEvent(
            #"[{"job_id":"j1","job_type":"interpret","result":"settled","settlement":"completed"}]"#)
        let completedTranscription = drainEvent(
            #"[{"job_id":"j2","job_type":"\#(TranscriptionJobHandler.transcriptionJobType)","result":"settled","settlement":"completed"}]"#)
        let retriedInterpretation = drainEvent(
            #"[{"job_id":"j3","job_type":"interpret","result":"transient","settlement":null}]"#)

        // Read from the event, so a re-run that started (or failed to start) since cannot hide what this drain applied.
        XCTAssertEqual(ServiceCompositionRoot.itemChange(afterDrainEvent: completedInterpretation), .allItems)
        XCTAssertEqual(ServiceCompositionRoot.itemChange(afterDrainEvent: completedTranscription), .allItems)
        XCTAssertNil(ServiceCompositionRoot.itemChange(afterDrainEvent: retriedInterpretation))
        XCTAssertNil(ServiceCompositionRoot.itemChange(afterDrainEvent: drainEvent("[]")))
        XCTAssertNil(ServiceCompositionRoot.itemChange(
            afterDrainEvent: CoreEvent(operationID: 7, outcome: .failure(CoreFailure.handleClosed))))
    }

    func testDeliveredNotificationsAreRecordedOnceAtForegroundThroughTheProductionRoot() throws {
        let center = FakeNotificationCenter()
        let services = try makeInjectedServices(
            grant: nil, center: center, lifecycleCenter: NotificationCenter(), recognizer: nil)
        services.start(isActive: true)
        // A reminder pass that is asked for while another is running joins it. The launch pass began before the save, so
        // it must be over before the save, or the pass after the drain could join it and install nothing.
        wait("the launch pass has finished") { services.lifecycle.finishedPassCount >= 1 }
        services.root.saveText(
            captureID: UUID().uuidString, text: "remind me 2099-01-01 09:00:00 to call mom", capturedAt: Date()
        ) { _ in }
        func reminderRequest() -> NotificationCenterRequest? {
            center.addedRequests.first { $0.identifier != PendingProcessingAlert.identifier }
        }
        wait("the reminder request is installed") { reminderRequest() != nil }
        let request = try XCTUnwrap(reminderRequest(), "the reminder was scheduled")
        settleBackgroundWork(services)
        XCTAssertEqual(scalar("SELECT delivery_state FROM reminders"), "unknown")

        let readsAfterLaunch = center.deliveredReads
        XCTAssertEqual(readsAfterLaunch, 1, "the launch pass reads the delivered list once")

        // The OS delivered it while the app was away, and nothing tapped it.
        center.deliver(identifier: request.identifier, at: Date())
        services.lifecycle.foregroundActivated()
        wait("the delivered history event is recorded") { self.scalar("SELECT COUNT(*) FROM reminder_history_events WHERE event_kind = 'delivered'") == "1" }
        wait("the first return to the foreground read the delivered list") { center.deliveredReads >= readsAfterLaunch + 1 }
        settleBackgroundWork(services)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminder_history_events WHERE event_kind = 'delivered'"), "1")
        XCTAssertNotEqual(scalar("SELECT delivery_state FROM reminders"), "unknown")
        XCTAssertEqual(
            center.deliveredReads, readsAfterLaunch + 1,
            "one return to the foreground reads the delivered list once, though two steps reconcile reminders")

        services.lifecycle.foregroundActivated()
        wait("the second return to the foreground read the delivered list") {
            center.deliveredReads >= readsAfterLaunch + 2
        }
        settleBackgroundWork(services)
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM reminder_history_events WHERE event_kind = 'delivered'"), "1",
            "a second return to the foreground records nothing new")
        XCTAssertEqual(center.deliveredReads, readsAfterLaunch + 2)
    }

    func testASaveThatFailsRequestsNoDrainAndTakesNoBackgroundTime() throws {
        let grant = FakeBackgroundGrant()
        let services = try makeInjectedServices(
            grant: grant, center: FakeNotificationCenter(), lifecycleCenter: NotificationCenter(), recognizer: nil)
        let handoff = CompositionRecordingHandoff(
            captureID: UUID().uuidString, inProgressFileName: "missing-in-progress.wav",
            finalizedFileName: "missing-finalized.wav", startedAt: Date())

        var outcome: CompositionRecordingOutcome?
        services.root.saveRecording(handoff) { outcome = $0 }
        wait("the failed save has reported an outcome") { outcome != nil }
        pause(seconds: 0.5)

        let result = try XCTUnwrap(outcome)
        if case .saved = result { XCTFail("a recording with no audio must not be saved: \(result)") }
        XCTAssertEqual(services.root.jobRunner.startedDrainCount, 0, "a failed save requests no drain")
        XCTAssertEqual(grant.beginCount, 0, "and takes no background time")
    }

    /// Lets the drain and the reminder and completion passes it triggers finish, so teardown closes an idle core.
    private func settleBackgroundWork(_ services: AppServices) {
        wait("the job drain has finished") { !services.root.jobRunner.isDraining }
        // The passes the drain triggers have no observable end; give them the time they had before.
        pause(seconds: 1.5)
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

    // MARK: notification wiring

    func testStartInstallsTheDelegateBeforeTheRootStartsAndThenReconciles() throws {
        let services = try makeServices()
        XCTAssertEqual(hooks.events, [], "building the services touches no system object")

        services.start(isActive: false)

        XCTAssertEqual(hooks.events, ["install", "reconcile"])
        XCTAssertTrue(
            hooks.installedDelegate === services.root.notificationCenterDelegate,
            "the root's own delegate is the one installed")
    }

    func testALockedSessionDropsARequestedItem() throws {
        let services = try makeServices()
        services.navigation.path = [ManagementRoute.item("item-1").pathComponent]
        XCTAssertEqual(services.root.readScope, .captureOnly)

        services.navigation.revalidate()

        XCTAssertNil(services.navigation.requestedItemID, "a locked session keeps no item open")
    }
}
