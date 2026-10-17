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

    // MARK: item and suggestion requests

    func testAnAuthenticatedTapPresentsTheItemWithoutAsking() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.settingsEntryID)

        model.openItem(itemID: "item-1")

        XCTAssertEqual(model.requestedItemID, "item-1")
        XCTAssertEqual(model.path.map { ManagementRoute(pathComponent: $0) }, [.item("item-1")])
        XCTAssertTrue(reasons.isEmpty)
    }

    func testPushingAnItemKeepsTheScreenItWasChosenFromUnderneath() {
        scope = .authenticated
        model.open(entryID: ManagementCatalog.searchEntryID)

        model.pushItem(itemID: "item-1")

        XCTAssertEqual(model.path.map { ManagementRoute(pathComponent: $0) }, [.entry(ManagementCatalog.searchEntryID), .item("item-1")])
        XCTAssertEqual(model.requestedItemID, "item-1")
        XCTAssertTrue(reasons.isEmpty)

        model.path.removeLast()
        XCTAssertEqual(model.path, [ManagementCatalog.searchEntryID], "Back lands on the screen the item was chosen from")
    }

    func testPushingAnItemBeforeAuthenticationAsksFirstAndOpensOnlyThatItem() {
        model.pushItem(itemID: "item-1")

        XCTAssertEqual(reasons, [ManagementNavigationModel.authenticationReason])
        XCTAssertEqual(model.path, [])

        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertEqual(model.path.map { ManagementRoute(pathComponent: $0) }, [.item("item-1")])
    }

    func testATapBeforeAuthenticationAsksFirstAndPresentsOnlyAfterSuccess() {
        model.openItem(itemID: "item-1")

        XCTAssertEqual(reasons, [ManagementNavigationModel.authenticationReason])
        XCTAssertNil(model.requestedItemID, "nothing is presented while the device owner has not answered")
        XCTAssertEqual(model.path, [])

        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertEqual(model.requestedItemID, "item-1")
        XCTAssertFalse(model.isAuthenticating)
    }

    func testASuggestionRequestFollowsTheSameRule() {
        model.openSuggestion()
        XCTAssertFalse(model.isSuggestionRequested)
        XCTAssertEqual(reasons.count, 1)

        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertTrue(model.isSuggestionRequested)
        XCTAssertNil(model.requestedItemID)
    }

    func testAnAuthenticatedSuggestionRequestPresentsWithoutAsking() {
        scope = .authenticated
        model.openSuggestion()
        XCTAssertTrue(model.isSuggestionRequested)
        XCTAssertTrue(reasons.isEmpty)
    }

    func testARequestThatAuthenticationDidNotGrantIsDroppedNotRetained() {
        model.openItem(itemID: "item-1")
        finishAuthentication(.cancelled, grantingScope: false)
        XCTAssertNil(model.requestedItemID)
        XCTAssertNil(model.notice)

        scope = .authenticated
        model.revalidate()
        XCTAssertEqual(model.path, [], "a cancelled request does not present later")
    }

    func testADeniedRequestSaysTheItemsStayLocked() {
        model.openItem(itemID: "item-1")
        finishAuthentication(.denied, grantingScope: false)
        XCTAssertNil(model.requestedItemID)
        XCTAssertNotNil(model.notice)
    }

    func testASuccessThatLeftTheSessionLockedDoesNotPresentTheRequest() {
        model.openItem(itemID: "item-1")
        finishAuthentication(.authenticated, grantingScope: false)
        XCTAssertNil(model.requestedItemID)
    }

    func testATapDuringAnAuthenticationInFlightIsNotLostAndDoesNotAskTwice() {
        model.open(entryID: ManagementCatalog.settingsEntryID)
        model.openItem(itemID: "item-1")
        XCTAssertEqual(reasons.count, 1)

        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertEqual(model.requestedItemID, "item-1", "the latest request is the one presented")
    }

    func testRevalidatingWhileTheOwnerIsAnsweringKeepsTheWaitingRequest() {
        model.openItem(itemID: "item-1")
        model.revalidate()
        finishAuthentication(.authenticated, grantingScope: true)
        XCTAssertEqual(model.requestedItemID, "item-1")
    }

    func testLosingTheSessionWhileAnItemIsPresentedLeavesIt() {
        scope = .authenticated
        model.openItem(itemID: "item-1")
        model.revalidate()
        XCTAssertEqual(model.requestedItemID, "item-1", "an authenticated session keeps the screen")

        scope = .captureOnly
        model.revalidate()

        XCTAssertNil(model.requestedItemID)
        XCTAssertEqual(model.path, [])
    }

    func testLosingTheSessionWhileTheSuggestionIsPresentedLeavesIt() {
        scope = .authenticated
        model.openSuggestion()
        scope = .captureOnly
        model.revalidate()
        XCTAssertFalse(model.isSuggestionRequested)
        XCTAssertEqual(model.path, [])
    }

    func testReturningToCaptureDropsARequestStillWaitingForAuthentication() {
        model.openItem(itemID: "item-1")

        model.returnToCapture()
        finishAuthentication(.authenticated, grantingScope: true)

        XCTAssertEqual(model.path, [], "a capture entry always lands on capture")
        XCTAssertNil(model.requestedItemID)
    }

    func testReturningToCaptureLeavesAPresentedItem() {
        scope = .authenticated
        model.openItem(itemID: "item-1")
        model.returnToCapture()
        XCTAssertNil(model.requestedItemID)
        XCTAssertEqual(model.path, [])
    }

    func testRoutesRoundTripThroughThePathAndEntryIdentifiersStayEntries() {
        for route in [ManagementRoute.entry("search"), .item("item-1"), .item("a:b"), .suggestion] {
            XCTAssertEqual(ManagementRoute(pathComponent: route.pathComponent), route)
        }
        XCTAssertEqual(ManagementRoute(pathComponent: ManagementCatalog.searchEntryID), .entry("search"))
    }

    func testOnlyTheSuggestionDestinationIsRegisteredSoItemRequestsShowPlaceholders() {
        XCTAssertFalse(ManagementCatalog.destinations.isRegistered(.item("item-1")))
        XCTAssertTrue(ManagementCatalog.destinations.isRegistered(.suggestion))
        XCTAssertTrue(ManagementCatalog.destinations.isRegistered(.entry(ManagementCatalog.searchEntryID)))
    }

    func testTheCatalogListsSearchAndSettingsAsSecondaryEntries() {
        XCTAssertEqual(ManagementCatalog.secondaryEntries.map(\.id), ["search", "settings"])
        XCTAssertNotNil(ManagementCatalog.entry(withID: "search"))
        XCTAssertNil(ManagementCatalog.entry(withID: "backlog"))
    }
}
