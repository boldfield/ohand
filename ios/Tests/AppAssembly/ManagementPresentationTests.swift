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
    private var placeholderFrames: [String: CGRect] = [:]

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
        placeholderFrames = [:]
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
        // A core read issued before the shutdown completes on its own queue; wait for its buffer to be freed.
        let bufferDeadline = Date().addingTimeInterval(5)
        while CoreHandle.liveResultBufferCount != baselineBuffers, Date() < bufferDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func host(_ destinations: ManagementDestinations) {
        let root = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: services.managementContext,
            destinations: destinations, presentationReporter: { [unowned self] in events.append($0) },
            placeholderFrameReporter: { [unowned self] in placeholderFrames = $0 })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: root)
        window.makeKeyAndVisible()
        retainedWindow = window
        settle { true }
    }

    /// Lays the hosted view out and turns the run loop until the condition holds, failing the test (naming the condition)
    /// if it does not within five seconds.
    private func waitForScreen(
        _ condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) {
        waitUntilPumping(condition, file: file, line: line) {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    /// Runs the main loop until the condition holds or five seconds pass, without failing. Only for a wait that is meant to
    /// run out (`{ false }`, proving that nothing is presented) or a first layout pass; a positive wait uses
    /// `waitForScreen`.
    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 5)
        repeat {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    private let windowBounds = CGRect(x: 0, y: 0, width: 390, height: 844)

    private func assertPlaceholderIsVisible(_ identifier: String, file: StaticString = #filePath, line: UInt = #line) {
        waitForScreen("the placeholder is reported") { placeholderFrames[identifier] != nil }
        let frame = placeholderFrames[identifier] ?? .zero
        XCTAssertGreaterThan(frame.width, 0, "the placeholder has width", file: file, line: line)
        XCTAssertGreaterThan(frame.height, 0, "the placeholder has height", file: file, line: line)
        XCTAssertTrue(windowBounds.intersects(frame), "the placeholder is on screen", file: file, line: line)
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
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.item("item-1"), isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-1"), isPlaceholder: true)))
        assertPlaceholderIsVisible(ItemPlaceholderView.identifier)
        XCTAssertNil(placeholderFrames[SuggestionPlaceholderView.identifier])
    }

    func testATapAfterAuthenticationPresentsAtOnce() {
        scope = .authenticated
        host(ManagementDestinations())

        navigation.openItem(itemID: "item-2")
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.item("item-2"), isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-2"), isPlaceholder: true)))
        assertPlaceholderIsVisible(ItemPlaceholderView.identifier)
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
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.item("item-3"), isPlaceholder: false)) }

        XCTAssertTrue(events.contains(.appeared(.item("item-3"), isPlaceholder: false)))
        XCTAssertEqual(Set(builtFor), ["item-3"])
        XCTAssertNil(placeholderFrames[ItemPlaceholderView.identifier], "a registered destination replaces the placeholder")
    }

    func testTheSuggestionShowsAPlaceholderUntilADestinationIsRegistered() {
        scope = .authenticated
        host(ManagementDestinations())

        navigation.openSuggestion()
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.suggestion, isPlaceholder: true)) }

        XCTAssertTrue(events.contains(.appeared(.suggestion, isPlaceholder: true)))
        assertPlaceholderIsVisible(SuggestionPlaceholderView.identifier)
        XCTAssertNil(placeholderFrames[ItemPlaceholderView.identifier])
    }

    func testARegisteredSuggestionDestinationIsPresented() {
        var destinations = ManagementDestinations()
        destinations.suggestion = { _ in AnyView(Text("suggestion")) }
        scope = .authenticated
        host(destinations)

        navigation.openSuggestion()
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.suggestion, isPlaceholder: false)) }

        XCTAssertTrue(events.contains(.appeared(.suggestion, isPlaceholder: false)))
        XCTAssertNil(placeholderFrames[SuggestionPlaceholderView.identifier], "a registered destination replaces the placeholder")
    }

    func testRelockingWhilePresentedLeavesTheScreen() {
        scope = .authenticated
        host(ManagementDestinations())
        navigation.openItem(itemID: "item-4")
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.item("item-4"), isPlaceholder: true)) }

        scope = .captureOnly
        navigation.revalidate()
        waitForScreen("the host reports that the screen disappeared") { events.contains(.disappeared(.item("item-4"))) }

        XCTAssertTrue(events.contains(.disappeared(.item("item-4"))))
        XCTAssertEqual(navigation.path, [])
        XCTAssertNil(navigation.requestedItemID)
    }

    func testACaptureEntryLeavesAPresentedItemForCapture() {
        scope = .authenticated
        host(ManagementDestinations())
        navigation.openItem(itemID: "item-5")
        waitForScreen("the host reports that the screen appeared") { events.contains(.appeared(.item("item-5"), isPlaceholder: true)) }

        navigation.returnToCapture()
        waitForScreen("the host reports that the screen disappeared") { events.contains(.disappeared(.item("item-5"))) }

        XCTAssertTrue(events.contains(.disappeared(.item("item-5"))))
        XCTAssertEqual(navigation.path, [])
    }
}
