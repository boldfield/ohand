import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ControlsUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Hands every read to the real core, first running `beforeRead`, so a test can relock the session between a stored
/// correction and the read that follows it.
private final class HookedDetailReader: SourceDetailReading, @unchecked Sendable {
    let inner: SourceDetailReading
    var beforeRead: (() -> Void)?
    init(_ inner: SourceDetailReading) { self.inner = inner }
    func detail(itemID: String) async throws -> CoreItemDetail {
        beforeRead?()
        return try await inner.detail(itemID: itemID)
    }
}

/// Hands every read to the real core, but while `isHeld` it waits for `release()`, so a test can look at the screen
/// while a correction is stored and its refresh is still pending.
private final class GatedDetailReader: SourceDetailReading, @unchecked Sendable {
    let inner: SourceDetailReading
    var isHeld = false
    private(set) var isWaiting = false
    private var waiter: CheckedContinuation<Void, Never>?
    init(_ inner: SourceDetailReading) { self.inner = inner }
    func detail(itemID: String) async throws -> CoreItemDetail {
        if isHeld {
            await withCheckedContinuation { continuation in
                waiter = continuation
                isWaiting = true
            }
        }
        return try await inner.detail(itemID: itemID)
    }
    func release() {
        isHeld = false
        isWaiting = false
        waiter?.resume()
        waiter = nil
    }
}

/// Drives `ItemControlsModel.assembled` and the source-detail screen over the real composition root and Rust core on an
/// on-disk store with synthetic data: what is stored, what survives a relaunch, and what the screen shows.
final class ItemControlsRealCoreTests: XCTestCase {
    private static let routeID = "route-item-controls"
    private static let captureText = "synthetic capture for item controls"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var retainedWindow: UIWindow?
    private var onScreen: Set<String> = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-controls-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        root = try makeRoot()
    }

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        root?.shutDown()
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testCorrectionsKeepTheCaptureLeaveScopeAloneAndSurviveARelaunch() throws {
        let itemID = try saveCapture()
        let scopeBefore = scalar("SELECT COALESCE(current_scope, '') FROM items WHERE item_id = '\(itemID)'")
        var controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        XCTAssertEqual(controls.textDraft, Self.captureText)

        onMain {
            controls.textDraft = "call the roofer tomorrow"
            await controls.saveText()
            controls.pendingType = .idea
            await controls.applyType()
            controls.topicDraft = "garage"
            await controls.saveSessionTopic()
        }
        XCTAssertEqual(controls.outcome, .saved(.sessionTopic))

        XCTAssertEqual(scalar("SELECT item_type FROM items WHERE item_id = '\(itemID)'"), "idea")
        XCTAssertEqual(scalar("SELECT current_session_topic FROM items WHERE item_id = '\(itemID)'"), "garage")
        XCTAssertEqual(
            scalar("SELECT COALESCE(current_scope, '') FROM items WHERE item_id = '\(itemID)'"), scopeBefore,
            "a topic correction never changes the privacy scope")
        XCTAssertEqual(
            scalar("SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = '\(itemID)')"),
            Self.captureText, "the original capture is kept")
        XCTAssertEqual(controls.snapshot?.itemType, .idea, "the type stays what the user applied, never an action")

        root.shutDown()
        root = try makeRoot()
        controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        XCTAssertEqual(controls.textDraft, "call the roofer tomorrow")
        XCTAssertEqual(controls.pendingType, .idea)
        XCTAssertEqual(controls.topicDraft, "garage")
        let detail = try XCTUnwrap(try readDetail(itemID))
        XCTAssertEqual(detail.originalText, Self.captureText, "the original is still shown after relaunch")
        XCTAssertEqual(detail.correctedText, "call the roofer tomorrow")
    }

    func testAStaleSaveShowsTheConflictAndTheHostCardShowsTheNewerValues() throws {
        let itemID = try saveCapture()
        let detailModel = SourceDetailModel.assembled(itemID: itemID, root: root)
        let controls = try XCTUnwrap(detailModel.itemControls)
        host(AnyView(NavigationStack { SourceDetailView(model: detailModel, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        }) }))
        settle { detailModel.content != nil && controls.snapshot != nil && onScreen.contains(ItemControlsView.sectionIdentifier) }
        XCTAssertTrue(onScreen.contains(ItemControlsView.sectionIdentifier), "the controls render under the detail card")

        // Someone else corrects the item after the controls loaded it.
        let revision = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        try run { try await self.root.itemCorrections.correctText(
            commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: revision,
            text: "someone else's words") }

        controls.textDraft = "my words"
        onMain { await controls.saveText() }
        guard case .conflict(.text, _)? = controls.outcome else {
            return XCTFail("expected a conflict, got \(String(describing: controls.outcome))")
        }
        settle { onScreen.contains(ItemControlsView.outcomeIdentifier) && detailModel.content?.currentText == "someone else's words" }
        XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), "the conflict message is on screen")
        XCTAssertEqual(controls.textDraft, "my words", "the draft is kept")
        XCTAssertEqual(detailModel.content?.currentText, "someone else's words", "the card agrees with the controls")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'user-correction:%'"), "1", "nothing was overwritten")
    }

    func testPickingActionShowsTheWarningAndChangesNothingUntilApplied() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain {
            await controls.load()
            controls.pendingType = .idea
            await controls.applyType()
        }
        host(AnyView(ItemControlsView(model: controls, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        })))
        settle { onScreen.contains(ItemControlsView.sectionIdentifier) }
        controls.pendingType = .action
        settle { onScreen.contains(ItemControlsView.actionWarningIdentifier) }
        XCTAssertTrue(onScreen.contains(ItemControlsView.actionWarningIdentifier))
        XCTAssertEqual(scalar("SELECT item_type FROM items WHERE item_id = '\(itemID)'"), "idea")
    }

    func testARelockBeforeTheRefreshKeepsTheSaveAndDraftsButPausesEditingUntilAReload() throws {
        let itemID = try saveCapture()
        let reader = HookedDetailReader(root.itemDetails)
        let controls = ItemControlsModel(itemID: itemID, reader: reader, corrector: root.itemCorrections)
        hostControls(controls)
        settle { controls.snapshot != nil && onScreen.contains(ItemControlsView.sectionIdentifier) }
        let revisionBefore = try XCTUnwrap(controls.snapshot?.revision)

        controls.topicDraft = "garage"
        controls.textDraft = "call the roofer tomorrow"
        reader.beforeRead = { [unowned self] in self.session.relock(.explicit) }
        onMain { await controls.saveText() }
        reader.beforeRead = nil

        XCTAssertEqual(controls.outcome, .saved(.text))
        XCTAssertEqual(controls.refreshFailure, .locked)
        XCTAssertEqual(controls.snapshot?.revision, revisionBefore, "the newer revision was not read")
        XCTAssertEqual(controls.topicDraft, "garage", "the unsaved topic is kept")
        XCTAssertFalse(controls.canSaveTopic, "nothing can be sent against the old revision")
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'user-correction:%'"), "1", "the save was stored")
        settle {
            onScreen.contains(ItemControlsView.outcomeIdentifier)
                && onScreen.contains(ItemControlsView.refreshFailureIdentifier)
                && onScreen.contains(ItemControlsView.reloadIdentifier)
        }
        XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), "the saved message is on screen")
        XCTAssertTrue(onScreen.contains(ItemControlsView.refreshFailureIdentifier), "the locked message is on screen")
        XCTAssertTrue(onScreen.contains(ItemControlsView.reloadIdentifier))

        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        onMain { await controls.reload() }
        XCTAssertNil(controls.refreshFailure)
        XCTAssertGreaterThan(controls.snapshot?.revision ?? 0, revisionBefore)
        XCTAssertEqual(controls.snapshot?.text, "call the roofer tomorrow")
        XCTAssertEqual(controls.topicDraft, "garage")
        settle { !onScreen.contains(ItemControlsView.refreshFailureIdentifier) }
        XCTAssertFalse(onScreen.contains(ItemControlsView.refreshFailureIdentifier))

        onMain { await controls.saveSessionTopic() }
        XCTAssertEqual(controls.outcome, .saved(.sessionTopic))
        XCTAssertEqual(scalar("SELECT current_session_topic FROM items WHERE item_id = '\(itemID)'"), "garage")
    }

    func testARelockAfterAStaleSaveKeepsTheConflictAndDoesNotPretendNewerValuesLoaded() throws {
        let itemID = try saveCapture()
        let reader = HookedDetailReader(root.itemDetails)
        let controls = ItemControlsModel(itemID: itemID, reader: reader, corrector: root.itemCorrections)
        hostControls(controls)
        settle { controls.snapshot != nil && onScreen.contains(ItemControlsView.sectionIdentifier) }
        let revisionBefore = try XCTUnwrap(controls.snapshot?.revision)
        try run { try await self.root.itemCorrections.correctText(
            commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: revisionBefore,
            text: "someone else's words") }

        controls.textDraft = "my words"
        reader.beforeRead = { [unowned self] in self.session.relock(.explicit) }
        onMain { await controls.saveText() }
        reader.beforeRead = nil

        guard case .conflict(.text, _)? = controls.outcome else {
            return XCTFail("expected a conflict, got \(String(describing: controls.outcome))")
        }
        XCTAssertEqual(controls.refreshFailure, .locked)
        XCTAssertEqual(controls.snapshot?.revision, revisionBefore)
        XCTAssertEqual(controls.textDraft, "my words")
        XCTAssertFalse(controls.canSaveText)
        XCTAssertFalse(
            ItemControlsView.message(for: controls.outcome!, newerValuesShown: controls.refreshFailure == nil)
                .contains("newer values are shown"))
        settle {
            onScreen.contains(ItemControlsView.outcomeIdentifier)
                && onScreen.contains(ItemControlsView.refreshFailureIdentifier)
        }
        XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), "the conflict stays on screen")
        XCTAssertTrue(onScreen.contains(ItemControlsView.refreshFailureIdentifier))
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'user-correction:%'"), "1")

        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        onMain { await controls.reload() }
        XCTAssertNil(controls.refreshFailure)
        XCTAssertEqual(controls.snapshot?.text, "someone else's words", "the newer values are now loaded")
        XCTAssertEqual(controls.textDraft, "my words", "the rejected draft is still kept")
        XCTAssertTrue(controls.canSaveText)
    }

    func testTheFieldsAreDisabledWhileASaveIsInFlightSoNothingTypedThenIsLost() throws {
        let itemID = try saveCapture()
        let reader = GatedDetailReader(root.itemDetails)
        let controls = ItemControlsModel(itemID: itemID, reader: reader, corrector: root.itemCorrections)
        hostControls(controls)
        settle { controls.snapshot != nil && onScreen.contains(ItemControlsView.sectionIdentifier) }
        settle { onScreen.contains(ItemControlsView.editableIdentifier) }
        XCTAssertTrue(onScreen.contains(ItemControlsView.editableIdentifier), "the fields are editable once loaded")

        controls.topicDraft = "garage"
        reader.isHeld = true
        var finished = false
        Task { @MainActor in
            await controls.saveSessionTopic()
            finished = true
        }
        settle { reader.isWaiting && !onScreen.contains(ItemControlsView.editableIdentifier) }
        XCTAssertTrue(reader.isWaiting, "the correction was stored and its refresh is pending")
        XCTAssertFalse(onScreen.contains(ItemControlsView.editableIdentifier), "the fields cannot be edited meanwhile")
        XCTAssertEqual(scalar("SELECT current_session_topic FROM items WHERE item_id = '\(itemID)'"), "garage")

        reader.release()
        settle { finished }
        XCTAssertTrue(finished)
        XCTAssertEqual(controls.outcome, .saved(.sessionTopic))
        XCTAssertEqual(controls.topicDraft, "garage")
        settle { onScreen.contains(ItemControlsView.editableIdentifier) }
        XCTAssertTrue(onScreen.contains(ItemControlsView.editableIdentifier), "editing resumes once the new values are read")
    }

    func testStopSuggestingMarksPullOnlyAndKeepsTheItemSearchableAndOpen() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        onMain { await controls.stopSuggesting() }
        XCTAssertEqual(controls.outcome, .actionDone(.stopSuggesting))
        XCTAssertEqual(scalar("SELECT pull_only FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "1")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertTrue(controls.canAct, "the item stays open, so completing and cancelling stay available")
        XCTAssertEqual(controls.snapshot?.suggestionsStopped, true)
        XCTAssertFalse(controls.canQuietSuggestions, "Not now is not offered once suggestions are stopped")
        onMain { await controls.notNow() }
        XCTAssertEqual(controls.outcome, .actionDone(.stopSuggesting), "a refused Not now writes nothing new")
        XCTAssertEqual(scalar("SELECT COALESCE(snoozed_until, '') FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "")
        XCTAssertNotNil(try readDetail(itemID), "the item is still retrievable")

        root.shutDown()
        root = try makeRoot()
        XCTAssertEqual(scalar("SELECT pull_only FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "1", "survives relaunch")
        let relaunched = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await relaunched.load() }
        XCTAssertFalse(relaunched.canQuietSuggestions, "the stopped state is read back after a relaunch")
        XCTAssertTrue(relaunched.canAct)
    }

    func testTheActionsAndTheirExplanationRenderAndAnEndedItemOffersNoneOfThem() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        hostControls(controls)
        settle {
            controls.snapshot != nil && onScreen.contains(ItemControlsView.actionsIdentifier)
                && onScreen.contains(ItemControlsView.actionsFooterIdentifier)
        }
        XCTAssertTrue(onScreen.contains(ItemControlsView.actionsIdentifier), "the actions section renders")
        XCTAssertTrue(onScreen.contains(ItemControlsView.actionsFooterIdentifier), "the cooldown/pull-only explanation renders")
        XCTAssertFalse(onScreen.contains(ItemControlsView.endedNoticeIdentifier))
        XCTAssertTrue(controls.canAct)
        XCTAssertTrue(controls.canQuietSuggestions)

        onMain { await controls.complete() }
        settle { onScreen.contains(ItemControlsView.endedNoticeIdentifier) }
        XCTAssertTrue(onScreen.contains(ItemControlsView.endedNoticeIdentifier), "the ended notice renders")
        XCTAssertFalse(controls.canAct)
        XCTAssertFalse(controls.canQuietSuggestions)
        XCTAssertEqual(ItemControlsView.message(for: .actionDone(.complete)), "Completed. Any reminder for it was cancelled.")
    }

    func testNotNowRecordsACooldownThatIsNotPullOnly() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        onMain { await controls.notNow() }
        XCTAssertEqual(controls.outcome, .actionDone(.notNow))
        XCTAssertNotEqual(scalar("SELECT COALESCE(snoozed_until, '') FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "")
        XCTAssertEqual(scalar("SELECT pull_only FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "0")
    }

    func testCompletingEndsTheItemAndALaterActionIsRefusedWithoutWriting() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        onMain { await controls.complete() }
        XCTAssertEqual(controls.outcome, .actionDone(.complete))
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
        XCTAssertFalse(controls.canAct, "an ended item offers no further actions")
        onMain { await controls.cancel() }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
    }

    func testAnActionOnAnItemThatMovedOnIsAConflictAndWritesNothing() throws {
        let itemID = try saveCapture()
        let controls = ItemControlsModel.assembled(itemID: itemID, root: root, onCorrected: {})
        onMain { await controls.load() }
        let revision = try XCTUnwrap(controls.snapshot?.revision)
        try run { try await self.root.itemCorrections.correctText(
            commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: revision,
            text: "someone else's words") }
        onMain { await controls.complete() }
        guard case .actionConflict(.complete, _)? = controls.outcome else {
            return XCTFail("expected a conflict, got \(String(describing: controls.outcome))")
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertGreaterThan(controls.snapshot?.revision ?? 0, revision, "the newer revision is loaded")
    }

    // MARK: helpers

    private func hostControls(_ controls: ItemControlsModel) {
        host(AnyView(ItemControlsView(model: controls, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        })))
    }

    private func makeRoot() throws -> ServiceCompositionRoot {
        try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: ControlsUnreachableSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false)))
    }

    private func host(_ view: AnyView) {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: view)
        window.makeKeyAndVisible()
        retainedWindow = window
        settle { true }
    }

    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 10)
        repeat {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    private func onMain(_ operation: @escaping @MainActor () async -> Void) {
        var finished = false
        Task { @MainActor in
            await operation()
            finished = true
        }
        settle { finished }
        XCTAssertTrue(finished, "the operation completed")
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws {
        var finished = false
        var failure: Error?
        Task {
            do { _ = try await operation() } catch { failure = error }
            finished = true
        }
        settle { finished }
        XCTAssertTrue(finished)
        if let failure { throw failure }
    }

    private func readDetail(_ itemID: String) throws -> CoreItemDetail? {
        var result: Result<CoreItemDetail, Error>?
        let reader = root.itemDetails
        Task {
            do { result = .success(try await reader.detail(itemID: itemID)) } catch { result = .failure(error) }
        }
        settle { result != nil }
        return try result?.get()
    }

    private func saveCapture() throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.captureText, capturedAt: Date()) {
            outcome = $0
        }
        settle { outcome != nil }
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
