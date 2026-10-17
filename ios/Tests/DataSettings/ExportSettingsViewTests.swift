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
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = hosting
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        do {
            try await body(model, world)
        } catch {
            close()
            throw error
        }
        close()
    }

    private func close() {
        window?.isHidden = true
        window = nil
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

    func testControlsLimitsAndTheNoScopePromptAreShownBeforeAnythingIsChosen() async {
        await withScreen { _, _ in
            XCTAssertEqual(text(ExportSettingsView.limitsIdentifier), ExportSettingsText.limitsNote)
            XCTAssertEqual(text(ExportSettingsView.audioNoteIdentifier), ExportSettingsText.audioNote)
            XCTAssertEqual(text(ExportSettingsView.noScopeIdentifier), ExportSettingsText.noScopeNote)
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
            XCTAssertNil(rendered[ExportSettingsView.noScopeIdentifier])
            XCTAssertEqual(rendered[ExportSettingsView.startIdentifier]?.isEnabled, true)
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
            XCTAssertEqual(text(ExportSettingsView.stateIdentifier), failed)

            await model.startExport()
            model.shareEnded(.completed)
            await settle()
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
}
