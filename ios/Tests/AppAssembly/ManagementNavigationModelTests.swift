import XCTest
import OhAndServices
@testable import OhAnd

final class ManagementNavigationModelTests: XCTestCase {
    private var scope: CompositionReadScope = .captureOnly
    private var reasons: [String] = []
    private var pendingCompletions: [(CompositionAuthenticationOutcome) -> Void] = []
    private var accessChangeCount = 0
    private var model: ManagementNavigationModel!

    override func setUp() {
        super.setUp()
        scope = .captureOnly
        reasons = []
        pendingCompletions = []
        accessChangeCount = 0
        model = ManagementNavigationModel(
            readScope: { [unowned self] in self.scope },
            authenticate: { [unowned self] reason, completion in
                self.reasons.append(reason)
                self.pendingCompletions.append(completion)
            },
            readAccessDidChange: { [unowned self] in self.accessChangeCount += 1 })
    }

    private func finishAuthentication(_ outcome: CompositionAuthenticationOutcome, grantingScope: Bool) {
        if grantingScope { scope = .authenticated }
        pendingCompletions.removeFirst()(outcome)
    }

    func testAnAuthenticatedSessionOpensTheEntryWithoutAsking() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.searchEntryID)
        XCTAssertEqual(model.path, [ManagementCatalog.searchEntryID])
        XCTAssertTrue(reasons.isEmpty)
    }

    func testALockedSessionAsksFirstAndOpensOnlyAfterSuccess() {
        model.open(entryID: ManagementCatalog.settingsEntryID)
        XCTAssertEqual(reasons, [ManagementNavigationModel.authenticationReason])
        XCTAssertTrue(model.isAuthenticating)
        XCTAssertEqual(model.path, [], "nothing opens while the device owner has not answered")

        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertFalse(model.isAuthenticating)
        XCTAssertEqual(model.path, [ManagementCatalog.settingsEntryID])
        XCTAssertEqual(accessChangeCount, 1, "the capture surface re-reads what it may read")
    }

    func testASuccessThatLeftTheSessionLockedDoesNotOpen() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.authenticated, grantingScope: false)
        XCTAssertEqual(model.path, [])
    }

    func testCancellingStaysOnCaptureWithoutAMessage() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.cancelled, grantingScope: false)
        XCTAssertEqual(model.path, [])
        XCTAssertNil(model.notice)
        XCTAssertFalse(model.isAuthenticating)
    }

    func testAnInterruptedAttemptStaysOnCaptureWithoutAMessage() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.interrupted, grantingScope: false)
        XCTAssertEqual(model.path, [])
        XCTAssertNil(model.notice)
    }

    func testADeniedAttemptSaysTheItemsStayLocked() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.denied, grantingScope: false)
        XCTAssertEqual(model.path, [])
        XCTAssertNotNil(model.notice)
    }

    func testADeviceWithoutAuthenticationSaysSo() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.unavailable, grantingScope: false)
        XCTAssertEqual(model.path, [])
        XCTAssertTrue(model.notice?.contains("no passcode") ?? false)
    }

    func testANoticeClearsWhenTheUserTriesAgain() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        finishAuthentication(.denied, grantingScope: false)
        model.open(entryID: ManagementCatalog.searchEntryID)
        XCTAssertNil(model.notice)
    }

    func testASecondOpenWhileAskingDoesNotAskAgain() {
        model.open(entryID: ManagementCatalog.searchEntryID)
        model.open(entryID: ManagementCatalog.settingsEntryID)
        XCTAssertEqual(reasons.count, 1)
    }

    func testLosingTheSessionReturnsToCapture() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.settingsEntryID)
        XCTAssertEqual(model.path.count, 1)

        scope = .captureOnly
        model.revalidate()

        XCTAssertEqual(model.path, [])
    }

    func testAStillAuthenticatedSessionKeepsItsScreen() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.settingsEntryID)
        model.revalidate()
        XCTAssertEqual(model.path, [ManagementCatalog.settingsEntryID])
    }

    func testAnOpenedItemIsRequestedWithoutChangingTheScreenOnDisplay() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.settingsEntryID)

        model.openItem(itemID: "item-1")

        XCTAssertEqual(model.requestedItemID, "item-1")
        XCTAssertEqual(model.path, [ManagementCatalog.settingsEntryID])
    }

    func testLosingTheSessionDropsTheRequestedItem() {
        scope = .authenticated
        model.openItem(itemID: "item-1")
        model.revalidate()
        XCTAssertEqual(model.requestedItemID, "item-1", "an authenticated session keeps the request")

        scope = .captureOnly
        model.revalidate()

        XCTAssertNil(model.requestedItemID)
    }

    func testTheCatalogListsSearchAndSettingsAsSecondaryEntries() {
        XCTAssertEqual(ManagementCatalog.secondaryEntries.map(\.id), ["search", "settings"])
        XCTAssertNotNil(ManagementCatalog.entry(withID: "search"))
        XCTAssertNil(ManagementCatalog.entry(withID: "backlog"))
    }
}
