import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class AnsweringSearcher: RetrievalSearching {
    var page = RetrievalPage(totalAccessible: 0, rows: [], offset: 0)

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        completion(.success(page))
    }
}

/// Hosts the real app root view with the search screen assembled the way production assembles it (`assembled` over the
/// real composition root and navigation model), selects a visible result, and goes Back.
final class RetrievalSearchNavigationTests: XCTestCase {
    private static let itemID = "0b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"
    private static let routeID = "route-retrieval-navigation"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var services: AppServices!
    private var navigation: ManagementNavigationModel!
    private var searchModel: RetrievalSearchModel!
    private var searcher = AnsweringSearcher()
    private var events: [ManagementPresentationEvent] = []
    private var rendered: [String: RetrievalRenderedElement] = [:]
    private var retainedWindow: UIWindow?

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("retrieval-navigation-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let lifecycle = LifecycleCoordinator()
        root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(), readSession: session,
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

    private func host() {
        var destinations = ManagementDestinations()
        destinations.item = { _, itemID in AnyView(Text("source detail \(itemID)")) }
        let context = ManagementContext(
            voiceRecovery: services.managementContext.voiceRecovery, retrieval: searchModel,
            retention: services.managementContext.retention,
            providerSettings: services.managementContext.providerSettings,
            promptSettings: services.managementContext.promptSettings,
            sourceDetail: services.managementContext.sourceDetail,
            suggestion: services.managementContext.suggestion,
            privacySettings: services.managementContext.privacySettings,
            retrievalRenderReporter: { [unowned self] in rendered = $0 })
        let rootView = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: context,
            destinations: destinations, presentationReporter: { [unowned self] in events.append($0) })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: rootView)
        window.makeKeyAndVisible()
        retainedWindow = window
        pauseRunLoop()
    }

    /// Lays the hosted view out and runs the main loop briefly.
    private func pauseRunLoop() {
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    /// Runs the main loop until the condition holds; fails the test naming it if ten seconds pass first.
    private func settle(
        _ condition: String, timeout: TimeInterval = 10, file: StaticString = #filePath, line: UInt = #line,
        _ holds: () -> Bool
    ) {
        pauseRunLoop()
        waitUntilPumping("\(condition) (events so far: \(events))", timeout: timeout, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    func testSelectingAResultShowsDetailAboveSearchAndBackReturnsToTheKeptResults() throws {
        let reference = try XCTUnwrap(RetrievalItemReference(candidate: Self.itemID))
        let shown = RetrievalResultRow(
            reference: reference, text: "synthetic roofer words", originalText: nil, basis: .original,
            matched: .current, snippet: RetrievalSnippet(text: "synthetic roofer words", span: nil),
            scopeName: "Personal", capturedAt: Date(timeIntervalSince1970: 1_791_462_600), itemType: .note)
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(root.readScope, .authenticated)
        host()

        navigation.open(entryID: ManagementCatalog.searchEntryID)
        settle("the search entry appeared") {
            events.contains(.appeared(.entry(ManagementCatalog.searchEntryID), isPlaceholder: false))
        }
        searchModel.query = "roofer"
        searchModel.search()
        let rowTextIdentifier = RetrievalSearchView.rowTextIdentifier(shown)
        settle("the result is on screen") { rendered[rowTextIdentifier] != nil }
        XCTAssertEqual(rendered[rowTextIdentifier]?.text, "synthetic roofer words", "the result is on screen")

        searchModel.select(shown)
        settle("the detail appeared above the search") {
            events.contains(.appeared(.item(Self.itemID), isPlaceholder: false))
                && navigation.path.count == 2 && navigation.requestedItemID == Self.itemID
        }

        XCTAssertEqual(
            navigation.path.map { ManagementRoute(pathComponent: $0) },
            [.entry(ManagementCatalog.searchEntryID), .item(Self.itemID)], "detail is pushed above the search")
        XCTAssertEqual(navigation.requestedItemID, Self.itemID)

        let searchRoute = ManagementRoute.entry(ManagementCatalog.searchEntryID)
        func lastIndex(of event: ManagementPresentationEvent) -> Int? { events.lastIndex(of: event) }
        let itemAppeared = try XCTUnwrap(lastIndex(of: .appeared(.item(Self.itemID), isPlaceholder: false)))
        navigation.path.removeLast()
        // 30 s, not 10: the pop transition completes (and fires the search's appearance) only after the hosted window has
        // laid out and animated, which run 40247595411 showed a loaded Intel runner can stretch past 10 s. The condition is unchanged.
        settle("the search appeared again after the detail and the detail left the screen", timeout: 30) {
            guard events.contains(.disappeared(.item(Self.itemID))) else { return false }
            guard let searchAppeared = lastIndex(of: .appeared(searchRoute, isPlaceholder: false)) else { return false }
            return searchAppeared > itemAppeared
        }

        XCTAssertEqual(navigation.path, [ManagementCatalog.searchEntryID], "Back returns to the search, not to capture")
        XCTAssertTrue(events.contains(.disappeared(.item(Self.itemID))), "the detail left the screen")
        let searchReappeared = try XCTUnwrap(lastIndex(of: .appeared(searchRoute, isPlaceholder: false)))
        XCTAssertGreaterThan(searchReappeared, itemAppeared, "the search screen is shown again after the detail")
        XCTAssertEqual(searchModel.query, "roofer", "the typed words were kept across detail")
        XCTAssertEqual(searchModel.rows, [shown], "the prior results were kept across detail")
        XCTAssertEqual(searchModel.phase, .results)
    }
}
