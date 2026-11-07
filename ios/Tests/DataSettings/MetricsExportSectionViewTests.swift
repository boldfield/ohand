import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real export screen, with its metrics section, in a window: the controls are on screen with their words
/// in an authenticated session, a capture-only session sees why instead, and Export metrics reaches the share sheet
/// and reports its ending.
@MainActor
final class MetricsExportSectionViewTests: XCTestCase {
    private final class FileBackedMetricsControls: MetricsReportControlling {
        let directory: URL
        var finished: [(String, ExportShareEnding)] = []
        var cancelled: [String] = []
        init(directory: URL) { self.directory = directory }

        func recordingEnabled() async -> Bool? { true }
        func setRecordingEnabled(_ enabled: Bool) async -> Bool { true }
        func clearAll() async -> Bool { true }
        func prepareExport() async throws -> PreparedMetricsExport {
            let file = directory.appendingPathComponent("MetricsExport.json")
            try Data("{}".utf8).write(to: file)
            return PreparedMetricsExport(id: "metrics-view-1", file: file)
        }
        func finishShare(exportID: String, ending: ExportShareEnding) -> ExportShareOutcome {
            finished.append((exportID, ending))
            return ExportShareOutcome(result: .shared, temporariesRemoved: true)
        }
        func cancelExport(exportID: String) -> ExportShareOutcome {
            cancelled.append(exportID)
            return ExportShareOutcome(result: .cancelled, temporariesRemoved: true)
        }
    }

    private static let phone = CGSize(width: 390, height: 844)
    private var worlds: [ExportWorld] = []
    private var rendered: [String: RetentionRenderedElement] = [:]
    private var window: UIWindow?
    private var previousKeyWindow: UIWindow?
    private var directory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("metrics-view-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        close()
        worlds.forEach { $0.tearDown() }
        worlds = []
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private func withScreen(
        authenticated: Bool, _ body: (FileBackedMetricsControls, ExportSettingsModel) async -> Void
    ) async {
        let world = try! ExportWorld()
        worlds.append(world)
        if authenticated { world.authenticate() }
        rendered = [:]
        let session = world.session
        let scope: () -> CompositionReadScope = { session.scope == .authenticated ? .authenticated : .captureOnly }
        let controls = FileBackedMetricsControls(directory: directory)
        let model = ExportSettingsModel(
            controls: world.makeService(), readScope: scope,
            metrics: MetricsExportSettingsModel(controls: controls, readScope: scope))
        let hosting = UIHostingController(
            rootView: NavigationStack {
                ExportSettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
            })
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        previousKeyWindow = scene?.keyWindow
        let shown = scene.map { UIWindow(windowScene: $0) } ?? UIWindow()
        shown.frame = CGRect(origin: .zero, size: Self.phone)
        shown.rootViewController = hosting
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        await body(controls, model)
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

    private func renderedAfterLayout(_ identifier: String) -> RetentionRenderedElement? {
        window?.rootViewController?.view.setNeedsLayout()
        window?.rootViewController?.view.layoutIfNeeded()
        return rendered[identifier]
    }

    private func waitForScreen(timeout: TimeInterval = 10, _ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    private func presentedShareSheet() async -> UIActivityViewController? {
        var sheet: UIActivityViewController? {
            window?.rootViewController?.presentedViewController as? UIActivityViewController
        }
        let shown = await waitForScreen { sheet.map { !$0.isBeingPresented } ?? false }
        return shown ? sheet : nil
    }

    func testTheControlsAndTheirWordsAreOnScreenInAnAuthenticatedSession() async {
        await withScreen(authenticated: true) { _, _ in
            await waitUntil("the metrics controls are rendered") {
                renderedAfterLayout(MetricsExportSection.exportIdentifier) != nil
            }
            XCTAssertEqual(rendered[MetricsExportSection.exportIdentifier]?.text, ExportSettingsText.metricsExportTitle)
            XCTAssertEqual(rendered[MetricsExportSection.pauseIdentifier]?.text, ExportSettingsText.metricsPauseTitle)
            XCTAssertEqual(rendered[MetricsExportSection.clearIdentifier]?.text, ExportSettingsText.metricsClearTitle)
            XCTAssertEqual(rendered[MetricsExportSection.exportIdentifier]?.isEnabled, true)
            XCTAssertEqual(rendered[MetricsExportSection.footerIdentifier]?.text, ExportSettingsText.metricsRecordingOnFooter)
            XCTAssertEqual(
                rendered[MetricsExportSection.holdsIdentifier]?.text,
                ExportSettingsText.metricsHolds + " " + ExportSettingsText.metricsHoldsNot)
            XCTAssertNil(rendered[MetricsExportSection.lockedIdentifier], "no locked explanation when authenticated")
        }
    }

    func testACaptureOnlySessionSeesWhyAndNoMetricsControl() async {
        await withScreen(authenticated: false) { _, _ in
            await waitUntil("the locked explanation is rendered") {
                renderedAfterLayout(MetricsExportSection.lockedIdentifier) != nil
            }
            XCTAssertEqual(rendered[MetricsExportSection.lockedIdentifier]?.text, ExportSettingsText.metricsLockedText)
            for identifier in [
                MetricsExportSection.exportIdentifier, MetricsExportSection.pauseIdentifier,
                MetricsExportSection.clearIdentifier, MetricsExportSection.holdsIdentifier,
            ] {
                XCTAssertNil(rendered[identifier], "\(identifier) is not offered without an authenticated session")
            }
        }
    }

    func testExportMetricsReachesTheShareSheetAndReportsItsEnding() async {
        await withScreen(authenticated: true) { controls, model in
            await waitUntil("the Export metrics control is rendered") {
                renderedAfterLayout(MetricsExportSection.exportIdentifier) != nil
            }
            // The control's action is a call to the section's model; the sheet is what the screen does with it.
            await model.metrics?.startExport()
            await settle()
            let sheet = await presentedShareSheet()
            XCTAssertNotNil(sheet, "the share sheet is presented for the metrics file")
            XCTAssertEqual(model.metrics?.pendingShare?.file.lastPathComponent, "MetricsExport.json")

            sheet?.completionWithItemsHandler?(nil, true, nil, nil)
            await waitUntil("the sheet's ending reached the metrics model") { controls.finished.count == 1 }
            XCTAssertEqual(controls.finished.first?.0, "metrics-view-1")
            XCTAssertEqual(controls.finished.first?.1, .completed)
        }
    }
}
