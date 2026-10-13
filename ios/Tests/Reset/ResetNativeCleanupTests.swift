import XCTest
@testable import OhAndServices

/// The production native pieces of reset over real protected-storage files: the ingress staging drain and purge, the
/// export temporaries, and the durable markers.
final class ResetNativeCleanupTests: IngressStorageTestCase {
    private var stagingStore: IngressStagingStore {
        IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
    }

    private func makeIngress(
        importer: ForegroundIngressImporting,
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem()
    ) throws -> (NativeResetIngress, ForegroundIngressService) {
        let service = try makeService(importer: importer)
        return (NativeResetIngress(service: service, layout: layout, fileSystem: fileSystem), service)
    }

    private func filesMentioning(_ captureID: String) -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: layout.rootDirectory.path)) ?? []
        return subpaths.filter { ($0 as NSString).lastPathComponent.contains(captureID) }.sorted()
    }

    // MARK: Ingress staging

    func testSnapshotNamesStagedRecordsAndInterruptedWrites() throws {
        let (ingress, service) = try makeIngress(importer: FakeForegroundIngressImporter())
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        try Data("{\"text\":\"buy oat".utf8).write(to: recordURL("cap-b").appendingPathExtension("incomplete"))

        XCTAssertEqual(try ingress.snapshot().captureIDs, ["cap-a", "cap-b"])
    }

    func testPurgeRemovesTheSnapshotRecordsAndTheirAudioButNotInputsStagedAfterIt() async throws {
        let (ingress, service) = try makeIngress(importer: FakeForegroundIngressImporter())
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(try audioRecord("cap-a"))
        _ = try stagingStore.stage(textRecord("cap-b"))
        let snapshot = try ingress.snapshot()
        _ = try stagingStore.stage(textRecord("cap-late"))

        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(purge, .purged(count: 2))
        XCTAssertEqual(filesMentioning("cap-a"), [])
        XCTAssertEqual(filesMentioning("cap-b"), [])
        XCTAssertTrue(exists(recordURL("cap-late")), "a capture staged after the tap is fresh-generation content")
    }

    func testImportedInputsLeaveNothingToPurgeAndUnimportedOnesAreRemoved() async throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a")]
        let (ingress, service) = try makeIngress(importer: importer)
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        _ = try stagingStore.stage(textRecord("cap-b"))
        let snapshot = try ingress.snapshot()

        await ingress.importSnapshot(snapshot)
        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(Set(importer.importedRecords.map { $0.captureID }), ["cap-a", "cap-b"])
        XCTAssertEqual(purge, .purged(count: 1), "cap-a imported and its record was removed; cap-b was unimported")
        XCTAssertEqual(filesMentioning("cap-a"), [])
        XCTAssertEqual(filesMentioning("cap-b"), [])
    }

    func testOnlyTheSnapshotIsImportedAndAValidRecordStagedAfterItStaysForTheFreshGeneration() async throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [
            FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a"),
            FakeForegroundIngressImporter.confirmation("cap-late", itemID: "item-late"),
        ]
        let (ingress, service) = try makeIngress(importer: importer)
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        let snapshot = try ingress.snapshot()
        _ = try stagingStore.stage(textRecord("cap-late"))

        await ingress.importSnapshot(snapshot)
        let purge = await ingress.purgeUnimported(from: snapshot)

        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, ["cap-a"], "a record staged after the snapshot is never imported by reset")
        XCTAssertEqual(purge, .purged(count: 0), "the only snapshot record was imported and its staging record removed")
        XCTAssertTrue(exists(recordURL("cap-late")), "the later capture is still staged, valid and unimported")
        XCTAssertEqual(try stagingStore.load(captureID: "cap-late"), textRecord("cap-late"))
    }

    func testAnInputThatCannotBeRemovedIsReportedAndRetried() async throws {
        let faults = FaultPlan()
        let (ingress, service) = try makeIngress(
            importer: FakeForegroundIngressImporter(), fileSystem: FaultingDeletionFileSystem(faults: faults))
        withExtendedLifetime(service) {}
        _ = try stagingStore.stage(textRecord("cap-a"))
        let snapshot = try ingress.snapshot()
        faults.arm("fs.remove:cap-a.json")

        let first = await ingress.purgeUnimported(from: snapshot)
        XCTAssertEqual(first, .incomplete(remaining: 1))
        XCTAssertTrue(exists(recordURL("cap-a")), "a record that could not be removed stays for the retry")

        let retried = await ingress.purgeUnimported(from: snapshot)
        XCTAssertEqual(retried, .purged(count: 1))
        XCTAssertEqual(filesMentioning("cap-a"), [])
    }

    func testAnUnreadableStagingAreaIsUnknownNeverEmpty() async throws {
        let faults = FaultPlan()
        let (ingress, service) = try makeIngress(
            importer: FakeForegroundIngressImporter(), fileSystem: FaultingDeletionFileSystem(faults: faults))
        withExtendedLifetime(service) {}
        faults.arm("fs.entryNames")
        XCTAssertThrowsError(try ingress.snapshot())

        faults.arm("fs.entryNames")
        let purge = await ingress.purgeUnimported(from: ResetStagingSnapshot(captureIDs: ["cap-a"]))
        XCTAssertEqual(purge, .unknown)
    }

    // MARK: Export temporaries

    private var exportsDirectory: URL {
        layout.directory(for: .temporaryFiles).appendingPathComponent(ExportService.exportsDirectoryName, isDirectory: true)
    }

    func testExportTemporariesAreRemovedAndAnAbsentDirectoryIsClean() throws {
        let cleaner = ExportTemporaryCleaner(layout: layout)
        XCTAssertTrue(cleaner.removeAll(), "nothing to remove is success")

        let nested = exportsDirectory.appendingPathComponent("export-1", isDirectory: true)
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        try Data("synthetic export".utf8).write(to: nested.appendingPathComponent("items.csv"))
        try Data("synthetic export".utf8).write(to: exportsDirectory.appendingPathComponent("loose.json"))
        let sibling = layout.directory(for: .temporaryFiles).appendingPathComponent("keep-me")
        try Data("not an export".utf8).write(to: sibling)

        XCTAssertTrue(cleaner.removeAll())
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: exportsDirectory.path), [])
        XCTAssertTrue(exists(sibling), "only the exports directory is touched")
    }

    // MARK: Markers

    func testTheRecordedTapIsDurableReplaceableAndIndependentOfTheOtherMarkers() throws {
        let store = FileResetMarkerStore(layout: layout)
        XCTAssertNil(try store.requestedChoice())

        try store.recordRequested(.remove)
        try store.recordRequested(.remove)
        let relaunched = FileResetMarkerStore(layout: layout)
        XCTAssertEqual(try relaunched.requestedChoice(), .remove, "the tap survives a restart")
        XCTAssertFalse(try relaunched.isRemovalRequested(), "the choice is not armed by the tap alone")
        XCTAssertFalse(try relaunched.isCleanupOutstanding(), "cleanup is not outstanding for a reset that has not started")

        try store.recordRequested(.keep)
        XCTAssertEqual(try relaunched.requestedChoice(), .keep, "the latest tap replaces the earlier one")

        try store.recordRequested(.remove)
        try Data("1".utf8).write(to: layout.directory(for: .temporaryFiles).appendingPathComponent(FileResetMarkerStore.requestedKeepFileName))
        XCTAssertEqual(try relaunched.requestedChoice(), .keep, "an interrupted replacement never resolves toward removal")

        try store.clearRequested()
        try store.clearRequested()
        XCTAssertNil(try relaunched.requestedChoice())
    }

    func testMarkersAreDurableIdempotentAndIndependent() throws {
        let store = FileResetMarkerStore(layout: layout)
        XCTAssertFalse(try store.isRemovalRequested())
        XCTAssertFalse(try store.isCleanupOutstanding())

        try store.recordRemovalRequested()
        try store.recordRemovalRequested()
        let relaunched = FileResetMarkerStore(layout: layout)
        XCTAssertTrue(try relaunched.isRemovalRequested(), "the choice survives a restart")
        XCTAssertFalse(try relaunched.isCleanupOutstanding())

        try store.recordCleanupOutstanding()
        try store.clearRemovalRequest()
        try store.clearRemovalRequest()
        XCTAssertFalse(try relaunched.isRemovalRequested())
        XCTAssertTrue(try relaunched.isCleanupOutstanding())

        try store.clearCleanupOutstanding()
        XCTAssertFalse(try relaunched.isCleanupOutstanding())
    }
}
