import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// The fixture launch mode is debug-only and isolated from production state. This suite runs in a Debug build, so it
/// cannot observe a release binary; it proves instead that every source line that reads the launch argument or builds the
/// substitute adapters sits inside `#if DEBUG`, which is how the existing `injectedForTests` factory is kept out of release.
final class UITestFixtureTests: XCTestCase {
    private var iosDirectory: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    }

    // MARK: launch argument

    func testTheFixtureNameFollowsTheLaunchArgument() {
        XCTAssertEqual(
            UITestFixture.requestedName(arguments: ["OhAnd", "-OhAndUITestFixture", "populated"]), "populated")
    }

    func testWithoutTheLaunchArgumentThereIsNoFixture() {
        XCTAssertNil(UITestFixture.requestedName(arguments: ["OhAnd"]))
        XCTAssertNil(UITestFixture.requestedName(arguments: ["OhAnd", "-OhAndUITestFixture"]))
    }

    func testEveryNamedFixtureIsListed() {
        XCTAssertEqual(
            Set(UITestFixture.allCases.map(\.rawValue)),
            ["empty-first-run", "locked", "populated", "needs-correction", "failed-sorting", "offline"])
    }

    func testAnUnknownFixtureNameIsRefused() {
        XCTAssertThrowsError(try UITestFixtureLaunch(name: "no-such-fixture").makeServices())
    }

    // MARK: isolation

    func testAFixtureLaunchUsesItsOwnCredentialServiceAndNeverTheProductionStore() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("ui-fixture-config-\(UUID().uuidString)")
        let first = AppConfiguration.uiTestFixture(storageRoot: root)
        let second = AppConfiguration.uiTestFixture(storageRoot: root)
        XCTAssertNotEqual(first.credentialServiceName, CredentialService.defaultServiceName)
        XCTAssertNotEqual(first.credentialServiceName, second.credentialServiceName, "each launch has its own service")
        let production = try AppConfiguration.productionStorageRoot()
        XCTAssertFalse(first.storageRootPath.hasPrefix(production.path))
    }

    func testTheFixtureReadBoundaryUnlocksOnlyOnTheExplicitActionWhenLocked() {
        let locked = UITestFixtureReadBoundary(startsUnlocked: false)
        var quiet: ReadAuthenticationOutcome?
        var explicit: ReadAuthenticationOutcome?
        locked.authenticateQuietly(reason: "test") { quiet = $0 }
        locked.authenticate(reason: "test") { explicit = $0 }
        XCTAssertEqual(quiet, .unavailable)
        XCTAssertEqual(explicit, .succeeded)

        var unlockedQuiet: ReadAuthenticationOutcome?
        UITestFixtureReadBoundary(startsUnlocked: true).authenticateQuietly(reason: "test") { unlockedQuiet = $0 }
        XCTAssertEqual(unlockedQuiet, .succeeded)
    }

    // MARK: each fixture reaches the state it is named for

    private final class SeedingResult: @unchecked Sendable {
        var value: Result<[UITestFixtureSeededNote], Error>?
    }

    /// Builds a fixture the way the app does, fills it, and returns what the core reported for each note. The services
    /// are built on the main thread, which provisioning needs to be turning, so the seeding runs while the test waits.
    private func seededFixture(_ name: String) throws -> (launch: UITestFixtureLaunch, notes: [UITestFixtureSeededNote]) {
        let launch = UITestFixtureLaunch(name: name)
        let services = try launch.makeServices()
        services.start(isActive: true)
        defer { services.shutDown() }
        let outcome = SeedingResult()
        let finished = expectation(description: "the fixture is seeded")
        Task { @MainActor in
            do {
                outcome.value = .success(try await launch.seedAndWait().notes)
            } catch {
                outcome.value = .failure(error)
            }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 180)
        return (launch, try XCTUnwrap(outcome.value).get())
    }

    func testFailedSortingEndsUninterpretedWithARejectedJobNotAConfigurationWait() throws {
        let (_, notes) = try seededFixture("failed-sorting")
        let note = try XCTUnwrap(notes.first)
        let status = try XCTUnwrap(note.settledStatus)
        XCTAssertEqual(note.expectation, .rejected)
        XCTAssertEqual(status.processingState, "uninterpreted", "a rejection is permanent; an unauthorized one would wait as unprocessed")
    }

    func testOfflineLeavesTheNotesWaitingWithoutAFailure() throws {
        let (_, notes) = try seededFixture("offline")
        XCTAssertEqual(notes.count, 2)
        for note in notes {
            XCTAssertEqual(try XCTUnwrap(note.settledStatus).processingState, "unprocessed")
        }
    }

    func testNeedsCorrectionHoldsAReminderThatIsNotScheduled() throws {
        let (launch, notes) = try seededFixture("needs-correction")
        let status = try XCTUnwrap(try XCTUnwrap(notes.first).settledStatus)
        XCTAssertEqual(status.processingState, "processed")
        XCTAssertEqual(status.reminderRequestState, "not_scheduled_yet")
        XCTAssertNotEqual(status.reminderScheduleState, "scheduled")
        XCTAssertEqual(launch.notificationCenter?.pendingDueInstants.count, 0)
    }

    func testPopulatedHoldsTypedAndSpokenNotesOfEachTypeAndOneResolvedReminder() throws {
        let (launch, notes) = try seededFixture("populated")
        XCTAssertEqual(notes.count, 7)
        XCTAssertEqual(notes.filter { $0.text.contains("typed") }.count, 3)
        XCTAssertEqual(notes.filter { $0.text.contains("spoken") }.count, 3)
        for note in notes {
            XCTAssertEqual(try XCTUnwrap(note.settledStatus).processingState, "processed", note.text)
        }
        let reminder = try XCTUnwrap(notes.first { $0.text.contains("call mom") }?.settledStatus)
        XCTAssertEqual(
            reminder.reminderRequestState, "resolved",
            "status \(reminder); reminders \(launch.diagnosticScalar("SELECT COUNT(*) FROM reminders")); "
                + "item type \(launch.diagnosticScalar("SELECT item_type FROM items WHERE item_id = '\(reminder.itemID)'")); "
                + "tables \(launch.diagnosticScalar("SELECT group_concat(name) FROM sqlite_master WHERE type = 'table'"))")
        let voiceStatuses = notes.filter { $0.text.contains("spoken") }.compactMap(\.settledStatus)
        XCTAssertTrue(voiceStatuses.allSatisfy { $0.transcriptionState == "transcribed" })
    }

    func testLockedSeedsTheStoreAndEmptyFirstRunLeavesPermissionUnasked() throws {
        let (_, lockedNotes) = try seededFixture("locked")
        XCTAssertEqual(lockedNotes.count, 7)
        XCTAssertTrue(lockedNotes.allSatisfy { $0.settledStatus != nil })

        let (emptyLaunch, emptyNotes) = try seededFixture("empty-first-run")
        XCTAssertTrue(emptyNotes.isEmpty)
        XCTAssertEqual(emptyLaunch.notificationCenter?.chosenPermission, .notDetermined)
    }

    // MARK: release builds have no path

    func testEveryFixtureFileIsEntirelyInsideADebugConditional() throws {
        let directory = iosDirectory.appendingPathComponent("AppAssembly/UITestFixtures")
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        XCTAssertFalse(files.isEmpty)
        for file in files {
            let lines = try String(contentsOf: file).split(separator: "\n", omittingEmptySubsequences: false)
                .map { $0.trimmingCharacters(in: .whitespaces) }
            let content = lines.filter { !$0.isEmpty }
            XCTAssertEqual(content.first, "#if DEBUG", "\(file.lastPathComponent) opens with #if DEBUG")
            XCTAssertEqual(content.last, "#endif", "\(file.lastPathComponent) closes with #endif")
            XCTAssertEqual(
                lines.filter { $0.hasPrefix("#if") }.count, 1, "\(file.lastPathComponent) has one conditional that spans it")
        }
    }

    func testOutsideTheFixtureFolderEveryReferenceIsInsideADebugConditional() throws {
        let names = ["UITestFixture", "uiTestFixture", "OhAndUITestFixture"]
        let files = [
            "AppAssembly/AppDelegate.swift", "AppAssembly/AppConfiguration.swift", "Services/Assembly/CompositionRoot.swift",
        ]
        for relativePath in files {
            let text = try String(contentsOf: iosDirectory.appendingPathComponent(relativePath))
            var conditionals: [Bool] = []  // true when the enclosing conditional is `#if DEBUG`
            var references = 0
            for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if trimmed.hasPrefix("//") || trimmed.hasPrefix("///") { continue }
                if trimmed.hasPrefix("#if") {
                    conditionals.append(trimmed == "#if DEBUG")
                    continue
                }
                if trimmed.hasPrefix("#else") || trimmed.hasPrefix("#elseif") {
                    XCTFail("\(relativePath) uses #else, which this check cannot follow")
                    continue
                }
                if trimmed.hasPrefix("#endif") {
                    conditionals.removeLast()
                    continue
                }
                if names.contains(where: { trimmed.contains($0) }) {
                    references += 1
                    XCTAssertTrue(conditionals.contains(true), "\(relativePath): '\(trimmed)' is outside #if DEBUG")
                }
            }
            XCTAssertGreaterThan(references, 0, "\(relativePath) is expected to reference the fixture hook")
            XCTAssertTrue(conditionals.isEmpty, "\(relativePath) has balanced conditionals")
        }
    }
}
