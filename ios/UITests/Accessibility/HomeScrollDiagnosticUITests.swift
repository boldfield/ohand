import XCTest

/// TEMPORARY (U10a diagnosis, reverted before review): after Home scrolls, compares XCTest's snapshot frames, the app's
/// in-process accessibility frames and rendered frames (`-OhAndDiagAXFrames`), the screenshot, and what a tap opens.
@available(iOS 17.0, *)
final class HomeScrollDiagnosticUITests: XCTestCase {
    private var app: XCUIApplication!
    private var sizeName = "default"
    private var outputDirectory: URL!
    private var variant = ""

    override func setUpWithError() throws {
        continueAfterFailure = true
        let environment = ProcessInfo.processInfo.environment
        sizeName = environment["ACCESSIBILITY_CONTENT_SIZE"] ?? "default"
        let path = environment["ACCESSIBILITY_OUTPUT_DIR"] ?? NSTemporaryDirectory() + "accessibility"
        outputDirectory = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
        app = XCUIApplication()
        if sizeName == "xxxl" {
            app.launchArguments += ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"]
        }
    }

    func testDiagF_auditAllTypesThenScroll() { run("F-audit-all", ["-OhAndAnnouncementLog", "-OhAndDiagAXFrames"], auditFirst: .all) }
    func testDiagG_auditWithoutDynamicTypeThenScroll() {
        var types = XCUIAccessibilityAuditType.all
        types.remove(.dynamicType)
        run("G-audit-nodyn", ["-OhAndAnnouncementLog", "-OhAndDiagAXFrames"], auditFirst: types)
    }
    func testDiagH_auditDynamicTypeOnlyThenScroll() { run("H-audit-dynonly", ["-OhAndAnnouncementLog", "-OhAndDiagAXFrames"], auditFirst: .dynamicType) }

    private func run(_ name: String, _ arguments: [String], auditFirst: XCUIAccessibilityAuditType? = nil) {
        variant = name
        app.launchArguments += arguments
        guard app.launch(fixture: .populated, timeout: 120) else { return log("NO-HOME") }
        _ = app.waitForFixtureReady()
        _ = app.homeCard(containing: "Synthetic").waitForExistence(timeout: 60)
        dismissKeyboard()
        record("start")
        if let auditFirst {
            var count = 0
            do {
                try app.performAccessibilityAudit(for: auditFirst) { _ in count += 1; return true }
                log("AUDIT ran, \(count) issue(s) ignored")
            } catch {
                log("AUDIT error \(error)")
            }
            Thread.sleep(forTimeInterval: 2)
            record("after-audit")
        }
        for _ in 0..<4 { app.swipeUp() }
        Thread.sleep(forTimeInterval: 1)
        record("end")
        Thread.sleep(forTimeInterval: 8)
        record("end+8s")
        probeTap(atY: app.frame.height * 0.45)
    }

    private func probeTap(atY y: CGFloat) {
        let point = CGPoint(x: app.frame.midX, y: y)
        let cards = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'home.recents.card.'"))
            .allElementsBoundByIndex
        let claimed = cards.first { $0.frame.contains(point) }
        log("TAP at \(point): snapshot says '\(claimed.map { String($0.label.prefix(50)) } ?? "no card")'")
        app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: point.x, dy: point.y)).tap()
        let detail = app.descendants(matching: .any).matching(identifier: "source-detail.current-text").firstMatch
        if detail.waitForExistence(timeout: 20) {
            log("TAP opened '\(detail.label.prefix(50))' value '\(String(describing: detail.value).prefix(50))'")
        } else {
            log("TAP opened no source detail")
        }
        screenshot("after-tap")
    }

    private func record(_ stage: String) {
        screenshot(stage)
        guard let root = try? app.snapshot() else { return log("\(stage) NO-SNAPSHOT") }
        var lines: [String] = []
        func walk(_ node: XCUIElementSnapshot) {
            if node.identifier.hasPrefix("home.recents.card.") || node.label == "Today" || node.identifier == "home.composer.field" {
                lines.append("  SNAP \(node.identifier.suffix(12))|\(node.label.prefix(30))|\(node.frame)")
            }
            if node.elementType == .scrollView { lines.append("  SCROLLVIEW \(node.frame)") }
            node.children.forEach(walk)
        }
        walk(root)
        log("\(stage):\n" + lines.joined(separator: "\n"))
        let diag = app.descendants(matching: .any).matching(identifier: "diag.ax-frames").firstMatch
        if diag.exists {
            log("  APP " + diag.label.replacingOccurrences(of: " ; ", with: "\n  APP "))
        }
        // The same element queried live, not from the snapshot.
        let first = app.homeCard(containing: "Synthetic")
        if first.exists { log("  LIVE first card '\(first.label.prefix(30))' \(first.frame) hittable=\(first.isHittable)") }
    }

    private func dismissKeyboard() {
        if app.keyboards.firstMatch.waitForExistence(timeout: 3) {
            let done = app.buttons["home.done"]
            if done.waitForExistence(timeout: 10) { done.tap() }
        }
        _ = XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 0"), object: app.keyboards)], timeout: 30)
        Thread.sleep(forTimeInterval: 1)
    }

    private func screenshot(_ stage: String) {
        let name = "\(sizeName)-diag-\(variant)-\(stage).png"
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: outputDirectory.appendingPathComponent(name))
    }

    private func log(_ text: String) {
        let line = "[\(sizeName) \(variant)] \(text)\n"
        print(line, terminator: "")
        let file = outputDirectory.appendingPathComponent("audit-diag-\(sizeName).log")
        if let handle = try? FileHandle(forWritingTo: file) {
            handle.seekToEndOfFile()
            handle.write(Data(line.utf8))
            try? handle.close()
        } else {
            try? line.write(to: file, atomically: true, encoding: .utf8)
        }
    }
}
