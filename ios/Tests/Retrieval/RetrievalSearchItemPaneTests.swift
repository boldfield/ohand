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
        settle { rendered[RetrievalSearchView.itemPaneIdentifier] != nil && detailModels.first?.content != nil }

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
        settle { detail.content == nil }
        XCTAssertNil(detail.content, "a relock drops the pane's text")
        XCTAssertEqual(detail.failure, .locked)
    }

    func testACorrectionMadeInThePaneShowsTheNewTextThereAndInTheResults() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 1024, height: 768), sizeClass: .regular)
        searchModel.select(shown)
        settle { detailModels.first?.content != nil && detailModels.first?.itemControls?.snapshot != nil }
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

        settle {
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
        settle {
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

        settle {
            finished && MainActor.assumeIsolated { deletion.isRecorded } && searchModel.rows.isEmpty
                && detailModels.first?.content == nil
        }
        XCTAssertTrue(MainActor.assumeIsolated { deletion.isRecorded }, "the pane shows the deletion result")
        XCTAssertTrue(searchModel.rows.isEmpty, "the row is gone")
        XCTAssertNil(detailModels.first?.content, "no deleted text stays in the pane")
        XCTAssertEqual(searchModel.selectedReference, shown.reference, "the pane keeps the item it was showing")
        XCTAssertNotNil(rendered[RetrievalSearchView.itemPaneIdentifier])
    }

    func testAtCompactWidthAResultIsStillPushedAndNoPaneIsShown() throws {
        let itemID = try saveCapture()
        let shown = try showResult(itemID: itemID, size: CGSize(width: 390, height: 844), sizeClass: .compact)

        searchModel.select(shown)
        settle { events.contains(.appeared(.item(itemID), isPlaceholder: false)) && detailModels.first?.content != nil }

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
        itemID: String, size: CGSize, sizeClass: UserInterfaceSizeClass
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
                let built = SourceDetailModel.assembled(itemID: id, root: root)
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
        settle { true }

        navigation.open(entryID: ManagementCatalog.searchEntryID)
        settle { events.contains(.appeared(.entry(ManagementCatalog.searchEntryID), isPlaceholder: false)) }
        searchModel.query = "pane"
        searchModel.search()
        settle { rendered[RetrievalSearchView.rowIdentifier(shown)] != nil }
        XCTAssertEqual(searchModel.rows, [shown])
        return shown
    }

    /// Runs the main loop until the condition holds or ten seconds pass.
    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 10)
        repeat {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    private func saveCapture() throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(
            captureID: UUID().uuidString.lowercased(), text: "synthetic capture in the pane", capturedAt: Date()
        ) { outcome = $0 }
        settle { outcome != nil }
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
        settle { finished }
        XCTAssertNil(failure)
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
