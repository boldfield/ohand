import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Behavior of `ExportService` against a fake core, real protected-storage files and a real read session. The
/// production client over the real Rust core is covered by `ExportCoreIntegrationTests`.
final class ExportServiceTests: XCTestCase {
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

    private func failure(of operation: () async throws -> PreparedExport) async -> ExportFailure? {
        do {
            _ = try await operation()
            return nil
        } catch {
            return error as? ExportFailure
        }
    }

    // MARK: Authorization and scope

    func testCaptureOnlySessionReadsAndCreatesNothing() async throws {
        let world = try makeWorld(authenticated: false)
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        XCTAssertEqual(result, .notAuthenticated)
        XCTAssertEqual(world.core.requests, [], "the core is never asked")
        XCTAssertFalse(FileManager.default.fileExists(atPath: world.exportsDirectory.path))
    }

    func testExportWithoutAScopeIsRefusedBeforeTheCore() async throws {
        let world = try makeWorld()
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [])) }

        XCTAssertEqual(result, .noScopeSelected)
        XCTAssertEqual(world.core.requests, [])
    }

    func testRelockedSessionCannotStartAnExport() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        world.session.relock(.explicit)

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.work])) }

        XCTAssertEqual(result, .notAuthenticated)
        XCTAssertEqual(world.core.requests, [])
    }

    // MARK: Audio inclusion

    func testExportWithoutAudioRequestsExcludedAndCopiesNothing() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap-1.m4a")
        world.core.audioReferences = [ExportWorld.reference(item: "item-1", capture: "cap-1")]
        let service = world.makeService(identifiers: ["export-1"])

        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal, .work]))

        let request = try XCTUnwrap(world.core.requests.first)
        XCTAssertEqual(request.readAuthentication, .authenticated)
        XCTAssertEqual(request.scopes, [.personal, .work])
        XCTAssertEqual(request.audio, .excluded, "audio is opt-in")
        XCTAssertEqual(request.outputPath, world.exportsDirectory.appendingPathComponent("export-1/ohand-export.jsonl").path)
        XCTAssertEqual(prepared.shareItems, [prepared.exportFile])
        XCTAssertEqual(prepared.audio, ExportAudioReport(requested: false, includedCaptureIDs: [], unavailable: []))
        XCTAssertEqual(prepared.itemCount, 2)
        XCTAssertTrue(FileManager.default.fileExists(atPath: prepared.exportFile.path))
        XCTAssertEqual(world.exportRemnants(), ["Temporary/Exports/export-1", "Temporary/Exports/export-1/ohand-export.jsonl"])
    }

    func testRequestedAudioIsCopiedAndTheSourceIsLeftAlone() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap-1.m4a", contents: "audio one")
        world.core.audioReferences = [ExportWorld.reference(item: "item-1", capture: "cap-1")]
        let service = world.makeService()

        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal], includeAudio: true))

        XCTAssertEqual(world.core.requests.first?.audio, .references)
        XCTAssertEqual(prepared.audioFiles.count, 1)
        XCTAssertEqual(prepared.shareItems, [prepared.exportFile] + prepared.audioFiles)
        XCTAssertEqual(try String(contentsOf: try XCTUnwrap(prepared.audioFiles.first)), "audio one")
        XCTAssertEqual(prepared.audio, ExportAudioReport(requested: true, includedCaptureIDs: ["cap-1"], unavailable: []))
        XCTAssertTrue(world.finalizedAudioExists("cap-1.m4a"), "the source audio is only read")
    }

    func testUnavailableAudioIsReportedAndNeverClaimed() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap-1.m4a")
        try world.writeFinalizedAudio("cap-4.m4a")
        try world.writeFinalizedAudio("other.m4a")
        world.fileSystem.failingCopyNames = ["cap-4.m4a"]
        world.core.audioReferences = [
            ExportWorld.reference(item: "item-1", capture: "cap-1"),
            ExportWorld.reference(item: "item-2", capture: "cap-2"),
            ExportWorld.reference(item: "item-3", capture: "cap-3", file: "other.m4a"),
            ExportWorld.reference(item: "item-4", capture: "cap-4"),
            CoreExportAudioReference(itemID: "item-5", captureID: "cap-5", reference: "FinalizedAudio/../cap-5.m4a"),
            CoreExportAudioReference(itemID: "item-6", captureID: "cap-6", reference: "Elsewhere/cap-6.m4a"),
        ]
        let service = world.makeService()

        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal], includeAudio: true))

        XCTAssertEqual(prepared.audio.includedCaptureIDs, ["cap-1"])
        XCTAssertEqual(prepared.audioFiles.map { $0.lastPathComponent }, ["cap-1.m4a"])
        XCTAssertEqual(
            prepared.audio.unavailable,
            [
                ExportUnavailableAudio(itemID: "item-2", captureID: "cap-2", reason: .fileMissing),
                ExportUnavailableAudio(itemID: "item-3", captureID: "cap-3", reason: .referenceUnresolvable),
                ExportUnavailableAudio(itemID: "item-4", captureID: "cap-4", reason: .copyFailed),
                ExportUnavailableAudio(itemID: "item-5", captureID: "cap-5", reason: .referenceUnresolvable),
                ExportUnavailableAudio(itemID: "item-6", captureID: "cap-6", reason: .referenceUnresolvable),
            ])
        let copied = try FileManager.default.contentsOfDirectory(
            atPath: prepared.exportFile.deletingLastPathComponent().appendingPathComponent("audio").path)
        XCTAssertEqual(copied, ["cap-1.m4a"], "another capture's audio and a partial copy are not shared")
        XCTAssertTrue(world.finalizedAudioExists("cap-4.m4a"))
        XCTAssertTrue(world.finalizedAudioExists("other.m4a"))
    }

    // MARK: Share lifecycle

    func testCompletedShareRemovesTemporariesAndKeepsSources() async throws {
        let world = try makeWorld()
        try world.writeFinalizedAudio("cap-1.m4a")
        world.core.audioReferences = [ExportWorld.reference(item: "item-1", capture: "cap-1")]
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal], includeAudio: true))
        XCTAssertFalse(world.exportRemnants().isEmpty)

        let outcome = service.finishShare(exportID: prepared.id, ending: .completed)

        XCTAssertEqual(outcome, ExportShareOutcome(result: .shared, temporariesRemoved: true))
        XCTAssertEqual(world.exportRemnants(), [])
        XCTAssertTrue(world.finalizedAudioExists("cap-1.m4a"), "sharing never deletes the source")
    }

    func testCancelledAndFailedSharesRemoveTemporariesWithoutReportingSuccess() async throws {
        let world = try makeWorld()
        let service = world.makeService()

        let cancelled = try await service.prepareExport(ExportSelection(scopes: [.personal]))
        XCTAssertEqual(service.finishShare(exportID: cancelled.id, ending: .cancelled).result, .cancelled)
        XCTAssertEqual(world.exportRemnants(), [])

        let failed = try await service.prepareExport(ExportSelection(scopes: [.personal]))
        XCTAssertEqual(service.finishShare(exportID: failed.id, ending: .failed).result, .failed)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testDismissingBeforeSharingRemovesTemporaries() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))

        let outcome = service.cancelExport(exportID: prepared.id)

        XCTAssertEqual(outcome, ExportShareOutcome(result: .cancelled, temporariesRemoved: true))
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testRelockWhileSharingRemovesTemporariesAndNeverReportsSuccess() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))

        world.session.relock(.enteredBackground)

        XCTAssertEqual(world.exportRemnants(), [], "nothing readable remains once the session relocks")
        let outcome = service.finishShare(exportID: prepared.id, ending: .completed)
        XCTAssertEqual(outcome, ExportShareOutcome(result: .interrupted, temporariesRemoved: true))
    }

    func testReauthenticationDoesNotRevalidateAnInvalidatedExport() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))
        world.session.relock(.explicit)
        world.authenticate()

        XCTAssertEqual(service.finishShare(exportID: prepared.id, ending: .completed).result, .interrupted)
    }

    func testAnUnknownExportIsNeverReportedShared() async throws {
        let world = try makeWorld()
        let service = world.makeService()

        let outcome = service.finishShare(exportID: "never-prepared", ending: .completed)

        XCTAssertEqual(outcome.result, .interrupted)
    }

    func testRelockWhilePreparingRemovesEverythingAndFails() async throws {
        for createsParentDirectory in [false, true] {
            let world = try makeWorld()
            let gate = ExportGate()
            world.core.gate = gate
            world.core.createsParentDirectory = createsParentDirectory
            let service = world.makeService()
            let core = world.core

            let task = Task { try await service.prepareExport(ExportSelection(scopes: [.personal])) }
            let arrived = await waitUntil { core.requests.count == 1 }
            XCTAssertTrue(arrived)
            world.session.relock(.enteredBackground)
            gate.open()

            let outcome = await task.result
            XCTAssertEqual(outcome.failureValue as? ExportFailure, .relocked)
            XCTAssertEqual(world.exportRemnants(), [], "createsParentDirectory=\(createsParentDirectory)")
        }
    }

    func testCancellingWhilePreparingRemovesEverything() async throws {
        let world = try makeWorld()
        let gate = ExportGate()
        world.core.gate = gate
        let service = world.makeService()
        let core = world.core

        let task = Task { try await service.prepareExport(ExportSelection(scopes: [.personal])) }
        let arrived = await waitUntil { core.requests.count == 1 }
        XCTAssertTrue(arrived)
        task.cancel()
        gate.open()

        let outcome = await task.result
        XCTAssertEqual(outcome.failureValue as? ExportFailure, .cancelled)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    // MARK: Failures

    func testCoreFailureLeavesNothingAndIsReportedNormalized() async throws {
        let world = try makeWorld()
        let refusal = CoreFailure(
            errorClass: .unauthorized, code: "export_not_authorized", message: "export requires an authenticated read session")
        world.core.failure = refusal
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        XCTAssertEqual(result, .core(refusal))
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testUnexpectedCoreErrorLeavesNothing() async throws {
        let world = try makeWorld()
        world.core.failure = FaultingExportFileSystem.Injected()
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        guard case .core(let normalized)? = result else { return XCTFail("expected a normalized core failure") }
        XCTAssertEqual(normalized.code, "export_failed")
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testUnpreparableStorageNeverReachesTheCore() async throws {
        let world = try makeWorld()
        world.fileSystem.failsCreateDirectory = true
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        XCTAssertEqual(result, .storageUnavailable)
        XCTAssertEqual(world.core.requests, [])
    }

    func testAnExportFileThatCannotBeProtectedIsNotShared() async throws {
        let world = try makeWorld()
        world.fileSystem.failsProtectFile = true
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        XCTAssertEqual(result, .storageUnavailable)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testAnAnswerWithoutAFileIsNotShared() async throws {
        let world = try makeWorld()
        world.core.writesExportFile = false
        let service = world.makeService()

        let result = await failure { try await service.prepareExport(ExportSelection(scopes: [.personal])) }

        XCTAssertEqual(result, .storageUnavailable)
        XCTAssertEqual(world.exportRemnants(), [])
    }

    // MARK: Next launch

    func testFailedRemovalIsReportedAndTheNextLaunchSweepClearsIt() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))
        world.fileSystem.failsRemove = true

        let outcome = service.finishShare(exportID: prepared.id, ending: .completed)

        XCTAssertEqual(outcome, ExportShareOutcome(result: .shared, temporariesRemoved: false))
        XCTAssertFalse(world.exportRemnants().isEmpty)

        world.fileSystem.failsRemove = false
        let nextLaunch = world.makeService()
        let report = nextLaunch.sweepAtLaunch()

        XCTAssertEqual(report, ExportSweepReport(removedCount: 1, failedCount: 0))
        XCTAssertEqual(world.exportRemnants(), [])
    }

    func testSweepRemovesInterruptedExportsAndNothingElse() throws {
        let world = try makeWorld()
        let temporary = world.layout.directory(for: .temporaryFiles)
        for name in ["old-1", "old-2"] {
            let directory = world.exportsDirectory.appendingPathComponent(name + "/audio", isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try Data("leftover".utf8).write(to: directory.appendingPathComponent("cap-1.m4a"))
        }
        try Data("other temporary".utf8).write(to: temporary.appendingPathComponent("other.tmp"))
        try world.writeFinalizedAudio("cap-1.m4a")

        let report = world.makeService().sweepAtLaunch()

        XCTAssertEqual(report, ExportSweepReport(removedCount: 2, failedCount: 0))
        XCTAssertEqual(world.exportDirectoryNames(), [])
        XCTAssertTrue(FileManager.default.fileExists(atPath: temporary.appendingPathComponent("other.tmp").path))
        XCTAssertTrue(world.finalizedAudioExists("cap-1.m4a"))
    }

    func testSweepWithoutAnExportsDirectoryDoesNothing() throws {
        let world = try makeWorld()

        XCTAssertEqual(world.makeService().sweepAtLaunch(), ExportSweepReport(removedCount: 0, failedCount: 0))
    }

    func testSweepLeavesExportsActiveInThisProcess() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))

        XCTAssertEqual(service.sweepAtLaunch(), ExportSweepReport(removedCount: 0, failedCount: 0))
        XCTAssertTrue(FileManager.default.fileExists(atPath: prepared.exportFile.path))
    }

    // MARK: Share sheet

    func testShareSheetEndingsMapToServiceEndings() {
        XCTAssertEqual(ExportShareSheet.ending(completed: true, error: nil), .completed)
        XCTAssertEqual(ExportShareSheet.ending(completed: false, error: nil), .cancelled)
        XCTAssertEqual(ExportShareSheet.ending(completed: true, error: ExportFailure.relocked), .failed)
        XCTAssertEqual(ExportShareSheet.ending(completed: false, error: ExportFailure.relocked), .failed)
    }

    func testShareSheetReportsItsEndingThroughTheCompletionHandler() async throws {
        let world = try makeWorld()
        let service = world.makeService()
        let prepared = try await service.prepareExport(ExportSelection(scopes: [.personal]))
        var endings: [ExportShareEnding] = []

        let controller = ExportShareSheet.makeController(for: prepared) { endings.append($0) }
        let handler = try XCTUnwrap(controller.completionWithItemsHandler)
        handler(nil, true, nil, nil)
        handler(nil, false, nil, nil)

        XCTAssertEqual(endings, [.completed, .cancelled])
    }
}

private extension Result {
    var failureValue: Error? {
        if case .failure(let error) = self { return error }
        return nil
    }
}
