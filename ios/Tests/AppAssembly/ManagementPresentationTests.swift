import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real app root view in a window over production-built capture and management models, with only the
/// authentication answer and the destination registry controlled by the test. What the host reports on screen is
/// checked together with what the navigation model decided.
final class ManagementPresentationTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var services: AppServices!
    private var retainedWindow: UIWindow?
    private var scope: CompositionReadScope = .captureOnly
    private var pendingCompletions: [(CompositionAuthenticationOutcome) -> Void] = []
    private var navigation: ManagementNavigationModel!
    private var events: [ManagementPresentationEvent] = []

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("management-presentation-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: NotificationCenter(),
            isProtectedDataAvailable: { true })
        scope = .captureOnly
        pendingCompletions = []
        events = []
        navigation = ManagementNavigationModel(
            readScope: { [unowned self] in self.scope },
            authenticate: { [unowned self] _, completion in self.pendingCompletions.append(completion) })
    }

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        navigation = nil
        services.shutDown()
        services = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func host(_ destinations: ManagementDestinations) {
        let root = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: services.managementContext,
            destinations: destinations, presentationReporter: { [unowned self] in events.append($0) })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: root)
        window.makeKeyAndVisible()
        retainedWindow = window
        settle { true }
    }

    /// Runs the main loop until the condition holds or five seconds pass.
    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 5)
        repeat {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    private func authenticate(_ outcome: CompositionAuthenticationOutcome, grantingScope: Bool) {
        if grantingScope { scope = .authenticated }
        pendingCompletions.removeFirst()(outcome)
    }

    func testATapBeforeAuthenticationShowsNothingThenTheItemPlaceholderAfterwards() {
        host(ManagementDestinations())

        navigation.openItem(itemID: "item-1")
        settle { false }
        XCTAssertTrue(events.isEmpty, "nothing is presented while the device owner has not answered")

        authenticate(.authenticated, grantingScope: true)
        settle { events.contains(.appeared(.item("item-1"), isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-1"), isPlaceholder: true)))
    }

    func testATapAfterAuthenticationPresentsAtOnce() {
        scope = .authenticated
        host(ManagementDestinations())

        navigation.openItem(itemID: "item-2")
        settle { events.contains(.appeared(.item("item-2"), isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-2"), isPlaceholder: true)))
        XCTAssertTrue(pendingCompletions.isEmpty, "an authenticated session is not asked again")
    }

    func testARegisteredItemDestinationIsBuiltWithTheItemIdentifier() {
        var builtFor: [String] = []
        var destinations = ManagementDestinations()
        destinations.item = { _, itemID in
            builtFor.append(itemID)
            return AnyView(Text("source detail"))
        }
        scope = .authenticated
        host(destinations)

        navigation.openItem(itemID: "item-3")
        settle { events.contains(.appeared(.item("item-3"), isPlaceholder: false)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-3"), isPlaceholder: false)))
        XCTAssertEqual(Set(builtFor), ["item-3"])
    }

    func testTheSuggestionShowsAPlaceholderUntilADestinationIsRegistered() {
        scope = .authenticated
        host(ManagementDestinations())

        navigation.openSuggestion()
        settle { events.contains(.appeared(.suggestion, isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.suggestion, isPlaceholder: true)))
    }

    func testARegisteredSuggestionDestinationIsPresented() {
        var destinations = ManagementDestinations()
        destinations.suggestion = { _ in AnyView(Text("suggestion")) }
        scope = .authenticated
        host(destinations)

        navigation.openSuggestion()
        settle { events.contains(.appeared(.suggestion, isPlaceholder: false)) }

        XCTAssertTrue(events.contains(.appeared(.suggestion, isPlaceholder: false)))
    }

    func testRelockingWhilePresentedLeavesTheScreen() {
        scope = .authenticated
        host(ManagementDestinations())
        navigation.openItem(itemID: "item-4")
        settle { events.contains(.appeared(.item("item-4"), isPlaceholder: true)) }

        scope = .captureOnly
        navigation.revalidate()
        settle { events.contains(.disappeared(.item("item-4"))) }

        XCTAssertTrue(events.contains(.disappeared(.item("item-4"))))
        XCTAssertEqual(navigation.path, [])
        XCTAssertNil(navigation.requestedItemID)
    }

    func testACaptureEntryLeavesAPresentedItemForCapture() {
        scope = .authenticated
        host(ManagementDestinations())
        navigation.openItem(itemID: "item-5")
        settle { events.contains(.appeared(.item("item-5"), isPlaceholder: true)) }

        navigation.returnToCapture()
        settle { events.contains(.disappeared(.item("item-5"))) }

        XCTAssertTrue(events.contains(.disappeared(.item("item-5"))))
        XCTAssertEqual(navigation.path, [])
    }
}
