import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class HostedResponder: SuggestionResponding, @unchecked Sendable {
    var activations: [Result<SuggestionActivation, Error>]
    var responses: [Result<SuggestionResponseReceipt, Error>] = []
    /// When set, each activation waits for `releaseActivation()`, so a test can see the screen while it loads.
    var holdsActivations = false
    private let lock = NSLock()
    private var heldActivation: CheckedContinuation<Void, Never>?

    init(activations: [Result<SuggestionActivation, Error>]) {
        self.activations = activations
    }

    var isHoldingActivation: Bool { lock.lock(); defer { lock.unlock() }; return heldActivation != nil }

    func releaseActivation() {
        lock.lock()
        let continuation = heldActivation
        heldActivation = nil
        lock.unlock()
        continuation?.resume()
    }

    func activate(notificationID: String, scope: SuggestionScope?) async throws -> SuggestionActivation {
        if holdsActivations {
            await withCheckedContinuation { continuation in
                lock.lock(); defer { lock.unlock() }
                heldActivation = continuation
            }
        }
        guard !activations.isEmpty else { return .nothingToOffer }
        return try activations.removeFirst().get()
    }

    func respond(
        commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind
    ) async throws -> SuggestionResponseReceipt {
        guard !responses.isEmpty else { return SuggestionResponseReceipt(replayed: false) }
        return try responses.removeFirst().get()
    }
}

/// Hosts the real app root view with the suggestion destination registered the way production registers it
/// (`ManagementCatalog.destinations`), opens it the way a prompt tap does, and checks what is on screen.
final class SuggestionScreenHostedTests: XCTestCase {
    private static let promptID = "prompt:2026-10-08"
    private static let offerText = "synthetic action shown on the suggestion screen"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var services: AppServices!
    private var retainedWindow: UIWindow?
    private var scope: CompositionReadScope = .captureOnly
    private var pendingCompletions: [(CompositionAuthenticationOutcome) -> Void] = []
    private var navigation: ManagementNavigationModel!
    private var events: [ManagementPresentationEvent] = []
    private var rendered: [String: String] = [:]

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("suggestion-hosted-\(UUID().uuidString)", isDirectory: true)
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
        rendered = [:]
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

    private func host(responder: HostedResponder) -> SuggestionScreenModel {
        let model = SuggestionScreenModel(responder: responder)
        model.renderReporter = { [unowned self] in rendered = $0 }
        let current = services.managementContext
        let context = ManagementContext(
            voiceRecovery: current.voiceRecovery, retrieval: current.retrieval, retention: current.retention,
            providerSettings: current.providerSettings, promptSettings: current.promptSettings, suggestion: model,
            privacySettings: current.privacySettings)
        let rootView = AppRootView(
            navigation: navigation, captureEntry: services.captureEntry, context: context,
            destinations: ManagementCatalog.destinations, presentationReporter: { [unowned self] in events.append($0) })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: rootView)
        window.makeKeyAndVisible()
        retainedWindow = window
        settle { true }
        return model
    }

    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 10)
        repeat {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    /// A prompt tap before authentication: the identifier is kept, the owner is asked, and the screen opens after success.
    private func tapPromptAndAuthenticate(_ model: SuggestionScreenModel) {
        model.receive(promptID: Self.promptID)
        navigation.openSuggestion()
        XCTAssertEqual(pendingCompletions.count, 1, "the owner is asked before the screen opens")
        XCTAssertTrue(rendered.isEmpty, "nothing private is shown before authentication")
        scope = .authenticated
        pendingCompletions.removeFirst()(.authenticated)
    }

    private func offer() -> SuggestionOffer {
        SuggestionOffer(
            itemID: "item-1", revision: 3, text: Self.offerText, scope: .personal,
            responses: [.notNow, .stopSuggesting, .done])
    }

    func testAPromptTapPresentsTheRegisteredOfferWithItsAnswersAfterAuthentication() {
        let model = host(responder: HostedResponder(activations: [.success(.offer(offer()))]))
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.textIdentifier] != nil }

        XCTAssertTrue(events.contains(.appeared(.suggestion, isPlaceholder: false)), "the registered view, not the placeholder")
        XCTAssertEqual(rendered[SuggestionView.textIdentifier], Self.offerText)
        XCTAssertEqual(rendered[SuggestionView.notNowIdentifier], "Not now")
        XCTAssertEqual(rendered[SuggestionView.stopIdentifier], "Stop suggesting this")
        XCTAssertEqual(rendered[SuggestionView.doneIdentifier], "Done")
        XCTAssertNil(rendered[SuggestionView.messageIdentifier])
    }

    func testTheLoadingLineIsRenderedWhileTheSuggestionIsFoundAndThenGivesWayToTheOffer() {
        let responder = HostedResponder(activations: [.success(.offer(offer()))])
        responder.holdsActivations = true
        let model = host(responder: responder)
        tapPromptAndAuthenticate(model)
        settle { responder.isHoldingActivation && rendered[SuggestionView.messageIdentifier] != nil }

        XCTAssertTrue(responder.isHoldingActivation, "the activation is still running")
        XCTAssertEqual(model.state, .loading)
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.loadingLine)
        XCTAssertFalse(SuggestionView.loadingLine.isEmpty)
        XCTAssertNil(rendered[SuggestionView.textIdentifier], "no offer text while loading")

        responder.releaseActivation()
        settle { rendered[SuggestionView.textIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.textIdentifier], Self.offerText)
        XCTAssertNil(rendered[SuggestionView.messageIdentifier], "the loading line is gone")
    }

    func testAnEmptySetIsCalmAndRelockLeavesTheScreenAndDropsTheOffer() {
        let model = host(responder: HostedResponder(activations: [.success(.nothingToOffer)]))
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.messageIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.nothingToOfferLine)
        XCTAssertNil(rendered[SuggestionView.textIdentifier])

        scope = .captureOnly
        navigation.revalidate()
        settle { events.contains(.disappeared(.suggestion)) }
        XCTAssertTrue(events.contains(.disappeared(.suggestion)), "a relock leaves the screen")
        XCTAssertTrue(navigation.path.isEmpty)
        XCTAssertEqual(model.state, .locked)
    }

    func testARespondedAnswerAndAChangedItemNoticeAreRendered() {
        let responder = HostedResponder(activations: [.success(.offer(offer())), .success(.nothingToOffer)])
        responder.responses = [.failure(SuggestionResponseError.noLongerCurrent)]
        let model = host(responder: responder)
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.textIdentifier] != nil }

        Task { @MainActor in await model.respond(.notNow) }
        settle { rendered[SuggestionView.noticeIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.noticeIdentifier], SuggestionView.text(for: .changed))
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.nothingToOfferLine)
    }

    func testAStoredAnswerIsConfirmedOnScreen() {
        let model = host(responder: HostedResponder(activations: [.success(.offer(offer()))]))
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.textIdentifier] != nil }

        Task { @MainActor in await model.respond(.done) }
        settle { rendered[SuggestionView.messageIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.text(forResponded: .done))
        XCTAssertNil(rendered[SuggestionView.notNowIdentifier])
    }

    func testAnUnavailableSuggestionIsRendered() {
        let model = host(responder: HostedResponder(activations: [.failure(SuggestionResponseError.unreadable)]))
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.messageIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.unavailableLine)
        XCTAssertNil(rendered[SuggestionView.textIdentifier])
    }

    func testANotSavedAnswerKeepsTheOfferAndRendersTheRetryNotice() {
        let responder = HostedResponder(activations: [.success(.offer(offer()))])
        responder.responses = [.failure(SuggestionResponseError.unreadable)]
        let model = host(responder: responder)
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.textIdentifier] != nil }

        Task { @MainActor in await model.respond(.done) }
        settle { rendered[SuggestionView.noticeIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.noticeIdentifier], SuggestionView.text(for: .notSaved))
        XCTAssertEqual(rendered[SuggestionView.textIdentifier], Self.offerText, "the offer stays for a retry")
    }

    func testALockedScreenRendersTheUnlockLineAndNoOfferText() {
        let responder = HostedResponder(activations: [.success(.offer(offer()))])
        responder.responses = [.failure(SuggestionResponseError.relockedBeforeDelivery)]
        let model = host(responder: responder)
        tapPromptAndAuthenticate(model)
        settle { rendered[SuggestionView.textIdentifier] != nil }

        Task { @MainActor in await model.respond(.done) }
        settle { rendered[SuggestionView.messageIdentifier] != nil }
        XCTAssertEqual(rendered[SuggestionView.messageIdentifier], SuggestionView.lockedLine)
        XCTAssertNil(rendered[SuggestionView.textIdentifier])
    }
}
