import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class HostedUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class HostedSearcher: RetrievalSearching {
    var page = RetrievalPage(totalAccessible: 0, rows: [], offset: 0)

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        completion(.success(page))
    }
}

/// Hosts the real source-detail screen over the real composition root and Rust core with synthetic data: once reached
/// from a search result through the production destinations, and once to read what each state renders.
final class SourceDetailHostedTests: XCTestCase {
    private static let routeID = "route-source-detail-hosted"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var services: AppServices!
    private var navigation: ManagementNavigationModel!
    private var retainedWindow: UIWindow?
    private var onScreen: Set<String> = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("source-detail-hosted-\(UUID().uuidString)", isDirectory: true)
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
                sender: HostedUnreachableSender(), readSession: session,
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
    }

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        navigation = nil
        services.shutDown()
        services = nil
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testSelectingASearchResultOpensTheRealSourceDetailWithOriginalAndCorrectedText() throws {
        let (itemID, _) = try saveCapture()
        try correctText(itemID: itemID, to: "synthetic corrected words")
        let reference = try XCTUnwrap(RetrievalItemReference(candidate: itemID))
        let shown = RetrievalResultRow(
            reference: reference, text: "synthetic corrected words", originalText: "synthetic capture to play",
            basis: .corrected, matched: .current,
            snippet: RetrievalSnippet(text: "synthetic corrected words", span: nil),
            scopeName: "Personal", capturedAt: Date(), itemType: .note)
        let searcher = HostedSearcher()
        searcher.page = RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)
        let searchModel = RetrievalSearchModel.assembled(searcher: searcher, root: root, navigation: navigation)
        var detailModels: [SourceDetailModel] = []
        var events: [ManagementPresentationEvent] = []
        let context = ManagementContext(
            voiceRecovery: services.managementContext.voiceRecovery, retrieval: searchModel,
            retention: services.managementContext.retention,
            providerSettings: services.managementContext.providerSettings,
            promptSettings: services.managementContext.promptSettings,
            sourceDetail: { [root] id in
                let built = SourceDetailModel.assembled(itemID: id, root: root!)
                detailModels.append(built)
                return built
            },
            suggestion: services.managementContext.suggestion,
            privacySettings: services.managementContext.privacySettings,
            retrievalRenderReporter: { _ in })
        let rootView = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: context,
            destinations: ManagementCatalog.destinations, presentationReporter: { events.append($0) })
        host(AnyView(rootView))

        navigation.open(entryID: ManagementCatalog.searchEntryID)
        settle("the search entry appeared") {
            events.contains(.appeared(.entry(ManagementCatalog.searchEntryID), isPlaceholder: false))
        }
        searchModel.query = "corrected"
        searchModel.search()
        settle("the search shows the row") { searchModel.rows == [shown] }
        searchModel.select(shown)
        settle("the item's detail content loaded and its destination appeared") {
            detailModels.first?.content != nil && events.contains(.appeared(.item(itemID), isPlaceholder: false))
        }

        XCTAssertTrue(
            events.contains(.appeared(.item(itemID), isPlaceholder: false)),
            "the production destination, not a placeholder, is shown")
        let detail = try XCTUnwrap(detailModels.first)
        XCTAssertEqual(detail.itemID, itemID)
        XCTAssertEqual(detail.content?.sections.map(\.kind), [.original, .correction])
        XCTAssertEqual(
            detail.content?.sections.map(\.text), ["synthetic capture to play", "synthetic corrected words"])
    }

    func testTheRenderedScreenShowsSectionsPlayAndThenTheDisabledNoticeAfterDeletion() throws {
        let (itemID, captureID) = try saveCapture()
        try correctText(itemID: itemID, to: "synthetic corrected words")
        _ = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        host(AnyView(NavigationStack { SourceDetailView(model: model, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        }) }))

        settle("the sections and the play button are on screen") {
            model.content != nil && onScreen.contains(SourceDetailView.playButtonIdentifier)
                && onScreen.contains(SourceDetailView.sectionIdentifier(.original))
                && onScreen.contains(SourceDetailView.sectionIdentifier(.correction))
        }
        XCTAssertTrue(onScreen.contains(SourceDetailView.sectionIdentifier(.original)))
        XCTAssertTrue(onScreen.contains(SourceDetailView.sectionIdentifier(.correction)))
        XCTAssertTrue(onScreen.contains(SourceDetailView.playButtonIdentifier), "retained audio offers play")
        XCTAssertFalse(onScreen.contains(SourceDetailView.audioNoticeIdentifier))

        Task { @MainActor in await model.play() }
        settle("playing shows pause and no play") {
            onScreen.contains(SourceDetailView.pauseButtonIdentifier) && !onScreen.contains(SourceDetailView.playButtonIdentifier)
        }
        XCTAssertFalse(onScreen.contains(SourceDetailView.playButtonIdentifier), "playing shows pause only")

        let revision = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        var deleted = false
        Task {
            _ = try await root.itemDeletion.delete(itemID: itemID, expectedRevision: revision)
            deleted = true
        }
        settle("the deletion was recorded") { deleted }
        settle("the disabled notice replaces play and pause") {
            onScreen.contains(SourceDetailView.audioNoticeIdentifier)
                && !onScreen.contains(SourceDetailView.playButtonIdentifier)
                && !onScreen.contains(SourceDetailView.pauseButtonIdentifier)
        }

        XCTAssertTrue(onScreen.contains(SourceDetailView.audioNoticeIdentifier), "disabled notice is visible")
        XCTAssertFalse(onScreen.contains(SourceDetailView.playButtonIdentifier), "no play button remains")
        XCTAssertFalse(onScreen.contains(SourceDetailView.pauseButtonIdentifier))
    }

    func testTheRenderedScreenShowsOnlyTheLockedNoticeAfterARelock() throws {
        let (itemID, _) = try saveCapture()
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        host(AnyView(NavigationStack { SourceDetailView(model: model, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        }) }))
        settle("the original section is on screen") { onScreen.contains(SourceDetailView.sectionIdentifier(.original)) }
        XCTAssertTrue(onScreen.contains(SourceDetailView.sectionIdentifier(.original)))

        session.relock(.explicit)
        settle("the locked notice replaces the text") {
            onScreen.contains(SourceDetailView.lockedIdentifier)
                && !onScreen.contains(SourceDetailView.sectionIdentifier(.original))
                && !onScreen.contains(SourceDetailView.currentTextIdentifier)
        }

        XCTAssertTrue(onScreen.contains(SourceDetailView.lockedIdentifier))
        XCTAssertFalse(onScreen.contains(SourceDetailView.sectionIdentifier(.original)), "no text stays visible")
        XCTAssertFalse(onScreen.contains(SourceDetailView.currentTextIdentifier))
    }

    func testTheScreenFitsCompactAndRegularWidthInBothAppearancesAndAtTheLargestText() throws {
        let (itemID, captureID) = try saveCapture()
        try correctText(itemID: itemID, to: "synthetic corrected words")
        _ = try seedAudio(captureID: captureID)
        let layouts: [(size: CGSize, sizeClass: UserInterfaceSizeClass)] = [
            (CGSize(width: 390, height: 844), .compact), (CGSize(width: 700, height: 900), .regular),
        ]
        let identifiers = [
            SourceDetailView.currentTextIdentifier, SourceDetailView.sectionIdentifier(.original),
            SourceDetailView.sectionIdentifier(.correction), SourceDetailView.playButtonIdentifier,
        ]
        for layout in layouts {
            for style in [UIUserInterfaceStyle.light, .dark] {
                for category in [ContentSizeCategory.large, .accessibilityExtraExtraExtraLarge] {
                    let label = "\(layout.sizeClass) \(style == .dark ? "dark" : "light") \(category)"
                    var frames: [String: CGRect] = [:]
                    let model = SourceDetailModel.assembled(itemID: itemID, root: root)
                    let screen = NavigationStack {
                        SourceDetailView(model: model, layoutReporter: { id, frame in frames[id] = frame })
                    }
                    .environment(\.horizontalSizeClass, layout.sizeClass)
                    .environment(\.sizeCategory, category)
                    host(AnyView(screen), size: layout.size, style: style)
                    settle("every checked element is laid out, \(label)") { identifiers.allSatisfy { frames[$0].map { !$0.isEmpty } ?? false } }

                    XCTAssertEqual(
                        retainedWindow?.rootViewController?.traitCollection.userInterfaceStyle, style, label)
                    for identifier in identifiers {
                        let frame = try XCTUnwrap(frames[identifier], "\(identifier) is laid out, \(label)")
                        XCTAssertGreaterThanOrEqual(frame.minX, 0, "\(identifier) is not clipped, \(label)")
                        XCTAssertLessThanOrEqual(
                            frame.maxX, layout.size.width, "\(identifier) is not clipped, \(label)")
                    }
                    let original = try XCTUnwrap(frames[SourceDetailView.sectionIdentifier(.original)])
                    let correction = try XCTUnwrap(frames[SourceDetailView.sectionIdentifier(.correction)])
                    XCTAssertLessThanOrEqual(original.maxY, correction.minY, "the sections do not overlap, \(label)")
                    let play = try XCTUnwrap(frames[SourceDetailView.playButtonIdentifier])
                    XCTAssertGreaterThanOrEqual(play.height, 44, "play is a full touch target, \(label)")
                    XCTAssertGreaterThanOrEqual(play.minY, correction.maxY, "play sits below the text, \(label)")
                    let margin = DSSpacing.sideMargin(for: layout.sizeClass)
                    XCTAssertGreaterThanOrEqual(original.minX, margin, "the card keeps the side margin, \(label)")
                    retainedWindow?.isHidden = true
                    retainedWindow = nil
                    pauseRunLoop()
                }
            }
        }
    }

    // MARK: helpers

    private func host(
        _ view: AnyView, size: CGSize = CGSize(width: 390, height: 844), style: UIUserInterfaceStyle = .unspecified
    ) {
        let window = UIWindow(frame: CGRect(origin: .zero, size: size))
        window.overrideUserInterfaceStyle = style
        window.rootViewController = UIHostingController(rootView: view)
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
        _ condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) {
        pauseRunLoop()
        waitUntilPumping(condition, timeout: 10, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    private var layout: ProtectedStorageLayout { ProtectedStorageLayout(rootDirectory: storageRoot) }

    private func saveCapture() throws -> (itemID: String, captureID: String) {
        let captureID = UUID().uuidString.lowercased()
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: captureID, text: "synthetic capture to play", capturedAt: Date()) { outcome = $0 }
        settle("the save completes") { outcome != nil }
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return (itemID, captureID)
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
        settle("the correction finished") { finished }
        XCTAssertNil(failure)
    }

    /// Gives the saved item retained audio: a tiny synthetic WAV the real `AVAudioPlayer` can open.
    private func seedAudio(captureID: String) throws -> URL {
        let finalizedName = "\(captureID).wav"
        let audioURL = layout.directory(for: .finalizedAudio).appendingPathComponent(finalizedName)
        try FileManager.default.createDirectory(
            at: audioURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        let sampleRate: UInt32 = 8000
        // A minute, so playback is still going when the test acts however long a loaded simulator takes to get there: a clip
        // that ends on its own is not an end by deletion.
        let dataSize: UInt32 = sampleRate * 2 * 60
        func le32(_ value: UInt32) -> [UInt8] { (0..<4).map { UInt8((value >> (8 * UInt32($0))) & 0xff) } }
        func le16(_ value: UInt16) -> [UInt8] { [UInt8(value & 0xff), UInt8(value >> 8)] }
        var bytes: [UInt8] = Array("RIFF".utf8) + le32(36 + dataSize) + Array("WAVEfmt ".utf8)
        bytes += le32(16) + le16(1) + le16(1) + le32(sampleRate) + le32(sampleRate * 2) + le16(2) + le16(16)
        bytes += Array("data".utf8) + le32(dataSize)
        try (Data(bytes) + Data(count: Int(dataSize))).write(to: audioURL)
        let reference = IngressRecord.audioReference(finalizedFileName: finalizedName)
        try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'")
        return audioURL
    }

    private func execute(_ sql: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed")
        if code != SQLITE_OK { throw CompositionError.storageUnavailable }
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
