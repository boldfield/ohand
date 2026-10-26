import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real export screen in a window over the real model and `ExportService`. The screen reports where each
/// visible part landed, the words it shows and whether it is enabled; the tests drive the model calls the screen's
/// controls make and read what is on screen.
@MainActor
final class ExportSettingsViewTests: XCTestCase {
    private static let phone = CGSize(width: 390, height: 844)
    private var worlds: [ExportWorld] = []
    private var rendered: [String: RetentionRenderedElement] = [:]
    private var window: UIWindow?
    /// The host app's key window before the screen took over, made key again when the screen goes.
    private var previousKeyWindow: UIWindow?

    override func tearDown() {
        close()
        worlds.forEach { $0.tearDown() }
        worlds = []
        super.tearDown()
    }

    private func withScreen(
        authenticated: Bool = true, _ body: (ExportSettingsModel, ExportWorld) async throws -> Void
    ) async rethrows {
        let world = try! ExportWorld()
        worlds.append(world)
        if authenticated { world.authenticate() }
        rendered = [:]
        let session = world.session
        let model = ExportSettingsModel(
            controls: world.makeService(), readScope: { session.scope == .authenticated ? .authenticated : .captureOnly })
        let hosting = UIHostingController(
            rootView: NavigationStack {
                ExportSettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
            })
        // The share sheet is presented from UIKit, which needs a window in a scene; the host app provides one.
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        previousKeyWindow = scene?.keyWindow
        let shown = scene.map { UIWindow(windowScene: $0) } ?? UIWindow()
        shown.frame = CGRect(origin: .zero, size: Self.phone)
        shown.rootViewController = hosting
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        do {
            try await body(model, world)
        } catch {
            await closeScreen(model)
            throw error
        }
        await closeScreen(model)
    }

    /// Leaves the screen the way the app does, so any share sheet still up is taken down by the screen itself, then
    /// waits until the sheet is really gone before the window goes: a sheet left mid-dismissal in a released window
    /// would finish its out-of-process teardown during a later test.
    private func closeScreen(_ model: ExportSettingsModel) async {
        model.clear()
        _ = await waitForScreen { window?.rootViewController?.presentedViewController == nil }
        if let leftover = window?.rootViewController?.presentedViewController {
            leftover.dismiss(animated: false)
            _ = await waitForScreen { window?.rootViewController?.presentedViewController == nil }
        }
        await settle()
        close()
    }

    private func close() {
        window?.rootViewController?.presentedViewController?.dismiss(animated: false)
        window?.isHidden = true
        window = nil
        previousKeyWindow?.makeKey()
        previousKeyWindow = nil
    }

    private func settle() async {
        for _ in 0..<3 {
            window?.rootViewController?.view.setNeedsLayout()
            window?.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        window?.rootViewController?.view.layoutIfNeeded()
    }

    private func text(_ identifier: String) -> String? { rendered[identifier]?.text }

    /// Polls on the main actor, where the window and its presentations live.
    private func waitForScreen(timeout: TimeInterval = 10, _ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    /// The system share sheet the screen has put on screen, once its presentation has finished.
    private func presentedShareSheet() async -> UIActivityViewController? {
        var sheet: UIActivityViewController? {
            window?.rootViewController?.presentedViewController as? UIActivityViewController
        }
        let shown = await waitForScreen { sheet.map { !$0.isBeingPresented } ?? false }
        return shown ? sheet : nil
    }

    func testControlsLimitsAndTheNoScopePromptAreShownBeforeAnythingIsChosen() async {
        await withScreen { _, _ in
            XCTAssertEqual(
                ExportSettingsView.infoParagraphs,
                [ExportSettingsText.explanation, ExportSettingsText.limitsNote, ExportSettingsText.audioNote])
            XCTAssertEqual(
                text(ExportSettingsView.footerIdentifier), ExportSettingsText.noScopeNote,
                "with nothing chosen the one footer asks for a choice")
            XCTAssertEqual(text(ExportSettingsView.scopeIdentifier(.personal)), "Personal")
            XCTAssertEqual(text(ExportSettingsView.scopeIdentifier(.work)), "Work")
            XCTAssertNotNil(rendered[ExportSettingsView.audioToggleIdentifier])
            XCTAssertEqual(rendered[ExportSettingsView.startIdentifier]?.isEnabled, false, "no scope, no export")
        }
    }

    func testChoosingAScopeEnablesExportAndHidesThePrompt() async {
        await withScreen { model, _ in
            model.setScope(.work, selected: true)
            await settle()
            XCTAssertEqual(rendered[ExportSettingsView.startIdentifier]?.isEnabled, true)
            XCTAssertEqual(text(ExportSettingsView.footerIdentifier), ExportSettingsText.audioOffFooter)
            model.includeAudio = true
            await settle()
            XCTAssertEqual(
                text(ExportSettingsView.footerIdentifier), ExportSettingsText.audioOnFooter,
                "the footer follows the audio switch")
            model.setScope(.work, selected: false)
            await settle()
            XCTAssertEqual(text(ExportSettingsView.footerIdentifier), ExportSettingsText.noScopeNote)
        }
    }

    func testACancelledShareIsShownAsNothingSentNotAsSuccess() async {
        await withScreen { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            await settle()
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), ExportSettingsText.readyText)

            model.shareEnded(.cancelled)
            await settle()

            let shown = text(ExportSettingsView.stateIdentifier) ?? ""
            XCTAssertTrue(shown.contains("Nothing was sent"), shown)
            XCTAssertFalse(shown.hasPrefix("Shared"))
            XCTAssertEqual(world.exportRemnants(), [])
        }
    }

    func testAFailedShareAndACompletedShareShowTheirOwnOutcome() async {
        await withScreen { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            model.shareEnded(.failed)
            await settle()
            guard case .finished(.failed(let failed)) = model.phase else { return XCTFail("got \(model.phase)") }
            _ = await waitForScreen { text(ExportSettingsView.stateIdentifier) == failed }
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), failed)

            await model.startExport()
            model.shareEnded(.completed)
            await settle()
            // A fixed settle can end before the hosted view renders the new state on a loaded simulator (the run that failed
            // here still showed the ready text): wait for the asserted text, then assert it below.
            _ = await waitForScreen { (text(ExportSettingsView.stateIdentifier) ?? "").hasPrefix("Shared") }
            let shown = text(ExportSettingsView.stateIdentifier) ?? ""
            XCTAssertTrue(shown.hasPrefix("Shared"), shown)
            XCTAssertTrue(shown.contains("cannot recall"))
            XCTAssertEqual(world.exportRemnants(), [])
        }
    }

    func testAFailedPreparationShowsAGenericMessageWithoutCoreDetail() async {
        await withScreen { model, world in
            world.core.failure = CoreFailure(errorClass: .permanent, code: "secret-code", message: "secret-detail")
            model.setScope(.personal, selected: true)
            await model.startExport()
            await settle()
            let shown = text(ExportSettingsView.stateIdentifier) ?? ""
            XCTAssertFalse(shown.isEmpty)
            XCTAssertFalse(shown.contains("secret"))
        }
    }

    func testALockedSessionShowsOnlyTheLockedMessage() async {
        await withScreen(authenticated: false) { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            await settle()
            XCTAssertEqual(text(ExportSettingsView.lockedIdentifier), ExportSettingsText.lockedText)
            XCTAssertNil(rendered[ExportSettingsView.startIdentifier])
            XCTAssertNil(rendered[ExportSettingsView.scopeIdentifier(.personal)])
            XCTAssertEqual(world.core.requests, [])
        }
    }

    func testThePreparedExportIsPresentedInTheSystemShareSheetAndItsCancelIsShownAsNothingSent() async {
        await withScreen { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            guard let sheet = await presentedShareSheet() else { return XCTFail("the share sheet is presented") }

            sheet.completionWithItemsHandler?(nil, false, nil, nil)
            await settle()

            let shown = text(ExportSettingsView.stateIdentifier) ?? ""
            XCTAssertTrue(shown.contains("Nothing was sent"), shown)
            XCTAssertNil(model.pendingShare)
            XCTAssertEqual(world.exportRemnants(), [])
        }
    }

    /// The sheet goes off screen before its completion handler runs, and a share extension may still be sending. That
    /// dismissal must not be taken as a cancel: the files stay, and the handler's later answer is the one shown. The
    /// handler is held back while the sheet is dismissed, as a share extension holds it back until it has finished.
    func testDismissingTheSheetBeforeItsHandlerRunsKeepsTheExportUntilTheHandlerReportsCompletion() async {
        await withScreen { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            guard let sheet = await presentedShareSheet() else { return XCTFail("the share sheet is presented") }
            let pending = model.pendingShare
            let remnants = world.exportRemnants()
            XCTAssertFalse(remnants.isEmpty)

            let handler = sheet.completionWithItemsHandler
            sheet.completionWithItemsHandler = nil
            sheet.dismiss(animated: false)
            await settle()

            XCTAssertNil(window?.rootViewController?.presentedViewController, "the sheet is off screen")
            XCTAssertEqual(model.phase, .sharing, "a dismissal alone is not an ending")
            XCTAssertEqual(model.pendingShare, pending)
            XCTAssertEqual(world.exportRemnants(), remnants, "the files the activity may still read are kept")
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), ExportSettingsText.readyText)

            handler?(nil, true, nil, nil)
            await settle()

            let shown = text(ExportSettingsView.stateIdentifier) ?? ""
            XCTAssertTrue(shown.hasPrefix("Shared"), shown)
            XCTAssertEqual(world.exportRemnants(), [])

            handler?(nil, false, nil, nil)
            await settle()
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), shown, "the export ends once")
        }
    }

    func testAFailedActivityReportedByTheSheetIsShownAsAFailure() async {
        await withScreen { model, world in
            model.setScope(.work, selected: true)
            await model.startExport()
            guard let sheet = await presentedShareSheet() else { return XCTFail("the share sheet is presented") }

            sheet.completionWithItemsHandler?(nil, true, nil, ExportFailure.storageUnavailable)
            await settle()

            guard case .finished(.failed(let failed)) = model.phase else { return XCTFail("got \(model.phase)") }
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), failed)
            XCTAssertEqual(world.exportRemnants(), [])
        }
    }

    func testLeavingTheScreenTakesDownTheSheetAndRemovesTheExport() async {
        await withScreen { model, world in
            model.setScope(.personal, selected: true)
            await model.startExport()
            guard await presentedShareSheet() != nil else { return XCTFail("the share sheet is presented") }

            model.clear()
            let gone = await waitForScreen { window?.rootViewController?.presentedViewController == nil }

            XCTAssertTrue(gone, "the sheet is dismissed")
            XCTAssertEqual(model.phase, .idle)
            XCTAssertEqual(world.exportRemnants(), [])
        }
    }
}
