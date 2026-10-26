import XCTest
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
        XCTAssertThrowsError(try UITestFixtureServices.make(named: "no-such-fixture"))
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
