import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// The export section's model over the real `ExportService`, real protected-storage files and a real read session; only
/// the core is a fake. The share sheet's ending is delivered the way the sheet delivers it.
@MainActor
final class ExportSettingsModelTests: XCTestCase {
    private var worlds: [ExportWorld] = []

    override func tearDown() {
        worlds.forEach { $0.tearDown() }
        worlds = []
        super.tearDown()
    }

    private func makeWorld(authenticated: Bool = true) throws -> ExportWorld {
        let world = try ExportWorld()
        worlds.append(world)
        if authenticated { world.authenticate() }
        return world
    }

    private func makeModel(_ world: ExportWorld) -> ExportSettingsModel {
        let session = world.session
        return ExportSettingsModel(controls: world.makeService(), readScope: { session.scope == .authenticated ? .authenticated : .captureOnly })
    }

    func testNoScopeAndNoAudioByDefaultAndExportCannotStart() async throws {
        let world = try makeWorld()
        let model = makeModel(world)

        XCTAssertTrue(model.selectedScopes.isEmpty)
        XCTAssertFalse(model.includeAudio)
        XCTAssertFalse(model.canStart)
        await model.startExport()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertEqual(world.core.requests, [])
    }

    func testCompletedShareReportsSharedAndRemovesTemporaries() async throws {
        let world = try makeWorld()
        let model = makeModel(world)
        model.setScope(.work, selected: true)

        await model.startExport()

        XCTAssertEqual(model.phase, .sharing)
        XCTAssertEqual(world.core.requests.first?.scopes, [.work])
        XCTAssertEqual(world.core.requests.first?.audio, .excluded)
        XCTAssertNotNil(model.pendingShare)
        XCTAssertFalse(world.exportRemnants().isEmpty)

        model.shareEnded(.completed)

        guard case .finished(.shared(let text)) = model.phase else { return XCTFail("expected shared, got \(model.phase)") }
        XCTAssertTrue(text.contains("cannot recall"))
        XCTAssertFalse(text.lowercased().contains("backup complete"))
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testCancelledShareIsNotSuccessAndCleansProtectedTemporaries() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap1.m4a")
        world.core.audioReferences = [ExportWorld.reference(item: "i1", capture: "cap1")]
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        model.includeAudio = true

        await model.startExport()
        XCTAssertFalse(world.exportRemnants().isEmpty)
        model.shareEnded(.cancelled)

        guard case .finished(.cancelled(let text)) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("Nothing was sent"))
        XCTAssertEqual(world.exportRemnants(), [], "export file and copied audio are gone")
        XCTAssertTrue(world.finalizedAudioExists("cap1.m4a"), "the source audio is untouched")
    }

    func testFailedShareReportsFailureAndCleansUp() async throws {
        let world = try makeWorld()
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        await model.startExport()

        model.shareEnded(.failed)

        guard case .finished(.failed) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testCoreFailureIsReportedWithoutLeavingFilesOrDetail() async throws {
        let world = try makeWorld()
        world.core.failure = CoreFailure(errorClass: .permanent, code: "secret-code", message: "secret-detail")
        let model = makeModel(world)
        model.setScope(.personal, selected: true)

        await model.startExport()

        guard case .finished(.failed(let text)) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertFalse(text.contains("secret"))
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testAudioSummaryUsesCountsOnlyAndNamesNoRecord() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap1.m4a")
        world.core.audioReferences = [
            ExportWorld.reference(item: "item-secret", capture: "cap1"),
            ExportWorld.reference(item: "item-gone", capture: "cap2"),
        ]
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        model.includeAudio = true
        await model.startExport()
        model.shareEnded(.completed)

        guard case .finished(.shared(let text)) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("1 recording was included."))
        XCTAssertTrue(text.contains("1 recording could not be included"))
        XCTAssertFalse(text.contains("item-"))
        XCTAssertFalse(text.contains("cap1"))
    }

    func testUnremovableTemporariesAreSaidNotHidden() async throws {
        let world = try makeWorld()
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        await model.startExport()
        world.fileSystem.failsRemove = true

        model.shareEnded(.cancelled)

        guard case .finished(.cancelled(let text)) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("could not be removed"))
        world.fileSystem.failsRemove = false
    }

    func testRelockWhileSharingIsInterruptedNotShared() async throws {
        let world = try makeWorld()
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        await model.startExport()

        world.session.relock(.explicit)
        model.shareEnded(.completed)

        XCTAssertEqual(model.phase, .locked)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testLeavingTheScreenWhileSharingRemovesTheExport() async throws {
        let world = try makeWorld()
        let model = makeModel(world)
        model.setScope(.personal, selected: true)
        await model.startExport()
        XCTAssertFalse(world.exportRemnants().isEmpty)

        model.clear()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testLeavingDuringPreparationDiscardsTheLateExport() async throws {
        let world = try makeWorld()
        let gate = ExportGate()
        world.core.gate = gate
        let model = makeModel(world)
        model.setScope(.personal, selected: true)

        let started = Task { await model.startExport() }
        let arrived = await waitUntil { !world.core.requests.isEmpty }
        XCTAssertTrue(arrived)
        model.clear()
        gate.open()
        await started.value

        XCTAssertEqual(model.phase, .idle)
        XCTAssertNil(model.pendingShare)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testCaptureOnlySessionShowsLockedAndReadsNothing() async throws {
        let world = try makeWorld(authenticated: false)
        let model = makeModel(world)
        model.setScope(.personal, selected: true)

        await model.startExport()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertEqual(world.core.requests, [])
    }

    func testExplanationNeverPromisesBackupOrRestore() {
        let text = ExportSettingsText.limitsNote
        XCTAssertTrue(text.contains("not a backup"))
        XCTAssertTrue(text.contains("cannot restore"))
        XCTAssertTrue(text.contains("cannot take back"))
    }
}
