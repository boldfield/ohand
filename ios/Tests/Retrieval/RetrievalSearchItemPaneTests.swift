import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class PaneUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class PaneSearcher: RetrievalSearching {
    var page = RetrievalPage(totalAccessible: 0, rows: [], offset: 0)

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        completion(.success(page))
    }
}

/// Reads through the real item-detail service, but holds the answer to the first read, taken before anything changes,
/// until the test releases it.
private final class HeldFirstDetailReader: SourceDetailReading, @unchecked Sendable {
    private let reader: SourceDetailReading
    private let lock = NSLock()
    private var readCount = 0
    private var release: CheckedContinuation<Void, Never>?
    private var heldFirstRead = false

    init(reader: SourceDetailReading) { self.reader = reader }

    var firstReadIsHeld: Bool { lock.withLock { heldFirstRead } }

    func detail(itemID: String) async throws -> CoreItemDetail {
        let isFirst = lock.withLock { () -> Bool in
            readCount += 1
            return readCount == 1
        }
        let answer = try await reader.detail(itemID: itemID)
        if isFirst {
            await withCheckedContinuation { continuation in
                lock.withLock {
                    release = continuation
                    heldFirstRead = true
                }
            }
        }
        return answer
    }

    func releaseFirstRead() {
        let continuation = lock.withLock { () -> CheckedContinuation<Void, Never>? in
            defer { release = nil }
            return release
        }
        continuation?.resume()
    }
}

/// The real reminder controls, except that a time correction waits for the test before it is sent.
private final class HeldTimeReminderEditor: ReminderEditing, @unchecked Sendable {
    private let editor: ReminderEditing
    private let lock = NSLock()
    private var release: CheckedContinuation<Void, Never>?
    private var heldSave = false

    init(editor: ReminderEditing) { self.editor = editor }

    var saveIsHeld: Bool { lock.withLock { heldSave } }

    func reminder(itemID: String) async throws -> ReminderReadAnswer {
        try await editor.reminder(itemID: itemID)
    }

    func correctTime(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int, instant: Date,
        timeZoneID: String
    ) async throws -> ReminderControlReceipt {
        await withCheckedContinuation { continuation in
            lock.withLock {
                release = continuation
                heldSave = true
            }
        }
        return try await editor.correctTime(
            commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
            expectedReminderVersion: expectedReminderVersion, instant: instant, timeZoneID: timeZoneID)
    }

    func cancel(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int
    ) async throws -> ReminderControlReceipt {
        try await editor.cancel(
            commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
            expectedReminderVersion: expectedReminderVersion)
    }

    func releaseSave() {
        let continuation = lock.withLock { () -> CheckedContinuation<Void, Never>? in
            defer { release = nil }
            return release
        }
        continuation?.resume()
    }
}

/// Hosts the real app root with the production search entry and destinations over the real composition root and Rust
/// core with synthetic data. At regular width a selected result opens the real source detail beside the kept results;
/// at compact width it is pushed as before.
final class RetrievalSearchItemPaneTests: XCTestCase {
    private static let routeID = "route-retrieval-item-pane"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var services: AppServices!
    private var navigation: ManagementNavigationModel!
    private var searchModel: RetrievalSearchModel!
    private var searcher = PaneSearcher()
    private var events: [ManagementPresentationEvent] = []
    private var rendered: [String: RetrievalRenderedElement] = [:]
    private var detailModels: [SourceDetailModel] = []
    private var retainedWindow: UIWindow?

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("retrieval-item-pane-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let lifecycle = LifecycleCoordinator()
        root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: PaneUnreachableSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false)),
            modules: [lifecycle])
        services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: NotificationCenter(),
            isProtectedDataAvailable: { true })
        navigation = ManagementNavigationModel(
            readScope: { [unowned self] in self.root.readScope },
            authenticate: { [unowned self] reason, completion in
                self.root.authenticateReads(reason: reason, completion: completion)
            })
        searchModel = .assembled(searcher: searcher, root: root, navigation: navigation)
    }

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        detailModels = []
        navigation = nil
        searchModel = nil
        services.shutDown()
        services = nil
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testAtRegularWidthAResultOpensTheRealDetailBesideTheKeptResults() throws {
        let itemID = try saveCapture()
        try correctText(itemID: itemID, to: "synthetic corrected words")
        let shown = try showResult(itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular)

        searchModel.select(shown)
        settle("the item pane is shown with the detail loaded") {
            rendered[RetrievalSearchView.itemPaneIdentifier] != nil && detailModels.first?.content != nil
        }

        XCTAssertEqual(
            navigation.path.map { ManagementRoute(pathComponent: $0) }, [.entry(ManagementCatalog.searchEntryID)],
            "nothing is pushed: the search stays the screen")
        XCTAssertFalse(events.contains(.appeared(.item(itemID), isPlaceholder: false)))
        XCTAssertEqual(searchModel.rows, [shown], "the results are kept beside the detail")

        let row = try XCTUnwrap(rendered[RetrievalSearchView.rowIdentifier(shown)], "the result row is still shown")
        XCTAssertLessThanOrEqual(row.frame.width, RetrievalSearchView.regularResultsColumnWidth)
        XCTAssertEqual(
            rendered[RetrievalSearchView.selectedRowIdentifier]?.text, itemID, "the selected result is outlined")
        let selectedOutline = try XCTUnwrap(rendered[RetrievalSearchView.selectedRowIdentifier]?.frame)
        XCTAssertEqual(selectedOutline.midY, row.frame.midY, accuracy: 1, "the outline is on the selected row")
        let pane = try XCTUnwrap(rendered[RetrievalSearchView.itemPaneIdentifier])
        XCTAssertEqual(pane.text, itemID, "the pane shows the selected item")
        XCTAssertGreaterThanOrEqual(pane.frame.minX, row.frame.maxX, "the detail is to the right of the results")
        XCTAssertGreaterThan(pane.frame.width, RetrievalSearchView.regularResultsColumnWidth)
        XCTAssertLessThanOrEqual(pane.frame.maxX, 1024)

        let detail = try XCTUnwrap(detailModels.last)
        XCTAssertEqual(detailModels.count, 1, "one real source-detail screen, built by the production destination")
        XCTAssertEqual(detail.itemID, itemID)
        XCTAssertEqual(detail.content?.sections.map(\.kind), [.original, .correction])
        XCTAssertEqual(
            detail.content?.sections.map(\.text), ["synthetic capture in the pane", "synthetic corrected words"])

        session.relock(.explicit)
        settle("a relock drops the pane's text") { detail.content == nil && detail.failure == .locked }
        XCTAssertNil(detail.content, "a relock drops the pane's text")
        XCTAssertEqual(detail.failure, .locked)
    }

    func testACorrectionMadeInThePaneShowsTheNewTextThereAndInTheResults() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular)
        searchModel.select(shown)
        settle("the pane's detail and controls loaded") {
            detailModels.first?.content != nil && detailModels.first?.itemControls?.snapshot != nil
        }
        XCTAssertEqual(detailModels.first?.content?.sections.map(\.text), ["synthetic capture in the pane"])
        let controls = try XCTUnwrap(detailModels.first?.itemControls)
        let corrected = RetrievalResultRow(
            reference: shown.reference, text: "synthetic words after the correction",
            originalText: "synthetic capture in the pane", basis: .corrected, matched: .current,
            snippet: RetrievalSnippet(text: "synthetic words after the correction", span: nil),
            scopeName: "Personal", capturedAt: shown.capturedAt, itemType: .note)
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [corrected], offset: 0)

        var finished = false
        Task { @MainActor in
            controls.textDraft = "synthetic words after the correction"
            await controls.saveText()
            finished = true
        }

        settle("the pane's correction finished and the pane and the results show it") {
            finished && searchModel.rows == [corrected]
                && detailModels.first?.content?.sections.last?.text == "synthetic words after the correction"
        }
        XCTAssertEqual(controls.outcome, .saved(.text), "the pane keeps its confirmation")
        XCTAssertEqual(
            detailModels.first?.content?.sections.last?.text, "synthetic words after the correction",
            "the pane's own detail reads the item again")
        XCTAssertEqual(searchModel.rows, [corrected], "the results were asked for again")
        XCTAssertEqual(searchModel.selectedReference, shown.reference)
        XCTAssertEqual(detailModels.count, 1, "the pane is not rebuilt")
    }

    func testADeletionMadeInThePaneLeavesNoStaleTextAndKeepsThePaneOnItsResult() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular)
        searchModel.select(shown)
        settle("the pane's detail and delete control loaded") {
            detailModels.first?.content != nil
                && MainActor.assumeIsolated { detailModels.first?.deletion?.revision != nil }
        }
        searcher.page = RetrievalPage(totalAccessible: 0, rows: [], offset: 0)
        let deletion = try XCTUnwrap(detailModels.first?.deletion)

        var finished = false
        Task { @MainActor in
            deletion.requestDeletion()
            await deletion.confirmDeletion()
            finished = true
        }

        settle("the deletion finished and the pane and the results dropped the item") {
            finished && MainActor.assumeIsolated { deletion.isRecorded } && searchModel.rows.isEmpty
                && detailModels.first?.content == nil
        }
        XCTAssertTrue(MainActor.assumeIsolated { deletion.isRecorded }, "the pane shows the deletion result")
        XCTAssertTrue(searchModel.rows.isEmpty, "the row is gone")
        XCTAssertNil(detailModels.first?.content, "no deleted text stays in the pane")
        XCTAssertEqual(searchModel.selectedReference, shown.reference, "the pane keeps the item it was showing")
        XCTAssertNotNil(rendered[RetrievalSearchView.itemPaneIdentifier])
    }

    /// A change stored outside the pane, as a notification's Done or another screen stores it, reaches the same signal
    /// the pane's own controls do; the pane's detail reads its item again without being rebuilt.
    func testAChangeStoredOutsideThePaneIsReadAgainByThePanesDetail() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular)
        searchModel.select(shown)
        settle("the pane's detail, controls and delete control loaded") {
            detailModels.first?.content != nil && detailModels.first?.itemControls?.snapshot != nil
                && MainActor.assumeIsolated { detailModels.first?.deletion?.revision != nil }
        }
        let detail = try XCTUnwrap(detailModels.first)
        let controls = try XCTUnwrap(detail.itemControls)
        let deletion = try XCTUnwrap(detail.deletion)
        XCTAssertEqual(detail.content?.lifecycleLabel, "Active")

        // A text correction stored elsewhere: the pane shows the new words. The correction editor keeps the revision
        // the person started from, so a save from it meets the conflict that shows the newer values.
        let corrected = RetrievalResultRow(
            reference: shown.reference, text: "synthetic words corrected elsewhere",
            originalText: "synthetic capture in the pane", basis: .corrected, matched: .current,
            snippet: RetrievalSnippet(text: "synthetic words corrected elsewhere", span: nil),
            scopeName: "Personal", capturedAt: shown.capturedAt, itemType: .note)
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [corrected], offset: 0)
        let startRevision = try revision(of: itemID)
        try correctText(itemID: itemID, to: "synthetic words corrected elsewhere")
        let correctedRevision = try revision(of: itemID)
        settle("the pane and the results show the text corrected elsewhere") {
            detail.content?.currentText == "synthetic words corrected elsewhere"
                && MainActor.assumeIsolated { deletion.revision == correctedRevision }
                && searchModel.rows == [corrected]
        }
        XCTAssertEqual(detail.content?.currentText, "synthetic words corrected elsewhere", "the pane reads the item again")
        XCTAssertEqual(detail.content?.sections.last?.text, "synthetic words corrected elsewhere")
        XCTAssertEqual(
            MainActor.assumeIsolated { deletion.revision }, correctedRevision,
            "the delete control applies against the revision the pane now shows")
        XCTAssertEqual(controls.snapshot?.revision, startRevision, "the editor keeps the revision it started from")
        XCTAssertEqual(searchModel.rows, [corrected])

        // A completion stored elsewhere, as a notification's Done stores it: the pane shows the item completed.
        var completed = false
        var failure: Error?
        Task {
            do { _ = try await root.itemLifecycleControls.complete(
                commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: correctedRevision) }
            catch { failure = error }
            completed = true
        }
        settle("the completion was stored and the pane shows it") { completed && detail.content?.lifecycleLabel == "Completed" }
        XCTAssertNil(failure)
        XCTAssertEqual(detail.content?.lifecycleLabel, "Completed", "the pane shows the stored state")
        let completedRevision = try revision(of: itemID)
        settle("the delete control reads the completed revision") { MainActor.assumeIsolated { deletion.revision == completedRevision } }
        XCTAssertEqual(MainActor.assumeIsolated { deletion.revision }, completedRevision)

        // A deletion stored elsewhere: no deleted text stays in the pane, which says the item is not here.
        searcher.page = RetrievalPage(totalAccessible: 0, rows: [], offset: 0)
        var deleted = false
        Task {
            do { _ = try await root.itemDeletion.delete(itemID: itemID, expectedRevision: completedRevision) }
            catch { failure = error }
            deleted = true
        }
        settle("the deletion was stored and the pane says the item is not here") {
            deleted && detail.failure == .notFound && searchModel.rows.isEmpty && detail.content == nil
        }
        XCTAssertNil(failure)
        XCTAssertNil(detail.content, "no deleted text stays in the pane")
        XCTAssertEqual(detail.failure, .notFound)
        XCTAssertFalse(MainActor.assumeIsolated { deletion.isRecorded }, "the pane did not delete it")
        XCTAssertEqual(searchModel.selectedReference, shown.reference)
        XCTAssertEqual(detailModels.count, 1, "the pane is not rebuilt")
    }

    /// A change stored while the pane's first read is still in flight: that read, taken before the change, never
    /// replaces the pane's answer; the pane shows the item as it now is.
    func testAChangeStoredWhileThePanesFirstReadIsInFlightIsShownWhenThatReadEnds() throws {
        let itemID = try saveCapture()
        let heldReader = HeldFirstDetailReader(reader: root.itemDetails)
        // Held strongly, as the production assembly holds it: the screen can outlive this test's teardown.
        let root: ServiceCompositionRoot = self.root
        let shown = try showResult(
            itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular,
            makeDetail: { id in
                let built = SourceDetailModel(
                    itemID: id, reader: heldReader, opener: root.playback, readScope: { root.readScope })
                built.deletion = .assembled(itemID: id, root: root)
                return built
            })
        searchModel.select(shown)
        settle("the pane's first read is held") { heldReader.firstReadIsHeld }
        XCTAssertTrue(heldReader.firstReadIsHeld, "the pane's first read is in flight")
        let detail = try XCTUnwrap(detailModels.first)
        XCTAssertNil(detail.content)

        let corrected = RetrievalResultRow(
            reference: shown.reference, text: "synthetic words corrected during the first read",
            originalText: "synthetic capture in the pane", basis: .corrected, matched: .current,
            snippet: RetrievalSnippet(text: "synthetic words corrected during the first read", span: nil),
            scopeName: "Personal", capturedAt: shown.capturedAt, itemType: .note)
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [corrected], offset: 0)
        try correctText(itemID: itemID, to: "synthetic words corrected during the first read")
        settle("the pane shows the text corrected during the first read") {
            detail.content?.currentText == "synthetic words corrected during the first read"
        }

        heldReader.releaseFirstRead()
        settle("the released read ended and the results show the correction") { !detail.isLoading && searchModel.rows == [corrected] }
        // Give the released read time to land, were it allowed to.
        runMainLoop(for: 1)
        XCTAssertEqual(
            detail.content?.currentText, "synthetic words corrected during the first read",
            "the read taken before the change does not replace the pane's answer")
        XCTAssertEqual(detail.content?.sections.last?.text, "synthetic words corrected during the first read")
        XCTAssertEqual(searchModel.rows, [corrected])
        XCTAssertEqual(detailModels.count, 1, "the pane is not rebuilt")
    }

    /// A completion stored outside the pane, as a notification's Done stores it, while the pane's own reminder save is in
    /// flight: the pane waits for its save to settle, keeps the save's answer, and then shows the item completed.
    func testAChangeStoredWhileThePanesReminderSaveIsInFlightIsShownOnceTheSaveSettles() throws {
        let itemID = try saveCapture()
        try correctType(itemID: itemID, to: .action)
        let heldEditor = HeldTimeReminderEditor(editor: root.reminderControls)
        let root: ServiceCompositionRoot = self.root
        let shown = try showResult(
            itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular,
            makeDetail: { id in
                let built = SourceDetailModel.assembled(itemID: id, root: root)
                built.reminderEditor = ReminderEditModel(itemID: id, editor: heldEditor)
                return built
            })
        searchModel.select(shown)
        settle("the pane's detail and reminder editor loaded") {
            detailModels.first?.content != nil && detailModels.first?.reminderEditor?.hasRead == true
                && detailModels.first?.reminderEditor?.isLoading == false
                && MainActor.assumeIsolated { detailModels.first?.deletion?.revision != nil }
        }
        let detail = try XCTUnwrap(detailModels.first)
        let reminderEditor = try XCTUnwrap(detail.reminderEditor)
        XCTAssertEqual(detail.content?.lifecycleLabel, "Active")

        var saved = false
        Task { @MainActor in
            reminderEditor.draftDate = Date().addingTimeInterval(2 * 86_400)
            reminderEditor.draftTimeZoneID = "UTC"
            await reminderEditor.save()
            saved = true
        }
        settle("the reminder save is held and in flight") { heldEditor.saveIsHeld && reminderEditor.isWorking }
        XCTAssertTrue(reminderEditor.isWorking, "the pane's reminder save is in flight")

        var completed = false
        var failure: Error?
        let activeRevision = try revision(of: itemID)
        Task {
            do { _ = try await root.itemLifecycleControls.complete(
                commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: activeRevision) }
            catch { failure = error }
            completed = true
        }
        settle("the completion was stored") { completed }
        XCTAssertNil(failure)
        // Longer than the screen's watch interval, so a read that did not wait would have landed.
        runMainLoop(for: 1)
        XCTAssertEqual(detail.content?.lifecycleLabel, "Active", "the read waits while the pane's own save works")

        heldEditor.releaseSave()
        settle("the reminder save settled and the pane shows the item completed") {
            saved && !reminderEditor.isWorking && detail.content?.lifecycleLabel == "Completed"
        }
        XCTAssertTrue(saved)
        XCTAssertEqual(detail.content?.lifecycleLabel, "Completed", "the pane reads the item once its save settles")
        XCTAssertEqual(reminderEditor.message, .itemClosed, "the save's own answer stays shown")
        let completedRevision = try revision(of: itemID)
        settle("the delete control reads the completed revision") {
            MainActor.assumeIsolated { detail.deletion?.revision == completedRevision }
        }
        XCTAssertEqual(MainActor.assumeIsolated { detail.deletion?.revision }, completedRevision)
        XCTAssertEqual(searchModel.selectedReference, shown.reference)
        XCTAssertEqual(detailModels.count, 1, "the pane is not rebuilt")
    }

    func testAtCompactWidthAResultIsStillPushedAndNoPaneIsShown() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 390, height: 844), sizeClass: .compact)

        searchModel.select(shown)
        settle("the item is pushed with its detail loaded") {
            events.contains(.appeared(.item(itemID), isPlaceholder: false)) && detailModels.first?.content != nil
        }

        XCTAssertEqual(
            navigation.path.map { ManagementRoute(pathComponent: $0) },
            [.entry(ManagementCatalog.searchEntryID), .item(itemID)], "detail is pushed above the search")
        XCTAssertNil(searchModel.selectedReference)
        XCTAssertNil(rendered[RetrievalSearchView.selectedRowIdentifier])
        XCTAssertNil(rendered[RetrievalSearchView.itemPaneIdentifier])
        XCTAssertEqual(detailModels.first?.content?.sections.map(\.kind), [.original])
    }

    // MARK: helpers

    /// Hosts the root, opens Search through the production entry and shows one result for the saved item.
    private func showResult(
        itemID: String, size: CGSize, sizeClass: UserInterfaceSizeClass,
        makeDetail: ((String) -> SourceDetailModel)? = nil
    ) throws -> RetrievalResultRow {
        let reference = try XCTUnwrap(RetrievalItemReference(candidate: itemID))
        let shown = RetrievalResultRow(
            reference: reference, text: "synthetic capture in the pane", originalText: nil, basis: .original,
            matched: .current, snippet: RetrievalSnippet(text: "synthetic capture in the pane", span: nil),
            scopeName: "Personal", capturedAt: Date(), itemType: .note)
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)
        let context = ManagementContext(
            voiceRecovery: services.managementContext.voiceRecovery, retrieval: searchModel,
            retention: services.managementContext.retention,
            providerSettings: services.managementContext.providerSettings,
            promptSettings: services.managementContext.promptSettings,
            sourceDetail: { [unowned self] id in
                let built = makeDetail?(id) ?? SourceDetailModel.assembled(itemID: id, root: root)
                detailModels.append(built)
                return built
            },
            suggestion: services.managementContext.suggestion,
            privacySettings: services.managementContext.privacySettings,
            retrievalRenderReporter: { [unowned self] in rendered = $0 })
        let rootView = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: context,
            destinations: ManagementCatalog.destinations, presentationReporter: { [unowned self] in events.append($0) })
            .environment(\.horizontalSizeClass, sizeClass)
        let window = UIWindow(frame: CGRect(origin: .zero, size: size))
        let controller = UIHostingController(rootView: rootView)
        if #available(iOS 17.0, *) {
            controller.traitOverrides.horizontalSizeClass = sizeClass == .regular ? .regular : .compact
        }
        window.rootViewController = controller
        window.makeKeyAndVisible()
        retainedWindow = window
        pauseRunLoop()

        navigation.open(entryID: ManagementCatalog.searchEntryID)
        settle("the search entry appeared") {
            events.contains(.appeared(.entry(ManagementCatalog.searchEntryID), isPlaceholder: false))
        }
        searchModel.query = "pane"
        searchModel.search()
        settle("the result row is shown") { rendered[RetrievalSearchView.rowIdentifier(shown)] != nil }
        XCTAssertEqual(searchModel.rows, [shown])
        return shown
    }

    /// Lays the hosted view out and runs the main loop briefly.
    private func pauseRunLoop() {
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    /// Runs the main loop until the condition holds; fails the test naming it if ten seconds pass first.
    private func settle(
        _ condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) {
        pauseRunLoop()
        waitUntilPumping(condition, timeout: 10, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    /// Runs the main loop for a fixed time, for checks that something does not happen.
    private func runMainLoop(for seconds: TimeInterval) {
        let deadline = Date(timeIntervalSinceNow: seconds)
        while Date() < deadline {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        }
    }

    private func saveCapture() throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(
            captureID: UUID().uuidString.lowercased(), text: "synthetic capture in the pane", capturedAt: Date()
        ) { outcome = $0 }
        settle("the save completes") { outcome != nil }
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func correctText(itemID: String, to text: String) throws {
        let revision = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        var finished = false
        var failure: Error?
        Task {
            do { _ = try await root.itemCorrections.correctText(
                commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: revision, text: text) }
            catch { failure = error }
            finished = true
        }
        settle("the call finished") { finished }
        XCTAssertNil(failure)
    }

    private func correctType(itemID: String, to itemType: ItemCorrectionItemType) throws {
        let revision = try revision(of: itemID)
        var finished = false
        var failure: Error?
        Task {
            do { _ = try await root.itemCorrections.correctType(
                commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: revision,
                itemType: itemType) }
            catch { failure = error }
            finished = true
        }
        settle("the call finished") { finished }
        XCTAssertNil(failure)
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
    }

    private func scalar(_ sql: String) -> String {
        let databaseURL = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
