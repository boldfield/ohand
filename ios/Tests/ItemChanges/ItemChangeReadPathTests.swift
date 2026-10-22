import Foundation
import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ReadPathUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class ReadPathResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// An item changed through the real services, then the real home read and the real search read over the real Rust core:
/// what the home and the search screen list afterwards is what the core now holds.
final class ItemChangeReadPathTests: XCTestCase {
    private static let routeID = "route-item-change-read-path"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var module: RetrievalSearchModule!
    private var root: ServiceCompositionRoot!
    private var home: HomeRecentsModel!
    private var search: RetrievalSearchModel!

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-change-read-path-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        module = RetrievalSearchModule()
        root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: ReadPathUnreachableSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false)),
            modules: [module])
    }

    override func tearDownWithError() throws {
        home?.shutDown()
        home = nil
        search = nil
        root.shutDown()
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testADeletedItemLeavesTheHomeAndTheSearchResults() throws {
        let kept = try saveCapture("synthetic roofer words kept")
        let deleted = try saveCapture("synthetic roofer words deleted")
        try showHomeAndSearch(expecting: Set([kept, deleted]))

        let start = try revision(of: deleted)
        let deletion = root.itemDeletion
        _ = try run { try await deletion.delete(itemID: deleted, expectedRevision: start) }

        XCTAssertTrue(pump(until: { Set(self.home.entries.map(\.id)) == [kept] }), "home no longer lists the deleted note")
        XCTAssertTrue(pump(until: { self.search.phase == .results && Set(self.search.rows.map(\.id)) == [kept] }),
            "search no longer lists the deleted note")
        XCTAssertFalse(search.rows.map(\.id).contains(deleted))
    }

    func testACorrectedTextShowsOnTheHomeAndInTheSearchResults() throws {
        let corrected = try saveCapture("synthetic roofer words before")
        let other = try saveCapture("synthetic roofer words other")
        try showHomeAndSearch(expecting: Set([corrected, other]))

        let start = try revision(of: corrected)
        let corrections = root.itemCorrections
        _ = try run {
            try await corrections.correctText(
                commandID: "cmd-read-path", itemID: corrected, expectedRevision: start,
                text: "synthetic roofer words after")
        }

        XCTAssertTrue(pump(until: {
            self.home.entries.first { $0.id == corrected }?.text == "synthetic roofer words after"
        }), "home shows the corrected text")
        XCTAssertTrue(pump(until: {
            self.search.phase == .results
                && self.search.rows.first { $0.id == corrected }?.text == "synthetic roofer words after"
        }), "search shows the corrected text")
        XCTAssertEqual(Set(search.rows.map(\.id)), [corrected, other])
    }

    func testAStaleRevisionRefusalChangesNeitherListNorReadsAgain() throws {
        let item = try saveCapture("synthetic roofer words steady")
        try showHomeAndSearch(expecting: [item])
        let reads = search.rows.count
        let start = try revision(of: item)
        let announced = RecordingChangePublisher()
        let remove = root.observeItemChanges { announced.publish($0) }
        defer { remove() }

        let deletion = root.itemDeletion
        do {
            _ = try run {
                try await deletion.delete(itemID: item, expectedRevision: start + 5)
            }
            XCTFail("a stale revision is refused")
        } catch {}
        _ = pump(until: { false }, timeout: 0.3)

        XCTAssertTrue(announced.changes.isEmpty, "nothing was announced")
        XCTAssertEqual(home.entries.map(\.id), [item])
        XCTAssertEqual(search.rows.map(\.id), [item])
        XCTAssertEqual(search.rows.count, reads)
    }

    // MARK: deletion made on the detail screen

    func testDeletingOnTheDetailScreenShowsTheDeletionResultWithoutTheNotHereMessage() throws {
        let itemID = try saveCapture("synthetic roofer words to delete here")
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        var onScreen: Set<String> = []
        var everShown: Set<String> = []
        let window = hostDetail(model) { identifier, shown in
            if shown {
                onScreen.insert(identifier)
                everShown.insert(identifier)
            } else {
                onScreen.remove(identifier)
            }
        }
        defer {
            window.isHidden = true
            _ = pump(until: { false }, timeout: 0.2)
        }
        XCTAssertTrue(pump(until: {
            window.rootViewController?.view.layoutIfNeeded()
            return MainActor.assumeIsolated { model.content != nil && model.deletion?.revision != nil }
        }), "the item loads and the delete control has read its revision")
        let deletion = try XCTUnwrap(model.deletion)

        try runOnMain {
            deletion.requestDeletion()
            await deletion.confirmDeletion()
        }

        XCTAssertTrue(pump(until: {
            window.rootViewController?.view.layoutIfNeeded()
            return MainActor.assumeIsolated { deletion.isRecorded && model.failure == .notFound }
        }), "the screen reads the item again once the deletion is recorded")
        _ = pump(until: { false }, timeout: 0.3)
        XCTAssertNil(model.content)
        XCTAssertFalse(everShown.contains(SourceDetailView.notFoundIdentifier), "the deletion result is the whole answer")
        XCTAssertFalse(onScreen.contains(SourceDetailView.notFoundIdentifier))
    }

    func testOpeningAnItemThatWasDeletedEarlierStillSaysNotHere() throws {
        let itemID = try saveCapture("synthetic roofer words deleted earlier")
        let start = try revision(of: itemID)
        let deletion = root.itemDeletion
        _ = try run { try await deletion.delete(itemID: itemID, expectedRevision: start) }
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        var onScreen: Set<String> = []
        let window = hostDetail(model) { identifier, shown in
            if shown { onScreen.insert(identifier) } else { onScreen.remove(identifier) }
        }
        defer {
            window.isHidden = true
            _ = pump(until: { false }, timeout: 0.2)
        }

        XCTAssertTrue(pump(until: {
            window.rootViewController?.view.layoutIfNeeded()
            return onScreen.contains(SourceDetailView.notFoundIdentifier)
        }), "an item deleted before the screen opened still says it is not here")
    }

    // MARK: helpers

    private func showHomeAndSearch(expecting itemIDs: Set<String>) throws {
        home = HomeRecentsModel(source: RootHomeRecentsSource(root: root, unlock: {}))
        XCTAssertTrue(pump(until: { Set(self.home.entries.map(\.id)) == itemIDs }), "home lists the saved items")
        search = RetrievalSearchModel(
            searcher: module, readScope: { [weak root] in root?.readScope ?? .captureOnly }, onSelect: { _ in },
            observeItemChanges: { [weak root] handler in root?.observeItemChanges(handler) ?? {} })
        search.query = "roofer"
        search.search()
        XCTAssertTrue(pump(until: { self.search.phase == .results && Set(self.search.rows.map(\.id)) == itemIDs }),
            "search lists the saved items")
    }

    /// Hosts the real detail screen and reports every identified element it shows or hides.
    private func hostDetail(_ model: SourceDetailModel, onScreen: @escaping (String, Bool) -> Void) -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: NavigationStack {
            SourceDetailView(model: model, renderReporter: onScreen)
        })
        window.makeKeyAndVisible()
        return window
    }

    private func runOnMain(_ operation: @escaping @MainActor @Sendable () async -> Void) throws {
        let box = ReadPathResultBox<Void>()
        Task { @MainActor in
            await operation()
            box.set(.success(()))
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
    }

    private var databasePath: String { ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path }

    private func saveCapture(_ text: String) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func revision(of itemID: String) throws -> Int {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(
            sqlite3_prepare_v2(database, "SELECT revision FROM items WHERE item_id = '\(itemID)'", -1, &statement, nil),
            SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw CoreFailure.handleClosed }
        return Int(sqlite3_column_int64(statement, 0))
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = ReadPathResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }
}
