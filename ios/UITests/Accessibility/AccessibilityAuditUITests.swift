import UIKit
import XCTest

/// Runs `performAccessibilityAudit()` with the default audit types on every screen a person reaches in M1, using the
/// synthetic fixtures, and asserts what the simulator can prove about VoiceOver: labels, state and announcements.
///
/// The driving script (`ios/scripts/accessibility-simulator.sh`) runs this class once at the default content size and once
/// at the largest accessibility size (`ACCESSIBILITY_CONTENT_SIZE=default|xxxl`, passed as `TEST_RUNNER_ACCESSIBILITY_CONTENT_SIZE`)
/// and names the directory for the audit log and screenshots (`TEST_RUNNER_ACCESSIBILITY_OUTPUT_DIR`). Any audit issue fails
/// the test; the only exemptions are in `isExempt`, each for one named element, and the contrast decisions of the glass list
/// (`glassList`, `GlassContrastLedger`); both are listed in docs/validation/accessibility.md. An audit that throws fails.
@available(iOS 17.0, *)
final class AccessibilityAuditUITests: XCTestCase {
    private var app: XCUIApplication!
    private var sizeName = "default"
    private var outputDirectory: URL!
    private var screenshotIndex = 0
    private var auditLines: [String] = []
    private var currentScreen = ""
    /// The first screen this test audited; settle stops are named after it.
    private var firstScreenOfTest = ""
    /// The contrast excuses under and on glass this test has taken, and the unobstructed audits that settle them.
    private var ledger = GlassContrastLedger()
    private var owedPlacements: [String: GlassPlacement] = [:]
    private let screenTimeout: TimeInterval = 90

    private static let largestContentSize = "UICTContentSizeCategoryAccessibilityXXXL"
    /// The launch argument the app's debug announcement log reads (`AccessibilityAnnouncer.logLaunchArgument`).
    private static let announcementLogArgument = "-OhAndAnnouncementLog"
    private static let announcementLogIdentifier = "accessibility.announcement-log"
    private static let fixtureStatusIdentifier = "ui-fixture.status"
    /// The glass list: the elements whose contrast issue `GlassContrastLedger` decides (docs/validation/accessibility.md,
    /// "Content under and on glass"). A label matches by prefix where the rest of it varies from run to run (a card caption
    /// ends in how long ago the note was captured). Every other contrast issue fails the test.
    private static let glassList: [(identifier: String, label: String, placement: GlassPlacement)] = [
        // Drawn on the composer glass.
        (identifier: "", label: "What's on your mind?", placement: .drawnOnGlass),
        // The mark, drawn on the header glass; only in the header (`glassPlacement`).
        (identifier: "", label: "&", placement: .drawnOnGlass),
        // Home cards and the list heading, scrolled under the header and the composer.
        (identifier: "", label: "Synthetic typed note", placement: .scrollsUnderGlass),
        (identifier: "", label: "Synthetic typed action", placement: .scrollsUnderGlass),
        (identifier: "", label: "Synthetic typed idea", placement: .scrollsUnderGlass),
        (identifier: "", label: "Synthetic spoken", placement: .scrollsUnderGlass),
        (identifier: "", label: "Note · ", placement: .scrollsUnderGlass),
        (identifier: "", label: "Action · ", placement: .scrollsUnderGlass),
        (identifier: "", label: "Idea · ", placement: .scrollsUnderGlass),
        (identifier: "", label: "Remind me 2099-01-15 ", placement: .scrollsUnderGlass),
        (identifier: "home.recents.list", label: "Today", placement: .scrollsUnderGlass),
        // Source detail, scrolled under the back button.
        (identifier: "item-controls.topic.save", label: "Save topic", placement: .scrollsUnderGlass),
        (identifier: "item-controls.text.save", label: "Save words", placement: .scrollsUnderGlass),
        (identifier: "", label: "Remind me soon to call the plumber about the leaking tap", placement: .scrollsUnderGlass),
        (identifier: "", label: "What you wrote", placement: .scrollsUnderGlass),
        (identifier: "", label: "Type", placement: .scrollsUnderGlass),
    ]

    override func setUpWithError() throws {
        continueAfterFailure = true
        let environment = ProcessInfo.processInfo.environment
        sizeName = environment["ACCESSIBILITY_CONTENT_SIZE"] ?? "default"
        XCTAssertTrue(["default", "xxxl"].contains(sizeName), "unknown content size \(sizeName)")
        let path = environment["ACCESSIBILITY_OUTPUT_DIR"] ?? NSTemporaryDirectory() + "accessibility"
        outputDirectory = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
        app = XCUIApplication()
        app.launchArguments += [Self.announcementLogArgument]
        if sizeName == "xxxl" {
            app.launchArguments += ["-UIPreferredContentSizeCategoryName", Self.largestContentSize]
        }
    }

    override func tearDownWithError() throws {
        settleFromTheDefaultSize()
        settleOwedElements()
        for owed in ledger.unsettled {
            auditLines.append("UNVERIFIED \(owed.key): excused under or on glass at \(owed.firstScreen) and never audited unobstructed")
            XCTFail("'\(owed.key)' was excused under or on glass at \(owed.firstScreen) and never audited unobstructed")
        }
        try append(auditLines.joined(separator: "\n") + "\n", to: "audit-\(sizeName).log")
        // What this run audited unobstructed, for the largest-size run (`settleFromTheDefaultSize`).
        let settled = ledger.settled.sorted { $0.key < $1.key }.map { "\($0.key)\t\($0.value)\n" }.joined()
        try append(settled, to: "settled-\(sizeName).txt")
        app.terminate()
    }

    private func append(_ text: String, to name: String) throws {
        let file = outputDirectory.appendingPathComponent(name)
        if let handle = try? FileHandle(forWritingTo: file) {
            handle.seekToEndOfFile()
            handle.write(Data(text.utf8))
            try? handle.close()
        } else {
            try text.write(to: file, atomically: true, encoding: .utf8)
        }
    }

    /// At the largest size, an element still owed is settled by the default-size run's unobstructed audit of the same
    /// element (same card, identifier and label). Contrast is a property of the element's colours, which do not change
    /// with the text size; the largest size only lowers the bar for large text. This is needed for the Home list: after
    /// it scrolls at the largest size, the app's accessibility frames (which the audit and the snapshot both read) can stay
    /// near the top of the list while the screen shows another part of it, so a scrolled stop there proves nothing either
    /// way (runs 41702917752, 41711242088, 41720210504: the screenshots and the logged frames disagree).
    private func settleFromTheDefaultSize() {
        guard sizeName == "xxxl", !ledger.unsettled.isEmpty else { return }
        let file = outputDirectory.appendingPathComponent("settled-default.txt")
        guard let text = try? String(contentsOf: file, encoding: .utf8) else {
            auditLines.append("NOTE \(currentScreen): no settled-default.txt; nothing is settled from the default size")
            return
        }
        var atDefault: [String: String] = [:]
        for line in text.split(separator: "\n") {
            let parts = line.split(separator: "\t", maxSplits: 1).map(String.init)
            if parts.count == 2, atDefault[parts[0]] == nil { atDefault[parts[0]] = parts[1] }
        }
        for owed in ledger.unsettled {
            guard let screen = atDefault[owed.key] else { continue }
            ledger.settle(owed.key, by: "default size, \(screen)")
            auditLines.append("CLEAR-AT-DEFAULT \(owed.key): owed since \(owed.firstScreen), audited unobstructed at the default size at \(screen)")
        }
    }

    // MARK: Home and capture

    func testHomeEmpty() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        audit("home-empty")
    }

    func testHomePopulated() {
        guard launch(.populated) else { return }
        XCTAssertTrue(app.homeCard(containing: "Synthetic").waitForExistence(timeout: screenTimeout))
        dismissKeyboardIfShown()
        audit("home-populated")
        // The cards under the composer are audited again once the list is scrolled to its end; any card still excused is
        // scrolled clear of the glass and audited there (`settleOwedElements`, from tearDown).
        for _ in 0..<4 { app.swipeUp() }
        Thread.sleep(forTimeInterval: 1)
        audit("home-populated-end")
    }

    func testCaptureVoiceIdle() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        let record = app.buttons["home.composer.record"]
        XCTAssertTrue(record.waitForExistence(timeout: screenTimeout), "the Record button is missing")
        XCTAssertEqual(record.label, "Record")
        audit("capture-voice-idle")
    }

    func testCaptureVoiceRecording() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.composer.record"], "Record") else { return }
        let stop = app.buttons["home.composer.stop"]
        // The first recording asks for the microphone; the driving script grants it ahead, and a prompt that still
        // appears is answered as a person would.
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let deadline = Date().addingTimeInterval(screenTimeout)
        while !stop.exists, Date() < deadline {
            for alert in [springboard.alerts.firstMatch, app.alerts.firstMatch] where alert.exists {
                if alert.buttons["Allow"].exists { alert.buttons["Allow"].tap() } else if alert.buttons["OK"].exists { alert.buttons["OK"].tap() }
            }
            Thread.sleep(forTimeInterval: 0.5)
        }
        guard stop.exists else {
            return XCTFail("recording did not start: the Stop button never appeared")
        }
        XCTAssertEqual(stop.label, "Stop recording")
        audit("capture-voice-recording")
        if tapWhenReady(app.buttons["home.composer.discard"], "Discard") {
            let record = app.buttons["home.composer.record"]
            XCTAssertTrue(record.waitForExistence(timeout: screenTimeout), "Discard did not end the recording")
        }
    }

    func testCaptureSilentTextEntry() {
        guard launch(.emptyFirstRun) else { return }
        guard typeCapture("Synthetic note for the accessibility audit") else { return }
        audit("capture-text-entry")
    }

    /// Saving a capture shows its outcome in a status line, which the app also announces (`CaptureEntryView`).
    func testSavingACaptureShowsTheOutcomeLine() {
        guard launch(.emptyFirstRun) else { return }
        guard typeCapture("Synthetic note saved during the accessibility check") else { return }
        let save = app.buttons["home.composer.save"]
        XCTAssertFalse(save.label.isEmpty, "Save has no label")
        guard tapWhenReady(save, "Save") else { return }
        let message = app.element(identifier: "capture-entry.message")
        XCTAssertTrue(message.waitForExistence(timeout: screenTimeout), "saving a capture showed no outcome line to announce")
        XCTAssertFalse(message.label.isEmpty, "the outcome line has no text")
        // The same words are posted as a VoiceOver announcement (hosted tests assert the announcement through the injectable
        // announcer; this reads the debug log in the real app).
        assertAnnouncement(containing: message.label)
    }

    // MARK: Search

    func testSearchWithResults() {
        guard launch(.populated), openSearch(), search("garden") else { return }
        XCTAssertTrue(app.staticTexts["retrieval.search.count"].waitForExistence(timeout: screenTimeout), "no results for garden")
        audit("search-results")
    }

    func testSearchWithNoResults() {
        guard launch(.populated), openSearch(), search("zzzxqv") else { return }
        XCTAssertTrue(app.staticTexts["No matches"].waitForExistence(timeout: screenTimeout) || app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'No matches'")).firstMatch.waitForExistence(timeout: 5), "search showed no empty state")
        audit("search-no-results")
    }

    // MARK: Source detail

    func testSourceDetailWithReminderAndControls() {
        guard launch(.needsCorrection) else { return }
        let card = app.homeCard(containing: "call the plumber")
        guard card.waitForExistence(timeout: screenTimeout) else { return XCTFail("no card for the reminder note") }
        dismissKeyboardIfShown()
        guard tapWhenReady(card, "the reminder card") else { return }
        guard app.element(identifier: "source-detail.current-text").waitForExistence(timeout: screenTimeout) else {
            return XCTFail("source detail did not open")
        }
        audit("source-detail-top")
        app.swipeUp()
        audit("source-detail-controls")
        app.swipeUp()
        audit("source-detail-end")
    }

    // MARK: Settings

    /// Each Settings screen the person reaches, one launch per size: the row's label, then the screen's own marker.
    func testSettingsScreens() {
        guard launch(.populated), openSettings() else { return }
        audit("settings-root")
        let screens: [(row: String, slug: String)] = [
            ("Daily prompt", "prompt-settings"),
            ("Unfinished recordings", "recovery"),
            ("Processing", "processing"),
            ("Privacy", "privacy"),
            ("Status", "status"),
            ("Audio", "data-audio"),
            ("Export", "data-export"),
        ]
        for screen in screens {
            guard openSettingsRow(screen.row) else { return }
            audit(screen.slug)
            if screen.slug == "prompt-settings" { assertPromptToggleExposesState() }
            if screen.slug == "processing" {
                let pushed: [(identifier: String, slug: String, marker: String?)] = [
                    ("provider-settings.add-provider", "processing-add-provider", "provider-settings.field.model"),
                    ("provider-settings.advanced", "processing-advanced", nil),
                ]
                for (identifier, slug, marker) in pushed {
                    let link = app.buttons[identifier]
                    guard scrollUntilHittable(link), tapWhenReady(link, identifier) else { return }
                    guard waitForSettledDestination(slug, from: link, marker: marker) else { return }
                    audit(slug)
                    guard goBack() else { return }
                }
            }
            guard goBack() else { return }
        }
    }

    /// The pushed screen is in its final frame when its own marker (when it has a known one) exists and stops moving; the
    /// Processing link that opened it has left the screen by then, or the screen is accepted after the link wait times out.
    private func waitForSettledDestination(_ slug: String, from link: XCUIElement, marker: String?) -> Bool {
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false OR isHittable == false"), object: link)
        // Without a marker this is the only sign of arrival; a screen that keeps the link hittable settles on the frame check below.
        if XCTWaiter().wait(for: [gone], timeout: marker == nil ? 15 : 5) != .completed { auditLines.append("NOTE \(slug): the opening link stays on screen") }
        let destination = marker.map { app.element(identifier: $0) }
        if let destination, !destination.waitForExistence(timeout: screenTimeout) {
            XCTFail("\(slug) did not open: no \(marker ?? "")")
            return false
        }
        // The navigation transition is finished when two frames of the marker, a moment apart, are equal.
        var previous = destination?.frame
        for _ in 0..<10 {
            Thread.sleep(forTimeInterval: 0.5)
            let current = destination?.frame
            if current == previous { return true }
            previous = current
        }
        XCTFail("\(slug): the screen never settled")
        return false
    }

    func testSettingsPermissionsOnFirstRun() {
        guard launch(.emptyFirstRun), openSettings(), openSettingsRow("Permissions") else { return }
        audit("permissions-first-run")
    }

    func testDataSettingsEraseConfirmation() {
        guard launch(.populated), openSettings() else { return }
        let erase = app.element(identifier: "management.settings.erase")
        guard scrollUntilHittable(erase), tapWhenReady(erase, "Erase everything") else { return }
        XCTAssertTrue(app.element(identifier: "data-settings.reset.explanation").waitForExistence(timeout: screenTimeout), "the reset screen did not open")
        audit("data-erase")
        let keep = app.element(identifier: "data-settings.reset.credentials.keep")
        if scrollUntilHittable(keep), tapWhenReady(keep, "Keep credentials") {
            XCTAssertTrue(keep.isSelected, "the chosen credential option does not report that it is selected")
        }
        let proceed = app.element(identifier: "data-settings.reset.continue")
        guard scrollUntilHittable(proceed), tapWhenReady(proceed, "Erase everything (continue)") else { return }
        let confirm = app.element(identifier: "data-settings.reset.confirm")
        XCTAssertTrue(confirm.waitForExistence(timeout: screenTimeout), "the erase confirmation did not appear")
        audit("data-erase-confirmation")
        // Nothing is confirmed: the confirmation is left as it is.
    }

    // MARK: Status

    func testStatusWithFailedNote() {
        guard launch(.failedSorting), openSettings(), openSettingsRow("Status") else { return }
        XCTAssertTrue(
            app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'status.failed-note.'")).firstMatch
                .waitForExistence(timeout: screenTimeout), "Status shows no failed note")
        audit("status-failed-note")
        // A processing failure is announced when Status learns of it.
        assertAnnouncement(containing: "failed to sort")
    }

    func testStatusWithReminderNeedingATime() {
        guard launch(.needsCorrection), openSettings(), openSettingsRow("Status") else { return }
        XCTAssertTrue(
            app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'status.needs-time-note.'")).firstMatch
                .waitForExistence(timeout: screenTimeout), "Status shows no reminder that needs a time")
        audit("status-needs-time")
    }

    // MARK: Locked

    func testLockedScreen() {
        guard launch(.locked) else { return }
        let unlock = app.buttons["home.recents.unlock"]
        XCTAssertTrue(app.element(identifier: "home.recents.locked").waitForExistence(timeout: screenTimeout), "the locked card did not appear")
        XCTAssertTrue(unlock.waitForExistence(timeout: screenTimeout), "the locked screen has no unlock control")
        XCTAssertEqual(unlock.label, "Unlock")
        XCTAssertTrue(unlock.isEnabled, "the unlock control is disabled")
        dismissKeyboardIfShown()
        scrollUntilHittable(unlock)
        for _ in 0..<3 where unlock.frame.maxY > currentClearBand().upperBound { app.swipeUp() }
        XCTAssertTrue(unlock.isHittable, "the unlock control cannot be reached, so authentication has a dead end")
        audit("locked")
        assertAnnouncement(containing: "Your captures are locked")
    }

    // MARK: Announcements

    func testReminderNeedingACorrectionIsAnnounced() {
        guard launch(.needsCorrection) else { return }
        XCTAssertTrue(app.homeCard(containing: "call the plumber").waitForExistence(timeout: screenTimeout))
        assertAnnouncement(containing: "A reminder needs a time")
    }

    // MARK: Audit

    /// Every audit type (`.all`, the default of `performAccessibilityAudit()`); the default-size run leaves out `.dynamicType` (see `audit`).
    /// An entry ending in a space or starting with "Synthetic" matches by prefix; any other entry matches exactly.
    private static func matches(label: String, entry: String) -> Bool {
        entry.hasSuffix(" ") || entry.hasPrefix("Synthetic") ? label.hasPrefix(entry) : label == entry
    }

    private var auditTypes: XCUIAccessibilityAuditType {
        var types = XCUIAccessibilityAuditType.all
        if sizeName != "xxxl" { types.remove(.dynamicType) }
        return types
    }

    /// Screenshot, then the audit: every default type at the largest size, and at the default size every default type
    /// except `.dynamicType`, whose resize probe measures the scroll view's geometry and not the fonts (the deterministic
    /// replacement is `ios/scripts/check_font_tokens.py`; docs/validation/accessibility.md). An issue that is neither in
    /// `isExempt` nor excused by the glass ledger fails the test, and so does an audit that cannot run.
    private func audit(_ screen: String, checksLabels: Bool = true) {
        capture(screen)
        currentScreen = screen
        if firstScreenOfTest.isEmpty { firstScreenOfTest = screen }
        if checksLabels, app.keyboards.count == 0 { assertInteractiveElementsAreLabelled(screen) }
        let screenFrame = app.frame
        // An audit that finds something is repeated after the screen has settled: a card still fading in or a view
        // mid-transition is sampled against the wrong background and reports a contrast or text-size issue that is gone
        // a moment later. Only what the settled screen still shows counts, and every such issue fails the test. The glass
        // ledger takes only the final pass's decisions.
        var issueCount = 0
        for attempt in 1...2 {
            let elements = screenElements()
            let band = clearBand(from: elements)
            var trialLedger = ledger
            var trialOwed: [String: GlassPlacement] = [:]
            var notes: [String] = []
            var found: [String] = []
            var flaggedKeys: Set<String> = []
            var auditError: Error?
            do {
                try app.performAccessibilityAudit(for: auditTypes) { [self] issue in
                    let element = issue.element
                    let description = "\(screen): \(issue.auditType) \(issue.compactDescription) | \(issue.detailedDescription) | element: \(element?.identifier ?? "?") '\(element?.label ?? "?")' frame: \(element?.frame.debugDescription ?? "?") clear band: \(band)"
                    if issue.auditType == .contrast, let element,
                       let placement = glassPlacement(identifier: element.identifier, label: element.label, frame: element.frame, band: band) {
                        let shown = displayed(identifier: element.identifier, label: element.label, near: element.frame, in: elements)
                        if let shown { flaggedKeys.insert(shown.key) }
                        let decision = trialLedger.decide(
                            contrastIssueOn: shown, placement: placement, screenName: screen, clearBand: band, screen: screenFrame,
                            others: elements, measure: { shown.flatMap { measuredContrast(of: $0.frame) } })
                        let shownFrame = shown?.frame.debugDescription ?? "not shown"
                        switch decision {
                        case .owed:
                            if let shown { trialOwed[shown.key] = placement }
                            notes.append("OWED \(screen): '\(element.label)' (\(placement)) shown at \(shownFrame), obstructed\(obstruction(of: shown, placement: placement, in: elements)); the test must audit it unobstructed")
                        case .measured(let ratio):
                            notes.append("MEASURED \(screen): '\(element.label)' shown at \(shownFrame) with nothing overlapping, contrast \(String(format: "%.2f", ratio)):1 (minimum \(GlassContrastLedger.minimumContrastRatio):1)")
                        case .issue(let reason):
                            found.append("\(description) | shown at \(shownFrame): \(reason)")
                        }
                        return true
                    }
                    if isExempt(issue) {
                        notes.append("EXEMPT \(description)")
                        return true
                    }
                    found.append(description)
                    return true
                }
            } catch {
                auditError = error
            }
            // An audit that could not run proves nothing about the screen: it fails, it is never a pass.
            if let auditError {
                issueCount += 1
                auditLines.append("ERROR \(screen): the audit did not run: \(auditError)")
                XCTFail("\(screen): the accessibility audit did not run: \(auditError)")
                break
            }
            if !found.isEmpty, attempt == 1 {
                auditLines.append("RETRY \(screen): \(found.count) issue(s) on the first pass, auditing again after the screen settles")
                Thread.sleep(forTimeInterval: 3)
                continue
            }
            ledger = trialLedger
            owedPlacements.merge(trialOwed) { first, _ in first }
            auditLines += notes
            let listed = elements.compactMap { element in
                glassPlacement(identifier: element.identifier, label: element.label, frame: element.frame, band: band).map { (element, $0) }
            }
            let settled = ledger.recordStop(screen, listed: listed, flaggedKeys: flaggedKeys, clearBand: band, screen: screenFrame, others: elements)
            for element in settled {
                auditLines.append("CLEAR \(screen): '\(element.label)' at \(element.frame.debugDescription) audited unobstructed with no contrast issue")
            }
            for description in found {
                issueCount += 1
                auditLines.append("ISSUE \(description)")
                print("AUDIT-ISSUE (\(sizeName)) \(description)")
                XCTFail(description)
            }
            break
        }
        auditLines.append("\(issueCount == 0 ? "PASS" : "FAIL") \(screen) (\(sizeName)), \(issueCount) issue(s)")
    }

    /// What keeps text drawn on glass from being measured: the elements overlapping it, for the log.
    private func obstruction(of element: ScreenElement?, placement: GlassPlacement, in elements: [ScreenElement]) -> String {
        guard let element, placement == .drawnOnGlass else { return "" }
        let others = GlassContrastLedger.overlapping(element, in: elements).prefix(3).map { "'\($0.label.prefix(40))' \($0.frame.debugDescription)" }
        return others.isEmpty ? "" : " by \(others.joined(separator: ", "))"
    }

    /// The glass list entry for an element, when it has one. The mark counts only in the header, above the clear band.
    private func glassPlacement(identifier: String, label: String, frame: CGRect, band: ClosedRange<CGFloat>) -> GlassPlacement? {
        guard let entry = Self.glassList.first(where: { $0.identifier == identifier && Self.matches(label: label, entry: $0.label) }) else {
            return nil
        }
        if entry.label == "&", frame.maxY > band.lowerBound { return nil }
        return entry.placement
    }

    /// The element as the snapshot before the audit showed it: the same identifier and label (a caption's age aside),
    /// nearest to where the audit reports it.
    private func displayed(identifier: String, label: String, near frame: CGRect, in elements: [ScreenElement]) -> ScreenElement? {
        let stable = ScreenElement.stableLabel(label)
        func distance(_ candidate: ScreenElement) -> CGFloat {
            hypot(candidate.frame.midX - frame.midX, candidate.frame.midY - frame.midY)
        }
        return elements
            .filter { $0.identifier == identifier && ScreenElement.stableLabel($0.label) == stable }
            .min { distance($0) < distance($1) }
    }

    /// Every labelled element on screen, from one snapshot of the hierarchy. A Home card's caption and text are keyed by
    /// their card: the card in the hierarchy above them, or else the card whose frame holds them and whose label has their text.
    private func screenElements() -> [ScreenElement] {
        let root: XCUIElementSnapshot
        do {
            root = try app.snapshot()
        } catch {
            XCTFail("\(currentScreen): no snapshot of the screen before the audit: \(error)")
            return []
        }
        var nodes: [ScreenElement] = []
        var nextNodeID = 0
        func walk(_ node: XCUIElementSnapshot, ancestors: Set<Int>) {
            let nodeID = nextNodeID
            nextNodeID += 1
            // A scroll indicator (a field's "Horizontal scroll bar, 1 page") is not content and obstructs nothing.
            let isScrollIndicator = node.elementType == .scrollBar
                || node.label.hasPrefix("Horizontal scroll bar") || node.label.hasPrefix("Vertical scroll bar")
            if !node.label.isEmpty, !isScrollIndicator {
                nodes.append(ScreenElement(
                    nodeID: nodeID, ancestorIDs: ancestors, key: "", identifier: node.identifier, label: node.label, frame: node.frame))
            }
            for child in node.children { walk(child, ancestors: ancestors.union([nodeID])) }
        }
        walk(root, ancestors: [])
        let cards = nodes.filter { $0.identifier.hasPrefix("home.recents.card.") }
        return nodes.map { node in
            let stable = ScreenElement.stableLabel(node.label)
            let card = cards
                .filter { card in
                    card.nodeID != node.nodeID
                        && (node.ancestorIDs.contains(card.nodeID)
                            || (card.frame.insetBy(dx: -1, dy: -1).contains(node.frame) && card.label.contains(stable)))
                }
                .min { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }
            return ScreenElement(
                nodeID: node.nodeID, ancestorIDs: node.ancestorIDs,
                key: ScreenElement.key(card: card.map { ScreenElement.cardName($0.label) } ?? "", identifier: node.identifier, label: node.label),
                identifier: node.identifier, label: node.label, frame: node.frame)
        }
    }

    /// For each excuse the test has not settled, brings the element where it is unobstructed and audits there: content
    /// scrolled under glass is dragged into the middle of the clear band; for text drawn on glass the screen is scrolled
    /// to its top (the header) or its end (the composer), where nothing is behind the glass. What still is not settled
    /// fails the test in tearDown.
    private func settleOwedElements() {
        var stop = 0
        var attempted: Set<String> = []
        // A settle stop can owe something new (a card it scrolls under the header), so this runs until every excuse has
        // had its own stop, up to 12 stops.
        while stop < 12, let owed = ledger.unsettled.first(where: { !attempted.contains($0.key) }) {
            attempted.insert(owed.key)
            guard let placement = owedPlacements[owed.key] else { continue }
            let screenFrame = app.frame
            for _ in 0..<6 {
                let elements = screenElements()
                guard let target = elements.first(where: { $0.key == owed.key }) else { break }
                let band = clearBand(from: elements)
                if GlassContrastLedger.isUnobstructed(target, placement: placement, clearBand: band, screen: screenFrame, others: elements) { break }
                switch placement {
                case .scrollsUnderGlass:
                    let visibleBottom = min(band.upperBound, screenFrame.maxY)
                    scrollContent(by: target.frame.midY - (band.lowerBound + visibleBottom) / 2, between: band.lowerBound...visibleBottom)
                case .drawnOnGlass:
                    // Nothing is behind the header at the top of the screen, or behind the composer at its end.
                    if target.frame.midY < screenFrame.midY { app.swipeDown() } else { app.swipeUp() }
                    Thread.sleep(forTimeInterval: 1)
                }
            }
            stop += 1
            auditLines.append("SETTLE \(currentScreen): scrolled for '\(owed.key)' (excused at \(owed.firstScreen))")
            audit("\(firstScreenOfTest)-settle-\(stop)", checksLabels: false)
        }
    }

    /// Drags the scrolling content up by `distance` points (down when negative), at most the height of `band` per drag,
    /// slowly and with a hold at the end so it does not coast. The drag runs down the left margin, 8 points from the edge,
    /// beside the cards: started on a card (a button), the scroll still moves but the app's accessibility frames stay at
    /// the top of the list (runs 41702917752 and 41711242088, where the screenshots and the snapshots disagreed); at the
    /// right edge it catches the scroll indicator, which scrubs the whole list. A vertical drag does not start the back
    /// gesture. The caller drags again until the element is where it should be, and the frames are read only once two
    /// snapshots a second apart agree.
    private func scrollContent(by distance: CGFloat, between band: ClosedRange<CGFloat>) {
        let top = band.lowerBound + 20
        let bottom = band.upperBound - 20
        guard bottom > top, abs(distance) > 1 else { return }
        let step = max(-(bottom - top), min(bottom - top, distance))
        let startY = step > 0 ? bottom : top
        let origin = app.coordinate(withNormalizedOffset: .zero)
        let x: CGFloat = 8
        origin.withOffset(CGVector(dx: x, dy: startY)).press(
            forDuration: 0.05, thenDragTo: origin.withOffset(CGVector(dx: x, dy: startY - step)), withVelocity: .slow, thenHoldForDuration: 0.3)
        waitForStillFrames()
    }

    /// Waits (up to 5 seconds) until two snapshots a second apart show every element in the same place.
    private func waitForStillFrames() {
        var previous = screenElements().map(\.frame)
        for _ in 0..<5 {
            Thread.sleep(forTimeInterval: 1)
            let current = screenElements().map(\.frame)
            if current == previous { return }
            previous = current
        }
    }

    /// The exemptions outside the glass list. Each one is a single named issue on a single element and is listed with its
    /// reason in docs/validation/accessibility.md. Only the default-size run omits `.dynamicType`, as `auditTypes` documents;
    /// no exemption excludes a type.
    private func isExempt(_ issue: XCUIAccessibilityAuditIssue) -> Bool {
        guard let identifier = issue.element?.identifier else { return false }
        // The keyboard's prediction bar is the system's, not the app's.
        if issue.detailedDescription.contains("TUIPredictionViewCell") { return true }
        // A single-line field cannot wrap, so the audit's "may be clipped at larger Dynamic Type sizes" for a
        // text field is a heuristic it cannot clear; at the largest size the screenshot shows the field growing in height.
        if issue.auditType == .textClipped,
           ["provider-settings.field.model", "provider-settings.field.secret"].contains(identifier),
           issue.element?.elementType == .textField || issue.element?.elementType == .secureTextField {
            return true
        }
        // The debug-only 4-point markers that tell a test how far a fixture is and what was announced. They are not
        // part of the app a person uses (debug builds only, near-invisible by design), so their size and contrast are
        // not defects. They are exempt by identifier, for any issue.
        return identifier == Self.fixtureStatusIdentifier || identifier == Self.announcementLogIdentifier
    }

    /// The top of the screenshot the measurement never reads: the status bar's clock and indicators end above it on the
    /// iPhone simulators the job uses.
    private static let statusBarBottom: CGFloat = 50

    /// The contrast of the text in `frame` as a person sees it, from the screenshot pixels (`ScreenshotContrast`). Called
    /// only for text drawn on glass with nothing else overlapping its frame, so the darkest glyph in the crop is its own.
    private func measuredContrast(of frame: CGRect) -> Double? {
        guard let image = app.screenshot().image.cgImage else { return nil }
        let screenWidth = app.frame.width
        guard screenWidth > 0 else { return nil }
        let scale = CGFloat(image.width) / screenWidth
        var visible = frame.intersection(app.frame)
        if visible.minY < Self.statusBarBottom {
            visible = CGRect(x: visible.minX, y: Self.statusBarBottom, width: visible.width, height: visible.maxY - Self.statusBarBottom)
        }
        guard !visible.isNull, visible.height > 1 else { return nil }
        let pixelRect = visible.applying(CGAffineTransform(scaleX: scale, y: scale)).integral
            .intersection(CGRect(x: 0, y: 0, width: image.width, height: image.height))
        guard !pixelRect.isEmpty, let crop = image.cropping(to: pixelRect) else { return nil }
        let width = crop.width, height = crop.height
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        guard let context = CGContext(
            data: &pixels, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        context.draw(crop, in: CGRect(x: 0, y: 0, width: width, height: height))
        return ScreenshotContrast.ratio(pixels: pixels, width: width, height: height)
    }

    /// Every button, switch and field a person can act on has a label that names it.
    private func assertInteractiveElementsAreLabelled(_ screen: String) {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .textField, .textView, .slider, .stepper, .link, .segmentedControl]
        for kind in kinds {
            for element in app.descendants(matching: kind).allElementsBoundByIndex where element.exists {
                let identifier = element.identifier
                if identifier == Self.fixtureStatusIdentifier || identifier == Self.announcementLogIdentifier { continue }
                let hasName = !element.label.isEmpty || !element.title.isEmpty || !((element.placeholderValue ?? "").isEmpty)
                // SwiftUI exposes a toggle or menu as a labelled element that holds an unlabelled system control; VoiceOver
                // lands on the labelled one, so the inner control is named by the element around it.
                let isNamedByContainer = !hasName && (isInsideLabelledElement(element) || isNamedByRowText(element))
                XCTAssertTrue(hasName || isNamedByContainer, "\(screen): a \(kind) with identifier '\(identifier)' has no label: \(element.debugDescription.prefix(300)); parent \(element.value ?? "-")")
            }
        }
    }

    /// The vertical band between the overlays at the top and bottom of the screen.
    private func currentClearBand() -> ClosedRange<CGFloat> {
        clearBand(from: screenElements())
    }

    /// The vertical band between the overlays, from one snapshot. The top is 12 points below the header's glass buttons
    /// (or the back button). The bottom is the top of the composer's glass, which sits 20 points (its padding) above the
    /// field, the placeholder and the "Recording" line, less 4 points; a status line or the Saved pill above the composer
    /// (drawn over the list) lowers it to 8 points above that line.
    private func clearBand(from elements: [ScreenElement]) -> ClosedRange<CGFloat> {
        let headerIdentifiers: Set<String> = ["home.search", "home.settings", "home.done"]
        let top = elements.filter { headerIdentifiers.contains($0.identifier) || ($0.label == "Back" && $0.frame.minY < 200) }
            .map(\.frame.maxY).max().map { $0 + 12 } ?? 0
        let composerIdentifiers: Set<String> = [
            "home.composer.field", "home.composer.record", "home.composer.save", "home.composer.stop", "home.composer.discard",
        ]
        let composerTop = elements.filter { element in
            composerIdentifiers.contains(element.identifier)
                || (element.identifier.isEmpty && ["What's on your mind?", "Recording"].contains(element.label))
        }.map(\.frame.minY).min().map { $0 - 24 }
        let linesAboveComposer: Set<String> = ["capture-entry.message", "capture-entry.notice", "capture-entry.saved"]
        let lineTop = elements.filter { linesAboveComposer.contains($0.identifier) }.map(\.frame.minY).min().map { $0 - 8 }
        let bottom = [composerTop, lineTop].compactMap { $0 }.min() ?? CGFloat.greatestFiniteMagnitude
        return top...max(top, bottom)
    }

    /// A switch whose row text is a separate static text to its left: the row names it for VoiceOver, which reads the
    /// toggle's label with its state.
    private func isNamedByRowText(_ element: XCUIElement) -> Bool {
        guard element.elementType == .switch else { return false }
        let frame = element.frame
        let texts = app.staticTexts.matching(NSPredicate(format: "label != ''")).allElementsBoundByIndex
        return texts.contains { text in
            text.exists && text.frame.minX < frame.minX && text.frame.minY <= frame.midY && text.frame.maxY >= frame.midY
        }
    }

    private func isInsideLabelledElement(_ element: XCUIElement) -> Bool {
        let frame = element.frame
        let labelled = app.descendants(matching: .any).matching(NSPredicate(format: "label != ''")).allElementsBoundByIndex
        return labelled.contains { candidate in
            candidate.exists && candidate.isHittable && candidate.frame.insetBy(dx: -1, dy: -1).contains(frame)
                && [.switch, .button, .cell, .other, .toggle].contains(candidate.elementType)
        }
    }

    private func assertPromptToggleExposesState() {
        let toggle = app.element(identifier: "prompt-settings.toggle")
        guard toggle.waitForExistence(timeout: screenTimeout) else { return XCTFail("the daily prompt toggle is missing") }
        XCTAssertFalse(toggle.label.isEmpty, "the daily prompt toggle has no label")
        let value = (toggle.value as? String) ?? ""
        XCTAssertTrue(["0", "1", "On", "Off"].contains(value), "the daily prompt toggle exposes no state (value '\(value)')")
    }

    /// The app's debug announcement log holds every announcement made since launch.
    private func assertAnnouncement(containing phrase: String, file: StaticString = #filePath, line: UInt = #line) {
        let log = app.element(identifier: Self.announcementLogIdentifier)
        let predicate = NSPredicate(format: "label CONTAINS %@", phrase)
        let matched = XCTNSPredicateExpectation(predicate: predicate, object: log)
        let result = XCTWaiter().wait(for: [matched], timeout: screenTimeout)
        XCTAssertEqual(
            result, .completed, "no announcement containing '\(phrase)' (log: \(log.exists ? log.label : "no log element"))",
            file: file, line: line)
    }

    // MARK: Driving the app

    /// Launches with the walkthrough's fixture launch argument (`XCUIApplication.fixtureLaunchArgument`) and waits, as
    /// `launch(fixture:)` and `waitForFixtureReady()` do, for the composer and then for the fixture's `ready` marker, but
    /// rides out snapshot stalls (`pollToleratingSnapshotStalls`).
    private func launch(_ fixture: OhAndFixture) -> Bool {
        app.launchArguments += [XCUIApplication.fixtureLaunchArgument, fixture.rawValue]
        app.launch()
        let composer = app.element(identifier: "home.composer.field")
        var reachedHome = pollToleratingSnapshotStalls("waiting for the composer", timeout: screenTimeout) { composer.exists }
        // On the hosted runner a launch can hang until XCTest loses the app ("Lost connection to the application", run
        // 41720210504). It is launched once more, and the log says so; a second launch that does not reach home fails.
        if !reachedHome {
            auditLines.append("RELAUNCH \(fixture.rawValue): the first launch did not reach home within \(Int(screenTimeout)) s")
            app.terminate()
            app.launch()
            reachedHome = pollToleratingSnapshotStalls("waiting for the composer after a relaunch", timeout: screenTimeout) { composer.exists }
        }
        XCTAssertTrue(reachedHome, "the \(fixture.rawValue) fixture did not reach home")
        guard reachedHome else { return false }
        if fixture == .emptyFirstRun { return true }
        let marker = app.element(identifier: Self.fixtureStatusIdentifier)
        var status = "no marker"
        let finished = pollToleratingSnapshotStalls("waiting for the fixture", timeout: 150) {
            status = marker.exists ? marker.label : "no marker"
            return status == "ready" || status.hasPrefix("failed")
        }
        XCTAssertTrue(finished && status == "ready", "the \(fixture.rawValue) fixture was not ready (status: \(status))")
        return finished && status == "ready"
    }

    /// Polls `condition` once a second until it holds or `timeout` passes. On the hosted runner the app's main thread can
    /// be busy for half a minute after launch, and XCTest then records "Failed to get matching snapshots: Timed out while
    /// evaluating UI query" for a query that would have answered a moment later, or, when the launch hangs, "Lost connection
    /// to the application" (then `launch` relaunches once). Here, and only here, those two issues are expected failures and
    /// the poll goes on; any other issue counts, and a condition that never holds fails the caller.
    private func pollToleratingSnapshotStalls(_ activity: String, timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        let options = XCTExpectedFailure.Options()
        options.isStrict = false
        options.issueMatcher = { issue in
            let text = issue.compactDescription + " " + (issue.detailedDescription ?? "")
            return text.contains("Timed out while evaluating UI query") || text.contains("Lost connection to the application")
        }
        repeat {
            var holds = false
            XCTExpectFailure("a snapshot stall while \(activity)", options: options) { holds = condition() }
            if holds { return true }
            Thread.sleep(forTimeInterval: 1)
        } while Date() < deadline
        return false
    }

    private func typeCapture(_ text: String) -> Bool {
        let field = app.element(identifier: "home.composer.field")
        guard tapWhenReady(field, "the composer field") else { return false }
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: screenTimeout), "the keyboard did not appear")
        field.typeText(text)
        let save = app.buttons["home.composer.save"]
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND isEnabled == true"), object: save)
        let typed = XCTWaiter().wait(for: [enabled], timeout: 30) == .completed
        XCTAssertTrue(typed, "Save did not appear after typing")
        return typed
    }

    private func openSearch() -> Bool {
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.search"], "Search") else { return false }
        let query = app.textFields["retrieval.search.query"].firstMatch
        let opened = query.waitForExistence(timeout: screenTimeout)
        XCTAssertTrue(opened, "Search did not open")
        return opened
    }

    private func search(_ text: String) -> Bool {
        let query = app.textFields["retrieval.search.query"].firstMatch
        guard tapWhenReady(query, "the query field") else { return false }
        query.typeText(text)
        let key = app.keyboards.buttons["Search"]
        guard tapWhenReady(key, "the keyboard Search key") else { return false }
        return true
    }

    private func openSettings() -> Bool {
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.settings"], "Settings") else { return false }
        let header = app.staticTexts["Settings"]
        let opened = header.waitForExistence(timeout: screenTimeout)
        XCTAssertTrue(opened, "Settings did not open")
        return opened
    }

    /// A row reads its label, then its current value when one is known ("Processing, On this phone").
    private func openSettingsRow(_ label: String) -> Bool {
        let row = app.buttons.matching(NSPredicate(format: "label == %@ OR label BEGINSWITH %@", label, "\(label), ")).firstMatch
        guard row.waitForExistence(timeout: screenTimeout) else {
            XCTFail("settings row \(label) not found")
            return false
        }
        guard scrollUntilHittable(row), tapWhenReady(row, "settings row \(label)") else { return false }
        // The row is gone once the screen has opened (the Settings header leaves with it).
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.staticTexts["Settings"])
        _ = XCTWaiter().wait(for: [gone], timeout: 20)
        Thread.sleep(forTimeInterval: 1)
        return true
    }

    private func goBack() -> Bool {
        let glassBack = app.buttons["Back"].firstMatch
        let barBack = app.navigationBars.buttons.element(boundBy: 0)
        let deadline = Date().addingTimeInterval(screenTimeout)
        repeat {
            if glassBack.exists, glassBack.isHittable {
                glassBack.tap()
                Thread.sleep(forTimeInterval: 1)
                return true
            }
            if barBack.exists, barBack.isHittable {
                barBack.tap()
                Thread.sleep(forTimeInterval: 1)
                return true
            }
            // The back button sits at the top of the scrolling screen; bring it back into view.
            if glassBack.exists, !glassBack.isHittable { app.swipeDown() }
            Thread.sleep(forTimeInterval: 0.5)
        } while Date() < deadline
        let buttons = app.buttons.allElementsBoundByIndex.map { "\($0.label)|\($0.identifier)|hittable=\($0.isHittable)|\($0.frame)" }
        XCTFail("no back button was hittable; buttons: \(buttons.joined(separator: "; "))")
        return false
    }

    private func dismissKeyboardIfShown() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where button.waitForExistence(timeout: 1) {
            button.tap()
        }
        if app.keyboards.firstMatch.waitForExistence(timeout: 3) {
            let done = app.buttons["home.done"]
            if done.waitForExistence(timeout: 10) { done.tap() }
        }
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 0"), object: app.keyboards)
        _ = XCTWaiter().wait(for: [gone], timeout: 30)
    }

    /// Waits for `element` to exist and be hittable, then taps it.
    private func tapWhenReady(_ element: XCUIElement, _ name: String) -> Bool {
        guard element.waitForExistence(timeout: screenTimeout) else {
            XCTFail("\(name) did not appear")
            return false
        }
        let hittable = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isHittable == true"), object: element)
        guard XCTWaiter().wait(for: [hittable], timeout: screenTimeout) == .completed else {
            XCTFail("\(name) never became hittable")
            return false
        }
        // A screen still sliding in answers "hittable" and has no activation point a moment later.
        Thread.sleep(forTimeInterval: 0.5)
        element.tap()
        return true
    }

    /// Swipes the screen up until `element` can take a tap, up to a few screens.
    @discardableResult
    private func scrollUntilHittable(_ element: XCUIElement) -> Bool {
        guard element.waitForExistence(timeout: screenTimeout) else { return false }
        var swipesLeft = 8
        while !element.isHittable, swipesLeft > 0 {
            app.swipeUp()
            swipesLeft -= 1
        }
        return element.isHittable
    }

    private func capture(_ screen: String) {
        screenshotIndex += 1
        let name = String(format: "%@-%@-%02d.png", sizeName, screen, screenshotIndex)
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        do {
            try screenshot.pngRepresentation.write(to: outputDirectory.appendingPathComponent(name))
        } catch {
            XCTFail("could not write \(name): \(error)")
        }
    }
}
